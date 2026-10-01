use super::*;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("trace-analysis-caches-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn stats_merge_and_rates() {
    let dir = scratch("stats");
    let path = dir.join("stats.json");
    let mut delta = Stats::default();
    assert!(delta.is_empty());
    delta.semantic_files.record(10, 9);
    Stats::merge_into(&path, &delta).unwrap();
    Stats::merge_into(&path, &delta).unwrap();
    let total = Stats::load(&path);
    assert_eq!(
        total.semantic_files,
        HitCounter {
            lookups: 20,
            hits: 18
        }
    );
    assert_eq!(total.semantic_files.rate(), Some(0.9));
    // Counters written by an older version with a `context_rank` key still load.
    std::fs::write(
        &path,
        br#"{"version":1,"semantic_files":{"lookups":1,"hits":1},"context_rank":{"lookups":3,"hits":0}}"#,
    )
    .unwrap();
    assert_eq!(Stats::load(&path).semantic_files.lookups, 1);
    let _ = std::fs::remove_dir_all(&dir);
}
