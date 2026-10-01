//! Scala application rules over the in-index targets the server reports for ONE call
//! (language rules; engine answer mapping, applied before the overload rule).
//!
//! Metals answers an application with the method that runs and the declarations the callee
//! names on the way (the object or type of the applied value). Both are in-index targets of
//! the same call, so the call looked ambiguous although the language fixes what runs:
//!
//! 1. **`apply` sugar**: `x(args)` is `x.apply(args)`. An answer made only of `apply`
//!    methods and type declarations, where every type is the enclosing type of one of the
//!    `apply` methods or is named by the callee (its member or receiver: the companion
//!    object `DecodingFailure(...)`, the qualifier of `Encoder.encodeString(v)`), keeps the
//!    `apply` methods (`self(c)` -> `Decoder#apply`, `JawnParser(n)` -> `JawnParser.apply`).
//! 2. **Case class / creator application**: `C(args)` answered by exactly a class `C` and
//!    its companion `object C` (same name, same file, same enclosing declaration) that
//!    neither declares nor inherits an `apply` (no own `apply`, no `extends` clause), so only
//!    the synthetic companion `apply` of a case class or a Scala 3 creator application runs:
//!    the class is the target (a constructor edge).
//!
//! Both rules run on call hierarchy and definition answers. Anything else is left to the
//! other rules (ambiguous stays ambiguous). The object tests read the syntax tree of the
//! declaring file (`object_definition` and its `extend` field), never source text.

use trace_core::facts::CallSite;
use trace_core::{Language, SymbolKind};

use crate::mapping::{DeclRef, DeclTable};

/// Name of the method Scala calls when a value is applied to arguments.
const APPLY: &str = "apply";

/// The targets of `c` after the application rules (module docs); unchanged when no rule
/// applies.
pub(crate) fn narrow_application<'a>(
    targets: Vec<DeclRef<'a>>,
    c: &CallSite,
    language: Option<Language>,
    decls: &DeclTable<'a>,
) -> Vec<DeclRef<'a>> {
    if language != Some(Language::Scala) || targets.len() < 2 {
        return targets;
    }
    if let Some(applies) = apply_sugar(&targets, c, decls) {
        return applies;
    }
    if let Some(class) = companion_construction(&targets, decls) {
        return vec![class];
    }
    targets
}

/// Rule 1 (module docs): the `apply` methods of the answer.
fn apply_sugar<'a>(targets: &[DeclRef<'a>], c: &CallSite, decls: &DeclTable<'a>) -> Option<Vec<DeclRef<'a>>> {
    let (applies, others): (Vec<DeclRef<'a>>, Vec<DeclRef<'a>>) = targets
        .iter()
        .copied()
        .partition(|t| decls.get(*t).is_some_and(|d| d.kind.is_callable() && d.name == APPLY));
    if applies.is_empty() || others.is_empty() {
        return None;
    }
    let enclosing = |t: DeclRef<'a>| -> Option<DeclRef<'a>> {
        let parent = decls.get(t)?.parent?;
        Some(DeclRef {
            path: t.path,
            decl: parent,
        })
    };
    let parents: Vec<DeclRef<'a>> = applies.iter().filter_map(|a| enclosing(*a)).collect();
    let named_by_callee =
        |name: &str| c.member.as_deref() == Some(name) || c.receiver.as_deref() == Some(name);
    let all_explained = others.iter().all(|o| {
        decls
            .get(*o)
            .is_some_and(|d| d.kind.is_type() && (parents.contains(o) || named_by_callee(d.name.as_str())))
    });
    all_explained.then_some(applies)
}

/// Rule 2 (module docs): the class of a class / companion object pair.
fn companion_construction<'a>(targets: &[DeclRef<'a>], decls: &DeclTable<'a>) -> Option<DeclRef<'a>> {
    let [a, b] = targets else { return None };
    let (da, db) = (decls.get(*a)?, decls.get(*b)?);
    let pair = a.path == b.path
        && da.kind == SymbolKind::Class
        && db.kind == SymbolKind::Class
        && da.name == db.name
        && da.parent == db.parent;
    if !pair {
        return None;
    }
    let source = decls.source(a.path)?;
    let tree = trace_syntax::parse_tree(Language::Scala, source).ok()?;
    let root = tree.root_node();
    let definition = |d: &trace_core::facts::Declaration| {
        root.descendant_for_byte_range(d.name_span.start as usize, d.name_span.end as usize)?
            .parent()
    };
    let (na, nb) = (definition(da)?, definition(db)?);
    let (class, object, object_node) = match (na.kind(), nb.kind()) {
        ("class_definition", "object_definition") => (*a, *b, nb),
        ("object_definition", "class_definition") => (*b, *a, na),
        _ => return None,
    };
    // A companion that inherits (`extends` / `with`) may inherit an `apply`: not this rule.
    if object_node.child_by_field_name("extend").is_some() {
        return None;
    }
    // A companion declaring its own `apply` may construct anything: not this rule.
    let own_apply = decls
        .named(APPLY)
        .iter()
        .any(|m| m.path == object.path && decls.get(*m).is_some_and(|d| d.parent == Some(object.decl)));
    (!own_apply).then_some(class)
}

#[cfg(test)]
#[path = "../../../tests/unit/engine/rules/scala_apply.rs"]
mod tests;
