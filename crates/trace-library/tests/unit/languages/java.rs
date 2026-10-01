use super::*;
use std::io::Write;

fn write_zip(path: &Path, entries: &[(&str, &str)]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
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
fn rule_java_class_locations_map_to_their_sources() {
    let dir = tempfile::tempdir().expect("tempdir");
    let version = dir
        .path()
        .join("repository")
        .join("com")
        .join("acme")
        .join("lib")
        .join("1.0");
    let jar = version.join("lib-1.0.jar");
    write_zip(&jar, &[("com/acme/Pool.class", "")]);
    write_zip(&version.join("lib-1.0-sources.jar"), &[("com/acme/Pool.java", "class Pool {}")]);
    let jar_text = jar.to_string_lossy().replace('\\', "/");
    let uri = format!("jar:file:///{}!/com/acme/Pool$Worker.class", jar_text.trim_start_matches('/'));
    let src = source_of_location(&uri, &[]).expect("sources jar entry");
    let (archive_file, entry) = archive::split(&src).expect("archive path");
    assert!(archive_file.ends_with("lib-1.0-sources.jar"));
    assert_eq!(entry, "com/acme/Pool.java", "a nested class lives in its outer class's file");
    assert_eq!(module_name(&src, &[]).as_deref(), Some("com.acme"));
    // The JDK: module locations map into src.zip.
    let jdk = dir.path().join("jdk");
    write_zip(&jdk.join("lib").join("src.zip"), &[("java.base/java/util/List.java", "interface List {}")]);
    let src = source_of_location("jrt:/java.base/java/util/List.class", std::slice::from_ref(&jdk))
        .expect("src.zip entry");
    assert_eq!(archive::split(&src).map(|(_, e)| e).as_deref(), Some("java.base/java/util/List.java"));
    assert_eq!(
        module_name(&src, &[]).as_deref(),
        Some("java.util"),
        "the module directory is not the package"
    );
    let jdt =
        "jdt://contents/java.base/java.util/List.class?=p/%5C/modules%5C/java.base%3Cjava.util(List.class";
    assert!(source_of_location(jdt, std::slice::from_ref(&jdk)).is_some());
    // Imports resolve inside the importing archive, then in the JDK sources.
    let (found, member) =
        resolve_import(&src, "java.util.List", ImportKind::Member, &[jdk]).expect("jdk import");
    assert_eq!(member.as_deref(), Some("List"));
    assert!(archive::exists(&found));
    // No sources installed: no answer.
    let bare = dir.path().join("other").join("x-2.0.jar");
    write_zip(&bare, &[("x/Y.class", "")]);
    let text = bare.to_string_lossy().replace('\\', "/");
    assert!(source_of_location(&format!("jar:file:///{}!/x/Y.class", text.trim_start_matches('/')), &[])
        .is_none());
}

#[test]
fn rule_jdt_uri_names_its_jar_and_class() {
    let uri = "jdt://contents/gson-2.10.1.jar/com.google.gson/Gson.class?=gson/C:%5C/m2%5C/gson-2.10.1.jar%3Ccom.google.gson(Gson.class";
    let loc = parse_jdt(uri).expect("jdt");
    assert_eq!(loc.jar, Some(PathBuf::from("C:/m2/gson-2.10.1.jar")));
    assert_eq!(loc.class, "com/google/gson/Gson");
    assert_eq!(loc.module, None);
}
