//! `deps`.
//!
//! `Graph::reach(start, Forward, include, bounds(depth))`; results exclude the
//! start, sorted by (distance, uid); each result's tier = weakest tier of the edge that first
//! reached it (proven < inferred < possible), `via` = that edge's kind, `from` / `at` = that
//! edge's source symbol and evidence location ([`reaching_edges`]: an edge from a symbol one
//! step closer; at distance 1 from the queried symbol, its first call site in source order);
//! `edges` = traversed edges, the reaching edges first (in result order), then the rest
//! (I-14); `unresolved_inside` = unresolved sites owned by any reached node incl. start
//! (calls the server proved external, [`proven_external`], are resolved elsewhere).
//! `calls` = one row per site inside the start that links out (its outgoing edges at the
//! evidence tier, grouped by evidence location, plus its own unresolved calls as rows without
//! targets; rows merge by callee end, as completeness does), in source order, each with the
//! call on one line and the conditions it runs
//! under ([`crate::queries::sites`]). A call with an undecided dispatch / flow site ([`undecided`]:
//! which implementation runs is not known) is `undecided`: its candidates are listed as
//! `possible` targets and it counts as unresolved, so `deps` is never `complete` with it
//! (I-01). Every edge row and unresolved row carries the exact source line
//! of its location (`text`, empty when the file is unreadable).
//! Notice: "Static dependencies under the selected evidence tier; not proof of runtime
//! execution."
//!
//! "The edge that first reached" a node is chosen among the BFS-tree-consistent edges into
//! it (from a node one step closer); when several exist the strongest tier wins, so a node
//! reachable by a proven call is never reported as inferred.
//!
//! Traverses bridge edges (cross-language links, tier per record) unless the workspace was
//! opened with `OpenOptions::no_bridges`; fills the envelope `completeness` with the calls
//! inside the reached set ([`crate::completeness::calls_completeness`]) and `tiers_used`.

use std::collections::{BTreeMap, HashSet};

use trace_core::model::{DecisionStatus, EdgeId, FileId, Site, SiteCategory, SiteOperation};
use trace_core::source::SourceStore;
use trace_core::{Bounds, Direction, Graph, Index, SymbolId, Tier};

use crate::cards::{at, bounds_info, card, edge_row, line_text, tiers_used, unresolved_row, use_kind};
use crate::queries::sites::Sites;
use crate::report::{CallRow, CallTarget, DependenciesReport, Reached};
use crate::workspace::Workspace;
use crate::Result;

pub(crate) const DEPENDENCIES_NOTICE: &str =
    "Static dependencies under the selected evidence tier; not proof of runtime execution.";

pub fn dependencies(ws: &mut Workspace, symbol: &str, depth: u32, deep: bool) -> Result<DependenciesReport> {
    let start = ws.resolve_scope_ready(symbol)?;
    dependencies_of(ws, start, depth, deep)
}

/// `deps` for an already resolved symbol.
pub(crate) fn dependencies_of(
    ws: &mut Workspace,
    start: SymbolId,
    depth: u32,
    deep: bool,
) -> Result<DependenciesReport> {
    let ws: &Workspace = ws;
    let graph = ws.graph()?;
    let graph = &graph;
    let index = graph.index;
    let bounds = Bounds::default().with_depth(depth);
    let reach = graph.reach(start, Direction::Forward, ws.include, &bounds)?;
    let first = reaching_edges(graph, &reach);
    let mut results: Vec<(Reached, Option<EdgeId>)> = reach
        .nodes
        .iter()
        .filter(|(id, _)| *id != start)
        .map(|&(id, distance)| (reached_row(graph, id, distance, first[id.idx()]), first[id.idx()]))
        .collect();
    results.sort_by(|(a, _), (b, _)| a.distance.cmp(&b.distance).then_with(|| a.card.id.cmp(&b.card.id)));
    let sources = ws.sources()?;
    let ordered = reaching_first(results.iter().filter_map(|(_, e)| *e), &reach.edges);
    let edges = ordered
        .iter()
        .map(|&eid| edge_row(graph, &sources, graph.edge(eid)))
        .collect();
    let results: Vec<Reached> = results.into_iter().map(|(r, _)| r).collect();
    let reached: HashSet<SymbolId> = reach.nodes.iter().map(|(id, _)| *id).collect();
    let unresolved_inside = index
        .unresolved
        .iter()
        .filter(|u| u.owner.is_some_and(|o| reached.contains(&o)) && !proven_external(u))
        .map(|u| unresolved_row(index, &sources, u))
        .collect();
    let calls = call_rows(graph, &sources, ws.include, start);
    let bounds = bounds_info(&reach, depth);
    let bounded = (!bounds.complete).then(|| format!("search bounded ({})", bounds.hit.join(", ")));
    let mut envelope = ws.envelope("deps");
    envelope.tiers_used = tiers_used(results.iter().map(|r: &Reached| r.tier));
    envelope.completeness =
        Some(crate::completeness::calls_completeness(graph, &sources, &reached, ws.include, bounded));
    Ok(DependenciesReport {
        envelope,
        symbol: card(index, index.symbol(start)),
        deep,
        calls,
        results,
        edges,
        unresolved_inside,
        bounds,
        notice: DEPENDENCIES_NOTICE,
    })
}

fn tier_rank(tier: &str) -> u8 {
    match tier {
        "proven" => 0,
        "inferred" => 1,
        _ => 2,
    }
}

/// The edge that reached each node of a traversal (indexed by symbol id; `None` for the
/// starts and unreached nodes): among the traversed edges from a node one step closer to
/// the start (the BFS tree), the strongest tier, then the parent discovered first, then the
/// first evidence location in source order (file path, byte), then the edge id. So a
/// distance-1 result's evidence is always the queried symbol's own first call of it, never
/// another function's call (I-14).
pub(crate) fn reaching_edges(graph: &Graph<'_>, reach: &trace_core::Reach) -> Vec<Option<EdgeId>> {
    let n = graph.symbol_count();
    let mut dist = vec![u32::MAX; n];
    let mut order = vec![usize::MAX; n];
    for (pos, &(id, d)) in reach.nodes.iter().enumerate() {
        if dist[id.idx()] == u32::MAX {
            dist[id.idx()] = d;
            order[id.idx()] = pos;
        }
    }
    let reverse = reach.direction == Direction::Reverse;
    let key = |eid: EdgeId| {
        let e = graph.edge(eid);
        let near = if reverse { e.to } else { e.from };
        (e.tier, order[near.idx()], graph.file_path(e.at.file), e.at.bytes.start, e.at.bytes.end, eid)
    };
    let mut best: Vec<Option<EdgeId>> = vec![None; n];
    for &eid in &reach.edges {
        let e = graph.edge(eid);
        let (near, far) = if reverse { (e.to, e.from) } else { (e.from, e.to) };
        let (dn, df) = (dist[near.idx()], dist[far.idx()]);
        if dn == u32::MAX || df == u32::MAX || df != dn + 1 {
            continue;
        }
        let slot = &mut best[far.idx()];
        let better = match *slot {
            None => true,
            Some(cur) => key(eid) < key(cur),
        };
        if better {
            *slot = Some(eid);
        }
    }
    best
}

/// Traversed edges with the reaching edges first (in the order given: result order), then
/// every other traversed edge (ascending id), each once (I-14: consumers that take the first
/// edge to a result get its reaching edge).
pub(crate) fn reaching_first(
    reaching: impl IntoIterator<Item = EdgeId>,
    traversed: &[EdgeId],
) -> Vec<EdgeId> {
    let mut ordered: Vec<EdgeId> = Vec::with_capacity(traversed.len());
    let mut seen: HashSet<EdgeId> = HashSet::new();
    for eid in reaching {
        if seen.insert(eid) {
            ordered.push(eid);
        }
    }
    ordered.extend(traversed.iter().copied().filter(|e| !seen.contains(e)));
    ordered
}

/// A reached symbol with the tier, kind, source symbol and location of its reaching edge.
pub(crate) fn reached_row(graph: &Graph<'_>, id: SymbolId, distance: u32, edge: Option<EdgeId>) -> Reached {
    let index = graph.index;
    let (tier, via, from, at_) = match edge {
        Some(eid) => {
            let e = graph.edge(eid);
            let via = match e.bridge.and_then(|b| index.bridges.get(b as usize)) {
                Some(b) => b.kind.edge_label(),
                None => e.kind.as_str(),
            };
            (e.tier.as_str(), via, Some(index.symbol(e.from).uid.clone()), Some(at(index, &e.at)))
        }
        None => ("proven", "calls", None, None),
    };
    Reached {
        card: card(index, index.symbol(id)),
        tier,
        distance,
        via,
        from,
        at: at_,
    }
}

/// Sites that decide which implementation / target a call runs: dispatch sites and value
/// flow calls / override dispatch (not callbacks, implicit operations or field writes).
fn dispatch_like(site: &Site) -> bool {
    match site.category {
        SiteCategory::Dispatch => true,
        SiteCategory::Flow => {
            matches!(site.operation, Some(SiteOperation::Call) | Some(SiteOperation::OverrideDispatch))
        }
        _ => false,
    }
}

/// Whether site `i` is undecided: it has options (candidates minus test-only), no decision
/// decided it, and it is not a composed site whose parent did not decide its target (then
/// the parent is what is undecided).
pub fn undecided(index: &Index, i: usize) -> bool {
    let Some(site) = index.sites.get(i) else {
        return false;
    };
    if trace_infer::decide::options(site).is_empty() {
        return false;
    }
    if index
        .decision(i as u32)
        .is_some_and(|d| d.status == DecisionStatus::Decided)
    {
        return false;
    }
    // A composed site counts once its parent site decided the target it dispatches through
    // (otherwise the parent is what is undecided).
    site.via.is_none_or(|parent| {
        index.decision(parent).is_some_and(|d| {
            d.status == DecisionStatus::Decided
                && site.declared_target.is_some_and(|t| d.targets.contains(&t))
        })
    })
}

/// Calls of `owners` with an undecided dispatch / flow site: (file id, callee end byte), the
/// key `deps` rows and [`crate::completeness::calls_completeness`] merge calls by.
pub(crate) fn undecided_call_keys(index: &Index, owners: &HashSet<SymbolId>) -> HashSet<(u32, u32)> {
    index
        .sites
        .iter()
        .enumerate()
        .filter(|(i, s)| owners.contains(&s.owner) && dispatch_like(s) && undecided(index, *i))
        .map(|(_, s)| (s.at.file.0, s.at.bytes.end))
        .collect()
}

/// One row per site inside `start` that links out (module docs), source order.
pub(crate) fn call_rows(
    graph: &Graph<'_>,
    sources: &SourceStore<'_>,
    include: Tier,
    start: SymbolId,
) -> Vec<CallRow> {
    let index = graph.index;
    let out: Vec<&trace_core::Edge> = graph.outgoing(start, include).map(|(_, e)| e).collect();
    // Calls the server proved external (library / builtin, no in-index candidate) are
    // resolved elsewhere, as in `completeness::calls_completeness`.
    let unresolved: Vec<&trace_core::model::Unresolved> = index
        .unresolved
        .iter()
        .filter(|u| u.owner == Some(start) && !proven_external(u))
        .collect();
    // Undecided dispatch / flow sites of the symbol (I-01): which implementation runs is not
    // known, so their calls are unresolved and list the candidates as possible targets.
    let pending: Vec<&Site> = index
        .sites
        .iter()
        .enumerate()
        .filter(|(i, s)| s.owner == start && dispatch_like(s) && undecided(index, *i))
        .map(|(_, s)| s)
        .collect();
    let sites = Sites::load(
        index,
        sources,
        out.iter()
            .map(|e| (e.at.file, e.at.bytes.start))
            .chain(unresolved.iter().map(|u| (u.at.file, u.at.bytes.start)))
            .chain(pending.iter().map(|s| (s.at.file, s.at.bytes.start))),
    );
    let mut rows: BTreeMap<(FileId, u32, u32), CallRow> = BTreeMap::new();
    for e in &out {
        let key = (e.at.file, e.at.line, e.at.bytes.end);
        let target = index.symbol(e.to);
        let tier = e.tier.as_str();
        let row = rows.entry(key).or_insert_with(|| CallRow {
            at: at(index, &e.at),
            kind: match e.bridge {
                Some(_) => "bridge",
                None => use_kind(e.kind),
            },
            tier,
            call: sites.call_or(&e.at, &line_text(sources, &e.at)),
            when: sites.when(&e.at),
            targets: Vec::new(),
            undecided: false,
        });
        if tier_rank(tier) < tier_rank(row.tier) {
            row.tier = tier;
        }
        if !row.targets.iter().any(|t| t.id == target.uid) {
            row.targets.push(CallTarget {
                id: target.uid.clone(),
                file: index.file_path(target.file).to_string(),
                line: target.span.start_line,
                tier,
            });
        }
    }
    for u in unresolved {
        let key = (u.at.file, u.at.line, u.at.bytes.end);
        rows.entry(key).or_insert_with(|| CallRow {
            at: at(index, &u.at),
            kind: "call",
            tier: "possible",
            call: sites.call_or(&u.at, &line_text(sources, &u.at)),
            when: sites.when(&u.at),
            targets: Vec::new(),
            undecided: false,
        });
    }
    for s in pending {
        // The row of the same call (its edges may carry another line: servers whose ranges
        // cover the whole invocation), else a new one.
        let key = rows
            .keys()
            .find(|k| k.0 == s.at.file && k.2 == s.at.bytes.end)
            .copied()
            .unwrap_or((s.at.file, s.at.line, s.at.bytes.end));
        let row = rows.entry(key).or_insert_with(|| CallRow {
            at: at(index, &s.at),
            kind: "call",
            tier: "possible",
            call: sites.call_or(&s.at, &line_text(sources, &s.at)),
            when: sites.when(&s.at),
            targets: Vec::new(),
            undecided: false,
        });
        row.undecided = true;
        for k in trace_infer::decide::options(s) {
            let target = index.symbol(s.candidates[k]);
            if !row.targets.iter().any(|t| t.id == target.uid) {
                row.targets.push(CallTarget {
                    id: target.uid.clone(),
                    file: index.file_path(target.file).to_string(),
                    line: target.span.start_line,
                    tier: "possible",
                });
            }
        }
    }
    rows.into_values().collect()
}

/// An unresolved call the server proved to leave the index (a library or builtin target).
pub(crate) fn proven_external(u: &trace_core::model::Unresolved) -> bool {
    u.kind == trace_core::model::UnresolvedKind::ExternalOrAmbiguous && u.candidates.is_empty()
}

#[cfg(test)]
#[path = "../../tests/unit/queries/deps.rs"]
mod tests;
