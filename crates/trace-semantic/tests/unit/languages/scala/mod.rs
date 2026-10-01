use super::*;
use crate::languages::jvm::{classify_load, path_text, LoadOutcome};
use crate::languages::{Prepared, Server};
use crate::registry::{Recipe, Registry};
use crate::test_support::setup::{assert_placeholders_filled, items, with_context, write};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use trace_core::setup_error::SetupError;
use trace_core::Language;
use trace_env::jvm::{self, BuildSystem};
use trace_env::os::Arch;
use trace_env::os::{EnvVars, Os, Platform};

const CROSS_BUILD: &str = "val Scala212V: String = \"2.12.21\"\nval Scala213V: String = \"2.13.18\"\nval Scala3V: String = \"3.3.8\"\nThisBuild / crossScalaVersions := List(Scala3V, Scala212V, Scala213V)\nThisBuild / scalaVersion := Scala213V\n";

fn platform(os: Os) -> Platform {
    Platform {
        os,
        arch: Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    }
}

#[test]
fn rule_metals_own_tool_artifacts_are_not_project_dependencies() {
    let texts = vec![
            "Could not resolve platform artifacts: coursierapi.error.SimpleResolutionError$1: Error downloading ch.epfl.scala:bloop-native-bridge-0-5_2.12:2.1.2\n  not found: x".to_string(),
            "Error downloading Dependency(com.sourcegraph:semanticdb-javac, 0.12.3)".to_string(),
            "Error downloading org.example:core_2.13:2.13.0".to_string(),
        ];
    let (notes, rest) = split_tool_artifact_messages(texts);
    assert_eq!(notes.len(), 2);
    assert!(notes[0].starts_with("Could not resolve platform artifacts"));
    // Negative: a project dependency stays a dependency signal.
    assert_eq!(rest, vec!["Error downloading org.example:core_2.13:2.13.0".to_string()]);
    assert_eq!(classify_load(&rest), LoadOutcome::DepsMissing);
}

#[test]
fn rule_metals_import_build_is_answered() {
    let entry = Registry::builtin().entry("lsp:metals").cloned().unwrap();
    let actions = &entry.server_requests.message_actions;
    assert_eq!(actions.get("Import build"), Some(&true));
    assert_eq!(actions.get("Connect"), Some(&true));
    assert_eq!(actions.get("Reconnect"), Some(&true));
    // Everything else (scalafmt edits, starting the HTTP server) is declined.
    assert!(!actions.contains_key("Not now"));
    assert!(actions.values().all(|v| *v));
    assert_eq!(Hooks.answer_policy().retry_cancelled, 3);
}

#[test]
fn rule_other_scala_version_sources_are_outside_build() {
    let reason = Hooks
            .outside_build(
                "modules/core/shared/src/main/scala-3/io/example/Derivation.scala",
                "no build target for: file:///ws/modules/core/shared/src/main/scala-3/io/example/Derivation.scala",
            )
            .unwrap();
    assert!(reason.contains("Scala 3"), "{reason}");
    assert_eq!(
        Hooks
            .outside_build("a/src/main/scala-2.13+/X.scala", "No build target found")
            .as_deref(),
        Some("only built for Scala 2.13 and newer (the build imports its default Scala version)")
    );
    // Shared sources and other errors are not "outside the build".
    assert!(Hooks
        .outside_build("a/src/main/scala/X.scala", "no build target for: x")
        .is_none());
    assert!(Hooks
        .outside_build("a/src/main/scala-3/X.scala", "request cancelled")
        .is_none());
}

#[test]
fn rule_bloop_socket_path_is_short() {
    let short = Path::new("C:/Users/u/AppData/Local/trace/jh");
    let local = absolute_local();
    let windows_user = EnvVars::from_pairs(&[("LOCALAPPDATA", local.to_str().unwrap())]);
    assert!(socket_error(short, &platform(Os::Windows), &windows_user).is_none());
    let long = PathBuf::from(format!("/home/{}/cache/jh", "x".repeat(80)));
    let err = socket_error(&long, &platform(Os::Linux), &EnvVars::default()).unwrap();
    assert_eq!(err.kind(), "unsupported");
    assert!(err.lines()[0].starts_with("The Scala build server cannot start: its folder path is too long ("));
    assert_eq!(
        err.lines()[1],
        "       Set a shorter trace cache folder (TRACE_CACHE_DIR) and run trace again."
    );
    // The Metals JVM gets exactly this home (argument file).
    let args = metals_argfile(
        short,
        Path::new("C:/tools/metals/1.6.9/cache"),
        &[PathBuf::from(r"C:\t\a.jar"), PathBuf::from("C:/t/b.jar")],
        &platform(Os::Windows),
    );
    assert!(args.contains("-Duser.home=\"C:/Users/u/AppData/Local/trace/jh\""), "{args}");
    assert!(args.contains("-cp \"C:/t/a.jar;C:/t/b.jar\""), "{args}");
}

#[test]
fn rule_bloop_socket_path_is_checked_where_bloop_puts_it() {
    // Windows: the user's LocalAppData (Metals asks Windows for the known folder), never
    // the short home - a long trace cache folder does not matter there.
    let long_home = PathBuf::from(format!("C:/cache/{}/jh", "x".repeat(90)));
    let local = absolute_local();
    let windows_user = EnvVars::from_pairs(&[("LOCALAPPDATA", local.to_str().unwrap())]);
    let socket = bloop_socket_path(&long_home, &platform(Os::Windows), &windows_user);
    assert!(socket.starts_with(&local), "{}", socket.display());
    assert!(socket.ends_with(
        Path::new("ScalaCli")
            .join("data")
            .join("bloop")
            .join("daemon")
            .join("socket")
    ));
    assert!(socket_error(&long_home, &platform(Os::Windows), &windows_user).is_none());
    // A Windows user folder too long for a socket: its own advice (the cache folder is not it).
    let long_profile = absolute_local().join("u".repeat(80));
    let long_user = EnvVars::from_pairs(&[("USERPROFILE", long_profile.to_str().unwrap())]);
    let err = socket_error(Path::new("C:/t/jh"), &platform(Os::Windows), &long_user).unwrap();
    assert!(err.lines()[1].contains("Windows user profile"), "{:?}", err.lines());
    // Linux / macOS: below the short home (the Metals JVM's user.home, XDG_DATA_HOME).
    let home = Path::new("/c/trace/jh");
    assert_eq!(
        bloop_socket_path(home, &platform(Os::Linux), &windows_user),
        home.join(".local")
            .join("share")
            .join("scalacli")
            .join("bloop")
            .join("daemon")
            .join("socket")
    );
    assert_eq!(
        bloop_socket_path(home, &platform(Os::MacOs), &EnvVars::default()),
        home.join("Library")
            .join("Application Support")
            .join("ScalaCli")
            .join("bloop")
            .join("daemon")
            .join("socket")
    );
}

/// An absolute LocalAppData-like folder on the host running the test.
fn absolute_local() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from("C:\\Users\\u\\AppData\\Local")
    } else {
        PathBuf::from("/Users/u/AppData/Local")
    }
}

#[test]
fn rule_gradle_and_bloop_run_inside_trace_folders_without_daemons() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "build.sbt", "scalaVersion := \"2.13.18\"\n");
    let files = [("src/main/scala/A.scala", Language::Scala)];
    let short = PathBuf::from("/c/jh");
    let prepared = with_context("lsp:metals", tmp.path(), &files, true, |cx| {
        let readable = trace_core::paths::forbidden_roots();
        let setup = jvm::setup(&crate::languages::read_context(cx, trace_env::EcosystemId::Jvm, &readable));
        scala_prepared(cx, &setup, &scala_systems(&setup), None, Vec::new(), None, &[], short.clone())
    });
    // sbt runs its batch command without a server; its user home / global base are trace's.
    let sbt = &prepared.env["SBT_OPTS"];
    assert!(sbt.contains("-Dsbt.server.autostart=false"), "{sbt}");
    assert!(sbt.contains(&format!("-Duser.home={}", path_text(&short))), "{sbt}");
    // The Metals JVM's home is the short home.
    let data = scala_data(&prepared).unwrap();
    assert!(data.argfile.contains("-Duser.home=\"/c/jh\""), "{}", data.argfile);
    // Linux: Metals' data folders (Bloop's daemon folder) below the short home too.
    let linux: BTreeMap<&str, String> = metals_data_env(&short, &platform(Os::Linux)).into_iter().collect();
    assert_eq!(linux["XDG_DATA_HOME"], path_text(&short.join(".local").join("share")));
    assert!(linux.contains_key("XDG_CACHE_HOME") && linux.contains_key("XDG_CONFIG_HOME"));
    // Negative: macOS / Windows do not read them.
    assert!(metals_data_env(&short, &platform(Os::Windows)).is_empty());
    assert!(metals_data_env(&short, &platform(Os::MacOs)).is_empty());
    let host = trace_env::os::Platform::current();
    assert_eq!(prepared.env.contains_key("XDG_DATA_HOME"), host.os == Os::Linux);
}

#[test]
fn rule_sources_of_another_scala_version_are_outside_build() {
    let prepared = |build: BuildSystem, version: Option<&str>| Prepared {
        data: Some(Arc::new(ScalaData {
            build,
            argfile: String::new(),
            short_home: PathBuf::new(),
            sbt_global_plugin: None,
            build_scala: version.map(str::to_string),
        })),
        ..Prepared::default()
    };
    let p = prepared(BuildSystem::Sbt, Some("2.13.18"));
    let outside = |path: &str| Hooks.outside_build_file(path, &p);
    let reason = outside("modules/core/shared/src/main/scala-3/io/example/Derivation.scala").unwrap();
    assert_eq!(reason, "only built for Scala 3 (the build imports Scala 2.13.18)");
    assert!(outside("a/src/main/scala-2.12/X.scala").is_some());
    assert!(outside("a/src/test/scala-2.12-/X.scala").is_some());
    assert!(outside("a/target/scala-2.12/src_managed/main/X.scala").is_some());
    // Folders of the imported version, shared folders and undecidable boundaries are inside.
    for inside in [
        "a/src/main/scala/X.scala",
        "a/src/main/scala-2/X.scala",
        "a/src/main/scala-2.13/X.scala",
        "a/src/main/scala-2.13+/X.scala",
        "a/src/main/scala-2.x/X.scala",
        "a/src/main/scala-2.13-/X.scala",
        "a/target/scala-2.13/src_managed/main/X.scala",
        // A module folder named like a version folder is not one.
        "modules/scala-3/src/main/scala/X.scala",
        "scala-3/X.scala",
    ] {
        assert_eq!(outside(inside), None, "{inside}");
    }
    // Scala 3 builds: `scala-2.13+` holds Scala 3 sources too; `scala-2` does not.
    let s3 = prepared(BuildSystem::Sbt, Some("3.3.8"));
    assert!(Hooks
        .outside_build_file("a/src/main/scala-2.13+/X.scala", &s3)
        .is_none());
    assert!(Hooks.outside_build_file("a/src/main/scala-2/X.scala", &s3).is_some());
    assert!(Hooks
        .outside_build_file("a/target/scala-3.3.8/src_managed/X.scala", &s3)
        .is_none());
    // Negative: unknown build version, other build tools -> never claimed.
    assert!(Hooks
        .outside_build_file("a/src/main/scala-3/X.scala", &prepared(BuildSystem::Sbt, None))
        .is_none());
    assert!(Hooks
        .outside_build_file("a/src/main/scala-3/X.scala", &prepared(BuildSystem::Mill, Some("2.13.18")))
        .is_none());
    assert!(Hooks
        .outside_build_file("a/src/main/scala-3/X.scala", &Prepared::default())
        .is_none());
}

/// Lines of a metals.log entry (`yyyy.mm.dd hh:mm:ss LEVEL message`).
fn log_line(level: &str, message: &str) -> String {
    format!("2026.01.02 03:04:05 {level:<5} {message}\n")
}

#[test]
fn rule_failed_metals_import_is_a_build_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("metals.log");
    // The previous process failed; the current one is still importing: not a failure.
    let mut text = log_line("INFO", "Started: Metals version 1.6.9 in folders 'x' for client trace 0.1.0.");
    text += &log_line("ERROR", "sbt command failed: sbt -Dbloop.export-jar-classifiers=sources bloopInstall");
    text += &log_line("INFO", "Started: Metals version 1.6.9 in folders 'x' for client trace 0.1.0.");
    text += &log_line("INFO", "running 'sbt bloopInstall'");
    std::fs::write(&path, &text).unwrap();
    let mut log = MetalsLog::new(path.clone());
    log.poll();
    assert_eq!(log.failed, None);
    assert!(!log.indexed);
    // Its import fails: the failure is seen at once, with sbt's output kept for the cause.
    text += &log_line(
        "INFO",
        "[error] sbt.librarymanagement.ResolveException: Error downloading com.example:lib_2.13:1.0",
    );
    text += "  not found: https://repo1.maven.org/maven2/com/example/lib_2.13/1.0/lib_2.13-1.0.pom\n";
    text += &log_line("ERROR", "sbt command failed: sbt -Dbloop.export-jar-classifiers=sources bloopInstall");
    std::fs::write(&path, &text).unwrap();
    log.poll();
    let failure = log.failed.clone().unwrap();
    assert!(failure.starts_with("sbt command failed:"), "{failure}");
    let texts: Vec<String> = log.entries.iter().cloned().collect();
    // A project artifact missing offline is the dependency error.
    assert_eq!(
        import_error(BuildSystem::Sbt, &failure, &texts, path.clone()),
        SetupError::DepsMissing {
            language: Language::Scala,
            hint: "sbt update".to_string()
        }
    );
    // Without dependency signals it is the build error naming Metals' message.
    let err = import_error(BuildSystem::Sbt, &failure, std::slice::from_ref(&failure), path.clone());
    assert_eq!(err.kind(), "build_failed");
    // Metals' own sbt plugin missing is the missing-parts error, not the project's.
    let plugin =
        vec!["Error downloading ch.epfl.scala:sbt-bloop;sbtVersion=1.0;scalaVersion=2.12:2.1.2".to_string()];
    assert_eq!(import_error(BuildSystem::Sbt, &failure, &plugin, path.clone()).kind(), "unsupported");
    // Tool artifacts of a healthy import are never a failure or a dependency signal.
    assert!(!is_import_failure("Stopped configuration of Java SemanticDB in projects: Error downloading com.sourcegraph:semanticdb-javac:0.12.3"));
    assert!(!is_import_failure("time: ran 'sbt bloopInstall' in 1m52s"));
    assert!(is_import_failure(
        "Import project failed, no functionality will work. See the logs for more details"
    ));
    assert_eq!(
        import_failure_message(&[(
            1,
            "Import project failed, no functionality will work.\nmore".to_string()
        )])
        .as_deref(),
        Some("Import project failed, no functionality will work.")
    );
    // A finished index of the current process.
    text += &log_line("INFO", "Started: Metals version 1.6.9 in folders 'x' for client trace 0.1.0.");
    text += &log_line("INFO", "time: indexed workspace in 21s");
    std::fs::write(&path, &text).unwrap();
    log.poll();
    assert!(log.indexed);
    assert_eq!(log.failed, None);
}

/// A fake Metals (node) that writes `.metals/metals.log` in its working directory like
/// Metals: the start line, then per `init.mode` a failed import, or the index followed by a
/// compile reported as work-done progress (`compile`, ended after 1.5 s).
const FAKE_METALS: &str = r#"
const fs = require('fs'); const path = require('path');
let buf = Buffer.alloc(0); let mode = 'ok';
const log = path.join(process.cwd(), '.metals', 'metals.log');
function line(level, text) { fs.appendFileSync(log, '2026.01.02 03:04:05 ' + level.padEnd(5) + ' ' + text + '\n'); }
function send(m) { const s = JSON.stringify(m); process.stdout.write('Content-Length: ' + Buffer.byteLength(s) + '\r\n\r\n' + s); }
function progress(kind) { send({jsonrpc: '2.0', method: '$/progress', params: {token: 'compile', value: {kind: kind, title: 'Compiling'}}}); }
function handle(msg) {
  if (msg.method === 'initialize') {
    mode = (msg.params.initializationOptions || {}).mode || 'ok';
    fs.mkdirSync(path.dirname(log), {recursive: true});
    line('INFO', 'Started: Metals version 1.6.9 in folders ' + process.cwd() + ' for client trace 0.1.0.');
    send({jsonrpc: '2.0', id: msg.id, result: {capabilities: {}}});
  } else if (msg.method === 'initialized') {
    if (mode === 'fail') {
      setTimeout(() => line('ERROR', 'sbt command failed: sbt -Dbloop.export-jar-classifiers=sources bloopInstall'), 300);
    } else {
      setTimeout(() => line('INFO', 'time: indexed workspace in 1s'), 300);
      setTimeout(() => progress('begin'), 600);
      setTimeout(() => progress('end'), 2100);
    }
  } else if (msg.method === 'shutdown') send({jsonrpc: '2.0', id: msg.id, result: null});
  else if (msg.method === 'exit') process.exit(0);
  else if (msg.id !== undefined) send({jsonrpc: '2.0', id: msg.id, result: null});
}
process.stdin.on('data', (d) => {
  buf = Buffer.concat([buf, d]);
  for (;;) {
    const sep = buf.indexOf('\r\n\r\n'); if (sep < 0) return;
    const len = parseInt(buf.slice(0, sep).toString('ascii').split(':')[1], 10);
    if (buf.length < sep + 4 + len) return;
    const msg = JSON.parse(buf.slice(sep + 4, sep + 4 + len).toString('utf8'));
    buf = buf.slice(sep + 4 + len);
    handle(msg);
  }
});
"#;

/// Start the fake Metals in a fresh workspace; `None` without node on PATH (skipped).
fn fake_metals(mode: &str) -> Option<(crate::lsp::LspClient, PathBuf)> {
    let node = trace_env::os::find_executable(
        &["node"],
        &trace_env::lookup::path_dirs(),
        &trace_env::os::Platform::current(),
    )?;
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join("trace-semantic-tests")
        .join(format!("metals-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = crate::tools::canonical_or_self(&dir);
    let script = dir.join("fake-metals.js");
    std::fs::write(&script, FAKE_METALS).unwrap();
    let cmd = crate::lsp::ServerCommand {
        program: node,
        args: vec![script.display().to_string()],
        env: crate::tools::clean_env(&[], &[]).into_iter().collect(),
        cwd: dir.clone(),
    };
    let mut opts = crate::lsp::ClientOptions::new(
        Language::Scala,
        Duration::from_secs(20),
        Instant::now() + Duration::from_secs(60),
        4,
    );
    opts.initialization_options = serde_json::json!({ "mode": mode });
    opts.ready_timeout = Duration::from_secs(30);
    let client = crate::lsp::LspClient::start(&cmd, &dir, opts).unwrap();
    Some((client, dir))
}

fn sbt_prepared() -> Prepared {
    Prepared {
        data: Some(Arc::new(ScalaData {
            build: BuildSystem::Sbt,
            argfile: String::new(),
            short_home: PathBuf::new(),
            sbt_global_plugin: None,
            build_scala: Some("2.13.18".to_string()),
        })),
        ..Prepared::default()
    }
}

#[test]
fn rule_metals_readiness_waits_for_the_first_compile() {
    let Some((mut client, dir)) = fake_metals("ok") else {
        return;
    };
    let started = Instant::now();
    wait_for_metals(&mut client, &sbt_prepared(), &dir, Duration::from_secs(30)).unwrap();
    // The compile the fake began after the index ended before the wait returned.
    assert_eq!(client.wait_progress("compile", Duration::ZERO).unwrap().as_deref(), Some(""));
    assert!(started.elapsed() >= Duration::from_millis(1500), "{:?}", started.elapsed());
    let _ = client.shutdown();
}

#[test]
fn rule_failed_metals_import_stops_at_once_not_at_the_readiness_limit() {
    let Some((mut client, dir)) = fake_metals("fail") else {
        return;
    };
    let started = Instant::now();
    let err = wait_for_metals(&mut client, &sbt_prepared(), &dir, Duration::from_secs(600)).unwrap_err();
    assert_eq!(err.kind(), "build_failed");
    assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
    // The error points to Metals' own log.
    assert!(err.lines().iter().any(|l| l.contains("metals.log")), "{:?}", err.lines());
    let _ = client.shutdown();
}

#[test]
fn rule_scala_parts_are_never_fetched_at_analysis_time() {
    // The launch is offline: Coursier never downloads during analysis.
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "build.sbt", CROSS_BUILD);
    write(tmp.path(), "project/build.properties", "sbt.version=1.12.13\n");
    let files = [("modules/core/src/main/scala/A.scala", Language::Scala)];
    let prepared = with_context("lsp:metals", tmp.path(), &files, true, |cx| {
        let readable = trace_core::paths::forbidden_roots();
        let dcx = crate::languages::read_context(cx, trace_env::EcosystemId::Jvm, &readable);
        let setup = jvm::setup(&dcx);
        let tools = PathBuf::from("/tools/metals/1.6.9");
        scala_prepared(
            cx,
            &setup,
            &scala_systems(&setup),
            None,
            Vec::new(),
            Some(&tools),
            &[],
            PathBuf::from("/c/jh"),
        )
    });
    assert_eq!(prepared.env["COURSIER_MODE"], "offline");
    // Metals reads its parts through its own cache property; sbt gets the user's cache.
    assert!(!prepared.env.contains_key("COURSIER_CACHE"));
    assert!(prepared.env["SBT_OPTS"].contains("-Dcoursier.cache="));
    assert!(prepared.env["SBT_OPTS"].contains("-Dsbt.offline=true"));
    // Nothing is written into the project copy; the resolver is an sbt global plugin
    // setting pointing at the sbt plugin cache of the tools folder.
    assert!(prepared.generated.is_empty());
    let global = scala_data(&prepared)
        .and_then(|d| d.sbt_global_plugin.clone())
        .unwrap_or_default();
    assert!(global.contains("resolvers += \"trace-metals-parts\" at \"file:"), "{global}");
    assert!(global.contains("sbt-plugins/https/repo1.maven.org/maven2"), "{global}");
    let entry = Registry::builtin().entry("lsp:metals").cloned().unwrap();
    assert_placeholders_filled(&entry, &prepared);
    // Missing parts are an error naming the Scala versions, never a download.
    let metals = tempfile::tempdir().unwrap();
    let setup = with_context("lsp:metals", tmp.path(), &files, true, |cx| {
        let readable = trace_core::paths::forbidden_roots();
        jvm::setup(&crate::languages::read_context(cx, trace_env::EcosystemId::Jvm, &readable))
    });
    let missing = missing_parts(metals.path(), &setup, BuildSystem::Sbt);
    assert_eq!(
        missing,
        vec![
            "Scala 2.13.18".to_string(),
            "Scala 3.3.8".to_string(),
            "Scala 2.12.21".to_string(),
            "sbt 1.12.13".to_string(),
            "the Bloop build server".to_string(),
        ]
    );
    assert_eq!(
        parts_error(&missing[..1]).lines(),
        vec![
            "The Scala language server needs its files for Scala 2.13.18.".to_string(),
            "       Install them: trace status --install scala".to_string(),
        ]
    );
    // Installed parts (as the install extras leave them) satisfy the check.
    let base = central_base(metals.path());
    for (g, a, v) in [
        ("org.scalameta", "mtags_2.13.18", METALS_VERSION),
        ("org.scalameta", "semanticdb-scalac_2.13.18", SEMANTICDB_VERSION),
        ("org.scalameta", "mtags_2.12.21", METALS_VERSION),
        ("org.scalameta", "semanticdb-scalac_2.12.21", SEMANTICDB_VERSION),
        ("org.scala-lang", "scala3-presentation-compiler_3", "3.3.8"),
        ("ch.epfl.scala", "bloop-frontend_2.12", BLOOP_VERSION),
    ] {
        write(&maven_dir(&base, g, a, v), "x.jar", "");
    }
    // sbt builds: Metals' sbt plugin in the sbt plugin cache (a file repository for sbt).
    let plugin =
        maven_dir(&sbt_plugin_base(metals.path()), "ch.epfl.scala", "sbt-bloop_2.12_1.0", BLOOP_VERSION);
    let mut parts = Parts::default();
    parts.sbt.insert("1.12.13".into(), "2.12.21".into());
    parts.save(metals.path()).unwrap();
    assert_eq!(
        missing_parts(metals.path(), &setup, BuildSystem::Sbt),
        vec!["the Bloop build server".to_string()]
    );
    write(&plugin, "x.jar", "");
    let mut parts = Parts::default();
    parts.sbt.insert("1.12.13".into(), "2.12.21".into());
    parts.save(metals.path()).unwrap();
    assert!(missing_parts(metals.path(), &setup, BuildSystem::Sbt).is_empty());
}

#[test]
fn rule_sbt_build_needs_approval() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "build.sbt", "scalaVersion := \"2.13.18\"\n");
    let files = [("src/main/scala/A.scala", Language::Scala)];
    let err = with_context("lsp:metals", tmp.path(), &files, false, |cx| Hooks.preflight(cx)).unwrap_err();
    assert!(items(&err)
        .iter()
        .any(|e| e.lines()[0] == "Scala needs sbt, which runs this project's build definition."));
}

#[test]
fn rule_scala_without_a_supported_build_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/A.scala", "object A\n");
    let files = [("src/A.scala", Language::Scala)];
    let err = with_context("lsp:metals", tmp.path(), &files, true, |cx| Hooks.preflight(cx)).unwrap_err();
    assert_eq!(err.lines()[0], "These Scala files have no build (sbt, Mill or Scala CLI).");
    write(
        tmp.path(),
        "pom.xml",
        "<project><groupId>a</groupId><artifactId>b</artifactId><version>1</version></project>",
    );
    let err = with_context("lsp:metals", tmp.path(), &files, true, |cx| Hooks.preflight(cx)).unwrap_err();
    assert_eq!(err.lines()[0], "Scala projects built with Maven or Gradle are not supported yet (.).");
}

#[test]
fn rule_install_extras_follow_the_repository_scala_versions() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "build.sbt", CROSS_BUILD);
    write(tmp.path(), "project/build.properties", "sbt.version=1.12.13\n");
    let extras = Hooks.install_extras(Some(tmp.path()));
    let ids: Vec<&str> = extras.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "bloop:2.1.2",
            "semanticdb-javac:0.12.3",
            "sbt:1.12.13",
            "scala:2.13.18",
            "scala:3.3.8",
            "scala:2.12.21"
        ]
    );
    // Metals' Java SemanticDB plugin is installed with the parts (never fetched at analysis).
    assert_eq!(extras[1].coordinates, vec!["com.sourcegraph:semanticdb-javac:0.12.3".to_string()]);
    assert!(extras[3]
        .coordinates
        .contains(&"org.scalameta:mtags_2.13.18:1.6.9".to_string()));
    assert_eq!(
        bridge_coordinate("2.13.18", Some("1.10.8")).as_deref(),
        Some("org.scala-sbt:compiler-bridge_2.13:1.10.8")
    );
    assert!(Hooks.install_extras(None).is_empty());
}

#[test]
fn rule_metals_readonly_sources_are_external() {
    let loc = Hooks
            .external_location(
                "file:///C:/ws/.metals/readonly/dependencies/jsonlib-core_2.13-0.14.10-sources.jar/io/example/Json.scala",
                &Prepared::default(),
            )
            .unwrap();
    assert_eq!(loc.package, "jsonlib-core_2.13");
    assert_eq!(loc.version.as_deref(), Some("0.14.10"));
    assert!(loc.readable);
    assert_eq!(
        loc.path,
        "C:/ws/.metals/readonly/dependencies/jsonlib-core_2.13-0.14.10-sources.jar/io/example/Json.scala"
    );
    let lib = Hooks
        .external_location(
            "file:///ws/.metals/readonly/dependencies/scala-library-2.13.18-sources.jar/scala/Option.scala",
            &Prepared::default(),
        )
        .unwrap();
    assert!(lib.stdlib);
    assert!(Hooks
        .external_location("file:///ws/src/A.scala", &Prepared::default())
        .is_none());
}

#[test]
fn rule_metals_lock_is_the_launch_class_path() {
    let entry = Registry::builtin().entry("lsp:metals").cloned().unwrap();
    let metals = tempfile::tempdir().unwrap();
    assert!(lock_classpath(&entry, metals.path()).is_none());
    let Some(install) = &entry.install else { panic!("metals install record") };
    let Recipe::Coursier { lock, main_class, .. } = &install.recipe else { panic!("coursier recipe") };
    assert_eq!(main_class, "scala.meta.metals.Main");
    assert!(lock
        .iter()
        .any(|f| f.sha256 == "a38a26249d81dfb26222a2b93a5546750dd27b826f04e0f2724af3d7cc842a55"));
    let cache = metals.path().join("cache");
    for f in lock {
        let p = cache_path(&cache, &f.url).unwrap();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"").unwrap();
    }
    assert_eq!(lock_classpath(&entry, metals.path()).unwrap().len(), lock.len());
    assert!(entry.args.iter().any(|a| a == "@{outside}/metals.args"));
}

#[test]
fn rule_coursier_launcher_of_the_install_recipe_runs_the_install_extras() {
    // The Coursier recipe keeps the pinned launcher in `<tool>/<v>/cs/`.
    let p = trace_env::os::Platform::current();
    let tools = tempfile::tempdir().unwrap();
    let metals = tools.path().join("metals").join("1.6.9");
    assert!(Coursier::find(&metals, tools.path(), &p).is_none());
    let cs = metals.join("cs").join(p.exe("cs"));
    std::fs::create_dir_all(cs.parent().unwrap()).unwrap();
    std::fs::write(&cs, b"").unwrap();
    assert_eq!(Coursier::find(&metals, tools.path(), &p).map(|c| c.program), Some(cs));
}
