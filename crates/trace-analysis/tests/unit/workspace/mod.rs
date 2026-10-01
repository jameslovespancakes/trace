use super::resolve::suggest;
use super::settings::env_language_of;
use super::*;

/// I-58: an `--env` path no ecosystem accepts names the language it was given for: the
/// product language with the most files among those declaring dependencies (a Scala build
/// also names Java as a manifest language, a few Java files must not hide Scala).
#[test]
fn rule_env_not_found_names_the_language() {
    let files = [
        ("modules/core/src/main/scala/Json.scala", Language::Scala),
        ("modules/core/src/main/scala/Decoder.scala", Language::Scala),
        ("modules/core/src/main/scala/Encoder.scala", Language::Scala),
        ("modules/core/src/main/java/Util.java", Language::Java),
        ("scripts/release.sh", Language::Bash),
    ];
    assert_eq!(env_language_of(&files, &["build.sbt"]), Some(Language::Scala));
    // Nothing declares dependencies: no language to name.
    assert_eq!(env_language_of(&files, &["README.md"]), None);
    // TSX counts as TypeScript.
    let web = [
        ("src/App.tsx", Language::Tsx),
        ("src/main.tsx", Language::Tsx),
        ("tool.py", Language::Python),
    ];
    assert_eq!(env_language_of(&web, &["package.json", "requirements.txt"]), Some(Language::TypeScript));
    let err = trace_core::SetupError::EnvNotFound {
        language: env_language_of(&files, &["build.sbt"]),
        path: PathBuf::from("env"),
    };
    assert_eq!(err.to_string(), "No Scala environment found at env");
    assert_eq!(err.kind(), "env_not_found");
}

#[test]
fn protected_locations_and_ancestors_are_refused() {
    // Synthetic protected set: tests never name the real protected directories.
    let fake = [PathBuf::from("C:/work/secret"), PathBuf::from("/srv/private")];
    assert!(check_against(Path::new("C:/work/secret"), &fake).is_err());
    assert!(check_against(Path::new("/srv/private/sub/dir"), &fake).is_err());
    // A root containing a protected directory would inventory it.
    assert!(check_against(Path::new("C:/work"), &fake).is_err());
    // Prefix without a separator boundary is not a match.
    assert!(check_against(Path::new("C:/work/secret-notes"), &fake).is_ok());
    assert!(check_against(Path::new("C:/work/other"), &fake).is_ok());
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(check_against(&repo, &fake).is_ok());
}

#[cfg(windows)]
#[test]
fn protected_comparison_is_case_insensitive_on_windows() {
    let fake = [PathBuf::from("C:/work/secret")];
    assert!(check_against(Path::new(r"c:\WORK\Secret\src"), &fake).is_err());
}

/// DESIGN §1.3: an unknown name suggests up to 3 close names from the search index.
#[test]
fn rule_symbol_not_found_suggests_names() {
    let index = crate::test_support::project();
    let search = SearchIndex::build(&index);
    let names = suggest(&index, &search, "auth.py:Session.login_user");
    assert!(names.len() <= 3, "{names:?}");
    assert!(names.iter().any(|n| n == "login"), "{names:?}");
    assert!(suggest(&index, &search, "zzzz_nothing").is_empty());
    assert!(suggest(&index, &search, "  ").is_empty());
    let err = AnalysisError::SymbolNotFound {
        reference: "parse_config".into(),
        suggestions: vec!["load_config".into(), "parse_args".into()],
    };
    assert_eq!(err.to_string(), "No symbol named \"parse_config\". Did you mean: load_config, parse_args?");
    let bare = AnalysisError::SymbolNotFound {
        reference: "parse_config".into(),
        suggestions: Vec::new(),
    };
    assert_eq!(bare.to_string(), "No symbol named \"parse_config\".");
    assert_eq!(bare.kind(), "symbol_not_found");
}
