use super::*;

#[test]
fn globs_match_segments_and_names() {
    assert!(glob_match("go.mod", "go.mod"));
    assert!(glob_match("go.mod", "sub/go.mod"), "a name pattern matches anywhere");
    assert!(glob_match("**/pom.xml", "pom.xml"));
    assert!(glob_match("**/pom.xml", "a/b/pom.xml"));
    assert!(!glob_match("**/pom.xml", "a/b/pom.xml.bak"));
    assert!(glob_match("deps/**", "deps/jsonlib/lib/jsonlib.py"));
    assert!(!glob_match("deps/**", "lib/deps/x.py"));
    assert!(glob_match("**/obj/*.nuget.g.*", "src/App/obj/App.csproj.nuget.g.props"));
    assert!(glob_match("tsconfig*.json", "web/tsconfig.base.json"));
    assert!(glob_match("build.gradle*", "app/build.gradle.kts"));
    assert!(!glob_match("*.cabal", "cabal.project"));
    assert!(glob_match("project/*.sbt", "project/plugins.sbt"));
    assert!(!glob_match("project/*.sbt", "sub/project/plugins.sbt"));
}

fn temp_repo(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join("trace-semantic-tests")
        .join(format!("{name}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    trace_core::inventory::strip_verbatim(std::fs::canonicalize(&dir).unwrap())
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Rule: the mirror follows the repository's ignore rules, adds git-ignored restore
/// outputs named by `include_ignored`, drops `exclude` matches and never enters `.git`.
#[test]
fn rule_mirror_honours_gitignore_and_include_ignored() {
    let root = temp_repo("mirror-walk");
    write(&root, ".gitignore", "deps/\nout/\n*.log\nobj/\n");
    write(&root, "pyproject.toml", "[project]\nname = \"a\"\n");
    write(&root, "lib/a.py", "A = 1\n");
    write(&root, ".editorconfig", "root = true\n");
    write(&root, "deps/jsonlib/lib/jsonlib.py", "B = 2\n");
    write(&root, "out/dev/lib/a.pyc", "x");
    write(&root, "debug.log", "x");
    write(&root, "src/App/obj/project.assets.json", "{}");
    write(&root, "src/App/obj/Debug/x.dll", "x");
    write(&root, ".build/checkouts/p/Package.toml", "x");
    write(&root, ".git/config", "[core]\n");
    let rules = MirrorRules {
        include_ignored: vec!["deps/**".into(), "**/obj/project.assets.json".into()],
        exclude: vec![".build/**".into()],
    };
    let rels: Vec<String> = walk(&root, &rules).into_iter().map(|e| e.rel).collect();
    assert_eq!(
        rels,
        vec![
            ".editorconfig",
            ".gitignore",
            "deps/jsonlib/lib/jsonlib.py",
            "lib/a.py",
            "pyproject.toml",
            "src/App/obj/project.assets.json",
        ]
    );
    // Watcher hints: present files are copied, vanished ones deleted.
    std::fs::remove_file(root.join("lib/a.py")).unwrap();
    let (present, gone) = stat_paths(&root, &rules, &["pyproject.toml".into(), "lib/a.py".into()]);
    assert_eq!(present.iter().map(|e| e.rel.as_str()).collect::<Vec<_>>(), vec!["pyproject.toml"]);
    assert_eq!(gone, vec!["lib/a.py".to_string()]);
    let _ = std::fs::remove_dir_all(&root);
}

/// Rule: sensitive files (`.env*`, keys, credential files) never enter a mirror, not
/// even when a pattern includes them.
#[test]
fn rule_mirror_never_copies_sensitive_files() {
    let root = temp_repo("mirror-sensitive");
    write(&root, "app.py", "x = 1\n");
    write(&root, ".env", "KEY=1\n");
    write(&root, ".env.local", "KEY=1\n");
    write(&root, "certs/server.key", "x");
    write(&root, "config/credentials.json", "{}");
    write(&root, ".gitignore", "secrets/\n");
    write(&root, "secrets/.env", "KEY=2\n");
    let rules = MirrorRules {
        include_ignored: vec!["secrets/**".into()],
        exclude: Vec::new(),
    };
    let rels: Vec<String> = walk(&root, &rules).into_iter().map(|e| e.rel).collect();
    assert_eq!(rels, vec![".gitignore", "app.py"]);
    let (present, _) = stat_paths(&root, &rules, &[".env".into(), "app.py".into()]);
    assert_eq!(present.iter().map(|e| e.rel.as_str()).collect::<Vec<_>>(), vec!["app.py"]);
    assert!(!rules.allowed("certs/server.key"));
    assert!(!rules.allowed(".git/config"));
    let _ = std::fs::remove_dir_all(&root);
}
