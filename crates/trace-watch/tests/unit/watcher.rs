use super::*;
use notify::event::{AccessKind, CreateKind, ModifyKind};

fn fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
    let base = std::env::temp_dir().join("trace-tests");
    std::fs::create_dir_all(&base).unwrap();
    let dir = tempfile::Builder::new().prefix(name).tempdir_in(&base).unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    let root = trace_core::inventory::canonical_root(dir.path()).unwrap();
    (dir, root)
}

fn event(kind: EventKind, path: PathBuf) -> Event {
    Event::new(kind).add_path(path)
}

/// Whether `event` would start an update (the paths the watcher delivers for it).
fn relevant(filter: &PathFilter, event: &Event) -> bool {
    !filter.changes_of(event).is_empty()
}

#[test]
fn filters_paths_the_inventory_skips() {
    let (_dir, root) = fixture("watch-filter");
    std::fs::write(root.join(".gitignore"), "generated/\n*.log\n").unwrap();
    let filter = PathFilter::new(&root);
    let modify = EventKind::Modify(ModifyKind::Any);

    assert!(relevant(&filter, &event(modify, root.join("src/app.py"))));
    assert!(!relevant(&filter, &event(modify, root.join("node_modules/x/index.js"))));
    assert!(!relevant(&filter, &event(modify, root.join(".git/index"))));
    assert!(!relevant(&filter, &event(modify, root.join(".env"))));
    assert!(!relevant(&filter, &event(modify, root.join("src/.env.local"))));
    assert!(!relevant(&filter, &event(modify, root.join("certs/server.pem"))));
    assert!(!relevant(&filter, &event(modify, root.join("generated/api.py"))));
    assert!(!relevant(&filter, &event(modify, root.join("debug.log"))));
    assert!(!relevant(&filter, &event(EventKind::Access(AccessKind::Any), root.join("src/app.py"))));
    assert!(relevant(&filter, &event(EventKind::Create(CreateKind::File), root.join("src/new.rs"))));
    assert!(relevant(&filter, &Event::new(EventKind::Any)));
    assert!(filter.changes_of(&Event::new(EventKind::Any)).overflow);
    let c = filter.changes_of(&event(modify, root.join("node_modules/x/index.js")));
    assert!(c.is_empty());
}

/// I-11: events on build output and tool caches (an excluded directory itself, anything
/// below it, hidden tool folders) and below the user's exclusion globs never start an
/// update; a file named like an excluded directory (an extensionless script) still does.
#[test]
fn rule_events_below_excluded_dirs_are_ignored() {
    let (_dir, root) = fixture("watch-excluded");
    std::fs::create_dir_all(root.join("build/classes")).unwrap();
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::create_dir_all(root.join("sub/build")).unwrap();
    std::fs::create_dir_all(root.join("examples/demo")).unwrap();
    std::fs::write(root.join("sub/dist"), "#!/bin/sh\necho dist\n").unwrap();
    let filter = PathFilter::new(&root).with_exclude(&["examples/".to_string()]);
    let modify = EventKind::Modify(ModifyKind::Any);
    let create = EventKind::Create(CreateKind::Folder);
    assert!(!relevant(&filter, &event(create, root.join("build"))));
    assert!(!relevant(&filter, &event(create, root.join("sub/build"))));
    assert!(!relevant(&filter, &event(modify, root.join("build/classes/A.class"))));
    assert!(!relevant(&filter, &event(modify, root.join("target"))));
    assert!(!relevant(&filter, &event(modify, root.join(".gradle/8.0/fileHashes.bin"))));
    assert!(!relevant(&filter, &event(modify, root.join(".idea/workspace.xml"))));
    assert!(!relevant(&filter, &event(modify, root.join("examples/demo/main.py"))));
    assert!(!relevant(&filter, &event(create, root.join("examples"))));
    // Still relevant: sources, and a script file named like an excluded directory.
    assert!(relevant(&filter, &event(modify, root.join("sub/dist"))));
    assert!(relevant(&filter, &event(modify, root.join("src/app.py"))));
    assert!(filter.changes_of(&event(create, root.join("build"))).is_empty());
}

#[test]
fn ignore_file_changes_reload_rules() {
    let (_dir, root) = fixture("watch-reload");
    let mut filter = PathFilter::new(&root);
    let modify = EventKind::Modify(ModifyKind::Any);
    assert!(relevant(&filter, &event(modify, root.join("out.tmp"))));
    std::fs::write(root.join(".gitignore"), "*.tmp\n").unwrap();
    assert!(filter.note_ignore_change(&event(modify, root.join(".gitignore"))));
    assert!(!relevant(&filter, &event(modify, root.join("out.tmp"))));
    assert!(!filter.note_ignore_change(&event(modify, root.join("src/a.py"))));
}

#[test]
fn rule_watcher_debounces_and_ignores_excluded_paths() {
    let buffer = Mutex::new(Buffer::default());
    let (tx, rx) = crossbeam_channel::unbounded();
    let (_dir, root) = fixture("watch-debounce");
    let filter = PathFilter::new(&root);
    let modify = EventKind::Modify(ModifyKind::Any);
    let t0 = Instant::now();
    // Three quick events on a source and one on an excluded directory.
    for (i, p) in ["src/a.py", "src/a.py", "node_modules/x.js", "src/b.py"]
        .iter()
        .enumerate()
    {
        note(
            &buffer,
            filter.changes_of(&event(modify, root.join(p))),
            t0 + Duration::from_millis(50 * i as u64),
        );
    }
    // Not quiet yet: nothing delivered.
    let timing = Timing::of(&WatchSettings::default());
    assert!(deliver_if_due(&buffer, &tx, t0 + Duration::from_millis(200), timing));
    assert!(rx.try_recv().is_err());
    // Quiet for the debounce after the last event: one delivery with both sources.
    let quiet = t0 + Duration::from_millis(150) + timing.debounce;
    assert!(deliver_if_due(&buffer, &tx, quiet, timing));
    let got = rx.try_recv().unwrap();
    assert_eq!(got.paths, BTreeSet::from([root.join("src/a.py"), root.join("src/b.py")]));
    assert!(!got.overflow);
    // A continuous stream is delivered after the maximum delay at the latest.
    let mut t = quiet;
    for _ in 0..30 {
        t += timing.debounce / 2;
        note(&buffer, filter.changes_of(&event(modify, root.join("src/c.py"))), t);
        assert!(deliver_if_due(&buffer, &tx, t, timing));
    }
    assert!(rx.try_recv().is_ok(), "delivered within the maximum delay despite the stream");
}
