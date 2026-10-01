use super::*;

#[test]
fn controlled_configuration_matches_the_reference() {
    let config = pyright_config(&["a.py", "pkg/b.pyi"], &PyrightSetup::default());
    assert_eq!(config["pythonVersion"], "3.11");
    assert_eq!(config["typeCheckingMode"], "off");
    assert_eq!(config["exclude"], json!([".tmp"]));
    assert_eq!(config["include"], json!(["a.py", "pkg/b.pyi"]));
    assert_eq!(config["extraPaths"], json!(["src"]));
    assert!(config.get("venvPath").is_none(), "no environment: no venv");
    let env = trace_env::PythonEnv {
        root: PathBuf::from("/proj/.venv"),
        site_packages: vec![PathBuf::from("/proj/.venv/Lib/site-packages")],
        version: Some("3.12".into()),
        origin: trace_env::Origin::Project,
        kind: trace_env::python::EnvKind::Venv,
        system_site_packages: Vec::new(),
        stdlib: None,
    };
    let mut project = serde_json::Map::new();
    project.insert("stubPath".into(), json!("stubs"));
    let setup = PyrightSetup {
        venv: Some(env),
        python_version: Some("3.12".into()),
        extra_paths: vec!["lib".into(), "/base/site-packages".into()],
        project,
    };
    let with = pyright_config(&["a.py"], &setup);
    assert_eq!(with["venv"], ".venv");
    assert_eq!(with["pythonVersion"], "3.12");
    assert_eq!(with["stubPath"], "stubs");
    assert_eq!(with["extraPaths"], json!(["lib", "/base/site-packages"]));
    assert!(with["venvPath"]
        .as_str()
        .unwrap()
        .replace('\\', "/")
        .ends_with("/proj"));
    let s = settings(Path::new("/snap"), &setup);
    assert_eq!(s["python"]["analysis"]["autoSearchPaths"], false);
    assert_eq!(s["python"]["analysis"]["diagnosticMode"], "openFilesOnly");
    let extra = s["python"]["analysis"]["extraPaths"].as_array().unwrap();
    assert!(extra[0].as_str().unwrap().replace('\\', "/").ends_with("/snap/lib"));
}

/// Rule 16: the Node heap cap is a launch argument only when the pool asks for it.
#[test]
fn heap_cap_is_added_only_when_asked() {
    let (guard, server) = (Path::new("/a/lsp_guard.cjs"), Path::new("/t/langserver.index.js"));
    let plain = node_args(None, guard, server);
    assert_eq!(plain[0], "--require");
    assert!(!plain.iter().any(|a| a.starts_with("--max-old-space-size")));
    let capped = node_args(Some(4_096), guard, server);
    assert_eq!(capped[0], "--max-old-space-size=4096");
    assert_eq!(&capped[1..], &plain[..]);
}

/// Rule (I-22): an extensionless (shebang) Python file is given to Pyright as a module
/// (`<path>.py`); files with a Python extension keep their name.
#[test]
fn rule_extensionless_python_file_is_analysed_as_a_module() {
    assert_eq!(server_name("benchsuite/benchsuite").as_deref(), Some("benchsuite/benchsuite.py"));
    assert_eq!(server_name("scripts/copy-examples").as_deref(), Some("scripts/copy-examples.py"));
    assert_eq!(server_name("tool").as_deref(), Some("tool.py"));
    assert_eq!(server_name("bin/run.v2").as_deref(), Some("bin/run.v2.py"));
    assert_eq!(server_name("pkg/mod.py"), None);
    assert_eq!(server_name("pkg/mod.pyi"), None);
    assert_eq!(server_name("pkg/MOD.PY"), None);
    assert_eq!(server_name(""), None);
}

#[test]
fn symbol_kinds_are_declarations_only() {
    assert!(SYMBOL_KINDS.contains(&12));
    assert!(!SYMBOL_KINDS.contains(&13), "variables are not declarations");
}
