use super::*;

#[test]
fn rule_python_module_names_follow_packages() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pkg = dir.path().join("pkg");
    std::fs::create_dir_all(pkg.join("sub")).unwrap();
    std::fs::write(pkg.join("__init__.py"), "").unwrap();
    std::fs::write(pkg.join("sub").join("__init__.py"), "").unwrap();
    std::fs::write(pkg.join("sub").join("m.py"), "").unwrap();
    std::fs::write(dir.path().join("top.py"), "").unwrap();
    assert_eq!(module_name(&pkg.join("sub").join("m.py"), &[]).as_deref(), Some("pkg.sub.m"));
    assert_eq!(module_name(&pkg.join("__init__.py"), &[]).as_deref(), Some("pkg"));
    assert_eq!(module_name(&dir.path().join("top.py"), &[]).as_deref(), Some("top"));
    let from = pkg.join("sub").join("m.py");
    let (file, member) = resolve_import(&from, "..sub.m.f", ImportKind::Module, &[]).expect("relative");
    assert_eq!(file, pkg.join("sub").join("m.py"));
    assert_eq!(member.as_deref(), Some("f"));
    let (file, member) = resolve_import(&from, "top", ImportKind::Module, &[]).expect("absolute");
    assert_eq!(file, dir.path().join("top.py"));
    assert_eq!(member, None);
}

/// Imports of a stub file resolve to the stub tree first (`from typing import Mapping`
/// in `builtins.pyi` is `typing.pyi`); source files keep resolving to source modules.
#[test]
fn rule_python_stub_imports_resolve_to_stubs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let stubs = dir.path().join("stubs");
    std::fs::create_dir_all(stubs.join("collections")).unwrap();
    for f in [
        "builtins.pyi",
        "typing.pyi",
        "typing.py",
        "collections/__init__.pyi",
        "helper.py",
    ] {
        std::fs::write(stubs.join(f), "").unwrap();
    }
    let from = stubs.join("builtins.pyi");
    let (file, member) = resolve_import(&from, "typing.Mapping", ImportKind::Module, &[]).expect("stub");
    assert_eq!((file, member.as_deref()), (stubs.join("typing.pyi"), Some("Mapping")));
    let (file, _) =
        resolve_import(&from, "collections.OrderedDict", ImportKind::Module, &[]).expect("stub package");
    assert_eq!(file, stubs.join("collections").join("__init__.pyi"));
    let (file, _) = resolve_import(&from, "helper.f", ImportKind::Module, &[]).expect("source module");
    assert_eq!(file, stubs.join("helper.py"));
    let (file, _) =
        resolve_import(&stubs.join("helper.py"), "typing.Mapping", ImportKind::Module, &[]).expect("source");
    assert_eq!(file, stubs.join("typing.py"));
}
