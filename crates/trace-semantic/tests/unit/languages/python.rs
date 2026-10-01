use super::*;
use std::fs;

#[test]
fn rule_project_pyright_keys_are_merged_safely() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.pyright]\nextraPaths = [\"lib\"]\nstubPath = \"typings\"\nvenvPath = \"/elsewhere\"\npythonPath = \"/usr/bin/python\"\n",
        )
        .unwrap();
    let (safe, extra) = project_config(dir.path());
    assert_eq!(extra, Some(vec!["lib".to_string()]));
    assert_eq!(safe.get("stubPath"), Some(&Value::from("typings")));
    assert!(
        !safe.contains_key("venvPath") && !safe.contains_key("pythonPath"),
        "never the project's interpreter"
    );
    // pyrightconfig.json (with comments) wins.
    fs::write(dir.path().join("pyrightconfig.json"), "{ // c\n \"extraPaths\": [\"src2\"], }\n").unwrap();
    let (_, extra) = project_config(dir.path());
    assert_eq!(extra, Some(vec!["src2".to_string()]));
}

#[test]
fn rule_site_packages_env_becomes_an_extra_path() {
    let dir = tempfile::tempdir().unwrap();
    let sp = dir.path().join("lib").join("python3.12").join("site-packages");
    fs::create_dir_all(&sp).unwrap();
    let env = trace_env::python::PythonEnv {
        root: sp.clone(),
        site_packages: vec![sp.clone()],
        version: Some("3.12".into()),
        origin: trace_env::Origin::Override,
        kind: EnvKind::SitePackages,
        system_site_packages: Vec::new(),
        stdlib: None,
    };
    let setup = trace_env::python::PythonSetup {
        env: Some(env),
        env_not_found: None,
        python_version: Some("3.12".into()),
        deps: trace_env::DepsReport::none_declared(),
    };
    let p = pyright_setup(&setup, Map::new(), None);
    assert!(p.venv.is_none(), "a bare site-packages is no venv");
    assert_eq!(p.extra_paths[0], "src");
    assert_eq!(p.extra_paths[1], sp.display().to_string());
    assert_eq!(p.python_version.as_deref(), Some("3.12"));
}

/// Preflight errors of a repository whose `.venv` holds `installed` distributions, with
/// the repository itself in the tools' never-execute list (as `ToolEnv::discover` sets it).
fn preflight_with_repo_venv(installed: &[&str]) -> String {
    use trace_env::os::{Arch, EnvVars, Os, Platform};
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("trace-python-venv-{}", uuid::Uuid::new_v4().simple()));
    let root = dir.join("repo");
    let site = root
        .join(".venv")
        .join("lib")
        .join("python3.12")
        .join("site-packages");
    fs::create_dir_all(&site).unwrap();
    fs::write(root.join(".venv").join("pyvenv.cfg"), "version_info = 3.12.4\n").unwrap();
    for d in installed {
        fs::create_dir_all(site.join(d)).unwrap();
    }
    fs::write(root.join("pyproject.toml"), "[project]\nname = \"app\"\ndependencies = [\"flask>=3\"]\n")
        .unwrap();
    fs::write(root.join("app.py"), "import flask\n").unwrap();
    let repo = trace_core::paths::RepoPaths::resolve_in(&root, &dir.join("home")).unwrap();
    let registry = crate::registry::Registry::builtin();
    let entry = registry.entry("pyright").unwrap();
    let mut tools = crate::test_support::setup::tool_env(None);
    tools.forbidden_roots = vec![root.clone()];
    let settings = trace_core::repo_settings::RepoSettings::default();
    let files = [("app.py", Language::Python)];
    let facts = |_: &str| -> Option<&trace_core::facts::FileFacts> { None };
    let platform = Platform {
        os: Os::Linux,
        arch: Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    };
    let vars = EnvVars::from_pairs(&[]);
    let cx = SetupContext {
        repo: &repo,
        entry,
        languages: &[Language::Python],
        files: &files,
        facts: &facts,
        settings: &settings,
        tools: &tools,
        platform: &platform,
        vars: &vars,
        report_only: true,
    };
    let text = match Hooks.preflight(&cx) {
        Ok(_) => String::new(),
        Err(e) => e.lines().join("\n"),
    };
    let _ = fs::remove_dir_all(&dir);
    text
}

#[test]
fn rule_in_project_environment_is_read_although_the_repository_is_never_executed() {
    // The repository is in the never-execute list; its `.venv` is still read (reading is
    // not executing), so installed dependencies satisfy the preflight.
    let text = preflight_with_repo_venv(&["flask-3.0.0.dist-info"]);
    assert!(!text.contains("Dependencies not installed"), "{text}");
    assert!(text.contains("The Python language server is not installed"), "{text}");
    // Negative: the same environment without the distribution is the dependency error.
    let text = preflight_with_repo_venv(&[]);
    assert!(text.contains("Dependencies not installed"), "{text}");
}
