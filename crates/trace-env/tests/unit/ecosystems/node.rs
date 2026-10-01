use std::fs;
use std::path::Path;

use super::*;
use crate::os::{EnvVars, Platform};
use crate::test_support::write;

fn cx<'a>(
    root: &'a Path,
    platform: &'a Platform,
    vars: &'a EnvVars,
    env: Option<&'a Path>,
) -> DetectContext<'a> {
    DetectContext {
        root,
        platform,
        vars,
        env_override: env,
        forbidden: &[],
        files: &[],
    }
}

fn install(root: &Path, dir: &str, name: &str) {
    let base = if dir.is_empty() {
        root.to_path_buf()
    } else {
        root.join(dir)
    };
    write(&base, &format!("node_modules/{name}/package.json"), "{}");
}

#[test]
fn rule_env_finds_node_modules_next_to_package_json_and_respects_forbidden_roots() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "package.json", "{}");
    fs::create_dir_all(root.join("node_modules/react")).unwrap();
    write(root, "apps/web/package.json", "{}");
    fs::create_dir_all(root.join("apps/web/node_modules/next")).unwrap();
    // node_modules without a package.json, and nested node_modules, are not environments.
    fs::create_dir_all(root.join("loose/node_modules/x")).unwrap();
    let found = find_node_modules(root, &|_| true, None);
    let dirs: Vec<&str> = found.iter().map(|m| m.dir.as_str()).collect();
    assert_eq!(dirs, vec!["", "apps/web"]);
    let apps = root.join("apps");
    let found = find_node_modules(root, &|p: &Path| !p.starts_with(&apps), None);
    assert_eq!(found.len(), 1, "forbidden roots are never read");
}

#[test]
fn rule_node_deps_found_walking_up() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "package.json",
        r#"{"name":"mono","workspaces":["packages/*"],"devDependencies":{"typescript":"^5"}}"#,
    );
    write(
        root,
        "packages/a/package.json",
        r#"{"name":"@m/a","dependencies":{"lodash":"^4","@m/b":"workspace:*"},"peerDependencies":{"react":"*"}}"#,
    );
    write(root, "packages/b/package.json", r#"{"name":"@m/b","dependencies":{"@scope/x":"1"}}"#);
    // Hoisted to the root, and a package-local install.
    install(root, "", "typescript");
    install(root, "", "lodash");
    install(root, "packages/b", "@scope/x");
    let p = Platform::current();
    let vars = EnvVars::default();
    let s = setup(&cx(root, &p, &vars, None));
    assert_eq!(s.projects, vec!["", "packages/a", "packages/b"]);
    assert_eq!(s.deps.status, DepsStatus::Installed, "{:?}", s.deps.missing);
    // Removing a hoisted package makes it missing, with the lockfile's hint.
    fs::remove_dir_all(root.join("node_modules/lodash")).unwrap();
    write(root, "pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
    let s = setup(&cx(root, &p, &vars, None));
    assert_eq!(s.deps.status, DepsStatus::Missing);
    assert_eq!(s.deps.missing, vec!["lodash".to_string()]);
    assert_eq!(s.deps.hint, "pnpm install");
    // `--env` pointing to a node_modules that has it satisfies the check.
    let other = tempfile::tempdir().unwrap();
    write(other.path(), "node_modules/lodash/package.json", "{}");
    let env_dir = other.path().join("node_modules");
    let s = setup(&cx(root, &p, &vars, Some(&env_dir)));
    assert_eq!(s.deps.status, DepsStatus::Installed);
    let bad = other.path().join("nothing");
    let s = setup(&cx(root, &p, &vars, Some(&bad)));
    assert_eq!(s.env_not_found, Some(bad));
}

#[test]
fn rule_pnpm_workspace_members_required() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "package.json", r#"{"name":"hono","devDependencies":{"vitest":"1"}}"#);
    write(root, "pnpm-workspace.yaml", "packages:\n  - 'packages/*'\n  - '!packages/ignored'\n");
    write(root, "packages/core/package.json", r#"{"name":"core","dependencies":{"zod":"3"}}"#);
    write(root, "packages/ignored/package.json", r#"{"name":"ign","dependencies":{"left-pad":"1"}}"#);
    write(root, "benchmarks/deno/package.json", r#"{"name":"bench","dependencies":{"express":"4"}}"#);
    install(root, "", "vitest");
    let p = Platform::current();
    let vars = EnvVars::default();
    let s = setup(&cx(root, &p, &vars, None));
    assert_eq!(s.projects, vec!["", "packages/core"]);
    assert_eq!(s.deps.missing, vec!["zod".to_string()], "members are required");
    let subs: Vec<&str> = s.deps.subprojects.iter().map(|s| s.dir.as_str()).collect();
    assert_eq!(subs, vec!["benchmarks/deno", "packages/ignored"]);
    assert!(s
        .deps
        .notes
        .iter()
        .any(|n| n.starts_with("benchmarks/deno") && n.contains("express")));
    install(root, "packages/core", "zod");
    assert_eq!(setup(&cx(root, &p, &vars, None)).deps.status, DepsStatus::Installed);
}

#[test]
fn rule_manifest_without_analysed_script_files_is_not_required() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // A C repository with one loose script and an excluded (not inventoried) JS project.
    write(root, "docs/js/search.js", "");
    write(root, "excluded_js/package.json", r#"{"name":"x","dependencies":{"absent":"1"}}"#);
    write(root, "excluded_js/index.js", "");
    let p = Platform::current();
    let vars = EnvVars::default();
    let files = [("docs/js/search.js", Language::JavaScript), ("src/main.c", Language::C)];
    let mut c = cx(root, &p, &vars, None);
    c.files = &files;
    let s = setup(&c);
    assert!(s.projects.is_empty(), "{:?}", s.projects);
    assert_eq!(s.deps.status, DepsStatus::NoneDeclared, "{:?}", s.deps.missing);
    // Once its files are analysed, the manifest's dependencies are required.
    let files = [("excluded_js/index.js", Language::JavaScript)];
    c.files = &files;
    let s = setup(&c);
    assert_eq!(s.projects, vec!["excluded_js"]);
    assert_eq!(s.deps.status, DepsStatus::Missing);
}

/// The project walks follow the inventory's ignore rules: a git-ignored folder (a tool
/// cache holding a `package.json` with uninstalled dependencies and a `node_modules`) is
/// never a sub-project, never a status line and never a mapping; a folder the inventory
/// entered still is.
#[test]
fn rule_ignored_directories_are_never_sub_projects() {
    let dir = tempfile::tempdir().unwrap();
    // A non-hidden repository folder (temp dir names may start with a dot).
    fs::create_dir_all(dir.path().join("repo")).unwrap();
    let root = trace_core::inventory::canonical_root(&dir.path().join("repo")).unwrap();
    let root = root.as_path();
    write(root, "package.json", r#"{"name":"app"}"#);
    write(root, "src/index.js", "module.exports = 1;\n");
    write(root, ".gitignore", "tools/\n");
    write(root, "tools/cache/pkg/package.json", r#"{"name":"cached","dependencies":{"absent-dep":"1"}}"#);
    write(root, "tools/cache/pkg/index.js", "module.exports = 2;\n");
    write(root, "tools/cache/pkg/node_modules/x/package.json", "{}");
    write(root, "examples/demo/package.json", r#"{"name":"demo","dependencies":{"missing-dep":"1"}}"#);
    write(root, "examples/demo/main.js", "module.exports = 3;\n");
    let inventory = trace_core::inventory::scan(root, &Default::default()).unwrap();
    let owned: Vec<(String, Language)> = inventory
        .sources
        .iter()
        .filter_map(|e| e.language.map(|l| (e.path.clone(), l)))
        .collect();
    let files: Vec<(&str, Language)> = owned.iter().map(|(p, l)| (p.as_str(), *l)).collect();
    assert!(!files.iter().any(|(p, _)| p.starts_with("tools/")), "{files:?}");
    let p = Platform::current();
    let vars = EnvVars::default();
    let mut c = cx(root, &p, &vars, None);
    c.files = &files;
    let s = setup(&c);
    let subs: Vec<&str> = s.deps.subprojects.iter().map(|s| s.dir.as_str()).collect();
    assert_eq!(subs, vec!["examples/demo"], "an ignored folder is never a sub-project");
    assert!(s.deps.notes.iter().any(|n| n.contains("missing-dep")));
    assert!(!s.deps.notes.iter().any(|n| n.contains("absent-dep")), "{:?}", s.deps.notes);
    assert!(s.node_modules.iter().all(|m| !m.dir.starts_with("tools")), "{:?}", s.node_modules);
    // Without an inventory (no analysed files known) every folder is walked.
    let all = package_dirs(root, &cx(root, &p, &vars, None), None);
    assert!(all.iter().any(|d| d == "tools/cache/pkg"), "{all:?}");
}

#[test]
fn rule_yarn_pnp_is_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "package.json", r#"{"name":"x"}"#);
    write(root, "yarn.lock", "");
    write(root, ".yarnrc.yml", "yarnPath: .yarn/releases/yarn.cjs\n");
    let p = Platform::current();
    let vars = EnvVars::default();
    assert_eq!(setup(&cx(root, &p, &vars, None)).limit, Some(NodeLimit::YarnPnp));
    write(root, ".yarnrc.yml", "nodeLinker: node-modules\n");
    assert_eq!(setup(&cx(root, &p, &vars, None)).limit, None);
    write(root, ".pnp.cjs", "");
    assert_eq!(setup(&cx(root, &p, &vars, None)).limit, Some(NodeLimit::YarnPnp));
    let deno = tempfile::tempdir().unwrap();
    write(deno.path(), "deno.json", "{}");
    assert_eq!(setup(&cx(deno.path(), &p, &vars, None)).limit, Some(NodeLimit::DenoOnly));
}
