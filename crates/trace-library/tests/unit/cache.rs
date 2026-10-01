use super::*;
use crate::derive::FunctionSummary;
use crate::model::{ArgSel, Effect};

fn key(version: &str, content: &[u8]) -> SummaryKey {
    SummaryKey {
        language: Language::Python,
        package: "pkg".to_string(),
        version: Some(version.to_string()),
        file_hash: Hash32::of(content),
        context: Hash32::of(b"tables"),
    }
}

fn summaries() -> FileSummaries {
    let mut s = FileSummaries::default();
    s.functions.insert(
        "pkg.run".to_string(),
        FunctionSummary {
            symbol: "pkg.run".to_string(),
            qualified: "run".to_string(),
            line: 3,
            column: 4,
            params: vec![(
                ArgSel::PosOrKw(0, "fn".to_string()),
                vec![Effect::Calls(ArgSel::PosOrKw(0, "fn".to_string()))],
            )],
            effects: Vec::new(),
        },
    );
    s
}

#[test]
fn rule_summary_cache_is_per_package_version() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = SummaryCache::open(dir.path());
    let k1 = key("1.0", b"def run(fn): fn()");
    cache.put(&k1, &summaries()).expect("put");
    assert_eq!(cache.get(&k1), Some(summaries()));
    assert_eq!(cache.get(&key("2.0", b"def run(fn): fn()")), None, "other version");
    assert_eq!(cache.get(&key("1.0", b"def run(fn): pass")), None, "other content");
    let mut other_context = k1.clone();
    other_context.context = Hash32::of(b"other tables");
    assert_eq!(cache.get(&other_context), None, "other tables");
    // A damaged entry is rebuilt, never misread.
    std::fs::write(cache.entry_path(&k1), b"garbage").expect("write");
    assert_eq!(cache.get(&k1), None);
}

#[test]
fn rule_summary_cache_is_bounded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = SummaryCache::open(dir.path());
    for i in 0..10 {
        cache
            .put(&key("1.0", format!("file {i}").as_bytes()), &summaries())
            .expect("put");
    }
    let one = std::fs::metadata(cache.entry_path(&key("1.0", b"file 0")))
        .expect("entry")
        .len();
    let removed = cache.prune(one * 5);
    assert!(removed >= 5, "removed {removed}");
    assert_eq!(cache.prune(u64::MAX), 0);
}
