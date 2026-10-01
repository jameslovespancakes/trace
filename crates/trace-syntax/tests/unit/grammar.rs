use super::*;

#[test]
fn every_grammar_and_query_compiles() {
    let errors: Vec<String> = Language::ALL
        .into_iter()
        .filter_map(grammar_error)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "grammar errors: {errors:#?}");
    let langs = crate::test_support::compiled_languages();
    for lang in [
        Language::Python,
        Language::JavaScript,
        Language::TypeScript,
        Language::Tsx,
        Language::Rust,
        Language::Go,
        Language::Java,
        Language::C,
        Language::Cpp,
        Language::CSharp,
        Language::Php,
        Language::Bash,
        Language::Scala,
        Language::R,
        Language::Haskell,
    ] {
        assert!(langs.contains(&lang), "{lang} missing");
    }
    assert!(grammar(Language::Sql).is_none());
}
