use super::*;
use crate::test_support::Fixture;

/// The embedded defaults parse, are complete and valid, and hold today's values.
#[test]
fn rule_defaults_parse() {
    let d = defaults();
    d.validate().unwrap();
    assert_eq!(d.memory.budget_mb, 4_096);
    assert_eq!(d.workers.server_processes, Count::Auto);
    assert_eq!(d.workers.server_processes_max, 4);
    assert_eq!(d.semantic.request_timeout_secs, 60);
    assert_eq!(d.semantic.session_deadline_secs, 900);
    assert_eq!(d.semantic.max_in_flight, 16);
    assert!(d.semantic.auto_install);
    assert_eq!(d.inventory.max_files, 50_000);
    assert!(d.bridges.enabled && d.bridges.http && d.bridges.weak);
    assert!(d.debug.syntax_answers && d.debug.calls_by_definition.is_empty());
    assert!(d.non_default().is_empty());
}

#[test]
fn defaults_and_partial_files() {
    let fx = Fixture::new("config");
    let home = fx.dir("home");
    assert_eq!(Settings::load(&home).unwrap(), Settings::default());
    fx.write("home/config.json", r#"{"cache": {"compact_ratio": 8}, "inventory": {"max_files": 10}}"#);
    let cfg = Settings::load(&home).unwrap();
    assert_eq!(cfg.cache.compact_ratio, 8);
    assert_eq!(cfg.cache.idle_compact_segments, 16);
    assert_eq!(cfg.inventory.max_files, 10);
    assert_eq!(cfg.inventory.max_file_bytes, 2_000_000);
    assert_eq!(
        cfg.non_default(),
        vec![
            ("cache.compact_ratio".to_string(), "8".to_string()),
            ("inventory.max_files".to_string(), "10".to_string())
        ]
    );
}

#[test]
fn limits_are_validated() {
    let fx = Fixture::new("config-invalid");
    let home = fx.dir("home");
    for bad in [
        r#"{"cache": {"compact_ratio": 0}}"#,
        r#"{"semantic": {"max_in_flight": 0}}"#,
        r#"{"inventory": {"max_total_bytes": 0}}"#,
        r#"{"workers": {"server_processes": "many"}}"#,
        r#"{"flow": {"eval_budget": 0}}"#,
        r#"{"cache": "#,
    ] {
        fx.write("home/config.json", bad);
        assert!(matches!(Settings::load(&home), Err(CoreError::Config(_))), "{bad}");
    }
}

/// `memory.budget_mb`: default 4096, a user value is read, 0 accepted (unbounded).
#[test]
fn memory_budget_default_and_zero() {
    assert_eq!(MemorySettings::default().budget_mb, 4_096);
    let fx = Fixture::new("config-memory");
    let home = fx.dir("home");
    fx.write("home/config.json", r#"{"memory": {"budget_mb": 2048}}"#);
    assert_eq!(Settings::load(&home).unwrap().memory.budget_mb, 2_048);
    fx.write("home/config.json", r#"{"memory": {"budget_mb": 0}}"#);
    assert_eq!(Settings::load(&home).unwrap().memory.budget_mb, 0);
    fx.write("home/config.json", r#"{"memory": {"budget_mb": -1}}"#);
    assert!(
        matches!(Settings::load(&home), Err(CoreError::Config(_))),
        "negative budgets are rejected by the type"
    );
}

/// A key of `config.json` that names no setting is an error naming the key and, when one is
/// close, the setting it probably means; removed switches are unknown keys too. The backend
/// ids of `semantic.per_backend` are free, their entries use the resource keys.
#[test]
fn rule_unknown_settings_are_errors() {
    let fx = Fixture::new("config-unknown-keys");
    let home = fx.dir("home");
    for (doc, message) in [
        (
            r#"{"semantic": {"request_timeout": 30}}"#,
            "unknown setting `semantic.request_timeout`; did you mean `semantic.request_timeout_secs`?",
        ),
        (r#"{"semantc": {"max_in_flight": 4}}"#, "unknown setting `semantc`; did you mean `semantic`?"),
        (r#"{"semantic": {"disabled": ["pyright"]}}"#, "unknown setting `semantic.disabled`"),
        (r#"{"jev": {"enabled": false}}"#, "unknown setting `jev`"),
        (
            r#"{"semantic": {"per_backend": {"lsp:gopls": {"proceses": 2}}}}"#,
            "unknown setting `semantic.per_backend.lsp:gopls.proceses`; did you mean `semantic.per_backend.lsp:gopls.processes`?",
        ),
    ] {
        fx.write("home/config.json", doc);
        match Settings::load(&home) {
            Err(CoreError::Config(text)) => assert!(text.ends_with(message), "{doc}: {text}"),
            other => panic!("{doc}: expected a config error, got {other:?}"),
        }
    }
    // A backend id of the user's own registry entry is accepted.
    fx.write("home/config.json", r#"{"semantic": {"per_backend": {"lsp:my-server": {"processes": 2}}}}"#);
    let cfg = Settings::load(&home).unwrap();
    assert_eq!(cfg.semantic.per_backend["lsp:my-server"].processes, Some(2));
}

/// `"auto"` server processes: `min(cores / 2, server_processes_max)`, at least 1; a number
/// is used as given (clamped to 1..=32).
#[test]
fn rule_auto_is_computed_from_cores() {
    let s = Settings::default();
    let auto = s.auto();
    if crate::env::semantic_processes().is_none() {
        assert_eq!(auto.server_processes, (auto.cores / 2).clamp(1, 4));
        let fixed = Settings::from_user(serde_json::json!({"workers": {"server_processes": 64}})).unwrap();
        assert_eq!(fixed.auto().server_processes, 32);
    }
    assert_eq!(auto.request_shards, 8.min((auto.cores / 2).max(1)));
}
