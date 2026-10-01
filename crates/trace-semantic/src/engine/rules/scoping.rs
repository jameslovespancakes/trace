//! Lexical visibility of partition declarations for the by-name rule (engine module docs,
//! syntax answer rule 3; SPEC section 8.8).
//!
//! Language rule: in Haskell (`where` / `let` bindings) and Scala (local `def`s) a function
//! declared directly inside a function body is in scope only inside that body. A bare call
//! (no receiver, the callee is the member name) outside the enclosing function can never
//! denote it: the compiler resolves the name to another binding (an import, a local
//! variable). So when every partition declaration of a bare call's name is such a nested
//! function out of scope at the call, no repository declaration can be the target and the
//! call is answered like a name no declaration carries (`external_by_name`: external without
//! candidates, never an edge).
//!
//! Everything else stays visible (conservative): top-level declarations, members of types
//! (classes, objects, instances, type classes, records), functions nested in non-callable
//! declarations, declarations of other languages, member accesses and calls through a
//! receiver (a returned local function can be reached by value, never by its name outside).

use trace_core::facts::CallSite;
use trace_core::Language;

use crate::mapping::{DeclRef, DeclTable};

/// Whether `language` scopes a function declared inside a function body to that body for bare
/// names (module docs).
pub(crate) fn nested_functions_are_local(language: Language) -> bool {
    trace_syntax::language_rules::rules(language).nested_functions_are_local
}

/// Whether declaration `d` of the partition can be denoted by the bare call `c` of `path`
/// (module docs).
pub(crate) fn visible_to_bare_call(decls: &DeclTable<'_>, d: DeclRef<'_>, path: &str, c: &CallSite) -> bool {
    let Some(facts) = decls.facts(d.path) else { return true };
    if !facts.language.is_some_and(nested_functions_are_local) {
        return true;
    }
    let Some(decl) = decls.get(d) else { return true };
    if !decl.kind.is_callable() {
        return true;
    }
    let Some(parent) = decl.parent.and_then(|p| facts.declarations.get(p as usize)) else {
        return true;
    };
    if !parent.kind.is_callable() {
        return true;
    }
    // A nested function: visible only inside its enclosing function.
    decls.path_key(path) == Some(d.path) && parent.span.bytes.contains(c.callee_span.start)
}

/// Whether `c` is a bare call (no receiver, the callee is the member name).
pub(crate) fn is_bare_call(c: &CallSite) -> bool {
    c.receiver.is_none() && c.member.as_deref().is_some_and(|m| c.callee.trim() == m)
}

/// Whether no partition declaration of the call's member name is visible to the bare call
/// `c` of `path` (module docs); false for other calls and names without declarations (those
/// are rule 3 already).
pub(crate) fn only_out_of_scope_declarations(decls: &DeclTable<'_>, path: &str, c: &CallSite) -> bool {
    let Some(m) = c.member.as_deref() else { return false };
    if !is_bare_call(c) {
        return false;
    }
    let named = decls.named(m);
    !named.is_empty() && !named.iter().any(|d| visible_to_bare_call(decls, *d, path, c))
}

#[cfg(test)]
#[path = "../../../tests/unit/engine/rules/scoping.rs"]
mod tests;
