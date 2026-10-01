//! Rule tests of [`crate::engine::rules::scoping`] over real syntax facts.

use trace_core::facts::FileFacts;
use trace_core::Language;

use super::*;

fn facts(path: &str, language: Language, source: &str) -> FileFacts {
    trace_syntax::extract(trace_syntax::SourceInput {
        path,
        language,
        source: source.as_bytes(),
    })
    .expect("fixture parses")
}

/// The bare calls of `name` in `facts`, in source order.
fn calls_of<'f>(facts: &'f FileFacts, name: &str) -> Vec<&'f CallSite> {
    facts
        .calls
        .iter()
        .filter(|c| c.member.as_deref() == Some(name) && is_bare_call(c))
        .collect()
}

/// Haskell: a `where` binding is in scope only inside its equation. A bare call of the same
/// name in another function can never denote it (only out-of-scope declarations); the call
/// inside the equation can (visible); a top-level declaration of the name elsewhere is
/// always visible.
#[test]
fn rule_where_binding_out_of_scope_is_not_a_bare_call_target() {
    let checks = "module Checks where\n\
                  \n\
                  getChecker :: [Int] -> [Int]\n\
                  getChecker xs = map xs\n\
                  \x20 where\n\
                  \x20   map ys = reverse ys\n\
                  \n\
                  render :: [Int] -> [String]\n\
                  render xs = map show xs\n";
    let checks_facts = facts("src/Checks.hs", Language::Haskell, checks);
    let other = "module Other where\n\nrun :: [Int] -> [Int]\nrun xs = map id xs\n";
    let other_facts = facts("src/Other.hs", Language::Haskell, other);
    let decls = DeclTable::new([
        ("src/Checks.hs", checks.as_bytes(), &checks_facts),
        ("src/Other.hs", other.as_bytes(), &other_facts),
    ]);
    let local = calls_of(&checks_facts, "map");
    assert_eq!(local.len(), 2, "{:?}", checks_facts.calls);
    assert!(!only_out_of_scope_declarations(&decls, "src/Checks.hs", local[0]), "inside the equation");
    assert!(only_out_of_scope_declarations(&decls, "src/Checks.hs", local[1]), "another function");
    let remote = calls_of(&other_facts, "map");
    assert!(only_out_of_scope_declarations(&decls, "src/Other.hs", remote[0]), "another module");

    // A top-level `map` anywhere in the partition is visible: the rule never fires.
    let prelude = "module Mine where\n\nmap :: Int -> Int\nmap x = x\n";
    let prelude_facts = facts("src/Mine.hs", Language::Haskell, prelude);
    let decls = DeclTable::new([
        ("src/Checks.hs", checks.as_bytes(), &checks_facts),
        ("src/Other.hs", other.as_bytes(), &other_facts),
        ("src/Mine.hs", prelude.as_bytes(), &prelude_facts),
    ]);
    assert!(!only_out_of_scope_declarations(&decls, "src/Other.hs", remote[0]));
}

/// Scala: a local `def` is in scope only inside its enclosing method; a method of an object
/// or class stays visible to every bare call (members are reached by import or inheritance).
#[test]
fn rule_local_def_out_of_scope_is_not_a_bare_call_target() {
    let src = "object A {\n\
               \x20 def outer(x: Int): Int = {\n\
               \x20   def step(y: Int): Int = y + 1\n\
               \x20   step(x)\n\
               \x20 }\n\
               \x20 def other(x: Int): Int = step(x)\n\
               \x20 def member(x: Int): Int = x\n\
               \x20 def user(x: Int): Int = member(x)\n\
               }\n";
    let f = facts("src/A.scala", Language::Scala, src);
    let decls = DeclTable::new([("src/A.scala", src.as_bytes(), &f)]);
    let step = calls_of(&f, "step");
    assert_eq!(step.len(), 2, "{:?}", f.calls);
    assert!(!only_out_of_scope_declarations(&decls, "src/A.scala", step[0]));
    assert!(only_out_of_scope_declarations(&decls, "src/A.scala", step[1]));
    let member = calls_of(&f, "member");
    assert!(!only_out_of_scope_declarations(&decls, "src/A.scala", member[0]));
}

/// Languages whose nested functions can be rebound outside their body (Python `global`, R
/// `<<-`) never use the rule.
#[test]
fn rule_nested_function_scoping_only_for_lexically_local_languages() {
    let src = "def outer():\n    def step():\n        return 1\n    return step()\n\ndef other():\n    return step()\n";
    let f = facts("a.py", Language::Python, src);
    let decls = DeclTable::new([("a.py", src.as_bytes(), &f)]);
    let step = calls_of(&f, "step");
    assert_eq!(step.len(), 2);
    assert!(!only_out_of_scope_declarations(&decls, "a.py", step[1]));
    assert!(!nested_functions_are_local(Language::R));
}
