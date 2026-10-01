use super::*;
use crate::registry::Registry;
use crate::test_support::facts::facts_with_imports;
use trace_core::facts::FileFacts;
use trace_env::os::{Arch, EnvVars, Platform};

fn versions() -> Vec<GoInstallVersion> {
    let registry = Registry::builtin();
    match &registry.entry("lsp:gopls").unwrap().install.as_ref().unwrap().recipe {
        Recipe::GoInstall { versions, .. } => versions.clone(),
        _ => panic!("gopls is a go_install recipe"),
    }
}

#[test]
fn rule_gopls_version_follows_go_minor() {
    let v = versions();
    let pick = |go: &str| gopls_for_go(&v, &Version::parse(go).unwrap()).map(|g| g.version.clone());
    assert_eq!(pick("1.27.0").as_deref(), Some("v0.23.0"));
    assert_eq!(pick("1.26.0").as_deref(), Some("v0.23.0"));
    assert_eq!(pick("1.25.3").as_deref(), Some("v0.21.1"));
    assert_eq!(pick("1.24.2").as_deref(), Some("v0.20.0"));
    assert_eq!(pick("1.24.1"), None);
    assert_eq!(pick("1.22.0"), None);
}

#[test]
fn rule_cgo_needs_approval() {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("trace-fixtures-cgo-{}", uuid::Uuid::new_v4().simple()));
    let root = dir.join("repo");
    let goroot = dir.join("go");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("go.mod"), "module example.com/app\n\ngo 1.22\n").unwrap();
    std::fs::write(root.join("main.go"), "package main\n\nimport \"C\"\n").unwrap();
    std::fs::create_dir_all(goroot.join("bin")).unwrap();
    std::fs::create_dir_all(goroot.join("src/runtime")).unwrap();
    std::fs::write(goroot.join("bin/go"), "").unwrap();
    std::fs::write(goroot.join("VERSION"), "go1.26.1\n").unwrap();
    let repo = trace_core::paths::RepoPaths::resolve_in(&root, &dir.join("home")).unwrap();
    let registry = Registry::builtin();
    let entry = registry.entry("lsp:gopls").unwrap();
    let tools = crate::test_support::setup::tool_env(None);
    let mut settings = trace_core::repo_settings::RepoSettings::default();
    settings.env.insert("go".into(), goroot.clone());
    let files = [("main.go", Language::Go)];
    let cgo_facts = facts_with_imports(Language::Go, &["C"]);
    let facts = |p: &str| -> Option<&FileFacts> { (p == "main.go").then_some(&cgo_facts) };
    let platform = Platform {
        os: Os::Linux,
        arch: Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    };
    let vars = EnvVars::from_pairs(&[("GOENV", "off")]);
    let cx = SetupContext {
        repo: &repo,
        entry,
        languages: &[Language::Go],
        files: &files,
        facts: &facts,
        settings: &settings,
        tools: &tools,
        platform: &platform,
        vars: &vars,
        report_only: true,
    };
    let text = Hooks.preflight(&cx).unwrap_err().lines().join("\n");
    assert!(text.contains("Go needs cgo, which runs the C compiler on its code."), "{text}");
    assert!(text.contains("The Go language server is not installed."), "{text}");
    assert!(!text.contains("Go is not installed"), "the --env GOROOT is used: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rule_custom_build_tag_file_is_outside_build() {
    let reason = Hooks.outside_build(
        "binding/binding_nomsgpack.go",
        "no package metadata for file file:///ws/binding/binding_nomsgpack.go",
    );
    assert!(reason.unwrap().contains("build constraints"));
    assert_eq!(Hooks.outside_build("main.go", "request timed out"), None);
    assert_eq!(Hooks.outside_build("x.py", "no package metadata"), None);
}
