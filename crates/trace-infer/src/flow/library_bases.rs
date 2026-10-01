//! Rule "member of a library base" (value flow side; child of [`crate::flow`]).
//!
//! A repository class whose base the server located outside the index inherits that library
//! class's members (`LibraryKnowledge::classes`, read from the library source by
//! trace-library). A member call `recv.m(..)` without a proven target, whose receiver
//! evaluates (strictly, every receiver context) only to instances / classes of repository
//! classes on which member lookup finds nothing in repository code (no method along the
//! repository MRO, no stored attribute, no delegate) while a library base of that MRO
//! declares `m`, runs the library's `m`: a library receiver (`Index::library_receivers`)
//! whose evidence is the library-qualified member (`pkg.mod.Client.get`).
//!
//! Python data model: a repository class along the MRO declaring `__getattribute__`
//! intercepts every access, so the rule never applies to it. The call's result is not
//! modelled as a library object: the receiver is a repository object that the library code
//! may hand back.

use std::collections::HashMap;

use trace_core::{Index, SymbolId};
use trace_library::library_class::LibraryClass;
use trace_library::LibraryKnowledge;
use trace_syntax::language_rules::rules;

use super::Flow;

/// Library bases per repository type, in base order: a class of
/// `LibraryKnowledge::classes` belongs to the type declaration whose header (declaration
/// start up to the body) contains the base's name.
pub(super) fn library_bases(
    index: &Index,
    knowledge: &LibraryKnowledge,
) -> HashMap<SymbolId, Vec<LibraryClass>> {
    let mut out: HashMap<SymbolId, Vec<LibraryClass>> = HashMap::new();
    for ((path, start), class) in &knowledge.classes {
        let Some(file) = index.file_by_path(path) else { continue };
        let Some(facts) = index.file(file).facts.as_ref() else { continue };
        let owner = index.symbols_of(file).iter().find(|s| {
            s.kind.is_type()
                && !s.is_synthetic()
                && facts
                    .declarations
                    .get(s.decl as usize)
                    .is_some_and(|d| d.span.bytes.start <= *start && *start < d.body_start)
        });
        if let Some(owner) = owner {
            out.entry(owner.id).or_default().push(class.clone());
        }
    }
    out
}

impl Flow<'_> {
    /// The library member that lookup of `attr` on `class` finds when the repository MRO
    /// declares no member of that name (callers check stored attributes): the first library
    /// base along the MRO declaring it.
    pub(super) fn library_member(&self, class: SymbolId, attr: &str) -> Option<&str> {
        let mro = self.mro(class);
        if mro.iter().any(|&k| self.hierarchy.method(k, attr).is_some()) {
            return None;
        }
        let hook = rules(self.index.symbol(class).language).attribute_hook;
        if hook.is_some_and(|hook| mro.iter().any(|&k| self.hierarchy.method(k, hook).is_some())) {
            return None;
        }
        mro.iter().find_map(|k| {
            self.library_bases
                .get(k)?
                .iter()
                .find_map(|c| c.members.get(attr).map(String::as_str))
        })
    }
}

#[cfg(test)]
#[path = "../../tests/unit/flow/library_bases.rs"]
mod tests;
