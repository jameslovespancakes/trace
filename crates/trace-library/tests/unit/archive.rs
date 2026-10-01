use super::*;
use std::io::Write;

fn write_zip(path: &Path, entries: &[(&str, &str)]) {
    let file = std::fs::File::create(path).expect("create");
    let mut w = zip::ZipWriter::new(file);
    for (name, text) in entries {
        w.start_file(*name, zip::write::SimpleFileOptions::default())
            .expect("start");
        w.write_all(text.as_bytes()).expect("write");
    }
    w.finish().expect("finish");
}

#[test]
fn rule_archive_entries_are_read_as_library_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let jar = dir.path().join("lib-1.0-sources.jar");
    write_zip(
        &jar,
        &[
            ("com/acme/Pool.java", "class Pool {}"),
            ("com/acme/Task.java", "class Task {}"),
            ("com/acme/inner/Deep.java", "class Deep {}"),
        ],
    );
    let pool = entry_path(&jar, "com/acme/Pool.java");
    let (archive, entry) = split(&pool).expect("archive path");
    assert_eq!(archive, jar);
    assert_eq!(entry, "com/acme/Pool.java");
    assert_eq!(read(&pool, 1 << 20).as_deref(), Some(&b"class Pool {}"[..]));
    assert!(read(&pool, 4).is_none(), "entries over the byte bound are not read");
    assert!(exists(&pool));
    assert!(!exists(&entry_path(&jar, "com/acme/Missing.java")));
    assert_eq!(siblings(&pool, &["java"]), vec![entry_path(&jar, "com/acme/Task.java")]);
    assert_eq!(find_suffix(&jar, "inner/Deep.java").as_deref(), Some("com/acme/inner/Deep.java"));
    assert!(split(&dir.path().join("plain.java")).is_none(), "plain files are not archive paths");
}
