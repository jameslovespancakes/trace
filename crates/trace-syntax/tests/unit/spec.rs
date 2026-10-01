use super::*;
use crate::languages::syntax;

#[test]
fn rule_type_call_is_conversion_only_where_the_language_says_so() {
    assert!(type_call_is_conversion(Language::Go));
    assert!(!type_call_is_conversion(Language::Python));
    assert!(!type_call_is_conversion(Language::Rust));
    assert!(!type_call_is_conversion(Language::Cpp));
}

#[test]
fn every_grammar_language_has_a_spec() {
    #[allow(unused_mut)]
    let languages = [
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
    ];
    for lang in languages {
        let spec = syntax(lang).expect("spec");
        assert_eq!(spec.language, lang);
        assert!(!spec.comments.is_empty(), "{lang} comments");
        assert!(!spec.calls.is_empty(), "{lang} calls");
        for kind in spec.anonymous_functions {
            assert!(spec.is_lazy(kind), "{lang}: anonymous {kind} must be lazy");
        }
        for kind in spec.keyword_spreads {
            assert!(spec.spreads.contains(kind), "{lang}: {kind} must be a spread");
        }
        assert_eq!(
            spec.builtins.is_empty(),
            spec.builtins_module.is_empty(),
            "{lang}: builtins need a module"
        );
    }
    assert!(syntax(Language::Sql).is_none());
}

/// Every node kind named by a spec table (kinds, language rules, callback forms, scope rules,
/// typed bindings, return types, lowering and import tables) exists in the pinned grammar (a
/// misspelt kind would silently disable a rule).
#[test]
fn rule_spec_tables_name_real_grammar_kinds() {
    for language in crate::test_support::compiled_languages() {
        let Some(g) = crate::grammar::grammar(language) else {
            continue;
        };
        let spec = g.spec;
        let mut kinds: Vec<&str> = Vec::new();
        kinds.extend(spec.anonymous_functions);
        kinds.extend(spec.declaring_stores);
        kinds.extend(spec.type_contexts);
        kinds.extend(spec.type_names);
        kinds.extend(spec.imports);
        kinds.extend(spec.import_calls);
        kinds.extend(spec.lazy_scopes);
        kinds.extend(spec.export_specifiers.iter().map(|e| e.kind));
        kinds.extend(spec.operator_stores.iter().map(|o| o.kind));
        kinds.extend(spec.type_callees);
        kinds.extend(spec.pointer_conversions.iter().map(|p| p.kind));
        kinds.extend(spec.name_calls.iter().flat_map(|n| n.kinds.iter().copied()));
        kinds.extend(spec.separators);
        kinds.extend(spec.macro_definitions);
        kinds.extend(spec.placeholders);
        for form in spec.callback_forms {
            kinds.push(form.kind);
            kinds.extend(form.name_kinds);
        }
        let rules = spec.scopes;
        kinds.extend(rules.blocks);
        kinds.extend(rules.closed);
        kinds.extend(rules.pattern_leaves);
        kinds.extend(rules.binders.iter().map(|b| b.kind).filter(|k| !k.is_empty()));
        kinds.extend(rules.statics.iter().map(|s| s.kind));
        kinds.extend(spec.type_forms.iter().map(|f| f.kind));
        kinds.extend(spec.return_types.iter().map(|r| r.0));
        kinds.extend(spec.object_literals);
        kinds.extend(spec.yields);
        kinds.extend(spec.library_loops.iter().map(|l| l.kind));
        kinds.extend(spec.import_readers.iter().map(|r| r.0));
        for kind in kinds {
            assert_ne!(g.ts.id_for_node_kind(kind, true), 0, "{language}: unknown node kind {kind}");
        }
        // Only languages whose calls of a type convert know pointer conversions.
        assert!(spec.pointer_conversions.is_empty() || type_call_is_conversion(language), "{language}");
    }
}
