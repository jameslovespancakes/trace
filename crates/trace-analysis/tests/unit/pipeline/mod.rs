use super::*;
use crate::test_support::fixture_paths;
use std::collections::BTreeSet;
use std::fs;
use std::time::Duration;
use trace_core::SupportLevel;

/// I-11: an event batch whose files all hash unchanged (a save without changes, a touched
/// file) leaves the index current: the freshness check (`is_current`, what
/// `Workspace::update_if_stale` asks first) says so without any server work, and a run
/// anyway reports `unchanged` (the watcher prints no line for it).
#[test]
fn rule_unchanged_event_batch_costs_no_update() {
    let (base, paths) = fixture_paths("unchanged-batch");
    let source = "syntax = \"proto3\";\nservice S { rpc Get (A) returns (B); }\n";
    fs::write(paths.root.join("api.proto"), source).unwrap();
    let config = Settings::default();
    let input = |prev: Option<Index>| PipelineInput {
        paths: &paths,
        config: &config,
        prev,
        mode: IndexMode::Incremental,
        sessions: None,
        scanned: None,
        invalidated: None,
        offline: true,
        stale: StalePolicy::Defer {
            resolve: BTreeSet::new(),
        },
    };
    let first = run(input(None), &mut Quiet).unwrap();
    // The same bytes written again: new modification time, same content.
    std::thread::sleep(Duration::from_millis(20));
    fs::write(paths.root.join("api.proto"), source).unwrap();
    let scanned = scan(&paths, &config, Some(&first.index), IndexMode::Incremental).unwrap();
    assert!(is_current(&first.index, &scanned), "no update needed");
    let second = run(input(Some(first.index)), &mut Quiet).unwrap();
    assert!(!second.changed);
    assert_eq!(second.report.mode, "unchanged");
    assert_eq!(second.report.semantic_requeried, 0);
    let _ = fs::remove_dir_all(&base);
}

/// The pipeline hands everything after the semantic phase to `update::apply` / `persist`:
/// the index exists on disk after a run, a second run over the same files is `unchanged`
/// and returns the same index, and an edit produces an incremental delta.
#[test]
fn rule_pipeline_hands_post_semantic_work_to_update() {
    let (base, paths) = fixture_paths("handover");
    fs::write(paths.root.join("api.proto"), "syntax = \"proto3\";\nservice S { rpc Get (A) returns (B); }\n")
        .unwrap();
    let config = Settings::default();
    let first = run(
        PipelineInput {
            paths: &paths,
            config: &config,
            prev: None,
            mode: IndexMode::Incremental,
            sessions: None,
            scanned: None,
            invalidated: None,
            offline: true,
            stale: StalePolicy::ResolveAll,
        },
        &mut Quiet,
    )
    .unwrap();
    assert!(first.changed);
    assert!(paths.index_file.exists(), "persisted by update::persist");
    assert_eq!(first.index.files.len(), 1);
    assert_eq!(first.index.files[0].support, SupportLevel::Inventoried);
    let files_before = first.index.files.len();
    let second = run(
        PipelineInput {
            paths: &paths,
            config: &config,
            prev: Some(first.index),
            mode: IndexMode::Incremental,
            sessions: None,
            scanned: None,
            invalidated: None,
            offline: true,
            stale: StalePolicy::ResolveAll,
        },
        &mut Quiet,
    )
    .unwrap();
    assert!(!second.changed);
    assert_eq!(second.report.mode, "unchanged");
    assert_eq!(second.index.files.len(), files_before);
    // An edit: the delta names exactly the modified file.
    fs::write(paths.root.join("api.proto"), "syntax = \"proto3\";\nservice S { rpc Put (A) returns (B); }\n")
        .unwrap();
    let scanned = scan(&paths, &config, Some(&second.index), IndexMode::Incremental).unwrap();
    let plan = incremental::plan(Some(&second.index), &scanned.sources, &scanned.configs);
    assert_eq!(plan.changed, vec!["api.proto".to_string()]);
    let none = incremental::Requery::default();
    let interfaces = HashSet::new();
    let delta = index_delta(Some(&second.index), &plan, &second.index.files, &none, &interfaces);
    assert!(!delta.full);
    assert!(delta.modified.contains("api.proto"));
    assert!(index_delta(None, &plan, &second.index.files, &none, &interfaces).full);
    let _ = fs::remove_dir_all(&base);
}
