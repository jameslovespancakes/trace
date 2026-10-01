use super::client::*;
use super::contexts::*;
use super::readiness::*;
use super::transport::*;
use super::*;
use std::io::Cursor;

use serde_json::json;
use trace_core::SetupError;

use crate::registry::ConfigurationMissing;
use crate::SemanticError;

fn frame(body: &str) -> Vec<u8> {
    let mut out = Vec::new();
    write_frame(&mut out, body.as_bytes()).unwrap();
    out
}

#[test]
fn frames_round_trip_with_extra_headers() {
    let mut bytes = b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n".to_vec();
    bytes.extend(frame(r#"{"id":1}"#));
    bytes.extend(frame(r#"{"id":2,"x":"\u00e9"}"#));
    let mut cursor = Cursor::new(bytes);
    assert_eq!(read_frame(&mut cursor).unwrap().unwrap(), br#"{"id":1}"#);
    let second: Value = serde_json::from_slice(&read_frame(&mut cursor).unwrap().unwrap()).unwrap();
    assert_eq!(second["x"], "é");
    assert_eq!(read_frame(&mut cursor).unwrap(), None);
}

#[test]
fn oversized_truncated_and_malformed_frames_are_errors() {
    let big = format!("Content-Length: {}\r\n\r\n", MAX_FRAME_BYTES + 1);
    assert!(read_frame(&mut Cursor::new(big.into_bytes())).is_err());
    let truncated = b"Content-Length: 10\r\n\r\n{}".to_vec();
    assert!(read_frame(&mut Cursor::new(truncated)).is_err());
    let missing = b"X: 1\r\n\r\n".to_vec();
    assert!(read_frame(&mut Cursor::new(missing)).is_err());
    let long = vec![b'a'; MAX_HEADER_LINE as usize + 10];
    assert!(read_frame(&mut Cursor::new(long)).is_err());
    let half_header = b"Content-Length: 2\r\n".to_vec();
    assert!(read_frame(&mut Cursor::new(half_header)).is_err());
}

#[test]
fn configuration_answers_sections() {
    let settings = json!({"python": {"analysis": {"typeCheckingMode": "off"}}});
    let params = json!({"items": [{"section": "python.analysis"}, {"section": "pyright"}, {}]});
    let answer = configuration(&settings, Some(&params), ConfigurationMissing::Object);
    assert_eq!(answer[0], json!({"typeCheckingMode": "off"}));
    assert_eq!(answer[1], json!({}));
    assert_eq!(answer[2], settings);
}

/// Rule (Roslyn): a missing section is answered `null` when the entry says so (Roslyn
/// crashes on `{}`), and `|` section names are looked up literally, never split.
#[test]
fn rule_missing_configuration_section_answers_null() {
    let settings = json!({
        "csharp|background_analysis": {"dotnet_analyzer_diagnostics_scope": "none"},
        "a.b": 1,
        "a": {"b": 2}
    });
    let params = json!({"items": [
        {"section": "csharp|background_analysis"},
        {"section": "csharp|code_style.formatting"},
        {"section": "a.b"},
        {"section": "missing.section"}
    ]});
    let answer = configuration(&settings, Some(&params), ConfigurationMissing::Null);
    assert_eq!(answer[0], json!({"dotnet_analyzer_diagnostics_scope": "none"}));
    assert_eq!(answer[1], Value::Null, "a `|` name is never split on `.`");
    assert_eq!(answer[2], json!(1), "a literal key wins over the dotted path");
    assert_eq!(answer[3], Value::Null);
    let object = configuration(&settings, Some(&params), ConfigurationMissing::Object);
    assert_eq!(object[3], json!({}), "the default answer stays {{}}");
}

/// Rule: `window/showMessageRequest` is answered with an offered action only when the
/// entry lists its title as `true`; everything else gets `null` (nothing is chosen).
#[test]
fn rule_show_message_request_answers_listed_actions_only() {
    let mut policy = ServerRequestPolicy::default();
    policy.message_actions.insert("Import build".into(), true);
    policy.message_actions.insert("Never".into(), false);
    let offered = json!({"type": 3, "message": "New build files", "actions": [
        {"title": "Never"}, {"title": "Import build"}, {"title": "Not now"}
    ]});
    assert_eq!(message_action(Some(&offered), &policy), json!({"title": "Import build"}));
    let other =
        json!({"type": 3, "message": "Telemetry?", "actions": [{"title": "Yes"}, {"title": "Never"}]});
    assert_eq!(message_action(Some(&other), &policy), Value::Null);
    assert_eq!(message_action(None, &policy), Value::Null);
    assert_eq!(message_action(Some(&offered), &ServerRequestPolicy::default()), Value::Null);
}

/// Rule (R languageserver): symbol-poll readiness needs a non-zero count seen twice in a
/// row; growing counts keep waiting.
#[test]
fn rule_symbol_poll_waits_for_stable_counts() {
    let mut poll = SymbolPoll::default();
    assert!(!poll.observe(0));
    assert!(!poll.observe(0), "zero is never ready");
    assert!(!poll.observe(2));
    assert!(!poll.observe(5), "still indexing");
    assert!(poll.observe(5), "stable twice");
    let mut poll = SymbolPoll::default();
    assert!(!poll.observe(3), "one poll is not stable");
}

#[test]
fn capability_detection() {
    let caps = json!({"callHierarchyProvider": {}, "definitionProvider": false});
    assert!(capability_enabled(&caps, "callHierarchyProvider"));
    assert!(!capability_enabled(&caps, "definitionProvider"));
    assert!(!capability_enabled(&caps, "documentSymbolProvider"));
}

#[test]
fn uri_round_trip() {
    let path = if cfg!(windows) {
        PathBuf::from(r"C:\Some Dir\a b.py")
    } else {
        PathBuf::from("/some dir/a b.py")
    };
    let uri = path_to_uri(&path).unwrap();
    assert!(uri.starts_with("file:///"));
    assert_eq!(uri_to_path(&uri).unwrap(), path);
    assert!(uri_to_path("https://example.com/a.py").is_err());
    assert!(uri_to_path("file://remote/share/a.py").is_err());
    if cfg!(windows) {
        assert_eq!(
            uri_to_path("file:///c%3A/Some%20Dir/a%20b.py").unwrap(),
            PathBuf::from(r"c:\Some Dir\a b.py")
        );
    }
}

/// A tiny fake language server (run by a trusted `node`, skipped when node is absent)
/// exercising pipelining, server->client requests, RPC errors, timeouts, readiness
/// signals, crashes and shutdown. Behaviour switches come from `initializationOptions`.
const FAKE_SERVER: &str = r#"
let buf = Buffer.alloc(0);
const send = (m) => { const b = Buffer.from(JSON.stringify(m), 'utf8');
  process.stdout.write('Content-Length: ' + b.length + '\r\n\r\n'); process.stdout.write(b); };
const waiting = [];
const docs = {}; let watched = []; let init = {}; const notes = []; let opens = 0;
const progress = (token, kind) => send({jsonrpc: '2.0', method: '$/progress', params: {token, value: {kind, title: 'Indexing'}}});
function handle(msg) {
  if (msg.method && msg.id === undefined) notes.push({method: msg.method, params: msg.params ?? null});
  if (msg.method === 'textDocument/didOpen') { opens += 1; docs[msg.params.textDocument.uri] = {text: msg.params.textDocument.text, version: msg.params.textDocument.version}; return; }
  if (msg.method === 'textDocument/didChange') { docs[msg.params.textDocument.uri] = {text: msg.params.contentChanges[0].text, version: msg.params.textDocument.version}; return; }
  if (msg.method === 'textDocument/didClose') { delete docs[msg.params.textDocument.uri]; return; }
  if (msg.method === 'workspace/didChangeWatchedFiles') { watched = watched.concat(msg.params.changes); return; }
  if (msg.method === 'test/doc') { send({jsonrpc: '2.0', id: msg.id, result: {doc: docs[msg.params.uri] ?? null, watched, opens, notes: notes.map((n) => n.method), params: notes}}); return; }
  if (msg.method === 'textDocument/_vs_getProjectContexts') { send({jsonrpc: '2.0', id: msg.id, result: {_vs_defaultIndex: 0, _vs_projectContexts: [
    {_vs_label: 'App (net48)', _vs_id: 'b', _vs_kind: 1}, {_vs_label: 'App (net8.0)', _vs_id: 'a', _vs_kind: 1}, {_vs_label: 'App (netstandard2.0)', _vs_id: 'c', _vs_kind: 1}]}}); return; }
  if (msg.method === 'test/ctx') { const ctx = (msg.params.textDocument || {})._vs_projectContext; const want = msg.params.want;
    send({jsonrpc: '2.0', id: msg.id, result: (!want || (ctx && ctx._vs_label.includes(want))) ? {label: ctx ? ctx._vs_label : null} : null}); return; }
  if (msg.method === 'test/publish') { send({jsonrpc: '2.0', method: 'textDocument/publishDiagnostics', params: {uri: 'file:///ws/a.py', diagnostics: []}});
    send({jsonrpc: '2.0', method: '$/progress', params: {token: 'r', value: {kind: 'report', message: '50%'}}});
    send({jsonrpc: '2.0', method: 'window/logMessage', params: {type: 3, message: 'x'.repeat(msg.params.size || 1)}});
    send({jsonrpc: '2.0', id: msg.id, result: {}}); return; }
  if (msg.method === 'initialize') { init = (msg.params.initializationOptions) || {}; send({jsonrpc: '2.0', id: msg.id, result: {capabilities: {positionEncoding: 'utf-16', callHierarchyProvider: true}}}); }
  else if (msg.method === 'initialized') {
    if (init.progress) { send({jsonrpc: '2.0', id: 'wdp-1', method: 'window/workDoneProgress/create', params: {token: 'index'}}); progress('index', 'begin'); setTimeout(() => progress('index', 'end'), 400); }
    if (init.lateProgress) { setTimeout(() => progress('late', 'begin'), 300); setTimeout(() => progress('late', 'end'), 700); }
    if (init.neverEnd) { progress('stuck', 'begin'); }
    if (init.notify) { setTimeout(() => send({jsonrpc: '2.0', method: init.notify, params: {}}), 300); }
    if (init.log) { setTimeout(() => progress('build', 'begin'), 100); setTimeout(() => send({jsonrpc: '2.0', method: 'window/logMessage', params: {type: 3, message: 'x ' + init.log + ' y'}}), 200); setTimeout(() => progress('build', 'end'), 500); }
  }
  else if (msg.method === 'test/sync') { setTimeout(() => send({jsonrpc: '2.0', id: msg.id, result: {}}), 300); }
  else if (msg.method === 'test/crash') { process.exit(3); }
  else if (msg.method === 'test/echo') { waiting.push(msg); send({jsonrpc: '2.0', id: 'cfg-' + msg.id, method: 'workspace/configuration', params: {items: [{section: 'python.analysis'}]}}); }
  else if (msg.method === undefined && String(msg.id).startsWith('wdp-')) {}
  else if (msg.method === undefined && String(msg.id).startsWith('cfg-')) { const req = waiting.shift(); send({jsonrpc: '2.0', id: req.id, result: {params: req.params, config: msg.result}}); }
  else if (msg.method === 'test/fail') send({jsonrpc: '2.0', id: msg.id, error: {code: -32000, message: 'nope'}});
  else if (msg.method === 'test/internal') send({jsonrpc: '2.0', id: msg.id, error: {code: -32603, message: 'no source'}});
  else if (msg.method === 'test/hang') {}
  else if (msg.method === 'shutdown') send({jsonrpc: '2.0', id: msg.id, result: null});
  else if (msg.method === 'exit') process.exit(0);
}
process.stdin.on('data', (d) => {
  buf = Buffer.concat([buf, d]);
  for (;;) {
    const sep = buf.indexOf('\r\n\r\n'); if (sep < 0) return;
    const header = buf.slice(0, sep).toString('ascii');
    const len = parseInt(header.split(':')[1], 10);
    if (buf.length < sep + 4 + len) return;
    const msg = JSON.parse(buf.slice(sep + 4, sep + 4 + len).toString('utf8'));
    buf = buf.slice(sep + 4 + len);
    handle(msg);
  }
});
"#;

/// Options of a fake-server run.
struct Fake {
    timeout: Duration,
    init: Value,
    ready: ReadySpec,
    progress_grace: Duration,
    ready_timeout: Duration,
    after: Vec<(String, Value)>,
    policy: AnswerPolicy,
}

impl Default for Fake {
    fn default() -> Self {
        Fake {
            timeout: Duration::from_secs(20),
            init: Value::Null,
            ready: ReadySpec::None,
            progress_grace: Duration::ZERO,
            ready_timeout: Duration::from_secs(30),
            after: Vec::new(),
            policy: AnswerPolicy::default(),
        }
    }
}

/// Start the fake server; `None` when node is not on PATH (test skipped).
fn fake_start(fake: Fake) -> Option<(Result<LspClient, SemanticError>, PathBuf)> {
    let node = trace_env::os::find_executable(
        &["node"],
        &trace_env::lookup::path_dirs(),
        &trace_env::os::Platform::current(),
    )?;
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join("trace-semantic-tests")
        .join(format!("lsp-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = crate::tools::canonical_or_self(&dir);
    let script = dir.join("fake-server.js");
    std::fs::write(&script, FAKE_SERVER).unwrap();
    let cmd = ServerCommand {
        program: node,
        args: vec![script.display().to_string()],
        env: crate::tools::clean_env(&[], &[]).into_iter().collect(),
        cwd: dir.clone(),
    };
    let mut opts =
        ClientOptions::new(Language::Python, fake.timeout, Instant::now() + Duration::from_secs(60), 4);
    opts.initialization_options = fake.init;
    opts.settings = json!({"python": {"analysis": {"typeCheckingMode": "off"}}});
    opts.ready = fake.ready;
    opts.progress_grace = fake.progress_grace;
    opts.ready_timeout = fake.ready_timeout;
    opts.after_initialized = fake.after;
    opts.answer_policy = fake.policy;
    Some((LspClient::start(&cmd, &dir, opts), dir))
}

fn fake_client(timeout: Duration) -> Option<(LspClient, PathBuf)> {
    let (client, dir) = fake_start(Fake {
        timeout,
        ..Fake::default()
    })?;
    Some((client.unwrap(), dir))
}

#[test]
fn client_pipelines_and_answers_server_requests() {
    let Some((mut client, dir)) = fake_client(Duration::from_secs(20)) else {
        eprintln!("skipped: node not found on PATH");
        return;
    };
    assert!(capability_enabled(client.capabilities(), "callHierarchyProvider"));
    let mut calls: Vec<(String, Value)> =
        (0..20).map(|i| ("test/echo".to_string(), json!({"n": i}))).collect();
    calls.insert(7, ("test/fail".to_string(), Value::Null));
    let results = client.request_many(calls).unwrap();
    assert_eq!(results.len(), 21);
    for (i, result) in results.iter().enumerate() {
        if i == 7 {
            assert!(matches!(result, Err(SemanticError::Rpc { code: -32000, .. })));
            continue;
        }
        let n = if i < 7 { i } else { i - 1 };
        let value = result.as_ref().unwrap();
        assert_eq!(value["params"]["n"], json!(n));
        assert_eq!(value["config"][0]["typeCheckingMode"], "off");
    }
    let metrics = client.shutdown().unwrap();
    assert!(metrics.requests >= 22);
    let _ = std::fs::remove_dir_all(dir);
}

/// Documents: exact text, full-text changes, never a second `didOpen` (Roslyn crashes).
#[test]
fn client_updates_documents_for_persistent_sessions() {
    let Some((mut client, dir)) = fake_client(Duration::from_secs(20)) else {
        eprintln!("skipped: node not found on PATH");
        return;
    };
    let uri = "file:///ws/a.py";
    client.open(uri, "python", "\u{FEFF}x = 1\n").unwrap();
    let doc = client.request("test/doc", json!({"uri": uri})).unwrap();
    assert_eq!(doc["doc"], json!({"text": "x = 1\n", "version": 1}));
    client.change(uri, 2, "x = 2\n").unwrap();
    client.open(uri, "python", "x = 3\n").unwrap();
    client
        .files_changed(&[("file:///ws/b.py".to_string(), FileChange::Created)])
        .unwrap();
    let doc = client.request("test/doc", json!({"uri": uri})).unwrap();
    assert_eq!(doc["doc"], json!({"text": "x = 3\n", "version": 3}));
    assert_eq!(doc["opens"], json!(1), "one didOpen per document");
    assert_eq!(doc["watched"], json!([{"uri": "file:///ws/b.py", "type": 1}]));
    client.close_document(uri).unwrap();
    client.close_document(uri).unwrap();
    assert!(client.change(uri, 9, "x").is_err(), "no didChange for a closed document");
    client.set_deadline(Instant::now() + Duration::from_secs(30));
    let doc = client.request("test/doc", json!({"uri": uri})).unwrap();
    assert_eq!(doc["doc"], Value::Null);
    assert!(client.is_healthy());
    client.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

/// Rule: a progress entry waits up to its grace for a FIRST begin (a server that starts
/// indexing late is still waited for), then until it ended; without any progress the
/// grace alone passes and readiness is `None`.
#[test]
fn rule_progress_readiness_waits_for_first_begin() {
    let started = Instant::now();
    let Some((client, dir)) = fake_start(Fake {
        init: json!({"lateProgress": true}),
        ready: ReadySpec::Progress,
        progress_grace: Duration::from_millis(2000),
        ..Fake::default()
    }) else {
        eprintln!("skipped: node not found on PATH");
        return;
    };
    let client = client.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(650), "waited for the late token to end");
    assert_eq!(client.ready(), Some(true));
    client.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(dir);
    let (silent, dir) = fake_start(Fake {
        ready: ReadySpec::Progress,
        progress_grace: Duration::from_millis(200),
        ..Fake::default()
    })
    .unwrap();
    let silent = silent.unwrap();
    assert_eq!(silent.ready(), None, "no signal within the grace");
    silent.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(dir);
    let t = Readiness::default();
    let now = Instant::now();
    assert!(t.quiet(now + PROGRESS_SETTLE, now, PROGRESS_SETTLE));
    assert!(!t.quiet(now, now, PROGRESS_SETTLE), "settle counts from `initialized`");
    assert_eq!(combine_ready([None, Some(true), None]), Some(true));
    assert_eq!(combine_ready([None, None]), None);
}

/// Rule: a readiness wait that does not finish within the entry's timeout is the
/// setup error `server_timeout` naming the log, never partial answers.
#[test]
fn rule_readiness_timeout_is_a_setup_error() {
    let Some((client, dir)) = fake_start(Fake {
        init: json!({"neverEnd": true}),
        ready: ReadySpec::Progress,
        progress_grace: Duration::from_millis(100),
        ready_timeout: Duration::from_millis(1500),
        ..Fake::default()
    }) else {
        eprintln!("skipped: node not found on PATH");
        return;
    };
    match client {
        Err(SemanticError::Setup(SetupError::ServerTimeout {
            language,
            minutes,
            log,
        })) => {
            assert_eq!(language, Language::Python);
            assert_eq!(minutes, 1);
            assert!(log.ends_with("lsp.stderr.log"), "{log:?}");
        }
        Err(other) => panic!("expected server_timeout, got {other}"),
        Ok(_) => panic!("expected server_timeout, got a ready client"),
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// Rule: `notification` readiness waits for the named server notification.
#[test]
fn rule_notification_readiness() {
    let started = Instant::now();
    let Some((client, dir)) = fake_start(Fake {
        init: json!({"notify": "workspace/projectInitializationComplete"}),
        ready: ReadySpec::Notification {
            method: "workspace/projectInitializationComplete".into(),
        },
        ..Fake::default()
    }) else {
        eprintln!("skipped: node not found on PATH");
        return;
    };
    let client = client.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(250));
    assert_eq!(client.ready(), Some(true));
    assert!(client
        .notifications()
        .iter()
        .any(|(m, _)| m == "workspace/projectInitializationComplete"));
    client.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

/// Rule: `request` readiness sends the request and is ready when it is answered.
#[test]
fn rule_request_readiness() {
    let started = Instant::now();
    let Some((client, dir)) = fake_start(Fake {
        ready: ReadySpec::Request {
            method: "test/sync".into(),
            params: json!({"index": true}),
        },
        ..Fake::default()
    }) else {
        eprintln!("skipped: node not found on PATH");
        return;
    };
    let client = client.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(250), "the answer came after 300 ms");
    assert_eq!(client.ready(), Some(true));
    client.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

/// Rule: `log` readiness waits for the log text, then for running progress to end.
#[test]
fn rule_log_readiness_then_progress() {
    let started = Instant::now();
    let Some((client, dir)) = fake_start(Fake {
        init: json!({"log": "Compile took"}),
        ready: ReadySpec::Log {
            contains: "Compile took".into(),
        },
        ..Fake::default()
    }) else {
        eprintln!("skipped: node not found on PATH");
        return;
    };
    let client = client.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(500), "progress ended at 500 ms");
    assert!(client
        .log_messages()
        .iter()
        .any(|(t, m)| *t == 3 && m.contains("Compile took")));
    assert_eq!(client.ready(), Some(true));
    client.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

/// Rule: the server process exiting while in use is the setup error `server_crashed`
/// naming its log; the exit status is appended to that log.
#[test]
fn rule_server_exit_is_a_crash_with_log() {
    let Some((mut client, dir)) = fake_client(Duration::from_secs(20)) else {
        eprintln!("skipped: node not found on PATH");
        return;
    };
    let err = client.request("test/crash", Value::Null).unwrap_err();
    let SemanticError::Setup(SetupError::ServerCrashed { language, log }) = err else {
        panic!("expected server_crashed, got {err}");
    };
    assert_eq!(language, Language::Python);
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("trace: language server closed its output"), "{text}");
    assert!(!client.is_healthy());
    drop(client);
    let _ = std::fs::remove_dir_all(dir);
}

/// Rule: `after_initialized` notifications are sent after `initialized` (Roslyn
/// `solution/open`), with their params unchanged.
#[test]
fn rule_after_initialized_notifications_are_sent() {
    let Some((client, dir)) = fake_start(Fake {
        after: vec![("solution/open".into(), json!({"solution": "file:///ws/app.sln"}))],
        ..Fake::default()
    }) else {
        eprintln!("skipped: node not found on PATH");
        return;
    };
    let mut client = client.unwrap();
    let doc = client.request("test/doc", json!({"uri": "none"})).unwrap();
    let methods: Vec<&str> = doc["notes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let at = |m: &str| {
        methods
            .iter()
            .position(|x| *x == m)
            .unwrap_or_else(|| panic!("{m} in {methods:?}"))
    };
    assert!(at("initialized") < at("solution/open"));
    let params = doc["params"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["method"] == "solution/open")
        .unwrap();
    assert_eq!(params["params"], json!({"solution": "file:///ws/app.sln"}));
    client.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

/// Rule (jdtls): `InternalError` answers are unresolved (`null`) when the entry says so.
#[test]
fn internal_errors_are_unresolved_per_answer_policy() {
    let policy = AnswerPolicy {
        internal_error_is_unresolved: true,
        ..AnswerPolicy::default()
    };
    let Some((client, dir)) = fake_start(Fake {
        policy,
        ..Fake::default()
    }) else {
        eprintln!("skipped: node not found on PATH");
        return;
    };
    let mut client = client.unwrap();
    assert_eq!(client.request("test/internal", Value::Null).unwrap(), Value::Null);
    client.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(dir);
    let (client, dir) = fake_start(Fake::default()).unwrap();
    let mut client = client.unwrap();
    assert!(matches!(
        client.request("test/internal", Value::Null),
        Err(SemanticError::Rpc { code: -32603, .. })
    ));
    client.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn client_request_timeout_is_an_error() {
    let Some((mut client, dir)) = fake_client(Duration::from_millis(300)) else {
        eprintln!("skipped: node not found on PATH");
        return;
    };
    let started = Instant::now();
    let err = client.request("test/hang", Value::Null).unwrap_err();
    assert!(matches!(err, SemanticError::Timeout { .. }));
    assert!(started.elapsed() < Duration::from_secs(10));
    drop(client);
    let _ = std::fs::remove_dir_all(dir);
}

/// Rule: every server-fed list is bounded by count and bytes; the first entries stay for
/// good and later ones roll (the latest stay readable); a non-rolling list stops growing;
/// long texts are cut at a character boundary.
#[test]
fn rule_session_logs_are_bounded() {
    let mut kept: Kept<u32> = Kept::new(8, 1000, true);
    for i in 0..100u32 {
        kept.push(i, 10);
    }
    let items = kept.as_slice();
    assert!(items.len() <= 8, "{items:?}");
    assert_eq!(&items[..4], &[0, 1, 2, 3], "the first entries stay");
    assert_eq!(items.last(), Some(&99), "the latest entry is kept");
    let mut bytes: Kept<u32> = Kept::new(1000, 100, true);
    for i in 0..50u32 {
        bytes.push(i, 20);
    }
    assert!(bytes.bytes <= 100, "{}", bytes.bytes);
    assert_eq!(bytes.as_slice().first(), Some(&0));
    assert_eq!(bytes.as_slice().last(), Some(&49));
    bytes.push(1000, 60);
    assert!(!bytes.as_slice().contains(&1000), "an item over half the byte bound is not kept");
    let mut first: Kept<u32> = Kept::new(4, 1000, false);
    for i in 0..10u32 {
        first.push(i, 1);
    }
    assert_eq!(first.as_slice(), &[0, 1, 2, 3], "a non-rolling list keeps its first entries");
    assert_eq!(cut_text("aé", 2), "a", "never inside a character");
    assert_eq!(cut_text("abc", 10), "abc");
    let mut message = json!({"jsonrpc": "2.0", "method": "window/logMessage", "params": {"type": 3, "message": "y".repeat(MAX_MESSAGE_BYTES + 5)}});
    assert!(keep_queued(&mut message, true));
    assert_eq!(message["params"]["message"].as_str().unwrap().len(), MAX_MESSAGE_BYTES);
    let mut response = json!({"id": 3, "result": null});
    assert!(keep_queued(&mut response, false), "responses are always queued");
    let mut readiness = Readiness::default();
    let now = Instant::now();
    for i in 0..(MAX_ENDED_TOKENS + 50) {
        let token = format!("t{i}");
        readiness.observe(
            "$/progress",
            Some(&json!({"token": token.as_str(), "value": {"kind": "begin"}})),
            now,
        );
        readiness.observe(
            "$/progress",
            Some(&json!({"token": token.as_str(), "value": {"kind": "end"}})),
            now,
        );
    }
    assert!(readiness.ended("t0").is_none(), "the oldest ended tokens are forgotten");
    assert_eq!(readiness.ended(&format!("t{}", MAX_ENDED_TOKENS + 49)), Some(""));
    assert!(!readiness.busy());
}

/// Rule: diagnostics are kept for the load checks and the warm-up only; after the process
/// was warmed up they are dropped before they are queued. Progress reports are observed,
/// never kept; long log texts are kept cut.
#[test]
fn rule_diagnostics_after_warm_up_are_not_kept() {
    let Some((mut client, dir)) = fake_client(Duration::from_secs(20)) else {
        eprintln!("skipped: node not found on PATH");
        return;
    };
    client.request("test/publish", json!({"size": 10})).unwrap();
    assert_eq!(client.diagnostics().len(), 1);
    assert!(!client.notifications().iter().any(|(m, _)| m == "$/progress"), "reports are not kept");
    assert!(client.log_messages().iter().any(|(_, t)| t == "xxxxxxxxxx"));
    client.mark_warmed();
    client
        .request("test/publish", json!({"size": MAX_MESSAGE_BYTES + 10}))
        .unwrap();
    assert_eq!(client.diagnostics().len(), 1, "no diagnostics kept after the warm-up");
    assert!(
        client
            .log_messages()
            .iter()
            .any(|(_, t)| t.len() == MAX_MESSAGE_BYTES),
        "long text kept cut"
    );
    client.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

/// Rule: clangd's inactive regions are kept as the latest notification per document,
/// outside the rolling bound of the other notifications (a big repository never rolls
/// them out), in arrival order.
#[test]
fn rule_inactive_regions_are_kept_per_document() {
    let Some((mut client, dir)) = fake_client(Duration::from_secs(20)) else {
        eprintln!("skipped: node not found on PATH");
        return;
    };
    let regions = |uri: &str, line: u64| json!({"params": {"textDocument": {"uri": uri}, "regions": [{"start": {"line": line, "character": 0}, "end": {"line": line + 1, "character": 0}}]}});
    client.notification("textDocument/inactiveRegions".into(), regions("file:///a.c", 1));
    client.notification("textDocument/inactiveRegions".into(), regions("file:///b.c", 2));
    for i in 0..(MAX_KEPT as u64 + 10) {
        client.notification("test/other".into(), json!({"params": {"i": i}}));
    }
    client.notification("textDocument/inactiveRegions".into(), regions("file:///a.c", 7));
    let kept = client.notifications_named("textDocument/inactiveRegions");
    assert_eq!(kept.len(), 2, "one per document");
    assert_eq!(kept[0]["textDocument"]["uri"], "file:///b.c");
    assert_eq!(kept[1]["regions"][0]["start"]["line"], 7, "the latest of a.c, last");
    assert!(!client
        .notifications()
        .iter()
        .any(|(m, _)| m == "textDocument/inactiveRegions"));
    assert!(client.notifications_named("test/other").len() <= MAX_KEPT);
    client.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

/// Rule (Roslyn): the project contexts of a file are ordered deterministically, newest
/// target framework first, whatever order (and default index) the server answers with.
#[test]
fn rule_project_context_is_chosen_deterministically() {
    let ctx = |label: &str, id: &str| json!({"_vs_label": label, "_vs_id": id, "_vs_kind": 1});
    let lists = [
        vec![
            ctx("Lib (net45)", "1"),
            ctx("Lib (netstandard2.0)", "2"),
            ctx("Lib (net6.0)", "3"),
            ctx("Lib (net10.0)", "4"),
        ],
        vec![
            ctx("Lib (net10.0)", "4"),
            ctx("Lib (net6.0)", "3"),
            ctx("Lib (net45)", "1"),
            ctx("Lib (netstandard2.0)", "2"),
        ],
    ];
    let labels = |list: &[Value]| -> Vec<String> {
        order_project_contexts(&json!({"_vs_projectContexts": list, "_vs_defaultIndex": 2}))
            .iter()
            .map(|c| c["_vs_label"].as_str().unwrap().to_string())
            .collect()
    };
    let expected = vec!["Lib (net10.0)", "Lib (net6.0)", "Lib (netstandard2.0)", "Lib (net45)"];
    for list in &lists {
        assert_eq!(labels(list), expected);
    }
    let mixed = vec![
        json!({"_vs_label": "Misc", "_vs_id": "m", "_vs_is_miscellaneous": true}),
        ctx("App (netcoreapp3.1)", "x"),
        ctx("App (net462)", "y"),
        ctx("App (net48)", "z"),
        ctx("App (net8.0-windows)", "w"),
    ];
    assert_eq!(
        labels(&mixed),
        vec![
            "App (net8.0-windows)",
            "App (netcoreapp3.1)",
            "App (net48)",
            "App (net462)",
            "Misc"
        ]
    );
    assert!(order_project_contexts(&Value::Null).is_empty());
}

/// Rule (Roslyn): requests on an open document carry its first project context; an
/// empty answer is asked again in the next contexts (the first non-empty one is used);
/// documents that are not open carry none.
#[test]
fn rule_requests_carry_the_project_context() {
    let policy = AnswerPolicy {
        project_contexts: true,
        ..AnswerPolicy::default()
    };
    let Some((client, dir)) = fake_start(Fake {
        policy,
        ..Fake::default()
    }) else {
        eprintln!("skipped: node not found on PATH");
        return;
    };
    let mut client = client.unwrap();
    let uri = "file:///ws/Reader.Async.cs";
    client.open(uri, "csharp", "class A {}\n").unwrap();
    let first = client
        .request("test/ctx", json!({"textDocument": {"uri": uri}}))
        .unwrap();
    assert_eq!(first["label"], "App (net8.0)", "newest target framework first");
    let retried = client
        .request("test/ctx", json!({"textDocument": {"uri": uri}, "want": "net48"}))
        .unwrap();
    assert_eq!(retried["label"], "App (net48)", "an empty answer is asked in the next contexts");
    let nowhere = client
        .request("test/ctx", json!({"textDocument": {"uri": uri}, "want": "net9.9"}))
        .unwrap();
    assert_eq!(nowhere, Value::Null, "empty in every context stays empty");
    let closed = client
        .request("test/ctx", json!({"textDocument": {"uri": "file:///ws/B.cs"}}))
        .unwrap();
    assert_eq!(closed["label"], Value::Null, "no context for a document that is not open");
    client.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

/// Rule: a server crash log names the documents opened / queried last, the last first.
#[test]
fn rule_crash_log_names_the_last_documents() {
    let Some((mut client, dir)) = fake_client(Duration::from_secs(20)) else {
        eprintln!("skipped: node not found on PATH");
        return;
    };
    client.open("file:///ws/first.go", "go", "package a\n").unwrap();
    client
        .open("file:///ws/multipart_test.go", "go", "package b\n")
        .unwrap();
    let err = client
        .request("test/crash", json!({"textDocument": {"uri": "file:///ws/multipart_test.go"}}))
        .unwrap_err();
    let SemanticError::Setup(SetupError::ServerCrashed { log, .. }) = err else {
        panic!("expected server_crashed, got {err}");
    };
    let text = std::fs::read_to_string(&log).unwrap();
    let line = text
        .lines()
        .find(|l| l.starts_with("trace: documents analysed last"))
        .unwrap_or_else(|| panic!("{text}"));
    let last = line.find("multipart_test.go").expect("names the last document");
    let earlier = line.find("first.go").expect("names the earlier document");
    assert!(last < earlier, "{line}");
    drop(client);
    let _ = std::fs::remove_dir_all(dir);
}
