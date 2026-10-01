use super::*;

#[test]
fn materialize_writes_and_repairs_assets() {
    let home = std::env::temp_dir()
        .join("trace-tests")
        .join("trace-semantic-tests")
        .join(format!("assets-{}", uuid::Uuid::new_v4().simple()));
    let paths = materialize(&home).unwrap();
    assert_eq!(fs::read_to_string(&paths.ts_worker).unwrap(), TS_WORKER_FILES[0].1);
    assert_eq!(fs::read_to_string(&paths.lsp_guard).unwrap(), LSP_GUARD);
    fs::write(&paths.lsp_guard, b"tampered").unwrap();
    let again = materialize(&home).unwrap();
    assert_eq!(again.dir, paths.dir);
    assert_eq!(fs::read_to_string(&again.lsp_guard).unwrap(), LSP_GUARD);
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn embedded_assets_are_the_reference_scripts() {
    assert!(TS_WORKER_FILES[0].1.contains("openProject"));
    assert!(TS_WORKER_FILES[1].1.contains("createVirtualFileSystem"));
    assert!(LSP_GUARD.contains("CODEPATH_LSP_WRITE_ROOT"));
}

/// The guard allows writes only below the workspace and the declared state roots.
#[test]
fn rule_lsp_guard_allows_only_declared_write_roots() {
    assert!(LSP_GUARD.contains("CODEPATH_LSP_WRITE_ROOTS"));
    assert!(LSP_GUARD.contains("path.delimiter"));
    assert!(LSP_GUARD.contains("write outside analysis workspace refused"));
    for worker in ["--serve", "clearSourceFileCache", "bundled_types"] {
        assert!(TS_WORKER_FILES.iter().any(|(_, text)| text.contains(worker)), "{worker}");
    }
}
