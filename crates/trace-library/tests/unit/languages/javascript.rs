use super::*;

#[test]
fn rule_javascript_imports_resolve_relative_and_package_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pkg = dir.path().join("node_modules").join("lib");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::write(pkg.join("package.json"), r#"{"name":"lib","version":"1.0.0","main":"./src/main.js"}"#)
        .unwrap();
    std::fs::write(pkg.join("src").join("main.js"), "").unwrap();
    std::fs::write(pkg.join("src").join("util.js"), "").unwrap();
    let from = pkg.join("src").join("main.js");
    let (file, member) = resolve_import(&from, "./util.helper", ImportKind::Member, &[]).expect("relative");
    assert_eq!(file, pkg.join("src").join("util.js"));
    assert_eq!(member.as_deref(), Some("helper"));
    let other = dir.path().join("app.js");
    let (file, member) = resolve_import(&other, "lib", ImportKind::Module, &[]).expect("package");
    assert_eq!(file, pkg.join("src").join("main.js"));
    assert_eq!(member, None);
    assert_eq!(module_name(&from, &[]).as_deref(), Some("lib/src/main"));
}

#[test]
fn rule_declaration_file_derives_from_its_implementation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let nm = dir.path().join("node_modules");
    // Mirrored layout named by the manifest's exports.
    let pkg = nm.join("web");
    std::fs::create_dir_all(pkg.join("dist").join("types").join("helper")).unwrap();
    std::fs::create_dir_all(pkg.join("dist").join("helper")).unwrap();
    std::fs::write(
        pkg.join("package.json"),
        r#"{"name":"web","exports":{".":{"types":"./dist/types/index.d.ts","import":"./dist/index.js"}}}"#,
    )
    .unwrap();
    std::fs::write(pkg.join("dist").join("types").join("helper").join("route.d.ts"), "").unwrap();
    std::fs::write(pkg.join("dist").join("helper").join("route.js"), "").unwrap();
    assert_eq!(
        implementation_of_declaration(&pkg.join("dist").join("types").join("helper").join("route.d.ts")),
        Some(pkg.join("dist").join("helper").join("route.js"))
    );
    // Sibling implementation.
    std::fs::write(pkg.join("dist").join("index.d.ts"), "").unwrap();
    std::fs::write(pkg.join("dist").join("index.js"), "").unwrap();
    assert_eq!(
        implementation_of_declaration(&pkg.join("dist").join("index.d.ts")),
        Some(pkg.join("dist").join("index.js"))
    );
    // A DefinitelyTyped package describes the package installed next to it.
    let types = nm.join("@types").join("server");
    std::fs::create_dir_all(&types).unwrap();
    std::fs::write(types.join("index.d.ts"), "").unwrap();
    let server = nm.join("server");
    std::fs::create_dir_all(server.join("lib")).unwrap();
    std::fs::write(server.join("package.json"), r#"{"name":"server","main":"./lib/server.js"}"#).unwrap();
    std::fs::write(server.join("lib").join("server.js"), "").unwrap();
    assert_eq!(
        implementation_of_declaration(&types.join("index.d.ts")),
        Some(server.join("lib").join("server.js"))
    );
    // No implementation installed: no answer; other files are not declaration files.
    let lone = nm.join("@types").join("missing");
    std::fs::create_dir_all(&lone).unwrap();
    std::fs::write(lone.join("index.d.ts"), "").unwrap();
    assert_eq!(implementation_of_declaration(&lone.join("index.d.ts")), None);
    assert_eq!(implementation_of_declaration(&server.join("lib").join("server.js")), None);
}
