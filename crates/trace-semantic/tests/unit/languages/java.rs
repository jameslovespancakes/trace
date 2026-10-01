use super::*;
use crate::registry::Registry;
use crate::test_support::facts::{facts_with_imports, sfile};
use crate::test_support::setup::{assert_placeholders_filled, items, with_context, write};

#[test]
fn rule_maven_import_needs_approval() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "pom.xml",
        "<project><groupId>a</groupId><artifactId>b</artifactId><version>1</version></project>",
    );
    write(tmp.path(), "src/main/java/a/A.java", "package a; class A {}");
    let files = [("src/main/java/a/A.java", Language::Java)];
    let err = with_context("lsp:jdtls", tmp.path(), &files, false, |cx| Hooks.preflight(cx)).unwrap_err();
    let all = items(&err);
    let approval = all
        .iter()
        .find(|e| e.kind() == "build_not_allowed")
        .unwrap_or_else(|| panic!("{all:?}"));
    assert_eq!(
        approval.lines(),
        vec![
            "Java needs Maven, which runs this project's build plugins.".to_string(),
            "       Only allow this for projects you trust: trace index --allow-build".to_string(),
        ]
    );
    // With approval the same repository has no approval error.
    let err = with_context("lsp:jdtls", tmp.path(), &files, true, |cx| Hooks.preflight(cx));
    if let Err(e) = err {
        assert!(!items(&e).iter().any(|x| x.kind() == "build_not_allowed"), "{e:?}");
    }
}

#[test]
fn rule_gradle_import_needs_approval() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "settings.gradle.kts", "rootProject.name = \"x\"\n");
    write(tmp.path(), "build.gradle.kts", "plugins { java }\n");
    let files = [("src/main/java/a/A.java", Language::Java)];
    let err = with_context("lsp:jdtls", tmp.path(), &files, false, |cx| Hooks.preflight(cx)).unwrap_err();
    let all = items(&err);
    assert!(
        all.iter()
            .any(|e| e.lines()[0] == "Java needs Gradle, which runs this project's build scripts."),
        "{all:?}"
    );
}

#[test]
fn rule_buildless_java_needs_no_approval() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/a/A.java", "package a; class A {}");
    let files = [("src/a/A.java", Language::Java)];
    let result = with_context("lsp:jdtls", tmp.path(), &files, false, |cx| Hooks.preflight(cx));
    if let Err(e) = &result {
        assert!(!items(e).iter().any(|x| x.kind() == "build_not_allowed"), "{e:?}");
    }
    // The plan of build-less sources: a snapshot, no import, no project code.
    let prepared = with_context("lsp:jdtls", tmp.path(), &files, false, |cx| {
        let readable = trace_core::paths::forbidden_roots();
        let dcx = crate::languages::read_context(cx, trace_env::EcosystemId::Jvm, &readable);
        let setup = jvm::setup(&dcx);
        java_prepared(cx, &setup, &java_systems(&setup), None, Vec::new())
    });
    assert_eq!(prepared.workspace, WorkspaceMode::Snapshot);
    assert!(!prepared.runs_project_code);
    assert_eq!(prepared.json_vars["maven_import"], json!(false));
    assert_eq!(prepared.json_vars["gradle_import"], json!(false));
}

#[test]
fn rule_project_release_above_installed_jdk_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    write(
            tmp.path(),
            "pom.xml",
            "<project><groupId>a</groupId><artifactId>b</artifactId><version>1</version><properties><maven.compiler.release>25</maven.compiler.release></properties></project>",
        );
    let files = [("src/main/java/a/A.java", Language::Java)];
    with_context("lsp:jdtls", tmp.path(), &files, true, |cx| {
        let readable = trace_core::paths::forbidden_roots();
        let dcx = crate::languages::read_context(cx, trace_env::EcosystemId::Jvm, &readable);
        let mut setup = jvm::setup(&dcx);
        assert_eq!(setup.java_pin.as_ref().map(|p| p.feature), Some(25));
        setup.jdks = vec![trace_env::jvm::Jdk {
            home: PathBuf::from("/jdk-21"),
            version: trace_env::os::Version::parse("21.0.12").unwrap(),
            feature: 21,
            origin: trace_env::Origin::Path,
        }];
        let status = setup.select_jdk(MIN_JAVA_FEATURE);
        let err = jdk_error(Language::Java, &setup, MIN_JAVA_FEATURE, &status).unwrap();
        assert_eq!(
                err.lines(),
                vec!["Java needs a JDK 25 or newer, which is not installed. Install one from https://adoptium.net and run trace again.".to_string()]
            );
        setup.jdks.clear();
        let status = setup.select_jdk(17);
        let err = jdk_error(Language::Scala, &setup, 17, &status).unwrap();
        assert_eq!(
                err.lines()[0],
                "Scala needs a JDK 25 or newer, which is not installed. Install one from https://adoptium.net and run trace again."
            );
    });
}

#[test]
fn rule_generated_maven_settings_are_offline() {
    let xml = maven_settings_xml(Path::new("/home/u/.m2/repository"));
    let root = trace_core::formats::xml::parse(&xml).unwrap();
    assert_eq!(root.text_at(&["offline"]), Some("true"));
    assert_eq!(root.text_at(&["localRepository"]), Some("/home/u/.m2/repository"));
    // The jdtls registry entry reads exactly this file and runs every import offline.
    let entry = Registry::builtin().entry("lsp:jdtls").cloned().unwrap();
    let java = &entry.settings["java"];
    assert_eq!(java["configuration"]["maven"]["userSettings"], json!("{outside}/maven-settings.xml"));
    assert_eq!(java["import"]["maven"]["offline"]["enabled"], json!(true));
    assert_eq!(java["import"]["gradle"]["offline"]["enabled"], json!(true));
    assert_eq!(java["import"]["gradle"]["arguments"], json!("--offline"));
    assert!(entry.args.iter().any(|a| a == "-Dhttps.proxyPort=9"));
}

/// Rule: jdtls answers call hierarchy without implementor search (trace asks
/// implementations itself); the preference joins the server's own instance preferences
/// (kept, an earlier value replaced) in the `-data` workspace the registry passes.
#[test]
fn rule_call_hierarchy_runs_without_implementor_search() {
    let own = "PREF_FILTER_TESTCODE=false\neclipse.preferences.version=1\nworkspaceInitialized=true\nPREF_USE_IMPLEMENTORS=true\n";
    let text = with_preference(own, "PREF_USE_IMPLEMENTORS", "false");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines,
        [
            "PREF_FILTER_TESTCODE=false",
            "eclipse.preferences.version=1",
            "workspaceInitialized=true",
            "PREF_USE_IMPLEMENTORS=false"
        ]
    );
    assert_eq!(
        with_preference("", "PREF_USE_IMPLEMENTORS", "false"),
        "eclipse.preferences.version=1\nPREF_USE_IMPLEMENTORS=false\n"
    );
    assert_eq!(IMPLEMENTOR_SEARCH_OFF, ("PREF_USE_IMPLEMENTORS", "false"));
    let entry = Registry::builtin().entry("lsp:jdtls").cloned().unwrap();
    let data = entry
        .args
        .iter()
        .position(|a| a == "-data")
        .map(|i| entry.args[i + 1].as_str());
    let dir = SERVER_PREFERENCES.split('/').next().unwrap();
    assert_eq!(data, Some(format!("{{outside}}/{dir}").as_str()));
}

#[test]
fn rule_missing_artifact_marker_is_deps_error() {
    let prepared = Prepared {
        data: Some(Arc::new(JavaData {
            build: Some(BuildSystem::Maven),
            maven_repo: PathBuf::from("/m2"),
        })),
        ..Prepared::default()
    };
    let diagnostics = vec![(
        "file:///ws/pom.xml".to_string(),
        json!({"diagnostics": [{"severity": 1, "message": "Missing artifact com.example:collections:jar:33.0"}]}),
    )];
    let cx = LoadedContext {
        prepared: &prepared,
        log_messages: &[(1, "Failed to configure some Maven project(s)".to_string())],
        notifications: &[],
        diagnostics: &diagnostics,
        log: Path::new("/log/jdtls.log"),
    };
    let err = Hooks.check_loaded(&cx).unwrap_err();
    assert_eq!(
        err,
        SetupError::DepsMissing {
            language: Language::Java,
            hint: "mvn dependency:go-offline".to_string()
        }
    );
    // An import failure without missing artifacts is the build error.
    let cx = LoadedContext {
        diagnostics: &[],
        ..cx
    };
    assert_eq!(Hooks.check_loaded(&cx).unwrap_err().kind(), "build_failed");
    // A Java source diagnostic is never a build signal.
    let source = vec![(
        "file:///ws/src/A.java".to_string(),
        json!({"diagnostics": [{"severity": 1, "message": "Missing artifact x"}]}),
    )];
    let cx = LoadedContext {
        log_messages: &[],
        diagnostics: &source,
        ..cx
    };
    assert!(Hooks.check_loaded(&cx).is_ok());
}

#[test]
fn rule_jdtls_config_dir_per_platform() {
    let p = |os, arch| Platform {
        os,
        arch,
        arch_name: String::new(),
        musl: false,
    };
    assert_eq!(jdtls_config_dir(&p(Os::Windows, Arch::X86_64)), "config_win");
    assert_eq!(jdtls_config_dir(&p(Os::Linux, Arch::X86_64)), "config_linux");
    assert_eq!(jdtls_config_dir(&p(Os::Linux, Arch::Aarch64)), "config_linux_arm");
    assert_eq!(jdtls_config_dir(&p(Os::MacOs, Arch::X86_64)), "config_mac");
    assert_eq!(jdtls_config_dir(&p(Os::MacOs, Arch::Aarch64)), "config_mac_arm");
    let entry = Registry::builtin().entry("lsp:jdtls").cloned().unwrap();
    assert!(entry
        .args
        .iter()
        .any(|a| a == "-Dosgi.sharedConfiguration.area={jdtls_config}"));
}

#[test]
fn rule_jdt_uri_is_an_external_library() {
    let uri = "jdt://contents/jsonlib-2.10.1.jar/com.example.jsonlib/Jsonlib.class?=jsonlib/C:%5C/Users%5C/me%5C/.m2%5C/repository%5C/com%5C/example%5C/code%5C/jsonlib%5C/jsonlib%5C/2.10.1%5C/jsonlib-2.10.1.jar%3Ccom.example.jsonlib(Jsonlib.class";
    let loc = Hooks.external_location(uri, &Prepared::default()).unwrap();
    assert_eq!(loc.package, "com.example.code.jsonlib:jsonlib");
    assert_eq!(loc.version.as_deref(), Some("2.10.1"));
    assert_eq!(loc.symbol.as_deref(), Some("com.example.jsonlib.Jsonlib"));
    assert!(!loc.readable);
    assert!(!loc.stdlib);
    let jdk = "jdt://contents/java.base/java.util/ArrayList.class?=jsonlib/%5C/modules%5C/java.base%3Cjava.util(ArrayList.class";
    let loc = Hooks.external_location(jdk, &Prepared::default()).unwrap();
    assert!(loc.stdlib);
    assert_eq!(loc.package, "jdk");
    assert_eq!(loc.symbol.as_deref(), Some("java.util.ArrayList"));
    // Files of the workspace are not library locations.
    assert!(Hooks
        .external_location("file:///ws/src/A.java", &Prepared::default())
        .is_none());
}

#[test]
fn rule_jar_paths_name_their_package() {
    assert_eq!(
        jar_library(
            "/h/.gradle/caches/modules-2/files-2.1/com.example.io/iolib-jvm/3.16.0/abc/iolib-jvm-3.16.0.jar"
        ),
        ("com.example.io:iolib-jvm".to_string(), Some("3.16.0".to_string()))
    );
    assert_eq!(
            jar_library("C:/cs/v1/https/repo1.maven.org/maven2/io/example/jsonlib-core_2.13/0.14.10/jsonlib-core_2.13-0.14.10.jar"),
            ("io.example:jsonlib-core_2.13".to_string(), Some("0.14.10".to_string()))
        );
    assert_eq!(jar_library("lib/foo-bar-1.2.jar"), ("foo-bar".to_string(), Some("1.2".to_string())));
    let loc = jar_location("jar:///c%3A/g/files-2.1/org.scala-lang/scala-library/2.13.18/h/scala-library-2.13.18.jar!/scala/collection/immutable/List.class").unwrap();
    assert!(loc.stdlib);
    assert_eq!(loc.symbol.as_deref(), Some("scala.collection.immutable.List"));
}

#[test]
fn rule_jdtls_entry_placeholders_are_filled_by_preflight() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "pom.xml",
        "<project><groupId>a</groupId><artifactId>b</artifactId><version>1</version></project>",
    );
    let files = [("src/main/java/a/A.java", Language::Java)];
    let prepared = with_context("lsp:jdtls", tmp.path(), &files, true, |cx| {
        let readable = trace_core::paths::forbidden_roots();
        let dcx = crate::languages::read_context(cx, trace_env::EcosystemId::Jvm, &readable);
        let setup = jvm::setup(&dcx);
        java_prepared(cx, &setup, &java_systems(&setup), None, Vec::new())
    });
    assert_eq!(prepared.workspace, WorkspaceMode::Mirror);
    assert!(prepared.runs_project_code);
    let entry = Registry::builtin().entry("lsp:jdtls").cloned().unwrap();
    assert_placeholders_filled(&entry, &prepared);
}

#[test]
fn rule_gradle_import_never_runs_on_a_jdk_older_than_gradle_needs() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "settings.gradle.kts", "rootProject.name = \"x\"\n");
    write(tmp.path(), "build.gradle.kts", "plugins { java }\n");
    let files = [("src/main/java/a/A.java", Language::Java)];
    let mut setup = with_context("lsp:jdtls", tmp.path(), &files, true, |cx| {
        let readable = trace_core::paths::forbidden_roots();
        jvm::setup(&crate::languages::read_context(cx, trace_env::EcosystemId::Jvm, &readable))
    });
    let jdk = |feature: u32, origin| trace_env::jvm::Jdk {
        home: PathBuf::from(format!("/jdk-{feature}")),
        version: trace_env::os::Version::parse(&format!("{feature}.0.1")).unwrap(),
        feature,
        origin,
    };
    setup.project.gradle_wrappers = vec![trace_env::jvm::GradleWrapper {
        dir: String::new(),
        version: "9.1.0".into(),
        kind: "bin".into(),
        installed: None,
    }];
    // JAVA_HOME is Java 8 (the project JDK); a JDK 21 is installed.
    setup.jdks = vec![jdk(8, trace_env::Origin::Path), jdk(21, trace_env::Origin::StandardLocation)];
    let project = match setup.select_jdk(MIN_JAVA_FEATURE) {
        ToolchainStatus::Found(t) => t,
        other => panic!("{other:?}"),
    };
    assert_eq!(project.facts["feature"], "8");
    let (home, note) = gradle_import_jdk(&setup, Some(&project));
    assert_eq!(home, json!(path_text(Path::new("/jdk-21"))));
    assert!(note.unwrap().starts_with("Gradle 9.1.0 import runs on JDK 21.0.1 ("));
    // No installed JDK is new enough: trace's JDK runtime.
    setup.jdks.truncate(1);
    let (home, note) = gradle_import_jdk(&setup, Some(&project));
    assert_eq!(home, json!("{runtime:jdk}"));
    assert!(note.is_some());
    // Negative: Gradle 8 runs on Java 8 - the project JDK is kept, nothing to say.
    setup.project.gradle_wrappers[0].version = "8.14.3".into();
    let (home, note) = gradle_import_jdk(&setup, Some(&project));
    assert_eq!(home, json!(path_text(Path::new("/jdk-8"))));
    assert!(note.is_none());
    // The jdtls entry reads exactly this value.
    let entry = Registry::builtin().entry("lsp:jdtls").cloned().unwrap();
    assert_eq!(entry.settings["java"]["import"]["gradle"]["java"]["home"], json!("{json:gradle_java_home}"));
}

#[test]
fn rule_gradle_and_bloop_run_inside_trace_folders_without_daemons() {
    // jdtls (Buildship's Tooling API runs in the server JVM): Gradle daemons keep their
    // registry and logs in the backend's state folder and stop soon after the import.
    let entry = Registry::builtin().entry("lsp:jdtls").cloned().unwrap();
    assert!(
        entry
            .args
            .iter()
            .any(|a| a == "-Dorg.gradle.daemon.registry.base={outside}/gradle-daemon"),
        "{:?}",
        entry.args
    );
    assert!(entry
        .args
        .iter()
        .any(|a| a == "-Dorg.gradle.daemon.idletimeout=15000"));
}

#[test]
fn rule_eclipse_classpath_roots_come_from_imports_and_package_directories() {
    let main = facts_with_imports(Language::Java, &["com.example.util.Helper"]);
    let test =
        facts_with_imports(Language::Java, &["com.example.util.Helper.run", "org.example.testing.Test"]);
    let none = facts_with_imports(Language::Java, &[]);
    let files = [
        sfile("lib/src/main/java/com/example/App.java", Language::Java, &main),
        sfile("lib/src/main/java/com/example/util/Helper.java", Language::Java, &none),
        sfile("lib/src/test/java/com/example/util/HelperTest.java", Language::Java, &test),
    ];
    let refs: Vec<&SemanticFile<'_>> = files.iter().collect();
    let out = eclipse_project(&refs);
    let classpath =
        String::from_utf8(out.iter().find(|(n, _)| n == ".classpath").unwrap().1.clone()).unwrap();
    assert!(classpath.contains("path=\"lib/src/main/java\""), "{classpath}");
    assert!(classpath.contains("path=\"lib/src/test/java\""), "{classpath}");
    assert!(out.iter().any(|(n, _)| n == ".project"));
}
