//! Rule "lexical scoping of local reads" (value flow side; child of [`crate::flow`]).
//!
//! Python: a bare-name read that the language's scoping proves to denote a local variable or
//! parameter binding (`FileFacts::local_spans`: function-scope rule, `global` / `nonlocal`
//! aware) reads the nearest enclosing function scope that binds the name - its own scope,
//! else the enclosing functions outward (named nested functions capture like lambdas) - and
//! never the module-level variable of the same name (`def app(): app = App(); return app`
//! returns the instance, not the module-level function `app`).

use std::collections::HashSet;

use trace_core::{ByteSpan, FileId, Index, SymbolId};
use trace_syntax::language_rules::rules;

use super::{Ev, Name, Sc, ScopeKey, Slot, Vals};

/// Python reads `FileFacts::local_spans` proves local, by (file, span).
pub(super) fn local_reads(index: &Index) -> HashSet<(FileId, ByteSpan)> {
    index
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| rules(f.language).lexical_local_reads)
        .filter_map(|(i, f)| f.facts.as_ref().map(|facts| (FileId(i as u32), facts)))
        .flat_map(|(file, facts)| facts.local_spans.iter().map(move |s| (file, *s)))
        .collect()
}

impl Ev<'_, '_> {
    /// Whether callable `f` binds `name` itself (parameter, receiver or assignment).
    fn binds(&self, f: SymbolId, name: Name) -> bool {
        self.f.params[f.idx()].contains(&name)
            || self.f.locals.contains(&(f, name))
            || self.f.selfs.get(&f).is_some_and(|sp| sp.name == name)
    }

    /// Values of a proven-local read of `name` in `sc` (module doc).
    pub(super) fn local_values(&self, name: Name, sc: Sc) -> Vals {
        let mut out = Vals::new();
        let ScopeKey::Symbol(f) = sc.scope else {
            return out;
        };
        if self.f.selfs.get(&f).is_some_and(|sp| sp.name == name) {
            return self.recv_values(f, sc.ctx);
        }
        for slot in [Slot::Var(sc.scope, sc.ctx, name), Slot::Default(f, name)] {
            if let Some(v) = self.slot(slot) {
                out.extend(v.iter());
            }
        }
        if self.binds(f, name) {
            return out;
        }
        let mut scope = f;
        for _ in 0..=self.f.index.symbols.len() {
            let Some(parent) = self.f.index.symbol(scope).parent else {
                break;
            };
            if !self.f.index.symbol(parent).kind.is_callable() {
                break;
            }
            if self.f.selfs.get(&parent).is_some_and(|sp| sp.name == name) {
                for ctx in self.contexts_of(ScopeKey::Symbol(parent)) {
                    out.extend(self.recv_values(parent, ctx));
                }
            }
            for slot in [Slot::VarAll(ScopeKey::Symbol(parent), name), Slot::Default(parent, name)] {
                if let Some(v) = self.slot(slot) {
                    out.extend(v.iter());
                }
            }
            if self.binds(parent, name) {
                break;
            }
            scope = parent;
        }
        out
    }
}

#[cfg(test)]
#[path = "../../tests/unit/flow/scoping.rs"]
mod tests;
