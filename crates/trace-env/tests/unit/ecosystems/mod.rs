use super::*;
use std::fs;

#[test]
fn rule_ecosystems_round_trip_and_map_languages() {
    for e in EcosystemId::ALL {
        assert_eq!(EcosystemId::parse(e.as_str()), Some(e));
        assert_eq!(serde_json::to_string(&e).unwrap(), format!("\"{}\"", e.as_str()));
    }
    assert_eq!(EcosystemId::of_language(Language::Tsx), Some(EcosystemId::Node));
    assert_eq!(EcosystemId::of_language(Language::Scala), Some(EcosystemId::Jvm));
    assert_eq!(EcosystemId::of_language(Language::Bash), None);
}

#[test]
fn rule_env_path_is_classified_by_ecosystem() {
    let dir = tempfile::tempdir().unwrap();
    crate::test_support::write(dir.path(), "venv/pyvenv.cfg", "version_info = 3.12.1\n");
    fs::create_dir_all(dir.path().join("venv/Lib/site-packages")).unwrap();
    assert_eq!(classify_env_path(&dir.path().join("venv")), Some(EcosystemId::Python));
    assert_eq!(classify_env_path(&dir.path().join("nothing")), None);
}
