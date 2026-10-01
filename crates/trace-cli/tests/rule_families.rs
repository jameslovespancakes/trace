//! Rule e2e: complete families (SPEC 7.10 "Complete families", 8.5a, 6.3 declarations).
//!
//! * trait implementations across crates (header compiler facts, server implementations)
//!   and calls through `(**self)` reaching the trait method;
//! * protocol conformance declared in an extension, requirement implemented in the class
//!   body;
//! * calls on subclass instances, decorator calls on pytest-fixture instances and
//!   `super().m()` reaching the base member;
//! * declarations that are not definitions (C / C++ header prototypes across namespaces,
//!   Haskell type signatures, TypeScript overload signatures) as `declaration` rows.
//!
//! Every test skips when a language server, toolchain or dependency of its fixture is
//! missing on this machine (`index_or_skip`; there is no syntax-only mode). Every report is
//! checked for contradictions (rule 1).

mod support;

use serde_json::Value;
use support::{assert_no_contradictions, check_at, rows_at, semantic_or_skip, use_row, Fixture};

/// Uids of a list of cards.
fn ids(cards: &Value) -> Vec<&str> {
    cards
        .as_array()
        .map(|a| a.iter().filter_map(|c| c["id"].as_str()).collect())
        .unwrap_or_default()
}

/// `impl Sink for X` in crate `b` (and `b/src/testutil.rs`) implements crate `a`'s trait;
/// the forwarding `impl<S: Sink + ?Sized> Sink for &mut S` is linked by the same-file rule
/// and its `(**self).matched(line)` is a use of the family.
#[test]
fn rule_trait_impls_across_crates_join_the_family() {
    let fx = Fixture::new("rule-family-trait-crates", "rule-family-trait-crates");
    let Some(index) = fx.index_or_skip() else { return };
    let semantic = semantic_or_skip(&index, "rust");

    let uses = fx.json(&["uses", "a/src/lib.rs:Sink.matched"]);
    assert_no_contradictions(&uses);
    let family = ids(&uses["family"]);
    // Syntax rule: the forwarding impl in the trait's own file.
    assert!(family.contains(&"a/src/lib.rs:S.matched"), "{uses}");
    // `(**self).matched(line)`: the receiver's bound is the trait (inferred), or the server
    // proves it; either way a use row, never an unresolved entry.
    let deref = rows_at(&uses, "a/src/lib.rs", 17);
    assert!(deref.iter().any(|r| r["kind"] == "call"), "{uses}");
    assert!(check_at(&uses, "a/src/lib.rs", 17).is_empty(), "{uses}");

    if semantic {
        for member in [
            "b/src/lib.rs:JsonSink.matched",
            "b/src/lib.rs:Summary.matched",
            "b/src/testutil.rs:KitchenSink.matched",
        ] {
            assert!(family.contains(&member), "{member}: {uses}");
        }
        assert!(use_row(&uses, "b/src/lib.rs", 13, "implements").is_some(), "{uses}");
        assert!(use_row(&uses, "b/src/testutil.rs", 11, "implements").is_some(), "{uses}");

        let context = fx.json(&["uses", "a/src/lib.rs:Sink.context"]);
        assert_no_contradictions(&context);
        let family = ids(&context["family"]);
        assert!(family.contains(&"b/src/lib.rs:Summary.context"), "{context}");
        assert!(family.contains(&"a/src/lib.rs:S.context"), "{context}");
        assert!(!family.contains(&"b/src/lib.rs:JsonSink.matched"), "{context}");
    }
}

/// Python: `App(Base)` inherits `get`; the pytest fixture `app` returns `App()`. The
/// decorator call `@app.get("/")` and the call `app.get("/items")` in tests, and
/// `super().get(rule)` in the override `LoggingApp.get`, are uses of `Base.get`'s family.
#[test]
fn rule_subclass_and_decorator_receivers_reach_the_family() {
    let fx = Fixture::new("rule-family-subclass-receivers", "rule-family-subclass-receivers");
    let Some(index) = fx.index_or_skip() else { return };
    let semantic = semantic_or_skip(&index, "python");

    let uses = fx.json(&["uses", "app/base.py:Base.get"]);
    assert_no_contradictions(&uses);
    // Syntax rules: the override joins the family; `super().get(rule)` reaches the base.
    assert!(ids(&uses["family"]).contains(&"app/base.py:LoggingApp.get"), "{uses}");
    assert!(use_row(&uses, "app/base.py", 29, "override").is_some(), "{uses}");
    assert!(use_row(&uses, "app/base.py", 31, "call").is_some(), "super().get: {uses}");

    if semantic {
        // Value flow through the fixture instance (needs the analyzer's value references).
        let decorator = use_row(&uses, "tests/test_routes.py", 2, "call")
            .unwrap_or_else(|| panic!("decorator call row: {uses}"));
        assert_ne!(decorator["tier"], "possible", "{decorator}");
        assert!(use_row(&uses, "tests/test_routes.py", 10, "call").is_some(), "{uses}");
        for line in [2, 10] {
            assert!(check_at(&uses, "tests/test_routes.py", line).is_empty(), "{uses}");
        }
    }
}

/// Declarations that are not definitions are `declaration` rows of their definition's
/// family: a C header prototype, C++ prototypes with namespaces on both sides (one
/// out-of-line member definition), a Haskell type signature and TypeScript overload
/// signatures.
#[test]
fn rule_declarations_are_family_rows() {
    let fx = Fixture::new("rule-declarations", "rule-declarations");
    let Some(_) = fx.index_or_skip() else { return };

    // C (declaration rule): the header prototype joins the definition.
    let area = fx.json(&["uses", "c/geometry.c:area"]);
    assert_no_contradictions(&area);
    assert!(ids(&area["family"]).contains(&"c/geometry.h:area"), "{area}");
    assert!(use_row(&area, "c/geometry.h", 4, "declaration").is_some(), "{area}");

    // C++ (declaration rule): `namespace shapes { class Box { int volume() const; }; }` and the
    // out-of-line `int shapes::Box::volume() const {}`; `shapes::scale` in both files.
    let volume = fx.json(&["uses", "cpp/shapes.hpp:shapes.Box.volume"]);
    assert_no_contradictions(&volume);
    assert!(
        ids(&volume["family"])
            .iter()
            .any(|id| id.starts_with("cpp/shapes.cc:")),
        "{volume}"
    );
    let scale = fx.json(&["uses", "cpp/shapes.cc:shapes.scale"]);
    assert_no_contradictions(&scale);
    assert!(ids(&scale["family"]).contains(&"cpp/shapes.hpp:shapes.scale"), "{scale}");
    assert!(use_row(&scale, "cpp/shapes.hpp", 10, "declaration").is_some(), "{scale}");

    // Haskell: `matches :: String -> String -> Bool` (a stub declaration) is a declaration
    // row of the binding `matches pat s = ...`.
    let matches = fx.json(&["uses", "hs/Match.hs:matches"]);
    assert_no_contradictions(&matches);
    assert!(use_row(&matches, "hs/Match.hs", 4, "declaration").is_some(), "{matches}");
    assert!(use_row(&matches, "hs/Match.hs", 5, "declaration").is_some(), "{matches}");

    // TypeScript: both overload signatures are declaration rows of `parse`.
    let parse = fx.json(&["uses", "ts/parse.ts:parse"]);
    assert_no_contradictions(&parse);
    for line in [1, 2] {
        assert!(use_row(&parse, "ts/parse.ts", line, "declaration").is_some(), "{parse}");
    }
}
