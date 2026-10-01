use super::*;
use crate::model::SymbolId;
use crate::test_support::index::index_with;
use crate::test_support::Fixture;

#[test]
fn atomic_write_replaces_and_leaves_no_temp() {
    let fx = Fixture::new("cache-atomic");
    let path = fx.path("cache/nested/file.bin");
    write_atomic(&path, b"one").unwrap();
    write_atomic(&path, b"two").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"two");
    let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn index_round_trip_and_root_check() {
    let fx = Fixture::new("cache-index");
    let path = fx.path("index.bin");
    let index = index_with(&["a", "b"], &[(0, 1)]);
    save_index(&path, &index).unwrap();
    let loaded = load_index(&path, &index.header.root).unwrap();
    assert_eq!(loaded, index);
    assert!(matches!(load_index(&path, "C:/elsewhere"), Err(CoreError::CacheRoot(_))));
}

#[test]
fn per_file_blocks_round_trip() {
    let fx = Fixture::new("cache-blocks");
    let path = fx.path("index.bin");
    let mut index = index_with(&["a", "b"], &[(0, 1)]);
    index.files[0].facts = Some(FileFacts {
        language: Some(crate::Language::Python),
        error_count: 2,
        body_identifiers: [(14, vec![("body_call".into(), 2)])].into(),
        data_definitions: vec![crate::facts::DataDefinition {
            name: "setting".into(),
            name_span: crate::ByteSpan::new(0, 7),
            span: crate::Span {
                bytes: crate::ByteSpan::new(0, 11),
                start_line: 1,
                end_line: 1,
            },
            conditional: true,
        }],
        ..FileFacts::default()
    });
    index.files[0].semantic = Some(FileSemantics {
        provider: crate::model::Provider::Pyright,
        tool_fingerprint: "fp".into(),
        edges: Vec::new(),
        unresolved: Vec::new(),
        value_refs: Vec::new(),
        diagnostics: Vec::new(),
        implementations: Vec::new(),
        resolved_elsewhere: Vec::new(),
        callback_params: Vec::new(),
        library_files: Vec::new(),
        library_calls: Vec::new(),
        outside_build: None,
        expanded: Vec::new(),
        library_dispatch: Vec::new(),
        library_bases: Vec::new(),
    });
    save_index(&path, &index).unwrap();
    assert_eq!(load_index(&path, &index.header.root).unwrap(), index);

    // A missing per-file block is corruption, never a silently empty file.
    let bytes = fs::read(&path).unwrap();
    let payload = &bytes[HEADER_LEN..];
    let head_len = le_u64(payload) as usize;
    let cut = payload[..8 + head_len].to_vec();
    save_payload(&path, INDEX_MAGIC, SCHEMA_VERSION, &cut).unwrap();
    assert!(matches!(load_index(&path, &index.header.root), Err(CoreError::CacheCorrupt(_))));
}

#[test]
fn corruption_is_detected() {
    let fx = Fixture::new("cache-corrupt");
    let path = fx.path("index.bin");
    let index = index_with(&["a", "b"], &[(0, 1)]);
    save_index(&path, &index).unwrap();
    let good = fs::read(&path).unwrap();
    let root = index.header.root.as_str();

    // Flipped payload byte -> checksum mismatch.
    let mut bad = good.clone();
    let last = bad.len() - 1;
    bad[last] ^= 0xFF;
    fs::write(&path, &bad).unwrap();
    assert!(matches!(load_index(&path, root), Err(CoreError::CacheCorrupt(_))));

    // Truncated file.
    fs::write(&path, &good[..good.len() / 2]).unwrap();
    assert!(matches!(load_index(&path, root), Err(CoreError::CacheCorrupt(_))));

    // Wrong magic.
    let mut magic = good.clone();
    magic[0] = b'X';
    fs::write(&path, &magic).unwrap();
    assert!(matches!(load_index(&path, root), Err(CoreError::CacheCorrupt(_))));

    // Other schema.
    let mut schema = good.clone();
    schema[8..12].copy_from_slice(&99u32.to_le_bytes());
    fs::write(&path, &schema).unwrap();
    assert!(matches!(
        load_index(&path, root),
        Err(CoreError::CacheVersion {
            found: 99,
            expected: SCHEMA_VERSION
        })
    ));

    // Refuse older main and experimental layouts before decoding incompatible file blocks.
    for old_version in [11u32, 12, 13] {
        let mut previous = good.clone();
        previous[8..12].copy_from_slice(&old_version.to_le_bytes());
        fs::write(&path, &previous).unwrap();
        assert!(matches!(
            load_index(&path, root),
            Err(CoreError::CacheVersion { found, expected: SCHEMA_VERSION }) if found == old_version
        ));
    }

    // Valid envelope, garbage payload.
    save_blob(&path, INDEX_MAGIC, SCHEMA_VERSION, &vec![7u8; 3]).unwrap();
    assert!(matches!(load_index(&path, root), Err(CoreError::CacheCorrupt(_))));

    // Valid envelope, structurally inconsistent index.
    let mut broken = index.clone();
    broken.edges[0].to = SymbolId(42);
    save_index(&path, &broken).unwrap();
    assert!(matches!(load_index(&path, root), Err(CoreError::CacheCorrupt(_))));

    // Similar-cache magic is not an index.
    save_blob(&path, SIMILAR_MAGIC, SCHEMA_VERSION, &index).unwrap();
    assert!(matches!(load_index(&path, root), Err(CoreError::CacheCorrupt(_))));
}

#[test]
fn lock_is_exclusive_and_released() {
    let fx = Fixture::new("cache-lock");
    let path = fx.path("repo/index.lock");
    assert!(!CacheLock::is_held(&path));
    let lock = CacheLock::acquire(&path).unwrap();
    assert!(CacheLock::is_held(&path));
    assert!(matches!(CacheLock::acquire(&path), Err(CoreError::Locked(_))));
    drop(lock);
    assert!(!CacheLock::is_held(&path));
    let again = CacheLock::acquire(&path).unwrap();
    assert_eq!(again.path(), path.as_path());
}

/// Child side of [`rule_os_lock_is_released_when_the_holder_dies`]: without the
/// environment variable it does nothing.
#[test]
fn lock_holder_child() {
    let Some(path) = crate::env::test::lock_child() else {
        return;
    };
    let _lock = CacheLock::acquire(&path).unwrap();
    fs::write(path.with_extension("ready"), b"1").unwrap();
    // Held until the parent kills this process.
    std::thread::sleep(Duration::from_secs(60));
}

#[test]
fn rule_os_lock_is_released_when_the_holder_dies() {
    let fx = Fixture::new("cache-lock-child");
    let path = fx.path("repo/index.lock");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["cache::tests::lock_holder_child", "--exact", "--nocapture", "--test-threads=1"])
        .env(crate::env::test::LOCK_CHILD, &path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let ready = path.with_extension("ready");
    let started = std::time::Instant::now();
    while !ready.exists() && started.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(ready.exists(), "the child took the lock");
    assert!(CacheLock::is_held(&path));
    assert!(matches!(CacheLock::acquire(&path), Err(CoreError::Locked(_))));
    // The holder dies without releasing anything itself.
    child.kill().unwrap();
    child.wait().unwrap();
    // Windows releases the locks of a terminated process asynchronously (shortly after).
    let started = std::time::Instant::now();
    let lock = loop {
        match CacheLock::acquire(&path) {
            Ok(lock) => break lock,
            Err(_) if started.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("the OS did not release the dead holder's lock: {e}"),
        }
    };
    drop(lock);
}

/// Two files: a.py (one symbol, small facts) and b.py (large facts, never changed).
fn journal_index() -> Index {
    let mut index = index_with(&["f"], &[]);
    index.files[0].facts = Some(FileFacts {
        error_count: 1,
        ..FileFacts::default()
    });
    let mut b = index.files[0].clone();
    b.path = "b.py".into();
    b.first_symbol = 1;
    b.symbol_count = 0;
    b.facts = Some(FileFacts {
        local_spans: (0..40_000u32)
            .map(|i| crate::model::ByteSpan::new(i * 3, i * 3 + 2))
            .collect(),
        ..FileFacts::default()
    });
    index.files.push(b);
    index
}

fn modified(paths: &[&str]) -> crate::delta::IndexDelta {
    crate::delta::IndexDelta {
        modified: paths.iter().map(|p| p.to_string()).collect(),
        ..Default::default()
    }
}

#[test]
fn rule_delta_journal_round_trips() {
    let fx = Fixture::new("cache-journal");
    let path = fx.path("index.bin");
    let mut index = journal_index();
    save_index(&path, &index).unwrap();
    let base = fs::read(&path).unwrap();
    let root = index.header.root.clone();

    // Two incremental updates of a.py: two segments, the base untouched.
    for round in 2..4u32 {
        index.files[0].facts.as_mut().unwrap().error_count = round;
        index.header.incremental_updates = round;
        index.stale.insert(format!("stale{round}.py"));
        save_index_delta(&path, &index, &modified(&["a.py"])).unwrap();
        assert_eq!(fs::read(&path).unwrap(), base, "the base is not rewritten");
        assert!(journal_segment(&path, round - 1).exists());
        assert_eq!(load_index(&path, &root).unwrap(), index);
    }
    assert!(index_stamp(&path).is_some());

    // A new base ends the journal.
    save_index(&path, &index).unwrap();
    assert!(!journal_segment(&path, 1).exists());
    assert_eq!(load_index(&path, &root).unwrap(), index);

    // A full delta rewrites the base.
    index.files[0].facts.as_mut().unwrap().error_count = 9;
    save_index_delta(&path, &index, &crate::delta::IndexDelta::full()).unwrap();
    assert!(!journal_segment(&path, 1).exists());
    assert_eq!(load_index(&path, &root).unwrap(), index);

    // Leftover segments of an older base are ignored (and replaced by the next delta).
    index.files[0].facts.as_mut().unwrap().error_count = 10;
    save_index_delta(&path, &index, &modified(&["a.py"])).unwrap();
    let leftover = fs::read(journal_segment(&path, 1)).unwrap();
    save_index(&path, &index).unwrap();
    fs::write(journal_segment(&path, 1), &leftover).unwrap();
    assert_eq!(load_index(&path, &root).unwrap(), index);
    index.files[0].facts.as_mut().unwrap().error_count = 11;
    save_index_delta(&path, &index, &modified(&["a.py"])).unwrap();
    assert_eq!(load_index(&path, &root).unwrap(), index);
}

#[test]
fn rule_bad_journal_segment_forces_rebuild() {
    let fx = Fixture::new("cache-journal-bad");
    let path = fx.path("index.bin");
    let mut index = journal_index();
    save_index(&path, &index).unwrap();
    let root = index.header.root.clone();
    index.files[0].facts.as_mut().unwrap().error_count = 2;
    save_index_delta(&path, &index, &modified(&["a.py"])).unwrap();
    index.files[0].facts.as_mut().unwrap().error_count = 3;
    save_index_delta(&path, &index, &modified(&["a.py"])).unwrap();
    let good = fs::read(journal_segment(&path, 2)).unwrap();

    // A flipped byte in the last segment: a cache error (the caller rebuilds), never the
    // older state of segment 1.
    let mut bad = good.clone();
    let last = bad.len() - 1;
    bad[last] ^= 0xFF;
    fs::write(journal_segment(&path, 2), &bad).unwrap();
    assert!(matches!(load_index(&path, &root), Err(CoreError::CacheCorrupt(_))));

    // A truncated segment.
    fs::write(journal_segment(&path, 2), &good[..good.len() / 2]).unwrap();
    assert!(matches!(load_index(&path, &root), Err(CoreError::CacheCorrupt(_))));

    // The intact segment loads again.
    fs::write(journal_segment(&path, 2), &good).unwrap();
    assert_eq!(load_index(&path, &root).unwrap(), index);

    // A segment of another base after a current one.
    let other = fx.path("other/index.bin");
    fs::create_dir_all(other.parent().unwrap()).unwrap();
    let mut foreign = journal_index();
    foreign.header.built_unix = 1.0;
    save_index(&other, &foreign).unwrap();
    save_index_delta(&other, &foreign, &modified(&["a.py"])).unwrap();
    save_index_delta(&other, &foreign, &modified(&["a.py"])).unwrap();
    fs::copy(journal_segment(&other, 2), journal_segment(&path, 2)).unwrap();
    assert!(matches!(load_index(&path, &root), Err(CoreError::CacheCorrupt(_))));
}

#[test]
fn rule_journal_compacts_over_threshold() {
    let fx = Fixture::new("cache-journal-compact");
    let path = fx.path("index.bin");
    let mut index = journal_index();
    save_index(&path, &index).unwrap();
    let root = index.header.root.clone();
    // Carrying the large b.py block exceeds 25% of the base: the base is rewritten.
    index.files[1].facts.as_mut().unwrap().error_count = 5;
    save_index_delta(&path, &index, &modified(&["b.py"])).unwrap();
    assert!(!journal_segment(&path, 1).exists(), "compacted into the base");
    assert_eq!(load_index(&path, &root).unwrap(), index);
    // Small deltas append until the segment limit, then compact.
    for round in 0..MAX_JOURNAL_SEGMENTS + 1 {
        index.files[0].facts.as_mut().unwrap().error_count = round + 10;
        save_index_delta(&path, &index, &modified(&["a.py"])).unwrap();
    }
    assert!(journal_segments(&path).len() <= MAX_JOURNAL_SEGMENTS as usize);
    assert_eq!(load_index(&path, &root).unwrap(), index);
}

/// I-11: each segment's index record is superseded by the next one, so a head-heavy index
/// (symbols, edges and sites are most of it) appends its second and third update instead
/// of rewriting the whole base on the second edit; an idle process compacts later.
#[test]
fn rule_second_update_appends_without_rewriting_the_base() {
    let fx = Fixture::new("cache-journal-heads");
    let path = fx.path("index.bin");
    let mut index = journal_index();
    // An index record about a third of the base.
    index.diagnostics = (0..4_000)
        .map(|i| crate::model::Diagnostic::new("note", None, format!("diagnostic number {i:05}")))
        .collect();
    save_index(&path, &index).unwrap();
    let base = fs::read(&path).unwrap();
    let root = index.header.root.clone();
    assert!(!journal_wants_compaction(&path));
    for round in 1..=2u32 {
        index.files[0].facts.as_mut().unwrap().error_count = round + 1;
        index.header.incremental_updates = round;
        save_index_delta(&path, &index, &modified(&["a.py"])).unwrap();
        assert_eq!(fs::read(&path).unwrap(), base, "update {round} appended");
        assert!(journal_segment(&path, round).exists());
        assert_eq!(load_index(&path, &root).unwrap(), index);
    }
    assert!(journal_wants_compaction(&path), "two index records in the journal: compact while idle");
    save_index(&path, &index).unwrap();
    assert!(!journal_wants_compaction(&path));
    assert_eq!(load_index(&path, &root).unwrap(), index);
}

#[test]
fn repo_meta_keeps_creation_time() {
    let fx = Fixture::new("cache-meta");
    let path = fx.path("meta.json");
    assert_eq!(RepoMeta::load(&path).unwrap(), None);
    let first = RepoMeta::record_index(&path, "C:/repo").unwrap();
    let second = RepoMeta::record_index(&path, "C:/repo").unwrap();
    assert_eq!(first.created_unix, second.created_unix);
    assert!(second.last_index_unix >= first.last_index_unix);
    assert_eq!(second.schema, SCHEMA_VERSION);
}
