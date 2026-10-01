use super::*;
use crate::test_support::Fixture;

/// Configuration names and patterns: server workspace configs and test-runner configs are
/// configs; a source file never is (editing it must not count as a config change).
#[test]
fn rule_config_patterns_never_match_sources() {
    for name in [
        "composer.json",
        "jsconfig.json",
        "pytest.ini",
        "tsconfig.build.json",
        "App.csproj",
    ] {
        assert!(is_config_name(name), "{name}");
    }
    for name in ["main.ts", "vite.config.ts", "tsconfig.ts", ".csproj", "setup.py"] {
        assert!(!is_config_name(name), "{name}");
    }
}

fn paths(entries: &[InventoryEntry]) -> Vec<&str> {
    entries.iter().map(|e| e.path.as_str()).collect()
}

fn reason<'i>(inv: &'i Inventory, path: &str) -> Option<&'i str> {
    inv.omitted.iter().find(|o| o.path == path).map(|o| o.reason.as_str())
}

#[test]
fn exclusions_and_reasons() {
    let fx = Fixture::new("inventory");
    let root = fx.dir("repo");
    fx.write("repo/src/a.py", "def a():\n    pass\n");
    fx.write("repo/src/b.ts", "export function b() {}\n");
    fx.write("repo/src/pkg.pyi", "def a() -> None: ...\n");
    fx.write("repo/.env", "SECRET=1\n");
    fx.write("repo/.env.local.py", "x = 1\n");
    fx.write("repo/.hidden/x.py", "x = 1\n");
    fx.write("repo/.git/info/exclude", "excluded_by_git.py\n");
    fx.write("repo/excluded_by_git.py", "x = 1\n");
    fx.write("repo/node_modules/m/index.js", "module.exports = 1\n");
    fx.write("repo/build/out.py", "x = 1\n");
    fx.write("repo/Vendor/lib.go", "package lib\n");
    fx.write("repo/secrets.json", "{}\n");
    fx.write("repo/certs/server.key", "---\n");
    fx.write("repo/ignored.py", "x = 1\n");
    fx.write("repo/sub/.gitignore", "local_ignored.rs\n");
    fx.write("repo/sub/local_ignored.rs", "fn x() {}\n");
    fx.write("repo/sub/kept.rs", "fn y() {}\n");
    fx.write("repo/.gitignore", "ignored.py\n");
    fx.write("repo/big.py", &"#".repeat(300));
    fx.write("repo/pyproject.toml", "[project]\nname = 'x'\n");
    fx.write("repo/notes.txt", "not source\n");

    let opts = InventoryOptions {
        max_file_bytes: 200,
        ..InventoryOptions::default()
    };
    let root = canonical_root(&root).unwrap();
    let inv = scan(&root, &opts).unwrap();
    assert_eq!(paths(&inv.sources), vec!["src/a.py", "src/b.ts", "src/pkg.pyi", "sub/kept.rs"]);
    assert_eq!(paths(&inv.configs), vec!["pyproject.toml"]);
    assert_eq!(inv.sources[0].language, Some(Language::Python));
    assert_eq!(reason(&inv, "big.py"), Some("file_size_limit"));
    assert_eq!(reason(&inv, "secrets.json"), Some("sensitive"));
    assert_eq!(reason(&inv, "certs/server.key"), Some("sensitive"));
    for never in [".env", ".env.local.py", ".hidden/x.py", "ignored.py"] {
        assert!(reason(&inv, never).is_none(), "{never} must not be listed");
    }
    assert!(inv.omitted.windows(2).all(|w| w[0].path <= w[1].path));
}

/// folders and files the user excludes are not inventoried (so never
/// analysed or required); excluded files are listed as omitted, never dropped silently.
#[test]
fn rule_excluded_folder_is_not_required() {
    let fx = Fixture::new("inventory-exclude");
    let root = fx.dir("repo");
    fx.write("repo/src/app.py", "x = 1\n");
    fx.write("repo/examples/demo/Main.java", "class Main {}\n");
    fx.write("repo/tools/gen.gen.py", "y = 2\n");
    let root = canonical_root(&root).unwrap();
    let opts = InventoryOptions {
        exclude: vec!["examples/".into(), "*.gen.py".into()],
        ..InventoryOptions::default()
    };
    let inv = scan(&root, &opts).unwrap();
    assert_eq!(paths(&inv.sources), vec!["src/app.py"]);
    assert_eq!(reason(&inv, "tools/gen.gen.py"), Some("excluded"));
    let all = scan(&root, &InventoryOptions::default()).unwrap();
    assert_eq!(all.sources.len(), 3);
    // An unclosed `[` is a literal in gitignore syntax; an inverted range is invalid.
    let bad = InventoryOptions {
        exclude: vec!["[z-a]".into()],
        ..InventoryOptions::default()
    };
    assert!(matches!(scan(&root, &bad), Err(CoreError::Config(_))));
}

#[test]
fn limits_are_errors() {
    let fx = Fixture::new("inventory-limits");
    let root = fx.dir("repo");
    fx.write("repo/a.py", "a = 1\n");
    fx.write("repo/b.py", "b = 1\n");
    let root = canonical_root(&root).unwrap();
    let few = InventoryOptions {
        max_files: 1,
        ..InventoryOptions::default()
    };
    assert!(matches!(scan(&root, &few), Err(CoreError::Limit(_))));
    let small = InventoryOptions {
        max_total_bytes: 8,
        ..InventoryOptions::default()
    };
    assert!(matches!(scan(&root, &small), Err(CoreError::Limit(_))));
}

#[test]
fn symlinks_are_omitted_and_refused() {
    let fx = Fixture::new("inventory-symlink");
    let root = fx.dir("repo");
    fx.write("repo/real.py", "x = 1\n");
    fx.write("outside/secret.py", "y = 2\n");
    let target = fx.path("outside/secret.py");
    let link = root.join("link.py");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&target, &link).is_ok();
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(&target, &link).is_ok();
    #[cfg(not(any(unix, windows)))]
    let made = false;
    if !made {
        // Creating symlinks needs privileges on some Windows setups; nothing to test.
        return;
    }
    let root = canonical_root(&root).unwrap();
    let inv = scan(&root, &InventoryOptions::default()).unwrap();
    assert_eq!(paths(&inv.sources), vec!["real.py"]);
    assert_eq!(reason(&inv, "link.py"), Some("symlink"));
    assert!(matches!(safe_source_path(&root, "link.py"), Err(CoreError::Symlink(_))));
}

#[test]
fn safe_source_path_rejections() {
    let fx = Fixture::new("inventory-safe");
    let root = fx.dir("repo");
    fx.write("repo/src/a.py", "x = 1\n");
    let root = canonical_root(&root).unwrap();
    assert!(safe_source_path(&root, "src/a.py").is_ok());
    for bad in ["", "/etc/passwd", "../x.py", "src/../a.py", "src\\a.py", "C:/x.py", "src//a.py"] {
        assert!(matches!(safe_source_path(&root, bad), Err(CoreError::InvalidRelativePath(_))), "{bad:?}");
    }
    for bad in [".env", "node_modules/x.js", "src/.hidden.py", "secrets.json"] {
        assert!(matches!(safe_source_path(&root, bad), Err(CoreError::Sensitive(_))), "{bad:?}");
    }
    assert!(matches!(safe_source_path(&root, "src/missing.py"), Err(CoreError::Io { .. })));
}

#[test]
fn hashing_reuses_and_fingerprints() {
    let fx = Fixture::new("inventory-hash");
    let root = fx.dir("repo");
    fx.write("repo/a.py", "a = 1\r\n");
    fx.write("repo/b.py", "\u{FEFF}b = 2\n");
    let root = canonical_root(&root).unwrap();
    let inv = scan(&root, &InventoryOptions::default()).unwrap();
    let hashed: Vec<HashedEntry> = hash_entries(&inv.sources, |_, _, _| None)
        .into_iter()
        .collect::<Result<_>>()
        .unwrap();
    assert_eq!(hashed[0].hash, Hash32::of(b"a = 1\r\n"));
    assert_eq!(hashed[1].hash, Hash32::of("\u{FEFF}b = 2\n".as_bytes()));
    let reused = hash_entries(&inv.sources, |_, _, _| Some(Hash32::default()));
    assert!(reused.iter().all(|r| r.as_ref().unwrap().hash == Hash32::default()));

    let fp = |h: &[HashedEntry]| {
        inventory_fingerprint(h.iter().map(|e| (e.entry.path.as_str(), &e.hash)), std::iter::empty())
    };
    let before = fp(&hashed[..]);
    let mut changed = hashed.clone();
    changed[0].hash = Hash32::of(b"other");
    assert_ne!(before, fp(&changed[..]));
    assert_eq!(before, fp(&hashed[..]));
}

#[test]
fn name_predicates() {
    assert!(is_excluded_dir(".git"));
    assert!(is_excluded_dir("node_modules"));
    assert!(is_excluded_dir("Build"));
    assert!(!is_excluded_dir("src"));
    assert!(is_sensitive_name(".env.production"));
    assert!(is_sensitive_name("ID_RSA"));
    assert!(is_sensitive_name("cert.PEM"));
    assert!(!is_sensitive_name("keys.py"));
}
