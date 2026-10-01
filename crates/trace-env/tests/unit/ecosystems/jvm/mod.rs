use super::*;
use crate::test_support::write;

fn platform() -> Platform {
    Platform::current()
}

fn fake_jdk(dir: &Path, version: &str) {
    let p = platform();
    write(dir, &format!("bin/{}", p.exe("java")), "");
    write(dir, "release", &format!("JAVA_VERSION=\"{version}\"\nIMPLEMENTOR=\"test\"\n"));
}

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    let home = tmp.path().join("home");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&home).unwrap();
    Fixture {
        _tmp: tmp,
        root,
        home,
    }
}

fn run_setup(f: &Fixture, env_override: Option<&Path>) -> JvmSetup {
    setup_at(&f.root, &f.home, env_override)
}

fn setup_at(root: &Path, home: &Path, env_override: Option<&Path>) -> JvmSetup {
    let p = platform();
    let home = home.display().to_string();
    let vars = EnvVars::from_pairs(&[("HOME", home.as_str()), ("USERPROFILE", home.as_str())]);
    let cx = DetectContext {
        root,
        platform: &p,
        vars: &vars,
        env_override,
        forbidden: &[],
        files: &[],
    };
    setup(&cx)
}

#[test]
fn rule_sbt_boot_folder_of_current_launchers_is_found() {
    let p = platform();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let local = tmp.path().join("local");
    let (h, l) = (home.display().to_string(), local.display().to_string());
    let vars = EnvVars::from_pairs(&[
        ("HOME", h.as_str()),
        ("USERPROFILE", h.as_str()),
        ("LOCALAPPDATA", l.as_str()),
        ("XDG_CACHE_HOME", l.as_str()),
    ]);
    let current = match p.os {
        Os::MacOs => home.join("Library").join("Caches").join("sbt").join("boot"),
        _ => local.join("sbt").join("boot"),
    };
    let legacy = home.join(".sbt").join("boot");
    // Nothing installed: the current launcher's folder.
    assert_eq!(sbt_boot_dir(&vars, &p, Some(&home)), Some(current.clone()));
    // Only an old launcher's folder exists: that one.
    fs::create_dir_all(&legacy).unwrap();
    assert_eq!(sbt_boot_dir(&vars, &p, Some(&home)), Some(legacy));
    // Both: the current one.
    fs::create_dir_all(&current).unwrap();
    assert_eq!(sbt_boot_dir(&vars, &p, Some(&home)), Some(current));
}

fn jdk(feature: u32, origin: Origin) -> Jdk {
    Jdk {
        home: PathBuf::from(format!("/jdk-{feature}")),
        version: Version::parse(&format!("{feature}.0.1")).unwrap(),
        feature,
        origin,
    }
}

#[test]
fn rule_jdk_version_from_release_file() {
    let tmp = tempfile::tempdir().unwrap();
    fake_jdk(tmp.path(), "21.0.12");
    let found = Jdk::from_home(tmp.path(), Origin::Path, &platform()).unwrap();
    assert_eq!(found.feature, 21);
    assert_eq!(found.version.text, "21.0.12");
    assert_eq!(found.environment_name(), "JavaSE-21");
    let legacy = tempfile::tempdir().unwrap();
    fake_jdk(legacy.path(), "1.8.0_504");
    let old = Jdk::from_home(legacy.path(), Origin::Path, &platform()).unwrap();
    assert_eq!(old.feature, 8);
    assert_eq!(old.environment_name(), "JavaSE-1.8");
    // A folder without a release file is a launcher stub, not a JDK.
    let stub = tempfile::tempdir().unwrap();
    write(stub.path(), &format!("bin/{}", platform().exe("java")), "");
    assert!(Jdk::from_home(stub.path(), Origin::Path, &platform()).is_none());
}

#[test]
fn rule_java_pin_texts_name_the_feature_release() {
    assert_eq!(pin_feature("21"), Some(21));
    assert_eq!(pin_feature("temurin-21.0.2"), Some(21));
    assert_eq!(pin_feature("openjdk64-17.0.2"), Some(17));
    assert_eq!(pin_feature("21.0.2-tem"), Some(21));
    assert_eq!(pin_feature("1.8"), Some(8));
    assert_eq!(pin_feature("zulu64-1.8.0.392"), Some(8));
}

#[test]
fn rule_project_release_selects_a_jdk_that_can_compile_it() {
    let p = platform();
    let jdks = vec![
        jdk(17, Origin::Path),
        jdk(21, Origin::StandardLocation),
        jdk(11, Origin::StandardLocation),
    ];
    let pin = JavaPin {
        feature: 21,
        source: "pom.xml".into(),
    };
    match select_jdk(&jdks, 8, Some(&pin), &[], &p) {
        ToolchainStatus::Found(t) => {
            assert_eq!(t.facts["feature"], "21");
            assert_eq!(t.origin, Origin::Pin);
        }
        other => panic!("{other:?}"),
    }
    // Without a pin, JAVA_HOME / PATH comes first.
    match select_jdk(&jdks, 8, None, &[], &p) {
        ToolchainStatus::Found(t) => assert_eq!(t.facts["feature"], "17"),
        other => panic!("{other:?}"),
    }
    // A server minimum (17) skips older JDKs.
    let old_first = vec![jdk(11, Origin::Path), jdk(17, Origin::StandardLocation)];
    match select_jdk(&old_first, 17, None, &[], &p) {
        ToolchainStatus::Found(t) => assert_eq!(t.facts["feature"], "17"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn rule_project_release_above_every_jdk_is_too_old() {
    let p = platform();
    let jdks = vec![jdk(17, Origin::Path), jdk(21, Origin::StandardLocation)];
    let pin = JavaPin {
        feature: 25,
        source: "pom.xml".into(),
    };
    match select_jdk(&jdks, 8, Some(&pin), &[], &p) {
        ToolchainStatus::TooOld {
            found,
            needed,
            source,
        } => {
            assert_eq!(found.facts["feature"], "21");
            assert_eq!(needed.min.unwrap().parts, vec![25]);
            assert_eq!(source, "pom.xml");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(select_jdk(&[], 8, None, &["PATH".into()], &p), ToolchainStatus::Missing { .. }));
    // The override is used even when another JDK would fit: too old is an error.
    let with_override = vec![jdk(11, Origin::Override), jdk(21, Origin::Path)];
    assert!(matches!(select_jdk(&with_override, 17, None, &[], &p), ToolchainStatus::TooOld { .. }));
}

#[test]
fn rule_env_override_jdk_is_classified() {
    let tmp = tempfile::tempdir().unwrap();
    fake_jdk(tmp.path(), "21.0.12");
    assert_eq!(env_kind(tmp.path(), &platform()), Some(EnvKind::Jdk));
    let gradle = tempfile::tempdir().unwrap();
    fs::create_dir_all(gradle.path().join("caches/modules-2")).unwrap();
    assert_eq!(env_kind(gradle.path(), &platform()), Some(EnvKind::GradleUserHome));
    let repo = tempfile::tempdir().unwrap();
    fs::create_dir_all(repo.path().join("repository")).unwrap();
    assert_eq!(env_kind(&repo.path().join("repository"), &platform()), Some(EnvKind::MavenRepository));
    let none = tempfile::tempdir().unwrap();
    assert_eq!(env_kind(none.path(), &platform()), None);
}

const PARENT_POM: &str = r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <groupId>com.example</groupId><artifactId>parent</artifactId><version>1.0</version>
  <packaging>pom</packaging>
  <modules><module>core</module></modules>
  <properties><guava.version>33.0-jre</guava.version><maven.compiler.release>17</maven.compiler.release></properties>
  <dependencyManagement><dependencies>
    <dependency><groupId>com.google.guava</groupId><artifactId>guava</artifactId><version>${guava.version}</version></dependency>
  </dependencies></dependencyManagement>
</project>"#;

const CORE_POM: &str = r#"<project>
  <parent><groupId>com.example</groupId><artifactId>parent</artifactId><version>1.0</version></parent>
  <artifactId>core</artifactId>
  <dependencies>
    <dependency><groupId>com.google.guava</groupId><artifactId>guava</artifactId></dependency>
    <dependency><groupId>junit</groupId><artifactId>junit</artifactId><version>4.13.2</version><scope>test</scope></dependency>
    <dependency><groupId>com.example</groupId><artifactId>parent</artifactId><version>${project.version}</version><type>pom</type></dependency>
  </dependencies>
</project>"#;

#[test]
fn rule_maven_dependencies_are_checked_in_the_local_repository() {
    let f = fixture();
    write(&f.root, "pom.xml", PARENT_POM);
    write(&f.root, "core/pom.xml", CORE_POM);
    write(&f.root, "core/src/main/java/com/example/A.java", "class A {}");
    let s = run_setup(&f, None);
    assert_eq!(s.project.maven_roots, vec![String::new()]);
    assert_eq!(s.project.maven_modules, vec![String::new(), "core".to_string()]);
    assert_eq!(s.project.java_release.as_ref().map(|r| r.feature), Some(17));
    let missing = &s.missing[&BuildSystem::Maven];
    assert!(missing.contains(&"com.google.guava:guava:33.0-jre".to_string()), "{missing:?}");
    assert!(missing.contains(&"junit:junit:4.13.2".to_string()), "{missing:?}");
    // Reactor modules are never looked up in the repository.
    assert!(!missing.iter().any(|m| m.starts_with("com.example:")), "{missing:?}");
    let report = s.deps_report(&[BuildSystem::Maven]);
    assert_eq!(report.status, DepsStatus::Missing);
    assert_eq!(report.hint, MAVEN_HINT);

    // Installed artifacts clear the report.
    let repo = f.home.join(".m2/repository");
    write(&repo, "com/google/guava/guava/33.0-jre/guava-33.0-jre.jar", "");
    write(&repo, "junit/junit/4.13.2/junit-4.13.2.jar", "");
    let s = run_setup(&f, None);
    assert!(s.missing.get(&BuildSystem::Maven).is_none_or(|m| m.is_empty()), "{:?}", s.missing);
    assert_eq!(s.deps_report(&[BuildSystem::Maven]).status, DepsStatus::Installed);
}

#[test]
fn rule_maven_project_outside_the_modules_is_a_subproject() {
    let f = fixture();
    write(&f.root, "pom.xml", PARENT_POM);
    write(&f.root, "core/pom.xml", CORE_POM);
    write(
        &f.root,
        "examples/demo/pom.xml",
        "<project><groupId>x</groupId><artifactId>demo</artifactId><version>1</version></project>",
    );
    let s = run_setup(&f, None);
    assert_eq!(s.project.subprojects.len(), 1);
    assert_eq!(s.project.subprojects[0].dir, "examples/demo");
}

#[test]
fn rule_maven_repository_from_maven_config() {
    let f = fixture();
    write(&f.root, ".mvn/maven.config", "-Dmaven.repo.local=local-repo -B\n");
    write(
        &f.root,
        "pom.xml",
        "<project><groupId>a</groupId><artifactId>b</artifactId><version>1</version></project>",
    );
    let s = run_setup(&f, None);
    assert_eq!(s.maven_repo, f.root.join("local-repo"));
    let g = fixture();
    write(
        &g.home,
        ".m2/settings.xml",
        "<settings><localRepository>${user.home}/m2repo</localRepository></settings>",
    );
    let s = run_setup(&g, None);
    assert_eq!(s.maven_repo, PathBuf::from(format!("{}/m2repo", g.home.display())));
}

#[test]
fn rule_generated_sources_need_a_build_first() {
    let f = fixture();
    write(
        &f.root,
        "pom.xml",
        r#"<project><groupId>a</groupId><artifactId>b</artifactId><version>1</version>
<build><plugins><plugin><artifactId>maven-compiler-plugin</artifactId><configuration>
<annotationProcessorPaths><path><groupId>x</groupId><artifactId>y</artifactId><version>1</version></path></annotationProcessorPaths>
</configuration></plugin></plugins></build></project>"#,
    );
    // Negative: without main sources the compiler never runs (nothing is generated).
    assert!(run_setup(&f, None).project.generated_sources.is_empty());
    write(&f.root, "src/main/java/a/A.java", "package a; class A {}\n");
    let s = run_setup(&f, None);
    assert_eq!(s.project.generated_sources, vec!["target/generated-sources".to_string()]);
    fs::create_dir_all(f.root.join("target/generated-sources/annotations")).unwrap();
    let s = run_setup(&f, None);
    assert!(s.project.generated_sources.is_empty());
}

#[test]
fn rule_pom_packaging_module_never_needs_generated_sources() {
    // The aggregator's compiler settings apply to its jar modules, never to itself.
    let f = fixture();
    write(
        &f.root,
        "pom.xml",
        r#"<project><groupId>a</groupId><artifactId>parent</artifactId><version>1</version>
<packaging>pom</packaging><modules><module>core</module></modules>
<build><pluginManagement><plugins><plugin><artifactId>maven-compiler-plugin</artifactId><configuration>
<annotationProcessorPaths><path><groupId>x</groupId><artifactId>y</artifactId><version>1</version></path></annotationProcessorPaths>
</configuration></plugin></plugins></pluginManagement></build></project>"#,
    );
    write(
        &f.root,
        "core/pom.xml",
        r#"<project><parent><groupId>a</groupId><artifactId>parent</artifactId><version>1</version></parent>
<artifactId>core</artifactId></project>"#,
    );
    write(&f.root, "core/src/main/java/a/A.java", "package a; class A {}\n");
    let s = run_setup(&f, None);
    assert_eq!(s.project.generated_sources, vec!["core/target/generated-sources".to_string()]);
    fs::create_dir_all(f.root.join("core/target/generated-sources/annotations")).unwrap();
    let s = run_setup(&f, None);
    assert!(s.project.generated_sources.is_empty(), "the pom module itself never needs them");
}

/// A stored zip archive of empty entries (local headers, central directory, end record),
/// after `prefix` bytes (self-extracting archives keep data in front of the zip part).
fn write_jar(path: &Path, prefix: &[u8], names: &[&str]) {
    let mut out: Vec<u8> = prefix.to_vec();
    let mut central: Vec<u8> = Vec::new();
    for name in names {
        let offset = u32::try_from(out.len() - prefix.len()).unwrap();
        let name_len = u16::try_from(name.len()).unwrap().to_le_bytes();
        out.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
        out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&[0; 12]);
        out.extend_from_slice(&name_len);
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(name.as_bytes());
        central.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
        central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        central.extend_from_slice(&[0; 12]);
        central.extend_from_slice(&name_len);
        central.extend_from_slice(&[0; 12]);
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd_offset = u32::try_from(out.len() - prefix.len()).unwrap();
    let cd_size = u32::try_from(central.len()).unwrap();
    let count = u16::try_from(names.len()).unwrap().to_le_bytes();
    out.extend_from_slice(&central);
    out.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0]);
    out.extend_from_slice(&count);
    out.extend_from_slice(&count);
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, out).unwrap();
}

#[test]
fn rule_jar_service_files_are_read_from_the_central_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let jar = tmp.path().join("a.jar");
    write_jar(&jar, &[], &["META-INF/MANIFEST.MF", PROCESSOR_SERVICE, "a/B.class"]);
    assert_eq!(zip_has_entry(&jar, PROCESSOR_SERVICE), Some(true));
    assert_eq!(zip_has_entry(&jar, "META-INF/services/com.sun.source.util.Plugin"), Some(false));
    // Data in front of the zip part (offsets relative to the zip part).
    let prefixed = tmp.path().join("b.jar");
    write_jar(&prefixed, b"#!/bin/sh\nexec java -jar \"$0\"\n", &[PROCESSOR_SERVICE]);
    assert_eq!(processor_jar_generates(&prefixed), Some(true));
    // Negative: a file that is not a zip archive is undecided, never "no processor".
    let text = tmp.path().join("c.jar");
    fs::write(&text, "not a zip archive at all, just some text").unwrap();
    assert_eq!(zip_has_entry(&text, PROCESSOR_SERVICE), None);
    assert_eq!(zip_has_entry(&tmp.path().join("missing.jar"), PROCESSOR_SERVICE), None);
}

#[test]
fn rule_compiler_plugin_is_not_a_source_generator() {
    let pom = |group: &str, artifact: &str, extra: &str| {
        format!(
            r#"<project><groupId>a</groupId><artifactId>b</artifactId><version>1</version>
<properties><plugin.version>2.0</plugin.version></properties>
<build><plugins><plugin><artifactId>maven-compiler-plugin</artifactId><configuration>
<compilerArgs><arg>-Xplugin:Checker</arg></compilerArgs>{extra}
<annotationProcessorPaths><path><groupId>{group}</groupId><artifactId>{artifact}</artifactId><version>${{plugin.version}}</version></path></annotationProcessorPaths>
</configuration></plugin></plugins></build></project>"#
        )
    };
    let f = fixture();
    let repo = f.home.join(".m2").join("repository");
    write(&f.root, "src/main/java/a/A.java", "package a; class A {}\n");
    // A javac plugin (only the compiler plugin service) generates nothing.
    write(&f.root, "pom.xml", &pom("com.example.check", "checker", ""));
    write_jar(
        &repo.join("com/example/check/checker/2.0/checker-2.0.jar"),
        &[],
        &[
            "META-INF/services/com.sun.source.util.Plugin",
            "com/example/check/Checker.class",
        ],
    );
    assert!(run_setup(&f, None).project.generated_sources.is_empty());
    // An annotation processor jar generates sources.
    write(&f.root, "pom.xml", &pom("com.example.gen", "generator", ""));
    write_jar(&repo.join("com/example/gen/generator/2.0/generator-2.0.jar"), &[], &[PROCESSOR_SERVICE]);
    assert_eq!(run_setup(&f, None).project.generated_sources, vec!["target/generated-sources".to_string()]);
    // A processor named explicitly runs even when its jar does not list it.
    write(
        &f.root,
        "pom.xml",
        &pom(
            "com.example.check",
            "checker",
            "<annotationProcessors><annotationProcessor>com.example.Gen</annotationProcessor></annotationProcessors>",
        ),
    );
    assert_eq!(run_setup(&f, None).project.generated_sources, vec!["target/generated-sources".to_string()]);
    // A jar that is not installed is undecided: the build must run first.
    write(&f.root, "pom.xml", &pom("com.example.absent", "unknown", ""));
    assert_eq!(run_setup(&f, None).project.generated_sources, vec!["target/generated-sources".to_string()]);
}

#[test]
fn rule_old_java_home_is_not_used_for_build_imports() {
    let v = |t: &str| Version::parse(t).unwrap();
    assert_eq!(gradle_min_jdk(Some(&v("9.1.0"))), 17);
    assert_eq!(gradle_min_jdk(Some(&v("8.14.3"))), 8);
    assert_eq!(gradle_min_jdk(None), 17);
    let f = fixture();
    let mut s = run_setup(&f, None);
    // JAVA_HOME is Java 8; newer JDKs are installed in the standard folders.
    s.jdks = vec![
        jdk(8, Origin::Path),
        jdk(17, Origin::StandardLocation),
        jdk(21, Origin::StandardLocation),
    ];
    let chosen = s.newest_jdk(gradle_min_jdk(Some(&v("9.1.0")))).unwrap();
    assert_eq!(chosen.facts["feature"], "21");
    // Negative: no installed JDK is new enough -> none (the caller uses trace's runtime).
    assert!(s.newest_jdk(25).is_none());
    // The wrapper's Gradle version decides.
    s.project.gradle_wrappers = vec![GradleWrapper {
        dir: String::new(),
        version: "8.14.3".into(),
        kind: "bin".into(),
        installed: None,
    }];
    assert_eq!(s.gradle_version().map(|g| g.text), Some("8.14.3".to_string()));
}

#[test]
fn rule_protobuf_test_sources_need_the_test_build() {
    let f = fixture();
    write(
        &f.root,
        "pom.xml",
        "<project><groupId>a</groupId><artifactId>b</artifactId><version>1</version></project>",
    );
    write(&f.root, "src/test/protobuf/bag.proto", "syntax = \"proto3\";\n");
    let s = run_setup(&f, None);
    assert_eq!(s.project.generated_sources, vec!["target/generated-test-sources".to_string()]);
    fs::create_dir_all(f.root.join("target/generated-test-sources/protobuf")).unwrap();
    let s = run_setup(&f, None);
    assert!(s.project.generated_sources.is_empty());
    // Negative: .proto files outside a source set's proto folder generate nothing.
    let g = fixture();
    write(
        &g.root,
        "pom.xml",
        "<project><groupId>a</groupId><artifactId>b</artifactId><version>1</version></project>",
    );
    write(&g.root, "docs/example.proto", "syntax = \"proto3\";\n");
    assert!(run_setup(&g, None).project.generated_sources.is_empty());
}

#[test]
fn rule_gradle_wrapper_distribution_checked() {
    let f = fixture();
    write(&f.root, "settings.gradle.kts", "rootProject.name = \"demo\"\ninclude(\":lib\")\n");
    write(&f.root, "build.gradle.kts", "plugins { java }\n");
    write(&f.root, "lib/build.gradle.kts", "plugins { `java-library` }\n");
    write(
        &f.root,
        "gradle/wrapper/gradle-wrapper.properties",
        "distributionUrl=https\\://services.gradle.org/distributions/gradle-9.7.1-bin.zip\n",
    );
    let s = run_setup(&f, None);
    assert_eq!(s.project.gradle_roots, vec![String::new()]);
    assert!(s.project.gradle_modules.contains(&"lib".to_string()));
    assert_eq!(s.project.gradle_wrappers[0].version, "9.7.1");
    assert!(s.project.gradle_wrappers[0].installed.is_none());
    assert!(s.missing[&BuildSystem::Gradle].contains(&"Gradle 9.7.1 (wrapper distribution)".to_string()));
    // No Gradle from the wrapper (a Gradle installed on this machine may still be found).
    assert!(s.gradle.as_ref().is_none_or(|g| !g.root.starts_with(&f.home)), "{:?}", s.gradle);
    // The unpacked distribution with its .ok marker counts as installed.
    let dist = f.home.join(".gradle/wrapper/dists/gradle-9.7.1-bin/abc123");
    write(&dist, "gradle-9.7.1-bin.zip.ok", "");
    write(&dist, "gradle-9.7.1/lib/gradle-core-api-9.7.1.jar", "");
    let s = run_setup(&f, None);
    assert!(s.project.gradle_wrappers[0].installed.is_some());
    assert!(s.missing.get(&BuildSystem::Gradle).is_none_or(|m| m.is_empty()), "{:?}", s.missing);
    assert_eq!(s.gradle.as_ref().map(|g| g.id), Some("gradle"));
}

/// Gradle build scripts are programs: never read. Every build directory below a settings
/// file is part of the build and no dependency is required from a script.
#[test]
fn rule_gradle_build_scripts_are_not_read() {
    let f = fixture();
    write(&f.root, "settings.gradle.kts", "rootProject.name = \"demo\"\n");
    write(
        &f.root,
        "build.gradle.kts",
        "dependencies {\n  implementation(\"com.google.code.gson:gson:2.13.0\")\n}\n",
    );
    write(&f.root, "core/build.gradle", "dependencies { implementation 'x:y:1.0' }\n");
    let s = run_setup(&f, None);
    assert_eq!(s.project.gradle_roots, vec![String::new()]);
    assert!(s.project.gradle_modules.contains(&"core".to_string()));
    assert!(s.missing.get(&BuildSystem::Gradle).is_none_or(|m| m.is_empty()), "{:?}", s.missing);
}

#[test]
fn rule_android_module_is_detected_by_its_manifest() {
    let f = fixture();
    write(&f.root, "settings.gradle.kts", "include(\":app\")\n");
    write(&f.root, "app/build.gradle.kts", "plugins { id(\"com.android.application\") }\n");
    write(&f.root, "app/src/main/AndroidManifest.xml", "<manifest/>\n");
    let s = run_setup(&f, None);
    assert_eq!(s.project.android, vec!["app".to_string()]);
    assert!(s.android_sdk.is_none());
}

#[test]
fn rule_scala_versions_come_from_the_build_definition() {
    let f = fixture();
    write(
        &f.root,
        "build.sbt",
        "val Scala212V: String = \"2.12.21\"\nval Scala213V: String = \"2.13.18\"\nval Scala3V = \"3.3.8\"\nThisBuild / crossScalaVersions := List(Scala3V, Scala212V, Scala213V)\nThisBuild / scalaVersion := Scala213V\n",
    );
    write(&f.root, "project/build.properties", "sbt.version=1.12.13\n");
    write(&f.root, "project/plugins.sbt", "addSbtPlugin(\"org.scala-js\" % \"sbt-scalajs\" % \"1.22.0\")\n");
    let s = run_setup(&f, None);
    assert_eq!(s.project.sbt_root.as_deref(), Some(""));
    assert_eq!(s.project.sbt_version.as_deref(), Some("1.12.13"));
    assert_eq!(s.project.scala_versions, vec!["2.13.18", "3.3.8", "2.12.21"]);
    assert_eq!(
        s.project.sbt_plugins,
        vec![("org.scala-js".to_string(), "sbt-scalajs".to_string(), "1.22.0".to_string())]
    );
    let missing = &s.missing[&BuildSystem::Sbt];
    assert!(missing.contains(&"sbt 1.12.13".to_string()), "{missing:?}");
    assert!(missing.contains(&"org.scala-js:sbt-scalajs_2.12_1.0:1.22.0".to_string()), "{missing:?}");
    assert_eq!(s.deps_report(&[BuildSystem::Sbt]).hint, SBT_HINT);
    let (versions, sbt) = scala_build_versions(&f.root);
    assert_eq!(versions[0], "2.13.18");
    assert_eq!(sbt.as_deref(), Some("1.12.13"));
}

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

#[test]
fn rule_maven_reactor_fixture_is_modelled() {
    let home = tempfile::tempdir().unwrap();
    let s = setup_at(&fixture_dir("rule-jvm-maven-java"), home.path(), None);
    assert_eq!(s.project.maven_roots, vec![String::new()]);
    assert_eq!(s.project.maven_modules, vec![String::new(), "app".to_string(), "core".to_string()]);
    assert_eq!(s.project.java_release.as_ref().map(|r| r.feature), Some(17));
    // gson comes from dependencyManagement; the reactor module `core` is never missing.
    assert_eq!(s.missing[&BuildSystem::Maven], vec!["com.google.code.gson:gson:2.13.1".to_string()]);
    assert!(s.project.subprojects.is_empty());
}

#[test]
fn rule_buildless_sources_declare_no_dependencies() {
    let f = fixture();
    write(&f.root, "src/com/example/A.java", "class A {}");
    let s = run_setup(&f, None);
    assert!(s.project.systems().is_empty());
    assert_eq!(s.deps_report(&BuildSystem::ALL).status, DepsStatus::NoneDeclared);
}
