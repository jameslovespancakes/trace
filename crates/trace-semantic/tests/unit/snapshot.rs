use super::*;

fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join("trace-semantic-tests")
        .join(format!("{name}-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&dir).unwrap();
    strip_verbatim(fs::canonicalize(&dir).unwrap())
}

#[test]
fn snapshot_copies_maps_and_verifies() {
    let target = test_dir("snap-target");
    let cache = test_dir("snap-cache");
    let bytes = b"\xEF\xBB\xBFdef f():\r\n    pass\r\n".to_vec();
    fs::create_dir_all(target.join("pkg")).unwrap();
    fs::write(target.join("pkg").join("m.py"), &bytes).unwrap();
    let files = [SnapshotFile {
        path: "pkg/m.py",
        hash: Hash32::of(&bytes),
        bytes: &bytes,
    }];
    let snap = Snapshot::create(&cache.join("workspaces"), "lsp:gopls", &target, &files).unwrap();
    assert!(snap.dir.ends_with(Path::new("lsp-gopls").join("tree")));
    let copied = snap.path_of("pkg/m.py");
    assert_eq!(fs::read(&copied).unwrap(), bytes);
    assert_eq!(snap.relative(&copied).as_deref(), Some("pkg/m.py"));
    assert_eq!(snap.relative(&target.join("pkg").join("m.py")), None);
    snap.verify_originals().unwrap();
    fs::write(target.join("pkg").join("m.py"), b"changed").unwrap();
    assert!(matches!(snap.verify_originals(), Err(SemanticError::SourceChanged(_))));
    let _ = fs::remove_dir_all(&target);
    let _ = fs::remove_dir_all(&cache);
}

/// Rule: a backend's workspace is one stable directory per repository cache: reopening
/// it (a new process) finds the same tree and state, syncs incrementally, and a new
/// stamp (another tool version) wipes both. A second concurrent user is refused.
#[test]
fn rule_workspace_dir_is_stable_per_backend() {
    let target = test_dir("stable-target");
    let cache = test_dir("stable-cache");
    let workspaces = cache.join("workspaces");
    let a1: &'static [u8] = b"package a\n";
    let file = |path: &'static str, bytes: &'static [u8]| SnapshotFile {
        path,
        hash: Hash32::of(bytes),
        bytes,
    };
    let opts = |stamp: &'static str| WorkspaceOptions {
        workspaces_dir: &workspaces,
        backend: "lsp:gopls",
        target_root: &target,
        mode: WorkspaceMode::Snapshot,
        stamp,
        rules: MirrorRules::default(),
        language: Language::Go,
        server_name: None,
    };
    let first = Snapshot::open(&opts("v1"), &[file("a.go", a1)]).unwrap();
    let dir = first.dir.clone();
    let state = first.outside_dir();
    assert!(state.ends_with(Path::new("lsp-gopls").join("state")));
    assert!(!state.starts_with(&dir), "{{outside}} is never inside the workspace");
    fs::write(state.join("server-cache"), b"warm").unwrap();
    assert!(matches!(Snapshot::open(&opts("v1"), &[]), Err(SemanticError::Worker(_))), "one user at a time");
    drop(first);
    let mut again = Snapshot::open_empty(&opts("v1")).unwrap();
    assert_eq!(again.dir, dir, "same directory, no uuid");
    let delta = again.sync(&[file("a.go", a1)]).unwrap();
    assert!(delta.is_empty(), "unchanged files are not rewritten: {delta:?}");
    assert!(state.join("server-cache").is_file(), "server state survives");
    drop(again);
    let wiped = Snapshot::open(&opts("v2"), &[file("a.go", a1)]).unwrap();
    assert_eq!(wiped.dir, dir);
    assert!(!state.join("server-cache").exists(), "a new stamp starts clean");
    drop(wiped);
    let _ = fs::remove_dir_all(&target);
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn sync_writes_changes_and_removes_stale_files() {
    let target = test_dir("sync-target");
    let cache = test_dir("sync-cache");
    let a1: &'static [u8] = b"def a(): pass\n";
    let b1: &'static [u8] = b"def b(): pass\n";
    let file = |path: &'static str, bytes: &'static [u8]| SnapshotFile {
        path,
        hash: Hash32::of(bytes),
        bytes,
    };
    let mut snap =
        Snapshot::create(&cache, "pyright", &target, &[file("a.py", a1), file("pkg/b.py", b1)]).unwrap();
    snap.write_aux("pyrightconfig.json", b"{}").unwrap();
    let unchanged = snap.sync(&[file("a.py", a1), file("pkg/b.py", b1)]).unwrap();
    assert!(unchanged.is_empty());
    let a2: &'static [u8] = b"def a(): return 2\n";
    let c1: &'static [u8] = b"def c(): pass\n";
    let delta = snap.sync(&[file("a.py", a2), file("c.py", c1)]).unwrap();
    assert_eq!(delta.added, vec!["c.py".to_string()]);
    assert_eq!(delta.changed, vec!["a.py".to_string()]);
    assert_eq!(delta.removed, vec!["pkg/b.py".to_string()]);
    assert_eq!(fs::read(snap.path_of("a.py")).unwrap(), a2);
    assert!(snap.path_of("c.py").is_file());
    assert!(!snap.path_of("pkg/b.py").exists());
    assert!(snap.dir.join("pyrightconfig.json").is_file(), "aux files survive");
    assert_eq!(snap.paths().collect::<Vec<_>>(), vec!["a.py", "c.py"]);
    // Only the requested originals are verified (none exist under the target here).
    assert!(snap.verify_paths(["a.py"]).is_err());
    assert!(snap.verify_paths(["unknown.py"]).is_ok());
    let bad = SnapshotFile {
        path: "a.py",
        hash: Hash32::of(b"other"),
        bytes: a1,
    };
    assert!(matches!(snap.sync(&[bad]), Err(SemanticError::SourceChanged(_))));
    drop(snap);
    let _ = fs::remove_dir_all(&target);
    let _ = fs::remove_dir_all(&cache);
}

/// Mirror mode: the repository tree is copied, analysed sources come from the verified
/// bytes, edits and deletions are synced incrementally, build outputs in the tree stay.
#[test]
fn mirror_syncs_the_repository_tree() {
    let target = test_dir("mirror-target");
    let cache = test_dir("mirror-cache");
    fs::write(target.join("pom.xml"), b"<project/>").unwrap();
    fs::create_dir_all(target.join("src")).unwrap();
    let src: &'static [u8] = b"class A {}\n";
    fs::write(target.join("src").join("A.java"), src).unwrap();
    let workspaces = cache.join("workspaces");
    let opts = WorkspaceOptions {
        workspaces_dir: &workspaces,
        backend: "lsp:jdtls",
        target_root: &target,
        mode: WorkspaceMode::Mirror,
        stamp: "fp",
        rules: MirrorRules::default(),
        language: Language::Java,
        server_name: None,
    };
    let a = SnapshotFile {
        path: "src/A.java",
        hash: Hash32::of(src),
        bytes: src,
    };
    let mut snap = Snapshot::open(&opts, std::slice::from_ref(&a)).unwrap();
    assert_eq!(fs::read(snap.path_of("pom.xml")).unwrap(), b"<project/>");
    assert!(snap.contains("src/A.java") && !snap.contains("pom.xml"));
    fs::create_dir_all(snap.dir.join("target")).unwrap();
    fs::write(snap.dir.join("target").join("A.class"), b"x").unwrap();
    fs::write(target.join("pom.xml"), b"<project><modules/></project>").unwrap();
    let delta = snap.sync(std::slice::from_ref(&a)).unwrap();
    assert_eq!(delta.changed, vec!["pom.xml".to_string()]);
    fs::remove_file(target.join("pom.xml")).unwrap();
    let delta = snap.sync_hinted(&[a], Some(&["pom.xml".to_string()])).unwrap();
    assert_eq!(delta.removed, vec!["pom.xml".to_string()]);
    assert!(snap.dir.join("target").join("A.class").is_file(), "build outputs stay");
    drop(snap);
    let _ = fs::remove_dir_all(&target);
    let _ = fs::remove_dir_all(&cache);
}

/// Rule: a backend's server name for an analysed file (Pyright: `<path>.py` for an
/// extensionless script) is the name in the tree, and the tree name maps back to the
/// repository path; a name another repository file already has is never used; renamed
/// files are removed and reopened like the others.
#[test]
fn rule_server_names_map_both_ways() {
    fn py_name(rel: &str) -> Option<String> {
        (!rel.ends_with(".py")).then(|| format!("{rel}.py"))
    }
    let target = test_dir("alias-target");
    let cache = test_dir("alias-cache");
    let workspaces = cache.join("workspaces");
    let tool: &'static [u8] = b"#!/usr/bin/env python\nprint(1)\n";
    let x: &'static [u8] = b"y = 1\n";
    let x_py: &'static [u8] = b"z = 2\n";
    fs::create_dir_all(target.join("bin")).unwrap();
    fs::write(target.join("bin").join("tool"), tool).unwrap();
    fs::write(target.join("x"), x).unwrap();
    fs::write(target.join("x.py"), x_py).unwrap();
    let file = |path: &'static str, bytes: &'static [u8]| SnapshotFile {
        path,
        hash: Hash32::of(bytes),
        bytes,
    };
    let opts = WorkspaceOptions {
        workspaces_dir: &workspaces,
        backend: "pyright",
        target_root: &target,
        mode: WorkspaceMode::Snapshot,
        stamp: "fp",
        rules: MirrorRules::default(),
        language: Language::Python,
        server_name: Some(py_name),
    };
    let all = [file("bin/tool", tool), file("x", x), file("x.py", x_py)];
    let snap = Snapshot::open(&opts, &all).unwrap();
    let aliased = snap.path_of("bin/tool");
    assert!(aliased.ends_with(Path::new("bin").join("tool.py")));
    assert_eq!(fs::read(&aliased).unwrap(), tool, "same bytes");
    assert!(!snap.dir.join("bin").join("tool").exists(), "one copy, under the server name");
    assert_eq!(snap.relative(&aliased).as_deref(), Some("bin/tool"));
    // `x.py` is a repository file: `x` keeps its own name.
    assert_eq!(snap.path_of("x"), snap.dir.join("x"));
    assert_eq!(fs::read(snap.path_of("x.py")).unwrap(), x_py);
    assert_eq!(snap.relative(&snap.dir.join("x.py")).as_deref(), Some("x.py"));
    snap.verify_originals().unwrap();
    drop(snap);
    let mut again = Snapshot::open_empty(&opts).unwrap();
    let delta = again.sync(&all).unwrap();
    assert!(delta.is_empty(), "server names are stable across opens: {delta:?}");
    let delta = again.sync(&[file("x", x), file("x.py", x_py)]).unwrap();
    assert_eq!(delta.removed, vec!["bin/tool".to_string()]);
    assert!(!aliased.exists(), "the renamed copy is removed");
    drop(again);
    let _ = fs::remove_dir_all(&target);
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn snapshot_refuses_hash_mismatch_and_bad_paths() {
    let target = test_dir("snap-target2");
    let cache = test_dir("snap-cache2");
    let bytes = b"x = 1\n".to_vec();
    let wrong = [SnapshotFile {
        path: "a.py",
        hash: Hash32::of(b"other"),
        bytes: &bytes,
    }];
    assert!(matches!(
        Snapshot::create(&cache, "pyright", &target, &wrong),
        Err(SemanticError::SourceChanged(_))
    ));
    let escape = [SnapshotFile {
        path: "../a.py",
        hash: Hash32::of(&bytes),
        bytes: &bytes,
    }];
    assert!(Snapshot::create(&cache, "pyright", &target, &escape).is_err());
    // A workspace inside the inspected root is refused.
    assert!(Snapshot::create(&target.join("ws"), "pyright", &target, &[]).is_err());
    let _ = fs::remove_dir_all(&target);
    let _ = fs::remove_dir_all(&cache);
}
