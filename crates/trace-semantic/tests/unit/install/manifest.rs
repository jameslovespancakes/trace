use super::*;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("trace-fixtures-manifest-{name}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn rule_manifest_schema_1_means_nothing_installed() {
    let dir = temp("schema");
    std::fs::create_dir_all(dir.join("clangd/22.1.6")).unwrap();
    std::fs::write(dir.join(MANIFEST_FILE), br#"{"schema":1,"tools":{"clangd":{}}}"#).unwrap();
    assert_eq!(Manifest::load(&dir), Manifest::default());
    std::fs::write(
        dir.join(MANIFEST_FILE),
        br#"{"schema":2,"tools":{"clangd":{"version":"22.1.6"},"metals":{"version":"1.6.0"}}}"#,
    )
    .unwrap();
    let m = Manifest::load(&dir);
    assert_eq!(m.tool_dir(&dir, "clangd"), Some(dir.join("clangd").join("22.1.6")));
    assert_eq!(m.tool_dir(&dir, "metals"), None, "listed but not on disk");
    assert_eq!(m.tool_dir(&dir, "gopls"), None);
    assert!(m.has_version(&dir, "clangd", "22.1.6"));
    assert!(!m.has_version(&dir, "clangd", "22.1.7"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// PLAN decision 11: an accepted licence is remembered per tool version (a new version
/// asks again) and survives a save/load round trip.
#[test]
fn rule_licence_acceptance_is_remembered_per_version() {
    let dir = temp("licence");
    let mut m = Manifest::default();
    assert!(!m.licence_accepted("intelephense", "1.18.5"));
    m.accept_licence("intelephense", "1.18.5");
    m.record_extra("lsp:metals", "mtags-2.13.18");
    m.save(&dir).unwrap();
    let loaded = Manifest::load(&dir);
    assert_eq!(loaded.schema, MANIFEST_SCHEMA);
    assert!(loaded.licence_accepted("intelephense", "1.18.5"));
    assert!(!loaded.licence_accepted("intelephense", "1.19.0"));
    assert!(loaded.has_extra("lsp:metals", "mtags-2.13.18"));
    assert!(!loaded.has_extra("lsp:metals", "mtags-3.3.8"));
    let _ = std::fs::remove_dir_all(&dir);
}
