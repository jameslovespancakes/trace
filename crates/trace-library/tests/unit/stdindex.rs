use super::*;

#[test]
fn rule_stdlib_module_index_is_unique_match() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("Lib");
    std::fs::create_dir_all(root.join("codec")).expect("dir");
    std::fs::write(root.join("codec").join("__init__.py"), "def dumps(obj, fp):\n    pass\n").expect("write");
    std::fs::write(
        root.join("tasks.py"),
        "class Runner:\n    def run(self, fn):\n        fn()\n\nclass Pool:\n    def run(self, fn, n):\n        fn()\n",
    )
    .expect("write");
    let index = StdIndex::build(Language::Python, &root);
    let (path, line, _) = index.lookup("codec", "dumps", Some(2)).expect("unique");
    assert!(path.ends_with("__init__.py"));
    assert_eq!(line, 0);
    assert!(index.lookup("codec", "dumps", Some(3)).is_none(), "arity must match");
    // Two methods named `run` in one module: no answer by the bare name ...
    assert!(index.lookup("tasks", "run", None).is_none());
    // ... the qualified name is unique.
    assert!(index.lookup("tasks", "Runner.run", None).is_some());
    assert!(index.lookup("missing", "dumps", None).is_none());
    let lib_root = LibraryRoot {
        path: root.clone(),
        kind: LibraryKind::Stdlib,
        ecosystem: trace_env::EcosystemId::Python,
        layout: "toolchain_stdlib",
        version: Some("3.12".to_string()),
    };
    let (path, line, column) = index.lookup("codec", "dumps", None).expect("located");
    let file = stdlib_file(Language::Python, &lib_root, path);
    assert!(file.stdlib && file.readable);
    assert_eq!(file.package, "python-stdlib");
    assert_eq!((line, column), (0, 4));
}
