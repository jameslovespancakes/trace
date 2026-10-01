//! `path`.
//!
//! One `shortest_path`; with `--deep` every bounded simple path (`all_paths`, at most
//! [`DEEP_PATHS`], shortest first). Path tier = weakest edge tier. Every hop carries its
//! site facts (`EdgeRow::site`: the call on one line, its conditions, `argument ->
//! parameter` pairs of the callee). Note: "No path in a static graph does not prove
//! impossibility." Without a path, `unresolved` counts the undecided sites reachable from
//! `from` whose candidates can reach `to` in the `possible` view ([`undecided_between`]):
//! the path may run through them, so the answer is `no path  <N> unresolved`, never
//! `complete` (I-01; `--deep` follows them).
//!
//! Traverses bridge edges unless the workspace was opened with `OpenOptions::no_bridges` and
//! reports every node's language so text output can print where a path changes language;
//! fills `tiers_used`.

use trace_core::{Bounds, Direction, Graph, SymbolId, Tier};

use super::deps::undecided;
use crate::cards::{bounds_info, card, edge_row, tiers_used, weakest_tier};
use crate::queries::sites::Sites;
use crate::report::{PathReport, PathRow};
use crate::workspace::Workspace;
use crate::Result;

pub(crate) const PATH_NOTE: &str = "No path in a static graph does not prove impossibility.";
/// Paths returned by `path --deep`.
pub(crate) const DEEP_PATHS: usize = 10;

/// Undecided sites reachable from `from` (view `include`, depth-bounded) whose candidates can
/// reach `to` in the `possible` view: a path may run through them. Always 0 for the
/// `possible` view (it traverses every candidate).
pub(crate) fn undecided_between(
    graph: &Graph<'_>,
    include: Tier,
    from: SymbolId,
    to: SymbolId,
    depth: u32,
) -> usize {
    if include == Tier::Possible {
        return 0;
    }
    let index = graph.index;
    let bounds = Bounds::default().with_depth(depth);
    let reachable = |start: SymbolId, direction: Direction, tier: Tier| -> Vec<bool> {
        let mut seen = vec![false; graph.symbol_count()];
        if let Ok(reach) = graph.reach(start, direction, tier, &bounds) {
            for (id, _) in reach.nodes {
                seen[id.idx()] = true;
            }
        }
        seen
    };
    let forward = reachable(from, Direction::Forward, include);
    let backward = reachable(to, Direction::Reverse, Tier::Possible);
    index
        .sites
        .iter()
        .enumerate()
        .filter(|(i, s)| {
            forward[s.owner.idx()]
                && undecided(index, *i)
                && trace_infer::decide::options(s)
                    .into_iter()
                    .any(|k| backward[s.candidates[k].idx()])
        })
        .count()
}

pub fn path(ws: &mut Workspace, from: &str, to: &str, depth: u32, deep: bool) -> Result<PathReport> {
    ws.resolve_scope_ready(from)?;
    let b = ws.resolve_scope_ready(to)?;
    // Setting up the second endpoint may have re-indexed: resolve the first one again.
    let a = ws.resolve_scope(from)?;
    let report = path_of(ws, a, b, depth, deep)?;
    if report.found {
        return Ok(report);
    }
    // A `file:line` selector resolves to the innermost named symbol (SPEC §9.4); the code on
    // that line may run in an anonymous function nested in it (`mutationFn: (d) => api(d)`).
    // When the named symbol reaches nothing, the path is searched from that scope.
    if let Some(inner) = line_scope(ws, from, a)? {
        let nested = path_of(ws, inner, b, depth, deep)?;
        // Found from the nested scope, or only a path below the view from it: that answer
        // (the code on the selected line is what reaches the target).
        if nested.found || (nested.possible_path && !report.possible_path) {
            return Ok(nested);
        }
    }
    Ok(report)
}

/// Innermost `<lambda>` scope containing the line of a `file:line` selector, nested (through
/// its parents) in the symbol the selector resolved to.
fn line_scope(ws: &Workspace, selector: &str, named: SymbolId) -> Result<Option<SymbolId>> {
    let Some((_, line)) = selector.trim().rsplit_once(':') else { return Ok(None) };
    let Ok(line) = line.trim().parse::<u32>() else { return Ok(None) };
    let graph = ws.graph()?;
    let index = graph.index;
    let file = index.symbol(named).file;
    let nested_in_named = |mut s: SymbolId| {
        for _ in 0..64 {
            match index.symbol(s).parent {
                Some(p) if p == named => return true,
                Some(p) => s = p,
                None => return false,
            }
        }
        false
    };
    Ok(index
        .symbols_of(file)
        .iter()
        .filter(|s| s.is_synthetic() && s.name != "<module>")
        .filter(|s| s.span.start_line <= line && line <= s.span.end_line)
        .filter(|s| nested_in_named(s.id))
        .min_by_key(|s| s.span.bytes.len())
        .map(|s| s.id))
}

/// `path` for already resolved endpoints.
pub fn path_of(
    ws: &mut Workspace,
    from: SymbolId,
    to: SymbolId,
    depth: u32,
    deep: bool,
) -> Result<PathReport> {
    let ws: &Workspace = ws;
    let graph = ws.graph()?;
    let graph = &graph;
    let index = graph.index;
    let sources = ws.sources()?;
    let bounds = Bounds {
        max_paths: DEEP_PATHS,
        ..Bounds::default().with_depth(depth)
    };
    let reach = if deep {
        graph.all_paths(from, to, ws.include, &bounds)?
    } else {
        graph.shortest_path(from, to, ws.include, &bounds)?
    };
    let sites = Sites::load(
        index,
        &sources,
        reach
            .paths
            .iter()
            .flat_map(|p| p.edges.iter())
            .map(|&e| graph.edge(e))
            .map(|e| (e.at.file, e.at.bytes.start)),
    );
    let paths: Vec<PathRow> = reach
        .paths
        .iter()
        .map(|p| {
            let edges: Vec<_> = p.edges.iter().map(|&e| graph.edge(e)).collect();
            PathRow {
                nodes: p.nodes.iter().map(|&n| index.symbol(n).uid.clone()).collect(),
                languages: p.nodes.iter().map(|&n| index.symbol(n).language).collect(),
                tier: weakest_tier(edges.iter().copied()).as_str(),
                edges: edges
                    .iter()
                    .map(|e| {
                        let mut row = edge_row(graph, &sources, e);
                        if e.bridge.is_none() {
                            row.site = Some(sites.info(&e.at, Some(index.symbol(e.to))));
                        }
                        row
                    })
                    .collect(),
            }
        })
        .collect();
    let mut envelope = ws.envelope("path");
    envelope.tiers_used = tiers_used(paths.iter().flat_map(|p| p.edges.iter().map(|e| e.tier)));
    let unresolved = if paths.is_empty() {
        undecided_between(graph, ws.include, from, to, depth)
    } else {
        0
    };
    // Nothing in the selected view: say when the `possible` view has a path (never "complete"
    // silence over a path that runs through a candidate or a possible cross-language link).
    let possible_path = paths.is_empty()
        && ws.include != Tier::Possible
        && graph
            .shortest_path(from, to, Tier::Possible, &bounds)
            .is_ok_and(|r| !r.paths.is_empty());
    Ok(PathReport {
        envelope,
        deep,
        from: card(index, index.symbol(from)),
        to: card(index, index.symbol(to)),
        found: !paths.is_empty(),
        paths,
        bounds: bounds_info(&reach, depth),
        unresolved,
        possible_path,
        note: PATH_NOTE,
    })
}

#[cfg(test)]
#[path = "../../tests/unit/queries/path.rs"]
mod tests;
