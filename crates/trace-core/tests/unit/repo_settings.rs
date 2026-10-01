use super::*;
use crate::test_support::Fixture;

fn paths(fx: &Fixture) -> RepoPaths {
    let root = fx.dir("repo");
    let home = fx.dir("home");
    RepoPaths::resolve_in(&root, &home).unwrap()
}

#[test]
fn rule_settings_missing_file_is_default() {
    let fx = Fixture::new("settings-missing");
    let p = paths(&fx);
    assert_eq!(RepoSettings::load(&p).unwrap(), RepoSettings::default());
}

#[test]
fn rule_settings_round_trip_outside_the_repository() {
    let fx = Fixture::new("settings-roundtrip");
    let p = paths(&fx);
    let mut s = RepoSettings {
        allow_build: true,
        ..RepoSettings::default()
    };
    s.env.insert("python".into(), fx.dir("venv"));
    s.save(&p).unwrap();
    assert!(p.settings_file.starts_with(&p.repo_dir));
    assert!(!p.settings_file.starts_with(&p.root));
    let loaded = RepoSettings::load(&p).unwrap();
    assert!(loaded.allow_build);
    assert_eq!(loaded.schema, SETTINGS_SCHEMA);
    assert_eq!(loaded.env_for("python"), Some(fx.path("venv").as_path()));
    assert_eq!(loaded.env_for("node"), None);
}

/// `trace index --allow-build` is given once: every later load of this repository's
/// settings keeps it (and so does a later save of other settings).
#[test]
fn rule_settings_remember_allow_build() {
    let fx = Fixture::new("settings-allow-build");
    let p = paths(&fx);
    RepoSettings {
        allow_build: true,
        ..RepoSettings::default()
    }
    .save(&p)
    .unwrap();
    let mut later = RepoSettings::load(&p).unwrap();
    assert!(later.allow_build);
    later.ready_languages.insert(Language::Java);
    later.ready_dirs.insert("examples/demo".into());
    later.save(&p).unwrap();
    let again = RepoSettings::load(&p).unwrap();
    assert!(again.allow_build);
    assert!(again.ready_languages.contains(&Language::Java));
    assert!(again.dir_ready("examples/demo/"));
    assert!(!again.dir_ready("examples"));
}

#[test]
fn rule_settings_of_another_schema_are_default() {
    let fx = Fixture::new("settings-schema");
    let p = paths(&fx);
    p.ensure_repo_dir().unwrap();
    fs::write(&p.settings_file, br#"{"schema":99,"allow_build":true}"#).unwrap();
    assert_eq!(RepoSettings::load(&p).unwrap(), RepoSettings::default());
}
