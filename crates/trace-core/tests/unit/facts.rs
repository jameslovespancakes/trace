use super::*;

fn p(name: &str) -> Param {
    Param {
        name: name.into(),
        kind: ParamKind::Positional,
        has_default: false,
    }
}

fn with_default(name: &str) -> Param {
    Param {
        has_default: true,
        ..p(name)
    }
}

fn of(name: &str, kind: ParamKind) -> Param {
    Param { kind, ..p(name) }
}

/// Rule `arity`: too many or too few positional arguments for every reading of the
/// parameter list rule the declaration out; defaults widen the accepted range.
#[test]
fn rule_arity_mismatch_rules_out_a_name_match() {
    let two = [p("a"), p("b")];
    assert_eq!(arity_accepts(Language::Python, &two, 2, false), Some(true));
    assert_eq!(arity_accepts(Language::Python, &two, 3, false), Some(false));
    assert_eq!(arity_accepts(Language::Python, &two, 1, false), Some(false));
    let defaulted = [p("a"), with_default("b")];
    assert_eq!(arity_accepts(Language::Python, &defaulted, 1, false), Some(true));
    assert_eq!(arity_accepts(Language::Python, &defaulted, 0, false), Some(false));
    assert_eq!(arity_accepts(Language::Java, &two, 2, true), Some(true));
    assert_eq!(arity_accepts(Language::Java, &two, 1, true), Some(false));
    // PHP: missing arguments are an error, extra ones are not.
    assert_eq!(arity_accepts(Language::Php, &two, 1, false), Some(false));
    assert_eq!(arity_accepts(Language::Php, &two, 5, false), Some(true));
    // Keyword-only and `**kw` parameters take no positional argument.
    let kw = [p("a"), of("key", ParamKind::KeywordOnly), of("opts", ParamKind::VarKeyword)];
    assert_eq!(arity_accepts(Language::Python, &kw, 2, false), Some(false));
}

/// Negative: a variadic parameter accepts any number of further arguments.
#[test]
fn rule_arity_variadic_accepts_more_arguments() {
    let variadic = [p("a"), of("rest", ParamKind::VarPositional)];
    for n in 1..6 {
        assert_eq!(arity_accepts(Language::Python, &variadic, n, false), Some(true));
        assert_eq!(arity_accepts(Language::Java, &variadic, n, false), Some(true));
    }
    assert_eq!(arity_accepts(Language::Java, &variadic, 0, false), Some(false));
}

/// Receiver parameters: Python `self` may or may not be bound (either reading accepts),
/// `cls` always is; Rust method-call syntax binds `self`, path calls pass it.
#[test]
fn rule_arity_receiver_parameter_per_language() {
    let method = [p("self"), p("a")];
    assert_eq!(arity_accepts(Language::Python, &method, 1, true), Some(true));
    assert_eq!(arity_accepts(Language::Python, &method, 2, true), Some(true), "Class.m(obj, a)");
    assert_eq!(arity_accepts(Language::Python, &method, 3, true), Some(false));
    let classmethod = [p("cls"), p("a")];
    assert_eq!(arity_accepts(Language::Python, &classmethod, 2, true), Some(false));
    let rust = [p("self"), p("a")];
    assert_eq!(arity_accepts(Language::Rust, &rust, 1, true), Some(true));
    assert_eq!(arity_accepts(Language::Rust, &rust, 2, true), Some(false));
    assert_eq!(arity_accepts(Language::Rust, &rust, 2, false), Some(true), "Type::m(x, a)");
    assert_eq!(arity_accepts(Language::Rust, &[p("a")], 1, true), None);
}

/// Negative: languages whose calls may pass any count, or whose parameter lists do not
/// mark optional / variadic parameters, never decide.
#[test]
fn rule_arity_is_undecided_where_the_language_accepts_any_count() {
    let one = [p("a")];
    for language in [
        Language::JavaScript,
        Language::TypeScript,
        Language::Go,
        Language::C,
        Language::Cpp,
        Language::CSharp,
        Language::Haskell,
    ] {
        assert_eq!(arity_accepts(language, &one, 5, false), None, "{language:?}");
    }
}
