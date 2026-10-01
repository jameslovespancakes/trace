//! Name occurrences: every syntax occurrence of the family's names in indexed files (calls,
//! references, import bindings, re-exports, declarations).

use std::collections::{BTreeSet, HashSet};

use trace_core::facts::Scope;
use trace_core::model::{FileId, SymbolId};
use trace_core::Index;

use super::{
    access::call_access, access::member_span, access::reference_access, family::stores_into_value, Access,
    Occurrence, State, OTHER_DECLARATION,
};
use crate::cards::ref_use_kind;

/// Every syntax occurrence of `names` in files with facts (states unset).
pub(super) fn collect(
    index: &Index,
    names: &BTreeSet<String>,
    family: &HashSet<SymbolId>,
) -> Vec<Occurrence> {
    let has = |n: &str| names.contains(n);
    let mut out = Vec::new();
    for (fi, rec) in index.files.iter().enumerate() {
        let Some(facts) = &rec.facts else { continue };
        let file = FileId(fi as u32);
        let sym = |decl: Option<u32>| decl.and_then(|d| rec.symbol_of_decl(d));
        let start = out.len();
        for (ci, c) in facts.calls.iter().enumerate() {
            let Some(member) = c.member.as_deref().filter(|m| has(m)) else {
                continue;
            };
            let span = member_span(c);
            out.push(Occurrence {
                file,
                span,
                line: c.line,
                kind: "call",
                owner: sym(facts.executing_owner(c.owner)),
                state: State::Unresolved(""),
                name: member.to_string(),
                access: call_access(facts, c, span),
                local: facts.is_local(span),
                statement: false,
                call: Some(ci as u32),
            });
        }
        for r in &facts.references {
            if !has(r.name.as_str()) {
                continue;
            }
            out.push(Occurrence {
                file,
                span: r.span,
                line: 0,
                kind: ref_use_kind(r.kind),
                owner: sym(facts.executing_owner(r.owner)),
                state: State::Unresolved(""),
                name: r.name.clone(),
                access: reference_access(facts, r.span),
                local: r.local || facts.is_local(r.span),
                statement: false,
                call: None,
            });
        }
        for (d, decl) in facts.declarations.iter().enumerate() {
            if !has(decl.name.as_str()) {
                continue;
            }
            let id = rec.symbol_of_decl(d as u32);
            // `obj.m = function ...` inside a function (JS): a store into a field of a
            // local value, which may be the target's method slot — a write, not a separate
            // declared entity.
            let field_store = id.is_some_and(|i| !family.contains(&i) && stores_into_value(index, i));
            let state = match id {
                Some(id) if family.contains(&id) => State::Target,
                _ if field_store => State::Unresolved(""),
                _ => State::Elsewhere(OTHER_DECLARATION),
            };
            out.push(Occurrence {
                file,
                span: decl.name_span,
                line: 0,
                kind: if field_store { "write" } else { "declaration" },
                owner: id,
                state,
                name: decl.name.clone(),
                access: Access::Unknown,
                local: false,
                statement: false,
                call: None,
            });
        }
        let last = |path: &str| -> String { path.rsplit(['.', '/', ':']).next().unwrap_or(path).to_string() };
        for imp in &facts.imports {
            let name = if has(imp.local.as_str()) {
                imp.local.clone()
            } else {
                let l = last(&imp.target);
                if !has(l.as_str()) {
                    continue;
                }
                l
            };
            let owner = match imp.scope {
                Scope::Decl(d) => sym(Some(d)),
                Scope::Module => sym(facts.module_decl),
            };
            out.push(Occurrence {
                file,
                span: imp.span,
                line: imp.line,
                kind: "import",
                owner,
                state: State::Unresolved(""),
                name,
                access: Access::Unknown,
                local: false,
                statement: true,
                call: None,
            });
        }
        for e in &facts.exports {
            let name = if has(e.exported.as_str()) {
                e.exported.clone()
            } else {
                let l = last(&e.target);
                if !has(l.as_str()) {
                    continue;
                }
                l
            };
            out.push(Occurrence {
                file,
                span: e.span,
                line: e.line,
                kind: "reexport",
                owner: sym(facts.module_decl),
                state: State::Unresolved(""),
                name,
                access: Access::Unknown,
                local: false,
                statement: true,
                call: None,
            });
        }
        // Statement facts enclosing an identifier occurrence of this file are duplicates.
        let (idents, statements): (Vec<Occurrence>, Vec<Occurrence>) =
            out.drain(start..).partition(|o| !o.statement);
        let kept: Vec<Occurrence> = statements
            .into_iter()
            .filter(|s| !idents.iter().any(|i| s.span.encloses(i.span) && s.span != i.span))
            .collect();
        out.extend(idents);
        out.extend(kept);
    }
    out
}
