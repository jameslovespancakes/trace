use super::*;

#[test]
fn rule_every_derivable_language_has_an_adapter() {
    for language in [
        Language::Python,
        Language::JavaScript,
        Language::TypeScript,
        Language::Tsx,
        Language::R,
        Language::Go,
        Language::Rust,
        Language::Php,
        Language::Java,
    ] {
        let spec = adapter(language).expect("adapter");
        assert!(!spec.extensions.is_empty(), "{language:?}");
    }
    assert!(adapter(Language::CSharp).is_none(), "no readable source: declared types");
    assert!(spec_for(Language::CSharp).is_some(), "the table still serves it");
}

#[test]
fn rule_every_language_spec_serves_distinct_languages_and_names_its_table() {
    let mut seen = Vec::new();
    for spec in ALL {
        for l in spec.languages {
            assert!(!seen.contains(l), "{l:?} served twice");
            seen.push(*l);
        }
        let key = spec.languages[0].as_str();
        let table: serde_json::Value = serde_json::from_str(spec.table).expect("table json");
        assert_eq!(table["language"].as_str(), Some(key), "assets/library/{key}.json");
    }
    assert_eq!(table_language(Language::Tsx), Language::JavaScript);
    assert_eq!(table_language(Language::Sql), Language::Sql, "no table: itself");
}

#[test]
fn rule_stdindex_only_for_languages_whose_server_may_give_no_location() {
    assert!(adapter(Language::Python).is_some_and(|s| s.stdlib_index));
    for language in [
        Language::Go,
        Language::Rust,
        Language::Java,
        Language::JavaScript,
        Language::TypeScript,
        Language::Php,
        Language::R,
    ] {
        assert!(adapter(language).is_some_and(|s| !s.stdlib_index), "{language:?}");
    }
}
