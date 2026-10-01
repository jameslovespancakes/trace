use super::*;

#[test]
fn rule_library_symbol_is_module_and_qualified_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pkg = dir.path().join("pkg");
    std::fs::create_dir_all(&pkg).expect("dir");
    std::fs::write(pkg.join("__init__.py"), "").expect("init");
    let file = pkg.join("events.py");
    let src = b"class Handle:\n    def __init__(self, callback):\n        self._callback = callback\n";
    std::fs::write(&file, src).expect("write");
    assert_eq!(
        library_symbol(Language::Python, &file, src, 1, 8).as_deref(),
        Some("pkg.events.Handle.__init__")
    );
    assert_eq!(library_symbol(Language::Python, &file, src, 0, 6).as_deref(), Some("pkg.events.Handle"));
    assert_eq!(library_symbol(Language::Python, &file, src, 2, 0), None, "no declaration on that line");
}
