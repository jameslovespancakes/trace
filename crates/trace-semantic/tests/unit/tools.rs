use super::*;

fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join("trace-semantic-tests")
        .join(format!("{name}-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&dir).unwrap();
    canonical_or_self(&dir)
}

/// Analyzer environments hold only allow-listed variables plus the backend's own additions.
#[test]
fn rule_clean_env_holds_only_listed_variables() {
    let env = clean_env(&[], &[("PYRIGHT_TMPDIR", "x".to_string())]);
    assert_eq!(env.get("PYRIGHT_TMPDIR").map(String::as_str), Some("x"));
    for key in env.keys() {
        assert!(
            BASE_ENV_ALLOWLIST.iter().any(|a| a.eq_ignore_ascii_case(key)) || key == "PYRIGHT_TMPDIR",
            "unexpected variable {key}"
        );
    }
}

#[test]
fn set_overrides_case_insensitively() {
    let env = clean_env(&[], &[("path", "/only".to_string())]);
    let paths: Vec<_> = env.iter().filter(|(k, _)| k.eq_ignore_ascii_case("PATH")).collect();
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].1, "/only");
}

/// Schema 2: registry executables resolve through MANIFEST schema 2 to
/// `<tools>/<tool>/<version>/<path>`; a schema-1 manifest (old dev layout) resolves
/// nothing; tools inside a forbidden root are never trusted; escaping paths are ignored.
#[test]
fn rule_registry_executables_resolve_through_the_manifest() {
    let root = std::env::temp_dir()
        .join("trace-tests")
        .join("trace-fixtures-registry")
        .join(format!("tools-{}", uuid::Uuid::new_v4().simple()));
    let exe = if cfg!(windows) { "gopls.exe" } else { "gopls" };
    let tools = root.join("tools");
    let bin = tools.join("gopls/v0.23.0/bin");
    fs::create_dir_all(&bin).unwrap();
    let file = bin.join(exe);
    fs::write(&file, b"binary").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let tools = canonical_or_self(&tools);
    let spec = crate::registry::ExecutableSpec::Tool {
        tool: "gopls".into(),
        path: "bin/gopls".into(),
    };
    fs::write(tools.join("MANIFEST.json"), br#"{"schema":1,"tools":{"gopls":{"version":"v0.23.0"}}}"#)
        .unwrap();
    let mut env = crate::test_support::setup::tool_env(Some(tools.clone()));
    env.manifest = crate::install::manifest::Manifest::load(&tools);
    assert_eq!(env.entry_executable(&spec, None), None, "old layout: nothing installed");
    fs::write(tools.join("MANIFEST.json"), br#"{"schema":2,"tools":{"gopls":{"version":"v0.23.0"}}}"#)
        .unwrap();
    env.manifest = crate::install::manifest::Manifest::load(&tools);
    assert_eq!(env.entry_executable(&spec, None), Some(canonical_or_self(&file)));
    assert_eq!(env.tool_dir("gopls"), Some(canonical_or_self(&tools.join("gopls/v0.23.0"))));
    assert_eq!(env.tool_file("gopls", "../gopls"), None);
    env.forbidden_roots = vec![tools.clone()];
    assert_eq!(env.entry_executable(&spec, None), None);
    let _ = fs::remove_dir_all(&root);
}

/// PLAN decision 11: `runtime_exe` resolves only the trace-managed runtime in the tools
/// directory, never a `node` on the user's PATH; `reload_installed` sees a new install.
#[test]
fn rule_runtime_exe_is_always_trace_managed() {
    let root = test_dir("runtime");
    let user_bin = root.join("user-bin");
    fs::create_dir_all(&user_bin).unwrap();
    let exe = if cfg!(windows) { "node.exe" } else { "node" };
    fs::write(user_bin.join(exe), b"user node").unwrap();
    let mut env = crate::test_support::setup::tool_env(None);
    assert_eq!(env.runtime_exe("node"), None, "the user's node is never used");
    let spec = env.registry.runtime("node").cloned().unwrap();
    let tools = root.join("tools");
    let dir = tools.join("node").join(&spec.version);
    fs::create_dir_all(dir.join("bin")).unwrap();
    for rel in ["node.exe", "bin/node"] {
        let file = dir.join(rel);
        fs::write(&file, b"trace node").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    fs::write(
        tools.join("MANIFEST.json"),
        format!(r#"{{"schema":2,"tools":{{"node":{{"version":"{}"}}}}}}"#, spec.version),
    )
    .unwrap();
    env.reload_installed(&tools);
    let found = env.runtime_exe("node").unwrap();
    assert!(is_within(&found, &canonical_or_self(&tools)), "{}", found.display());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn prefix_test_is_component_wise() {
    assert!(is_within(Path::new("/a/b/c"), Path::new("/a/b")));
    assert!(!is_within(Path::new("/a/bc"), Path::new("/a/b")));
    if cfg!(windows) {
        assert!(is_within(Path::new(r"C:\Repo\x"), Path::new(r"c:\repo")));
    }
}
