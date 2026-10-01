//! `context <symbol>`: the symbol's exact source and everything around it, in one answer.
//!
//! * `source`: the declaration's exact bytes (never cut; `show` gives the same text);
//! * `callers`: every site calling the symbol at the evidence tier (one row per site,
//!   `(file, line)` order) with the exact line, the conditions it runs under and the values
//!   bound to the symbol's parameters there when an argument differs from the parameter name;
//! * `calls`: the sites inside the symbol that link out, as `deps` rows
//!   ([`crate::queries::deps::call_rows`]);
//! * `below`: symbols reached below the direct calls: distance 2 (every level with `--deep`,
//!   as `deps` `then`);
//! * `callers_of_callers` (`--deep`): callers above the direct callers (BFS order, at most
//!   [`CALLERS_OF_CALLERS`]);
//! * `tests`: tests that set an option the calls into the symbol depend on first, then tests
//!   mentioning the symbol or its callers (as `uses`); `tests_total` counts them all;
//! * `next`: exact commands worth running next: `show` of the first in-repository targets
//!   of the symbol's calls that are not shown here yet, and `uses <symbol>` when it has
//!   callers.

use std::collections::HashSet;

use trace_core::{Bounds, Direction, SymbolId};

use crate::cards::{at, card, line_text};
use crate::queries::sites::{carries, option_tests, Sites, OPTION_TESTS};
use crate::queries::uses::SUMMARY_TESTS;
use crate::report::{CallerSite, ContextReport, GuardTest, Reached};
use crate::workspace::Workspace;
use crate::Result;

/// Callers of callers listed with `--deep`.
pub(crate) const CALLERS_OF_CALLERS: usize = 200;
/// Symbols suggested by `next: show ...`.
pub(crate) const NEXT_SHOW: usize = 2;

pub fn context(ws: &mut Workspace, symbol: &str, deep: bool) -> Result<ContextReport> {
    let id = ws.resolve_scope_ready(symbol)?;
    let depth = if deep { crate::DEPTH } else { 2 };
    let ws: &Workspace = ws;
    let graph = ws.graph()?;
    let index = graph.index;
    let sources = ws.sources()?;
    let s = index.symbol(id);
    let source = sources.text(s.file, s.span.bytes)?;

    // Callers, one row per site.
    let incoming: Vec<&trace_core::Edge> = graph
        .incoming(id, ws.include)
        .map(|(_, e)| e)
        .filter(|e| e.from != id)
        .collect();
    let sites = Sites::load(index, &sources, incoming.iter().map(|e| (e.at.file, e.at.bytes.start)));
    let mut callers: Vec<CallerSite> = Vec::new();
    let mut seen_sites: HashSet<(u32, u32)> = HashSet::new();
    for e in &incoming {
        if !seen_sites.insert((e.at.file.0, e.at.bytes.start)) {
            continue;
        }
        let pairs = sites
            .view(&e.at)
            .map(|v| carries(v, s))
            .unwrap_or_default()
            .into_iter()
            .filter(|c| c.argument != c.parameter)
            .collect();
        callers.push(CallerSite {
            caller: index.symbol(e.from).uid.clone(),
            at: at(index, &e.at),
            tier: e.tier.as_str(),
            text: line_text(&sources, &e.at),
            when: sites.when(&e.at),
            carries: pairs,
        });
    }
    callers.sort_by(|a, b| {
        (a.at.file.as_str(), a.at.line, a.at.start_byte).cmp(&(
            b.at.file.as_str(),
            b.at.line,
            b.at.start_byte,
        ))
    });

    // Calls and what lies below them.
    let calls = crate::queries::deps::call_rows(&graph, &sources, ws.include, id);
    let bounds = Bounds::default().with_depth(depth);
    let reach = graph.reach(id, Direction::Forward, ws.include, &bounds)?;
    let first = crate::queries::deps::reaching_edges(&graph, &reach);
    let mut below: Vec<Reached> = reach
        .nodes
        .iter()
        .filter(|(_, d)| *d >= 2)
        .map(|&(n, distance)| crate::queries::deps::reached_row(&graph, n, distance, first[n.idx()]))
        .collect();
    below.sort_by(|a, b| (a.distance, a.card.id.as_str()).cmp(&(b.distance, b.card.id.as_str())));

    let callers_of_callers: Vec<String> = if deep {
        let reach =
            graph.reach(id, Direction::Reverse, ws.include, &Bounds::default().with_depth(crate::DEPTH))?;
        reach
            .nodes
            .iter()
            .filter(|(_, d)| *d >= 2)
            .take(CALLERS_OF_CALLERS)
            .map(|&(n, _)| index.symbol(n).uid.clone())
            .collect()
    } else {
        Vec::new()
    };

    // Tests: options the calls into the symbol depend on, then mentions.
    let guards = sites.guards(incoming.iter().map(|e| &e.at));
    let mut tests: Vec<GuardTest> = option_tests(index, &sources, &guards, OPTION_TESTS);
    let mentions = crate::queries::tests_index::tests_with_graph(
        &graph,
        ws.include,
        &[id],
        crate::queries::impact::MAX_TESTS,
    )?;
    let extra = tests
        .iter()
        .filter(|t| !mentions.iter().any(|m| m.test == t.test))
        .count();
    for m in &mentions {
        if tests.len() >= SUMMARY_TESTS {
            break;
        }
        if !tests.iter().any(|t| t.test == m.test) {
            tests.push(GuardTest {
                test: m.test.clone(),
                file: m.file.clone(),
                line: m.line,
                sets: String::new(),
            });
        }
    }

    // Next commands.
    let mut next = Vec::new();
    let mut shown: Vec<&str> = Vec::new();
    for row in &calls {
        for t in &row.targets {
            if shown.len() >= NEXT_SHOW {
                break;
            }
            let target: Option<SymbolId> = graph.symbol_by_uid(&t.id);
            let in_repo = target.is_some_and(|t| !crate::queries::sites::is_test_code(index, t) && t != id);
            if in_repo && !shown.contains(&t.id.as_str()) {
                shown.push(t.id.as_str());
            }
        }
    }
    if !shown.is_empty() {
        next.push(format!("show {}", shown.join(" ")));
    }
    if !callers.is_empty() {
        next.push(format!("uses {} --deep", s.uid));
    }

    let mut envelope = ws.envelope("context");
    envelope.tiers_used =
        crate::cards::tiers_used(callers.iter().map(|c| c.tier).chain(calls.iter().map(|c| c.tier)));
    Ok(ContextReport {
        envelope,
        deep,
        symbol: card(index, s),
        source,
        callers,
        calls,
        callers_of_callers,
        below,
        tests,
        tests_total: mentions.len() + extra,
        next,
    })
}
