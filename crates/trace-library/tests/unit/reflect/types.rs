use super::*;
use std::io::Write;

fn jar(path: &Path, entries: &[(&str, &str)]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    let file = std::fs::File::create(path).expect("jar");
    let mut zip = zip::ZipWriter::new(file);
    for (name, text) in entries {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .expect("entry");
        zip.write_all(text.as_bytes()).expect("write");
    }
    zip.finish().expect("finish");
}

fn root(path: &Path, layout: &'static str) -> LibraryRoot {
    LibraryRoot {
        path: path.to_path_buf(),
        kind: trace_env::LibraryKind::Dependency,
        ecosystem: trace_env::EcosystemId::Jvm,
        layout,
        version: None,
    }
}

#[test]
fn rule_compiled_attribute_lineage_follows_bases_through_installed_assemblies() {
    let dir = tempfile::tempdir().expect("temp");
    let dll = dir.path().join("lib.web/1.0.0/lib/net10.0/Lib.Web.dll");
    std::fs::create_dir_all(dll.parent().expect("parent")).expect("dirs");
    std::fs::write(&dll, crate::test_support::fixture_assembly()).expect("dll");
    let roots = [LibraryRoot {
        path: dir.path().to_path_buf(),
        kind: trace_env::LibraryKind::Dependency,
        ecosystem: trace_env::EcosystemId::Dotnet,
        layout: "nuget_packages",
        version: None,
    }];
    let lineage = clr_lineage(&roots, "Lib.Web.ReadAttribute");
    let names: Vec<String> = lineage.iter().map(|t| t.def.full()).collect();
    assert_eq!(
        names[..2],
        ["Lib.Web.ReadAttribute".to_string(), "Lib.Web.Routing.VerbAttribute".to_string()]
    );
    assert!(lineage[0].literals.contains(&"GET".to_string()));
    assert!(lineage[1]
        .def
        .interfaces
        .iter()
        .any(|i| i.full() == "Lib.Web.Routing.IVerbSource"));
    // No installed assembly defines the name: no lineage.
    assert!(clr_lineage(&roots, "Lib.Web.MissingAttribute").is_empty());
}

#[test]
fn rule_installed_annotation_type_is_read_from_its_package_path_in_a_source_archive() {
    let dir = tempfile::tempdir().expect("temp");
    let text = "package lib.web;\n@Mapped(method = Verb.GET)\npublic @interface GetRoute {}\n";
    jar(&dir.path().join("m2/lib/web-kit/1.0/web-kit-1.0-sources.jar"), &[("lib/web/GetRoute.java", text)]);
    jar(
        &dir.path()
            .join("gradle/files-2.1/lib/web-kit/1.0/abc/web-kit-1.0-sources.jar"),
        &[("lib/web/GetRoute.java", text)],
    );
    let maven =
        type_sources(&[root(&dir.path().join("m2"), "maven_repo")], Language::Java, "lib.web.GetRoute");
    assert_eq!(maven.len(), 1);
    assert_eq!(String::from_utf8_lossy(&maven[0].1), text);
    let gradle =
        type_sources(&[root(&dir.path().join("gradle"), "gradle_cache")], Language::Java, "lib.web.GetRoute");
    assert_eq!(gradle.len(), 1);
    // A name no archive declares (and a name outside the group folders) finds nothing.
    assert!(type_sources(&[root(&dir.path().join("m2"), "maven_repo")], Language::Java, "lib.web.Other")
        .is_empty());
    assert!(type_sources(&[root(&dir.path().join("m2"), "maven_repo")], Language::Java, "other.GetRoute")
        .is_empty());
}
