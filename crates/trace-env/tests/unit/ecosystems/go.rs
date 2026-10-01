use super::*;
use crate::test_support::write;

fn linux() -> Platform {
    Platform {
        os: Os::Linux,
        arch: os::Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    }
}

fn windows() -> Platform {
    Platform {
        os: Os::Windows,
        ..linux()
    }
}

#[test]
fn rule_go_mod_is_read_structurally() {
    let m = parse_go_mod(
        "module github.com/x/App // comment\n\ngo 1.23.0\ntoolchain go1.24.2\n\nrequire (\n\tgithub.com/BurntSushi/toml v1.4.0\n\tgolang.org/x/sys v0.30.0 // indirect\n)\nrequire example.com/one v1.0.0\nreplace example.com/one => ../one\nreplace (\n\tgolang.org/x/sys v0.30.0 => golang.org/x/sys v0.31.0\n)\n",
    );
    assert_eq!(m.module, "github.com/x/App");
    assert_eq!(m.go.as_deref(), Some("1.23.0"));
    assert_eq!(m.toolchain.as_deref(), Some("go1.24.2"));
    assert_eq!(m.requires.len(), 3);
    assert_eq!(m.requires[0], ("github.com/BurntSushi/toml".to_string(), "v1.4.0".to_string()));
    assert_eq!(m.replaces.len(), 2);
    assert!(m.replaces[0].is_local());
    assert_eq!(m.replaces[1].new_version.as_deref(), Some("v0.31.0"));
    assert_eq!(escape_module("github.com/BurntSushi/toml"), "github.com/!burnt!sushi/toml");
}

#[test]
fn rule_gopath_list_uses_first_entry() {
    assert_eq!(gopath_first("/home/u/go:/opt/go2", &linux()), Some(PathBuf::from("/home/u/go")));
    assert_eq!(gopath_first(r"C:\go;D:\go2", &windows()), Some(PathBuf::from(r"C:\go")));
    // The module cache follows the first entry of this machine's list.
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    let list = std::env::join_paths([&a, &b]).unwrap().into_string().unwrap();
    let empty: [(&str, Language); 0] = [];
    let vars = EnvVars::from_pairs(&[("GOPATH", list.as_str()), ("GOENV", "off")]);
    let root = dir.path().join("repo");
    let p = Platform::current();
    let cx = DetectContext {
        root: &root,
        platform: &p,
        vars: &vars,
        env_override: None,
        forbidden: &[],
        files: &empty,
    };
    assert_eq!(module_cache_dir(&cx), Some(a.join("pkg").join("mod")));
}

#[test]
fn rule_go_mod_requires_checked_in_modcache() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let cache = dir.path().join("modcache");
    write(&root, "main.go", "package main\n");
    write(
        &root,
        "go.mod",
        "module example.com/app\n\ngo 1.22\n\nrequire (\n\tgithub.com/BurntSushi/toml v1.4.0\n\tgolang.org/x/text v0.20.0 // indirect\n)\n",
    );
    write(
        &root,
        "tools/go.mod",
        "module example.com/tools\n\ngo 1.22\n\nrequire example.com/nothere v1.0.0\n",
    );
    write(&root, "tools/main.go", "package main\n");
    write(&cache, "cache/download/.keep", "");
    write(&cache, "github.com/!burnt!sushi/toml@v1.4.0/go.mod", "module x\n");
    let cache_text = cache.display().to_string();
    let vars = EnvVars::from_pairs(&[("GOMODCACHE", cache_text.as_str()), ("GOENV", "off")]);
    let p = linux();
    let files = [("main.go", Language::Go), ("tools/main.go", Language::Go)];
    let cx = DetectContext {
        root: &root,
        platform: &p,
        vars: &vars,
        env_override: None,
        forbidden: &[],
        files: &files,
    };
    let report = deps(&cx, None);
    assert_eq!(report.status, DepsStatus::Missing);
    assert_eq!(report.missing, vec!["golang.org/x/text@v0.20.0"]);
    assert_eq!(report.hint, "go mod download");
    assert_eq!(report.subprojects.len(), 1, "tools/ is a separate module");
    assert_eq!(report.notes.len(), 1);
    write(&cache, "golang.org/x/text@v0.20.0/go.mod", "module x\n");
    assert_eq!(deps(&cx, None).status, DepsStatus::Installed);
}

#[test]
fn rule_go_version_below_go_directive_is_too_old() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let goroot = dir.path().join("go");
    write(&root, "go.mod", "module example.com/app\n\ngo 1.26\n");
    write(&root, "main.go", "package main\n");
    write(&goroot, "bin/go", "");
    write(&goroot, "src/runtime/.keep", "");
    write(&goroot, "VERSION", "go1.25.3\ntime 2026-01-01\n");
    let vars = EnvVars::from_pairs(&[("GOENV", "off")]);
    let p = linux();
    let files = [("main.go", Language::Go)];
    let cx = DetectContext {
        root: &root,
        platform: &p,
        vars: &vars,
        env_override: Some(&goroot),
        forbidden: &[],
        files: &files,
    };
    match toolchain(&cx) {
        ToolchainStatus::TooOld {
            found,
            needed,
            source,
        } => {
            assert_eq!(found.version.unwrap().text, "1.25.3");
            assert_eq!(found.origin, Origin::Override);
            assert_eq!(needed.describe("Go"), "Go 1.26 or newer");
            assert_eq!(source, "go.mod");
        }
        other => panic!("expected TooOld, got {other:?}"),
    }
}

/// Go installed by a version manager (mise: one GOROOT per version; asdf-golang: below
/// `go`) is found at its install folder, the version the project needs first; their `PATH`
/// entries are shims that name no GOROOT.
#[test]
fn rule_version_manager_go_installs_are_found() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let home = dir.path().join("home");
    write(&root, "go.mod", "module example.com/app\n\ngo 1.99\n");
    write(&root, "main.go", "package main\n");
    for (goroot, version) in [
        (home.join(".local/share/mise/installs/go/1.98.0"), "go1.98.0"),
        (home.join(".local/share/mise/installs/go/1.99.1"), "go1.99.1"),
        (home.join(".asdf/installs/golang/1.99.2/go"), "go1.99.2"),
    ] {
        write(&goroot, "bin/go", "");
        write(&goroot, "VERSION", &format!("{version}\n"));
    }
    let h = home.display().to_string();
    let vars = EnvVars::from_pairs(&[("GOENV", "off"), ("HOME", h.as_str())]);
    let p = linux();
    let files = [("main.go", Language::Go)];
    let cx = DetectContext {
        root: &root,
        platform: &p,
        vars: &vars,
        env_override: None,
        forbidden: &[],
        files: &files,
    };
    match toolchain(&cx) {
        ToolchainStatus::Found(t) => {
            assert_eq!(t.version.unwrap().text, "1.99.1", "mise before asdf, newest first");
            assert_eq!(t.origin, Origin::StandardLocation);
        }
        other => panic!("expected Found, got {other:?}"),
    }
}
