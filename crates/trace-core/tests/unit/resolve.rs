use super::*;
use crate::model::ByteSpan;
use crate::test_support::index::index_with;
use crate::Graph;

#[test]
fn exact_file_and_bare_forms() {
    let index = index_with(&["Session", "Session.login", "helper", "Other.login"], &[]);
    let g = Graph::new(&index);
    assert_eq!(g.resolve("a.py:Session.login").unwrap(), SymbolId(1));
    assert_eq!(g.resolve("./a.py:Session.login").unwrap(), SymbolId(1));
    assert_eq!(g.resolve(".\\a.py:Session.login").unwrap(), SymbolId(1));
    assert_eq!(g.resolve("Session.login").unwrap(), SymbolId(1));
    assert_eq!(g.resolve("Session::login").unwrap(), SymbolId(1));
    assert_eq!(g.resolve("a.py:Session::login").unwrap(), SymbolId(1));
    assert_eq!(g.resolve("helper").unwrap(), SymbolId(2));
    assert_eq!(g.resolve("helper()").unwrap(), SymbolId(2));
    assert_eq!(g.resolve("a.py:helper").unwrap(), SymbolId(2));
    // Line 3 is `helper` (one symbol per line in the fixture).
    assert_eq!(g.resolve("a.py:3").unwrap(), SymbolId(2));
    // Module-qualified spelling (`a.py` is module `a`).
    assert_eq!(g.resolve("a.Session.login").unwrap(), SymbolId(1));
    assert!(matches!(g.resolve("missing"), Err(CoreError::SymbolNotFound(_))));
    assert!(matches!(g.resolve("b.py:helper"), Err(CoreError::SymbolNotFound(_))));
    assert!(matches!(g.resolve("  "), Err(CoreError::SymbolNotFound(_))));
}

#[test]
fn ambiguity_lists_candidates_and_never_guesses() {
    let index = index_with(&["Session.login", "Other.login"], &[]);
    let g = Graph::new(&index);
    match g.resolve("login") {
        Err(CoreError::AmbiguousSymbol {
            reference,
            candidates,
        }) => {
            assert_eq!(reference, "login");
            assert_eq!(candidates, vec!["a.py:Other.login", "a.py:Session.login"]);
        }
        other => panic!("expected ambiguity, got {other:?}"),
    }
    assert!(matches!(g.resolve("a.py:login"), Err(CoreError::AmbiguousSymbol { .. })));
}

#[test]
fn generic_spellings_and_suffixes() {
    let index = index_with(&["Data", "Data.from_bytes", "Outer.Walk.current_dir"], &[]);
    let g = Graph::new(&index);
    assert_eq!(g.resolve("Data<'a>::from_bytes").unwrap(), SymbolId(1));
    assert_eq!(g.resolve("a.py:Data<'a>::from_bytes").unwrap(), SymbolId(1));
    assert_eq!(g.resolve("Data<T, Vec<u8>>.from_bytes()").unwrap(), SymbolId(1));
    // Suffix of a longer qualified name.
    assert_eq!(g.resolve("Walk.current_dir").unwrap(), SymbolId(2));
    assert_eq!(g.resolve("Walk::current_dir").unwrap(), SymbolId(2));
    assert_eq!(normalize_qualified("Data<'a>::from_bytes"), "Data.from_bytes");
    assert_eq!(normalize_qualified("Picker:set_selection"), "Picker.set_selection");
    assert_eq!(normalize_qualified("<module>"), "<module>");
    assert_eq!(normalize_qualified("f.<lambda>"), "f.<lambda>");
}

#[test]
fn synthetic_symbols_only_by_uid() {
    let mut index = index_with(&["<module>", "f", "f.<lambda>"], &[]);
    // `<module>` spans the whole file (lines 1..=10); `f` is line 2; the lambda line 3.
    index.symbols[0].span.bytes = ByteSpan::new(0, 100);
    index.symbols[0].span.start_line = 1;
    index.symbols[0].span.end_line = 10;
    let g = Graph::new(&index);
    assert!(index.symbols[0].is_synthetic() && index.symbols[2].is_synthetic());
    assert!(matches!(g.resolve("<module>"), Err(CoreError::SymbolNotFound(_))));
    assert!(matches!(g.resolve("<lambda>"), Err(CoreError::SymbolNotFound(_))));
    assert!(matches!(g.resolve("f.<lambda>"), Err(CoreError::SymbolNotFound(_))));
    // Exact uids always work.
    assert_eq!(g.resolve("a.py:<module>").unwrap(), SymbolId(0));
    assert_eq!(g.resolve("a.py:f.<lambda>").unwrap(), SymbolId(2));
    // Lines: named symbols only.
    assert_eq!(g.resolve("a.py:2").unwrap(), SymbolId(1));
    assert!(matches!(g.resolve("a.py:3"), Err(CoreError::NoNamedSymbolAt { .. })));
    assert_eq!(g.resolve("f").unwrap(), SymbolId(1));
    // Traversal start points (`path` / `deps`): the innermost executing scope at a line,
    // synthetic scopes included; outside every symbol there is none.
    assert_eq!(innermost_scope_at(&index, "a.py:3"), Some(SymbolId(2)));
    assert_eq!(innermost_scope_at(&index, "a.py:2"), Some(SymbolId(1)));
    assert_eq!(innermost_scope_at(&index, "a.py:9"), Some(SymbolId(0)), "module-level code");
    assert_eq!(innermost_scope_at(&index, "a.py:99"), None);
    assert_eq!(innermost_scope_at(&index, "f"), None, "not a file:line reference");
}

/// Rule 17: the selector `Type:method`, with and without a file part, and every
/// separator spelling of the same qualified name.
#[test]
fn rule_selector_type_colon_method() {
    let mut index =
        index_with(&["Picker", "Picker.set_selection", "Other.set_selection", "Picker:refresh"], &[]);
    index.files[0].path = "app/pickers.py".into();
    for s in &mut index.symbols {
        s.uid = format!("app/pickers.py:{}", s.qualified_name);
    }
    index.symbols[3].name = "refresh".into();
    let g = Graph::new(&index);
    for selector in [
        "Picker:set_selection",
        "Picker.set_selection",
        "Picker::set_selection",
        "app/pickers.py:Picker:set_selection",
        "app/pickers.py:Picker.set_selection",
        "app\\pickers.py:Picker:set_selection",
    ] {
        assert_eq!(g.resolve(selector).unwrap(), SymbolId(1), "{selector}");
    }
    // A stored `Type:method` spelling matches every separator.
    assert_eq!(g.resolve("Picker.refresh").unwrap(), SymbolId(3));
    assert_eq!(g.resolve("Picker:refresh").unwrap(), SymbolId(3));
    // The bare method name is ambiguous: candidates, never a guess.
    assert!(matches!(g.resolve("set_selection"), Err(CoreError::AmbiguousSymbol { .. })));
}

/// Rule 17: `obj.prop` names a function assigned to a property (`res.redirect =
/// function redirect(url) {...}`): container path or recorded qualified name.
#[test]
fn rule_selector_property_assigned_function() {
    let mut index = index_with(&["redirect", "res.send", "location"], &[]);
    index.files[0].path = "lib/response.js".into();
    for s in &mut index.symbols {
        s.uid = format!("lib/response.js:{}", s.qualified_name);
    }
    index.symbols[0].container = Some("res".into());
    let g = Graph::new(&index);
    assert_eq!(g.resolve("res.redirect").unwrap(), SymbolId(0));
    assert_eq!(g.resolve("lib/response.js:res.redirect").unwrap(), SymbolId(0));
    assert_eq!(g.resolve("res.send").unwrap(), SymbolId(1));
    assert_eq!(g.resolve("lib/response.js:res.send").unwrap(), SymbolId(1));
    assert_eq!(g.resolve("redirect").unwrap(), SymbolId(0));
    // Another receiver is not the property path.
    assert!(matches!(g.resolve("req.redirect"), Err(CoreError::SymbolNotFound(_))));
}

/// Rule 18: `file:line` on a line covered only by module-level code is an error that
/// names the nearest named symbols, never `<module>`.
#[test]
fn rule_selector_file_line_without_named_symbol_is_an_error() {
    let mut index = index_with(&["<module>", "helper", "Config", "Config.load"], &[]);
    index.symbols[0].span.bytes = ByteSpan::new(0, 100);
    index.symbols[0].span.start_line = 1;
    index.symbols[0].span.end_line = 12;
    let g = Graph::new(&index);
    match g.resolve("a.py:9") {
        Err(e @ CoreError::NoNamedSymbolAt { .. }) => {
            assert_eq!(e.kind(), "symbol_not_found");
            let CoreError::NoNamedSymbolAt { reference, nearest } = &e else { unreachable!() };
            assert_eq!(reference, "a.py:9");
            assert_eq!(nearest[0], "Config.load (line 4)");
            assert_eq!(nearest.len(), 3);
            assert!(e.to_string().contains("Nearest: Config.load (line 4)"), "{e}");
        }
        other => panic!("expected an error, got {other:?}"),
    }
    assert_eq!(g.resolve("a.py:2").unwrap(), SymbolId(1));
    // The module is reachable by its exact uid only.
    assert_eq!(g.resolve("a.py:<module>").unwrap(), SymbolId(0));
}

/// Rule 18: an inexact selector is an error; it never falls back to another symbol.
#[test]
fn rule_selector_never_falls_back() {
    let index = index_with(&["<module>", "Session", "Session.login", "helper"], &[]);
    let g = Graph::new(&index);
    for selector in [
        "Session.logout",
        "src/a.py:Session.login",
        "b.py:Session.login",
        "a.py:Session.logout",
        "helper.extra",
        "<module>",
        "Other:login",
    ] {
        assert!(matches!(g.resolve(selector), Err(CoreError::SymbolNotFound(_))), "{selector}");
    }
    assert_eq!(g.resolve("Session:login").unwrap(), SymbolId(2));
    assert!(looks_like_path("pkg/mod"));
    assert!(looks_like_path("mod.py"));
    assert!(!looks_like_path("Picker"));
    assert!(!looks_like_path("Picker.set_selection"));
}
