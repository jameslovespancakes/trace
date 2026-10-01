use super::*;

/// `trace status` lists every setting of the user's config.json that differs from the
/// defaults, with the file as its origin; values equal to the defaults are not listed.
#[test]
fn rule_status_lists_non_default_settings_with_their_origin() {
    let user = serde_json::json!({
        "semantic": {"max_in_flight": 3},
        "watch": {"debounce_ms": trace_core::config::defaults().watch.debounce_ms},
    });
    let settings = Settings::from_user(user).unwrap();
    let home = Path::new("/home/u/.trace");
    let rows: Vec<SettingRow> = settings_rows(&settings, home)
        .into_iter()
        .filter(|r| r.key.starts_with("semantic.max"))
        .collect();
    assert_eq!(
        rows,
        vec![SettingRow {
            key: "semantic.max_in_flight".into(),
            value: "3".into(),
            origin: Settings::path(home).display().to_string(),
        }]
    );
    assert!(settings_rows(&settings, home)
        .iter()
        .all(|r| r.key != "watch.debounce_ms"));
}
