use super::*;
use std::fs;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

#[test]
fn rule_project_configs_follow_files_extends_and_references() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "tsconfig.json", "{ // solution\n \"files\": [], \"references\": [{\"path\": \"./tsconfig.build.json\"}, {\"path\": \"./packages/core\"}], }");
    write(root, "tsconfig.build.json", "{\"extends\": \"./config/base.json\"}");
    write(root, "config/base.json", "{\"compilerOptions\": {\"strict\": true}}");
    write(root, "packages/core/tsconfig.json", "{\"extends\": \"@tsconfig/node20/tsconfig.json\"}");
    write(root, "packages/core/package.json", "{}");
    write(root, "package.json", "{}");
    write(root, "docs/tsconfig.json", "{}");
    let configs = project_configs(root, &["src/index.ts"], &[]);
    let paths: Vec<&str> = configs.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "config/base.json",
            "package.json",
            "packages/core/tsconfig.json",
            "tsconfig.build.json",
            "tsconfig.json"
        ],
        "configs of unrelated folders are not read"
    );
    assert!(resolve_config(root, "", "../../outside.json").is_none());
}

/// Default language: the install record is the complete closure for the six platforms
/// (compiler shim + one native compiler per platform + the bundled Node types).
#[test]
fn rule_typescript_install_covers_six_platforms() {
    let registry = crate::registry::Registry::builtin();
    let entry = registry.entry("typescript").expect("typescript entry");
    let install = entry.install.as_ref().unwrap();
    assert!(install.licence_gate.is_none());
    let crate::registry::Recipe::Npm { packages } = &install.recipe else {
        panic!("typescript installs from npm");
    };
    let natives: Vec<(&str, &str)> = packages
        .iter()
        .filter(|p| p.path.starts_with("node_modules/@typescript/"))
        .map(|p| (p.os[0].as_str(), p.cpu[0].as_str()))
        .collect();
    for platform in [
        ("win32", "x64"),
        ("win32", "arm64"),
        ("linux", "x64"),
        ("linux", "arm64"),
        ("darwin", "x64"),
        ("darwin", "arm64"),
    ] {
        assert!(natives.contains(&platform), "{platform:?}");
    }
    for path in [
        "node_modules/typescript",
        "node_modules/@types/node",
        "node_modules/undici-types",
    ] {
        assert!(packages.iter().any(|p| p.path == path), "{path}");
    }
    assert_eq!(entry.runtime, vec!["node".to_string()]);
}

#[test]
fn rule_yarn_pnp_and_deno_errors_say_how_to_fix() {
    let e = limit_error(Language::JavaScript, NodeLimit::YarnPnp);
    assert_eq!(e.kind(), "unsupported");
    let text = e.to_string();
    assert!(text.starts_with("This project uses Yarn Plug'n'Play, which trace cannot read."));
    assert!(text.contains("nodeLinker: node-modules"));
    let e = limit_error(Language::TypeScript, NodeLimit::DenoOnly);
    assert_eq!(e.to_string(), "Deno projects are not supported yet (deno.json).");
}
