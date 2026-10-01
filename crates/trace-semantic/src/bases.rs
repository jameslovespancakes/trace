//! Library bases (rule "member of a library base"): the base of a repository type that the
//! server locates outside the index is a library class. The engine asks `definition` at the
//! base's name identifier (also when no indexed declaration carries that name, unlike other
//! references) and records the library declaration in `FileSemantics::library_bases`; value
//! flow then looks members the repository hierarchy does not declare up in that class.
//!
//! Only headers of type declarations that no callable encloses are asked: they are outside
//! every reuse unit (outermost named callables), so an incremental run asks exactly what a
//! full run asks.

use trace_core::facts::{FileFacts, Reference};

/// The spelling of a base without generic arguments, reduced to its last name segment
/// (`pkg.Base[T]` -> `Base`, `Base<T>` -> `Base`); `None` for computed bases (`make_base()`).
fn base_leaf(spelling: &str) -> Option<&str> {
    if spelling.contains('(') {
        return None;
    }
    let head = spelling.split(['[', '<']).next().unwrap_or_default().trim();
    let leaf = head.rsplit(['.', ':']).next().unwrap_or_default().trim();
    (!leaf.is_empty()).then_some(leaf)
}

/// Whether `r` is the name identifier of a base of a type declaration that no callable
/// encloses: it lies in the declaration's header (between its name and its body) and is the
/// last name segment of one of its base spellings.
pub(crate) fn is_header_base(facts: &FileFacts, r: &Reference) -> bool {
    if r.local || facts.is_local(r.span) {
        return false;
    }
    facts.declarations.iter().enumerate().any(|(i, d)| {
        d.kind.is_type()
            && facts.module_decl != Some(i as u32)
            && d.name_span.end <= r.span.start
            && r.span.end <= d.body_start
            && d.bases.iter().any(|b| base_leaf(b) == Some(r.name.as_str()))
            && !encloses_callable(facts, d.parent)
    })
}

/// Whether a callable is `parent` or one of its ancestors.
fn encloses_callable(facts: &FileFacts, mut parent: Option<u32>) -> bool {
    let mut steps = 0;
    while let Some(p) = parent {
        let Some(d) = facts.declarations.get(p as usize) else { return false };
        if d.kind.is_callable() && facts.module_decl != Some(p) {
            return true;
        }
        parent = d.parent;
        steps += 1;
        if steps > facts.declarations.len() {
            return false;
        }
    }
    false
}

#[cfg(test)]
#[path = "../tests/unit/bases.rs"]
mod tests;
