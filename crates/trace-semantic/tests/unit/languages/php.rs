use super::*;
use std::fs;
use trace_env::DetectContext;

#[test]
fn rule_intelephense_include_paths_vendor() {
    let registry = crate::registry::Registry::builtin();
    let entry = registry.entry("lsp:intelephense").expect("php entry");
    // The server settings take the vendor tree and PHP version from the preflight.
    assert_eq!(entry.settings["intelephense"]["environment"], "{json:php_environment}");
    assert_eq!(entry.settings["intelephense"]["stubs"], "{json:php_stubs}");
    assert_eq!(entry.initialization_options["clearCache"], false);
    assert!(entry.initialization_options["storagePath"]
        .as_str()
        .unwrap()
        .starts_with("{outside}"));
    // The licence is accepted before install (PLAN decision 11).
    let gate = entry.install.as_ref().unwrap().licence_gate.as_ref().unwrap();
    assert_eq!(gate.url, "https://intelephense.com/eula");
    assert!(gate.summary.contains("section 3"));
}

#[test]
fn rule_required_extensions_add_their_stubs() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["redis", "mongodb", "SPL"] {
        fs::create_dir_all(dir.path().join(name)).unwrap();
    }
    let exts = vec![
        "Redis".to_string(),
        "json".to_string(),
        "nope".to_string(),
        "mongodb".to_string(),
    ];
    let out = stubs(&exts, Some(dir.path()));
    assert_eq!(&out[..DEFAULT_STUBS.len()], DEFAULT_STUBS);
    assert_eq!(&out[DEFAULT_STUBS.len()..], ["mongodb".to_string(), "redis".to_string()]);
    assert_eq!(stubs(&exts, None).len(), DEFAULT_STUBS.len());
}

const COOKIE: &str = "<?php\nclass Cookie\n{\n    public function getName()\n    {\n        return 'n';\n    }\n\n    public function toArray()\n    {\n        return [];\n    }\n}\n";

/// A changed PHP file in a temp dir: (dir guard, file URI).
fn changed_file() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Cookie.php");
    fs::write(&file, COOKIE).unwrap();
    let uri = crate::lsp::path_to_uri(&file).unwrap();
    (dir, uri)
}

fn doc_answer() -> Value {
    let range =
        |a: u32, b: u32| json!({"start": {"line": a, "character": 4}, "end": {"line": b, "character": 5}});
    json!([{
        "name": "Cookie", "kind": 5, "range": range(1, 12), "selectionRange": range(1, 1),
        "children": [
            {"name": "getName", "kind": 6, "range": range(3, 6), "selectionRange": range(3, 3)},
            {"name": "toArray", "kind": 6, "range": range(8, 11), "selectionRange": range(8, 8)}
        ]
    }])
}

fn index_answer(uri: &str, name: &str, line: u32) -> Value {
    json!([
        {"name": name, "kind": 6, "containerName": "Cookie",
         "location": {"uri": uri, "range": {"start": {"line": line, "character": 4}, "end": {"line": line + 3, "character": 5}}}},
        // The same name in another file never decides anything.
        {"name": name, "kind": 6, "containerName": "Other",
         "location": {"uri": "file:///elsewhere/Other.php", "range": {"start": {"line": 40, "character": 4}, "end": {"line": 41, "character": 5}}}}
    ])
}

/// Rule (warm updates, Intelephense): after an edit the dependents are re-queried only
/// once the workspace index agrees with the changed file - a round whose index still has
/// a declaration at its old line is asked again.
#[test]
fn rule_dependents_are_requeried_after_the_server_reindexed() {
    let (_dir, uri) = changed_file();
    let probes = changed_probes(std::slice::from_ref(&uri));
    assert_eq!(probes.len(), 1);
    let names: Vec<&str> = probes[0].decls.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, vec!["getName", "toArray"]);
    assert_eq!(probes[0].decls[1].line, 8);
    let mut rounds = 0;
    let mut ask = |calls: Vec<(String, Value)>| -> Result<Vec<Result<Value, SemanticError>>, SemanticError> {
        rounds += 1;
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].0, "textDocument/documentSymbol");
        assert_eq!(calls[2].1["query"], "toArray");
        // Round 1: the index still has `toArray` one line up (before the edit).
        let to_array = if rounds == 1 { 7 } else { 8 };
        Ok(vec![
            Ok(doc_answer()),
            Ok(index_answer(&uri, "getName", 3)),
            Ok(index_answer(&uri, "toArray", to_array)),
        ])
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    wait_index_current(&probes, &mut ask, deadline, Duration::ZERO).unwrap();
    assert_eq!(rounds, 2, "the stale round is asked again");
}

/// Rule: a document answer that is itself stale (the name is not on the syntax line)
/// is not current either; answers without the name, failed requests and entries of
/// other files give no evidence; a server that never agrees is the timeout error.
#[test]
fn rule_settle_waits_for_current_answers_and_is_bounded() {
    let (_dir, uri) = changed_file();
    let probes = changed_probes(std::slice::from_ref(&uri));
    let stale_doc = json!([{"name": "toArray", "range": {"start": {"line": 7}, "end": {"line": 10}},
            "selectionRange": {"start": {"line": 7}, "end": {"line": 7}}}]);
    let answers = vec![Ok(stale_doc), Ok(json!([])), Ok(json!([]))];
    assert!(!index_current(&probes, &answers));
    let unknown = vec![
        Ok(json!([])),
        Err(SemanticError::Rpc {
            method: "workspace/symbol".into(),
            code: -32601,
            message: "no".into(),
        }),
        Ok(json!(null)),
    ];
    assert!(index_current(&probes, &unknown), "no evidence is not a stale index");
    let current = vec![
        Ok(doc_answer()),
        Ok(index_answer(&uri, "Cookie::getName", 3)),
        Ok(index_answer(&uri, "toArray()", 8)),
    ];
    assert!(index_current(&probes, &current), "qualified names match their declaration");
    let mut never =
        |_calls: Vec<(String, Value)>| -> Result<Vec<Result<Value, SemanticError>>, SemanticError> {
            Ok(vec![Ok(doc_answer()), Ok(index_answer(&uri, "getName", 2)), Ok(json!([]))])
        };
    let err = wait_index_current(&probes, &mut never, Instant::now(), Duration::ZERO).unwrap_err();
    let setup = settle_failure(err, Path::new("lsp.log"));
    assert_eq!(setup.kind(), "server_timeout");
    assert!(
        setup
            .to_string()
            .starts_with("The PHP language server did not finish in 1 minute."),
        "{setup}"
    );
    assert!(!same_name("getNames", "getName") && !same_name("xgetName", "getName"));
    assert!(changed_probes(&["file:///nowhere/readme.txt".to_string()]).is_empty());
}

/// Rule (I-33): every Composer sub-project is listed in the status notes, installed or not.
#[test]
fn rule_every_composer_subproject_is_listed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let write = |rel: &str, text: &str| {
        let p = rel.split('/').fold(root.to_path_buf(), |p, s| p.join(s));
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    };
    write("composer.json", r#"{"name":"acme/lib","require":{"php":"^8.1"}}"#);
    write("vendor-bin/cs/composer.json", r#"{"require":{"acme/cs-tool":"^3"}}"#);
    write(
        "vendor-bin/cs/vendor/composer/installed.json",
        r#"{"packages":[{"name":"acme/cs-tool"}],"dev":true}"#,
    );
    write("vendor-bin/stan/composer.json", r#"{"require":{"acme/stan-tool":"^1"}}"#);
    let platform = trace_env::os::Platform::current();
    let vars = trace_env::os::EnvVars::default();
    let dcx = DetectContext {
        root,
        platform: &platform,
        vars: &vars,
        env_override: None,
        forbidden: &[],
        files: &[],
    };
    let setup = trace_env::php::setup(&dcx, None);
    let notes = subproject_notes(root, &setup.deps.subprojects);
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(
        notes[0].starts_with("sub-project vendor-bin/cs: ") && notes[0].contains("are installed"),
        "{notes:?}"
    );
    assert!(
        notes[1].starts_with("sub-project vendor-bin/stan: ") && notes[1].contains("not installed"),
        "{notes:?}"
    );
}
