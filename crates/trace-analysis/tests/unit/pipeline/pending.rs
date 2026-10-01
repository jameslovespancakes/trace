use super::*;
use crate::pipeline::records::file_support;

/// DESIGN §1.13: a language whose every file is test / fixture / example code in a
/// repository dominated by another language is pending (not set up at index time); its
/// files carry the reason; a query that sets it up makes it a product language.
/// Build scripts in a code language (Mill) do not make that language a product language:
/// a Java project built with Mill does not need the Scala server for them.
#[test]
fn rule_build_scripts_do_not_require_their_language() {
    let files = [
        ("src/main/java/com/example/App.java", Language::Java),
        ("build.mill", Language::Scala),
        ("tools/build.sc", Language::Scala),
    ];
    assert_eq!(pending_languages(&files), BTreeSet::from([Language::Scala]));
    // A Scala source file makes Scala a product language again.
    let mut with_source = files.to_vec();
    with_source.push(("src/main/scala/com/example/Util.scala", Language::Scala));
    assert!(pending_languages(&with_source).is_empty());
}

#[test]
fn rule_pending_language_is_not_analyzed() {
    let files = [
        ("app/src/main/scala/App.scala", Language::Scala),
        ("app/src/main/scala/Format.scala", Language::Scala),
        ("app/src/main/scala/Io.scala", Language::Scala),
        ("app/src/test/java/com/example/FormatTest.java", Language::Java),
        ("app/src/jvmTest/java/com/example/Helper.java", Language::Java),
        ("docs/examples/demo.py", Language::Python),
        ("docs/examples/schema.proto", Language::Proto),
    ];
    let pending = pending_languages(&files);
    assert_eq!(pending, BTreeSet::from([Language::Java, Language::Python]), "only code languages");
    let settings = RepoSettings::default();
    let reasons = pending_reasons(&files, &pending, &[], &settings);
    assert_eq!(reasons.len(), 3);
    assert!(reasons["docs/examples/demo.py"]
        .starts_with("Python is only used in tests, fixtures, examples or build scripts here"));
    assert!(!reasons.contains_key("app/src/main/scala/App.scala"));
    // Set up by a query: from then on a product language.
    let mut ready = RepoSettings::default();
    ready.ready_languages.insert(Language::Java);
    assert_eq!(pending_now(&files, &ready), BTreeSet::from([Language::Python]));
    // Product code of a language keeps it, however small; a language alone is never pending.
    let mixed = [
        ("src/a.scala", Language::Scala),
        ("src/b.scala", Language::Scala),
        ("src/c.scala", Language::Scala),
        ("src/Main.java", Language::Java),
        ("src/test/java/MainTest.java", Language::Java),
    ];
    assert!(pending_languages(&mixed).is_empty());
    // A JVM package path named `example` (com.example) is product code, not an examples dir.
    assert!(!is_non_product_location("app/src/main/scala/com/example/App.scala", Language::Scala));
    assert!(is_non_product_location("examples/demo/main.scala", Language::Scala));
    assert!(pending_languages(&[("tests/test_a.py", Language::Python)]).is_empty());
    // Pending files never reach a backend partition and are recorded as pending.
    assert_eq!(file_support(Language::Java, false, true), SupportLevel::Pending);
    assert_eq!(file_support(Language::Proto, false, false), SupportLevel::Inventoried);
    assert_eq!(file_support(Language::Java, true, false), SupportLevel::Semantic);
}

/// I-09: packaging files (package recipes, installer / release scripts, packaging folders)
/// are not project code: a language used only in them is pending with the packaging
/// reason (listed, set up when a query needs one of its files), never required at index
/// time. The rule is a location / file-role rule.
#[test]
fn rule_packaging_files_never_require_a_language() {
    let files = [
        ("crates/core/src/lib.rs", Language::Rust),
        ("crates/core/src/search.rs", Language::Rust),
        ("pkg/brew/tool-bin.php", Language::Php),
        ("install.sh", Language::Bash),
        ("ci/build_release.py", Language::Python),
        ("packaging/linux/postinst.sh", Language::Bash),
    ];
    let pending = pending_languages(&files);
    assert_eq!(
        pending,
        BTreeSet::from([Language::Php, Language::Bash, Language::Python]),
        "packaging-only languages are never required"
    );
    let reasons = pending_reasons(&files, &pending, &[], &RepoSettings::default());
    assert!(reasons["pkg/brew/tool-bin.php"].starts_with("PHP is only used in packaging files here"));
    assert!(!reasons.contains_key("crates/core/src/lib.rs"));
    assert_eq!(pending_dir(&reasons["install.sh"]), None, "set up per language when a query needs it");
    // Location and role rules.
    assert!(is_packaging_location("Formula/tool.py", Language::Python));
    assert!(is_packaging_location("dist/debian/rules.sh", Language::Bash));
    assert!(is_packaging_location("docker/entrypoint.sh", Language::Bash));
    assert!(is_packaging_location("PKGBUILD", Language::Bash));
    assert!(is_packaging_location("release.py", Language::Python));
    // Negatives: code packages under `pkg/`, scripts below the root, compiled `ci/` code.
    assert!(!is_packaging_location("pkg/server/handler.go", Language::Go));
    assert!(!is_packaging_location("scripts/install_deps.py", Language::Python));
    assert!(!is_packaging_location("ci/runner/main.go", Language::Go));
    assert!(!is_packaging_location("src/installer_ui.py", Language::Python));
    // A repository made only of packaging files: its language is its product.
    assert!(pending_languages(&[("Formula/a.py", Language::Python), ("Formula/b.py", Language::Python)])
        .is_empty());
}

/// I-09 negative: a language with real product files still requires its server, even if
/// some of its files are packaging files.
#[test]
fn rule_product_code_in_the_same_language_still_requires_it() {
    let files = [
        ("nvm.sh", Language::Bash),
        ("install.sh", Language::Bash),
        ("test/fast/unit.sh", Language::Bash),
        ("lib/tool.php", Language::Php),
        ("pkg/brew/tool.php", Language::Php),
        ("src/app.py", Language::Python),
        ("src/core.py", Language::Python),
        ("src/cli.py", Language::Python),
    ];
    assert!(pending_languages(&files).is_empty());
    assert!(packaging_only_languages(&files).is_empty());
}

/// Sub-projects a preflight reports are pending for that backend's languages only, until a
/// query sets them up.
#[test]
fn rule_pending_sub_project_follows_the_preflight() {
    let files = [
        ("src/App.hs", Language::Haskell),
        ("lib/App/Core.hs", Language::Haskell),
        ("integration_test/lib/Case.hs", Language::Haskell),
        ("integration_test/tool.py", Language::Python),
    ];
    let dirs = BTreeMap::from([("integration_test".to_string(), "its own cabal project".to_string())]);
    let prepared = vec![(vec![Language::Haskell], &dirs)];
    let settings = RepoSettings::default();
    let reasons = pending_reasons(&files, &BTreeSet::new(), &prepared, &settings);
    assert_eq!(
        reasons.get("integration_test/lib/Case.hs").map(String::as_str),
        Some("sub-project integration_test: its own cabal project")
    );
    assert!(!reasons.contains_key("integration_test/tool.py"), "other backends are not affected");
    assert!(!reasons.contains_key("lib/App/Core.hs"));
    let mut ready = RepoSettings::default();
    ready.ready_dirs.insert("integration_test".into());
    assert!(pending_reasons(&files, &BTreeSet::new(), &prepared, &ready).is_empty());
    assert!(!in_dir("integration_testing/A.hs", "integration_test"));
    assert_eq!(pending_dir(&reasons["integration_test/lib/Case.hs"]), Some("integration_test"));
    assert_eq!(pending_dir(&pending_language_reason(Language::Java)), None);
}
