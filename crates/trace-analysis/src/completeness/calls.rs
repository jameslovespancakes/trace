//! Call completeness of `deps`: calls inside the reached set not covered by a traversable
//! edge.

use std::collections::{BTreeSet, HashMap, HashSet};

use trace_core::model::{ByteSpan, FileId, SymbolId, UnresolvedKind};
use trace_core::source::SourceStore;
use trace_core::tiers::{FAMILY, USES};
use trace_core::{Graph, Tier};

use super::{summary::summarize, Access, Occurrence, State, SERVER};
use crate::report::Completeness;

/// Call completeness of `deps` (module docs).
pub(crate) fn calls_completeness(
    graph: &Graph<'_>,
    sources: &SourceStore<'_>,
    reached: &HashSet<SymbolId>,
    include: Tier,
    bounded: Option<String>,
) -> Completeness {
    let index = graph.index;
    let threshold = include.min(Tier::Inferred);
    let mut covered: HashSet<(u32, u32)> = HashSet::new();
    for &id in reached {
        for (_, e) in graph.outgoing_all(id) {
            if e.tier <= threshold && !USES.contains(e.kind) && !FAMILY.contains(e.kind) {
                covered.insert((e.at.file.0, e.at.bytes.end));
            }
        }
    }
    let files: BTreeSet<FileId> = reached.iter().map(|&id| index.symbol(id).file).collect();
    let mut occ: Vec<Occurrence> = Vec::new();
    let mut at_site: HashMap<(u32, u32), usize> = HashMap::new();
    let occurrence =
        |file: FileId, span: ByteSpan, line: u32, owner: Option<SymbolId>, state: State| Occurrence {
            file,
            span,
            line,
            kind: "call",
            owner,
            state,
            name: String::new(),
            access: Access::Unknown,
            local: false,
            statement: false,
            call: None,
        };
    for &f in &files {
        let rec = index.file(f);
        let Some(facts) = &rec.facts else { continue };
        for c in &facts.calls {
            let owner = facts.executing_owner(c.owner).and_then(|d| rec.symbol_of_decl(d));
            if !owner.is_some_and(|o| reached.contains(&o)) {
                continue;
            }
            at_site.insert((f.0, c.callee_span.end), occ.len());
            occ.push(occurrence(f, c.callee_span, c.line, owner, State::Target));
        }
    }
    // Calls through a library-declared member with in-index implementations (server
    // dispatch): undecided unless an edge covers them, never external.
    let mut library_dispatch: HashSet<(u32, u32)> = HashSet::new();
    for &f in &files {
        let Some(semantic) = &index.file(f).semantic else { continue };
        for d in &semantic.library_dispatch {
            if d.implementations.iter().any(|u| graph.symbol_by_uid(u).is_some()) {
                library_dispatch.insert((f.0, d.at.end));
            }
        }
    }
    for u in &index.unresolved {
        if !u.owner.is_some_and(|o| reached.contains(&o)) {
            continue;
        }
        let key = (u.at.file.0, u.at.bytes.end);
        let state = if covered.contains(&key) {
            State::Target
        } else if library_dispatch.contains(&key) {
            State::Unresolved("possible_only")
        } else if u.kind == UnresolvedKind::ExternalOrAmbiguous && u.candidates.is_empty() {
            State::Elsewhere(SERVER)
        } else {
            State::Unresolved(u.kind.as_str())
        };
        match at_site.get(&key) {
            Some(&i) => occ[i].state = state,
            None => {
                at_site.insert(key, occ.len());
                occ.push(occurrence(u.at.file, u.at.bytes, u.at.line, u.owner, state));
            }
        }
    }
    // Library dispatch calls recorded as library calls (no unresolved entry) are open too.
    for key in &library_dispatch {
        if covered.contains(key) {
            continue;
        }
        if let Some(&i) = at_site.get(key) {
            occ[i].state = State::Unresolved("possible_only");
        }
    }
    // Calls whose dispatch is undecided (which implementation runs is not known) are not
    // resolved, even though a proven edge reaches the abstract declaration (I-01; the `deps`
    // rows mark them `undecided` the same way).
    for key in crate::queries::deps::undecided_call_keys(index, reached) {
        if let Some(&i) = at_site.get(&key) {
            if occ[i].state == State::Target {
                occ[i].state = State::Unresolved("possible_only");
            }
        }
    }
    occ.sort_by_key(|o| (o.file, o.span.start, o.span.end));
    summarize(index, sources, &occ, bounded.as_deref(), 0, true)
}
