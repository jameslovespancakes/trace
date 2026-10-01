//! `show <symbol>...`: the exact source of each symbol, first to last line (hash-verified,
//! never cut), with how many symbols call it and how many sites inside it link out.
//!
//! Selectors resolve like every command (`Workspace::resolve_ready`: a symbol in a pending
//! file sets up its language first). Counts use the workspace evidence tier: `callers` =
//! distinct symbols with an edge into the symbol or its synthetic scopes' owner chain
//! excluded; `calls` = distinct evidence sites of outgoing edges plus unresolved calls owned
//! by the symbol.

use std::collections::HashSet;

use trace_core::SymbolId;

use crate::cards::card;
use crate::report::{ShowItem, ShowReport};
use crate::workspace::Workspace;
use crate::Result;

pub fn show(ws: &mut Workspace, selectors: &[String]) -> Result<ShowReport> {
    // Resolve every selector first (setting up pending languages may re-index), then read.
    for s in selectors {
        ws.resolve_ready(s)?;
    }
    let ids: Vec<SymbolId> = selectors.iter().map(|s| ws.resolve(s)).collect::<Result<_>>()?;
    let ws: &Workspace = ws;
    let graph = ws.graph()?;
    let index = graph.index;
    let sources = ws.sources()?;
    let mut symbols = Vec::with_capacity(ids.len());
    let mut shown: HashSet<SymbolId> = HashSet::new();
    for id in ids {
        if !shown.insert(id) {
            continue;
        }
        let s = index.symbol(id);
        let source = sources.text(s.file, s.span.bytes)?;
        symbols.push(ShowItem {
            symbol: card(index, s),
            source,
            callers: callers(&graph, ws.include, id),
            calls: calls(&graph, ws.include, id),
        });
    }
    Ok(ShowReport {
        envelope: ws.envelope("show"),
        symbols,
    })
}

/// Distinct calling symbols at `include`.
pub fn callers(graph: &trace_core::Graph<'_>, include: trace_core::Tier, id: SymbolId) -> usize {
    graph
        .incoming(id, include)
        .filter(|(_, e)| e.from != id)
        .map(|(_, e)| e.from)
        .collect::<HashSet<_>>()
        .len()
}

/// Distinct outgoing sites at `include` plus unresolved calls owned by `id`.
pub fn calls(graph: &trace_core::Graph<'_>, include: trace_core::Tier, id: SymbolId) -> usize {
    let index = graph.index;
    let linked: HashSet<(u32, u32)> = graph
        .outgoing(id, include)
        .map(|(_, e)| (e.at.file.0, e.at.bytes.start))
        .collect();
    let unresolved = index
        .unresolved
        .iter()
        .filter(|u| u.owner == Some(id))
        .filter(|u| !linked.contains(&(u.at.file.0, u.at.bytes.start)))
        .count();
    linked.len() + unresolved
}
