use super::*;

/// PLAN decision 10: `default` is exactly the ten default languages (C# included, no
/// on-request language), independent of the repository.
#[test]
fn rule_install_default_covers_the_ten_default_languages() {
    assert_eq!(parse_target("default").unwrap(), InstallTarget::Default);
    assert_eq!(parse_target(" DEFAULT ").unwrap(), InstallTarget::Default);
    assert_eq!(DEFAULT_LANGUAGES.len(), 10);
    for l in [
        Language::Python,
        Language::JavaScript,
        Language::TypeScript,
        Language::Java,
        Language::CSharp,
        Language::C,
        Language::Cpp,
        Language::Go,
        Language::Rust,
        Language::Bash,
    ] {
        assert!(DEFAULT_LANGUAGES.contains(&l), "{l}");
    }
    for l in [Language::Php, Language::Scala, Language::Haskell, Language::R] {
        assert!(!DEFAULT_LANGUAGES.contains(&l), "{l} installs on request");
    }
    assert_eq!(parse_target("all").unwrap(), InstallTarget::All);
    assert_eq!(parse_target("c#").unwrap(), InstallTarget::One(Language::CSharp));
    assert_eq!(parse_target("Scala").unwrap(), InstallTarget::One(Language::Scala));
}

#[test]
fn rule_unknown_install_language_lists_ids() {
    let err = parse_target("cobol").unwrap_err();
    assert_eq!(err.kind(), "invalid_argument");
    let text = err.lines().join("\n");
    assert!(text.starts_with("Unknown language \"cobol\". Use one of: default, all, "), "{text}");
    for id in ["python", "scala", "csharp", "cpp", "r", "haskell"] {
        assert!(text.contains(id), "{id}: {text}");
    }
    assert!(!text.contains("tsx"), "Tsx is installed with TypeScript");
}

#[test]
fn rule_licence_question_names_server_version_and_licence() {
    let spec: InstallSpec = serde_json::from_value(serde_json::json!({
        "id": "intelephense", "version": "1.18.5", "license": "proprietary",
        "display": "PHP language server", "product": "Intelephense",
        "licence_gate": {"url": "https://intelephense.com/legal", "summary": "Section 3 limits use."},
        "recipe": "npm", "packages": []
    }))
    .unwrap();
    let gate = spec.licence_gate.clone().unwrap();
    let q = licence_question(&spec, &gate);
    assert_eq!(
        q,
        "The PHP language server (Intelephense 1.18.5) is proprietary. Licence: https://intelephense.com/legal\nSection 3 limits use.\nAccept the licence and install it? [y/N] "
    );
    let mut report = InstallReport {
        command: "install",
        trace_version: TRACE_VERSION,
        request: "php".into(),
        languages: vec![Language::Php],
        tools_dir: "t".into(),
        installed: Vec::new(),
        already: Vec::new(),
        licence_accepted: Vec::new(),
        notes: Vec::new(),
    };
    assert_eq!(report_text(&report), "nothing to install (php)\n");
    report.notes.push("Skipped: x".into());
    assert!(report_text(&report).contains("note       Skipped: x"));
}
