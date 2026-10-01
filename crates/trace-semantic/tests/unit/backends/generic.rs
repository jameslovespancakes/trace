use super::*;
use crate::registry::{BackendKind, Registry};

/// `lsp` entries of the builtin registry (file order).
fn servers() -> Vec<BackendEntry> {
    Registry::builtin()
        .backends
        .into_iter()
        .filter(|b| b.kind == BackendKind::Lsp)
        .collect()
}

/// A registry entry built from JSON (tests never depend on the builtin entries' data).
fn entry(json: Value) -> BackendEntry {
    serde_json::from_value(json).expect("entry")
}

fn sample() -> BackendEntry {
    entry(serde_json::json!({
        "id": "lsp:sample",
        "kind": "lsp",
        "languages": ["go"],
        "language_ids": ["go"],
        "server": {"name": "sample", "version": "1.0.0", "license": "MIT"},
        "executable": {"from": "tool", "tool": "sample", "path": "bin/sample"},
        "args": ["-data", "{outside}/data", "--tmp={tmp}", "--root={snapshot}"],
        "env_set": {"TEMP": "{snapshot}/.tmp", "GOTOOLCHAIN": "local", "GOPATH": "{toolchain:gopath}"},
        "settings": {"a": {"b": "{snapshot}/x", "p": "{json:projects}"}},
        "after_initialized": [{"method": "solution/open", "params": {"solution": "{solution_uri}"}}],
        "ready": {"kind": "request", "method": "workspace/synchronize", "params": {"root": "{snapshot}"}}
    }))
}

fn placeholders(workspace: &str, tools: &str) -> Placeholders {
    let mut p = Placeholders {
        values: BTreeMap::new(),
        json: BTreeMap::new(),
        language: Language::Go,
    };
    p.values.insert("snapshot".into(), workspace.into());
    p.values.insert("outside".into(), format!("{workspace}.state"));
    p.values.insert("tmp".into(), format!("{workspace}.state/tmp"));
    p.values.insert("toolchain:gopath".into(), "/home/u/go".into());
    p.values.insert("solution_uri".into(), "file:///ws/app.sln".into());
    for (k, v) in [
        ("tool:go", format!("{tools}/go/1.27.1")),
        ("tool:jdtls", format!("{tools}/jdtls/1.61.0")),
        ("runtime:jdk", format!("{tools}/jdk/21")),
    ] {
        p.values.insert(k.into(), v);
    }
    p
}

/// Rule: a backend's `max_in_flight` caps the requests pipelined to one of its processes
/// (never raises the configured window); unset keeps the configured window.
#[test]
fn rule_backend_in_flight_limit_caps_the_configured_window() {
    assert_eq!(in_flight_limit(16, None), 16);
    assert_eq!(in_flight_limit(16, Some(0)), 16);
    assert_eq!(in_flight_limit(16, Some(4)), 4);
    assert_eq!(in_flight_limit(2, Some(4)), 2);
    assert_eq!(in_flight_limit(0, Some(4)), 1);
}

/// Rule: every §1.8.3 placeholder expands in arguments, environment and JSON values
/// (`{json:..}` as a whole value); an unknown placeholder is an error, never left in a
/// launch; a missing `{tool:..}` is the server-missing setup error.
#[test]
fn rule_placeholders_expand_and_unknown_fails() {
    let mut ph = placeholders("/s", "/t");
    ph.json.insert("projects".into(), serde_json::json!(["a.csproj"]));
    let v = serde_json::json!({"a": ["{snapshot}/x", 1], "b": {"c": "{tool:go}/y"}, "p": "{json:projects}"});
    let s = ph.expand_json(&v).unwrap();
    assert_eq!(s["a"][0], "/s/x");
    assert_eq!(s["b"]["c"], "/t/go/1.27.1/y");
    assert_eq!(s["p"], serde_json::json!(["a.csproj"]));
    assert!(matches!(ph.expand("{unknown}/x"), Err(SemanticError::Protocol(_))));
    assert!(matches!(ph.expand_json(&serde_json::json!("{json:missing}")), Err(SemanticError::Protocol(_))));
    assert!(matches!(
        ph.expand("{tool:missing-server}/bin"),
        Err(SemanticError::Setup(SetupError::ServerMissing {
            language: Language::Go
        }))
    ));
    assert_eq!(ph.expand("no braces").unwrap(), "no braces");
    let e = sample();
    let (program, args, env) = command_line(
        &e,
        &Launch::Exe(PathBuf::from("/t/sample/bin/sample")),
        Path::new("/s"),
        &ph,
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(program, PathBuf::from("/t/sample/bin/sample"));
    assert_eq!(args, vec!["-data", "/s.state/data", "--tmp=/s.state/tmp", "--root=/s"]);
    let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
    assert_eq!(get("TEMP").as_deref(), Some("/s/.tmp"));
    assert_eq!(get("GOPATH").as_deref(), Some("/home/u/go"));
    // The preflight's environment wins over the entry's.
    let mut prepared_env = BTreeMap::new();
    prepared_env.insert("TEMP".to_string(), "{outside}/other".to_string());
    let (_, _, env) =
        command_line(&e, &Launch::Exe(PathBuf::from("/g")), Path::new("/s"), &ph, &prepared_env).unwrap();
    assert_eq!(
        env.iter()
            .filter(|(k, _)| k == "TEMP")
            .map(|(_, v)| v.as_str())
            .collect::<Vec<_>>(),
        vec!["/s.state/other"]
    );
    // An unknown placeholder in the entry fails the launch.
    let mut bad = sample();
    bad.args.push("{nope}".into());
    assert!(command_line(&bad, &Launch::Exe(PathBuf::from("/g")), Path::new("/s"), &ph, &BTreeMap::new())
        .is_err());
}

/// Rule: a setting whose `{json:NAME}` variable the preflight did not set (absent or
/// `null`) is omitted with its key - never sent as `null` -, in objects and arrays alike;
/// set variables (also `false` / empty values) are kept.
#[test]
fn rule_absent_json_var_omits_the_setting() {
    let mut ph = placeholders("/s", "/t");
    ph.json.insert("sbt".into(), serde_json::json!("/opt/sbt/bin/sbt"));
    ph.json.insert("mill".into(), Value::Null);
    ph.json.insert("off".into(), serde_json::json!(false));
    let v = serde_json::json!({"metals": {
        "sbtScript": "{json:sbt}",
        "millScript": "{json:mill}",
        "scalaCliLauncher": "{json:scala_cli}",
        "enabled": "{json:off}",
        "javaHome": "{snapshot}/jdk",
        "list": ["{json:mill}", "{json:sbt}", "{json:missing}"]
    }});
    let s = ph.expand_json(&v).unwrap();
    let metals = s["metals"].as_object().unwrap();
    assert_eq!(metals["sbtScript"], "/opt/sbt/bin/sbt");
    assert!(!metals.contains_key("millScript"), "null variable: setting omitted");
    assert!(!metals.contains_key("scalaCliLauncher"), "absent variable: setting omitted");
    assert_eq!(metals["enabled"], false, "a set false stays");
    assert_eq!(metals["javaHome"], "/s/jdk");
    assert_eq!(metals["list"], serde_json::json!(["/opt/sbt/bin/sbt"]));
    // The whole value: an absent variable is still a registry error, a null one is null.
    assert!(matches!(ph.expand_json(&serde_json::json!("{json:missing}")), Err(SemanticError::Protocol(_))));
    assert_eq!(ph.expand_json(&serde_json::json!("{json:mill}")).unwrap(), Value::Null);
}

/// Rule: every entry of the builtin registry expands completely with a fully populated
/// preflight (all `PREPARED_VARS` and every referenced `{json:..}` / `{toolchain:..}`).
#[test]
fn builtin_entries_expand_with_a_full_preflight() {
    for e in servers() {
        let mut ph = placeholders("/s", "/t");
        for var in crate::registry::PREPARED_VARS {
            ph.values
                .entry((*var).to_string())
                .or_insert_with(|| format!("<{var}>"));
        }
        for s in entry_strings(&e) {
            for name in crate::registry::placeholders(&s) {
                match name.split_once(':') {
                    Some(("json", key)) => {
                        ph.json.insert(key.to_string(), Value::Null);
                    }
                    Some(("toolchain" | "tool" | "runtime", _)) => {
                        ph.values
                            .entry(name.to_string())
                            .or_insert_with(|| format!("<{name}>"));
                    }
                    _ => {}
                }
            }
        }
        for key in ["tools", "os", "arch", "repo_cache", "heap_mb"] {
            ph.values.entry(key.to_string()).or_insert_with(|| format!("<{key}>"));
        }
        let launch = Launch::Exe(PathBuf::from("/x"));
        command_line(&e, &launch, Path::new("/s"), &ph, &BTreeMap::new())
            .unwrap_or_else(|err| panic!("{}: {err}", e.id));
        ph.expand_json(&e.settings)
            .unwrap_or_else(|err| panic!("{}: {err}", e.id));
        ph.expand_json(&e.initialization_options)
            .unwrap_or_else(|err| panic!("{}: {err}", e.id));
    }
}

#[test]
fn node_script_launch_uses_the_guard() {
    let e = entry(serde_json::json!({
        "id": "lsp:script",
        "kind": "lsp",
        "languages": ["bash"],
        "server": {"name": "s", "version": "1", "license": "MIT"},
        "executable": {"from": "node_script", "tool": "s", "script": "node_modules/s/cli.js"},
        "args": ["start"],
        "env_set": {"CODEPATH_LSP_WRITE_ROOT": "/elsewhere"}
    }));
    let launch = Launch::Node {
        node: PathBuf::from("/tools/node/24/bin/node"),
        guard: PathBuf::from("/home/assets/lsp_guard.cjs"),
        script: PathBuf::from("/tools/s/1/node_modules/s/cli.js"),
    };
    let ph = placeholders("/snap", "/tools");
    let (program, args, env) = command_line(&e, &launch, Path::new("/snap"), &ph, &BTreeMap::new()).unwrap();
    assert_eq!(program, PathBuf::from("/tools/node/24/bin/node"));
    assert_eq!(
        args,
        vec![
            "--require",
            "/home/assets/lsp_guard.cjs",
            "/tools/s/1/node_modules/s/cli.js",
            "start"
        ]
    );
    assert_eq!(
        env.iter()
            .filter(|(k, _)| k == "CODEPATH_LSP_WRITE_ROOT")
            .map(|(_, v)| v.as_str())
            .collect::<Vec<_>>(),
        vec!["/snap"],
        "the guard's write root is always the workspace"
    );
}

/// Missing servers are setup errors: an empty tools folder resolves nothing, and every
/// tools-folder path an entry names must exist.
#[test]
fn missing_servers_are_setup_errors() {
    let empty = crate::test_support::setup::tool_env(None);
    let e = sample();
    assert!(matches!(
        resolve_launch(&e, &empty, &Prepared::default(), Language::Go),
        Err(SemanticError::Setup(SetupError::ServerMissing {
            language: Language::Go
        }))
    ));
    let mut with_tool_path = sample();
    with_tool_path
        .args
        .push("{tool:jdtls}/plugins/org.eclipse.equinox.launcher_*.jar".into());
    assert!(tool_references(&with_tool_path)
        .iter()
        .any(|r| r.contains("launcher_*.jar")));
    assert!(missing_tool_path(&with_tool_path, &empty).is_some());
}

#[test]
fn placeholders_and_globs_are_expanded() {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join("trace-fixtures-registry")
        .join(format!("glob-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(dir.join("plugins")).unwrap();
    for name in [
        "org.eclipse.equinox.launcher_1.6.0.jar",
        "org.eclipse.equinox.launcher_1.7.0.jar",
        "other.jar",
    ] {
        std::fs::write(dir.join("plugins").join(name), b"x").unwrap();
    }
    let arg = format!("{}/plugins/org.eclipse.equinox.launcher_*.jar", dir.display());
    assert!(expand_glob(&arg).ends_with("org.eclipse.equinox.launcher_1.7.0.jar"));
    assert_eq!(expand_glob("--stdio"), "--stdio");
    assert_eq!(expand_glob("missing/dir/x_*.jar"), "missing/dir/x_*.jar");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn interned_configs_are_shared() {
    let a = intern(&["go.mod".to_string(), "go.sum".to_string()]);
    let b = intern(&["go.mod".to_string(), "go.sum".to_string()]);
    assert!(std::ptr::eq(a, b));
    assert_eq!(a, ["go.mod", "go.sum"]);
    let e = sample();
    assert_eq!(GenericLsp::new(e).name(), "sample");
}

#[test]
fn server_tables_are_consistent() {
    for spec in servers() {
        assert!(spec.settings.is_object() || spec.settings.is_null(), "{} settings", spec.id);
        let init = &spec.initialization_options;
        assert!(init.is_null() || init.is_object(), "{} init", spec.id);
    }
}
