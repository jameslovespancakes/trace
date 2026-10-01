//! Conversions from core types to report rows, plus small shared helpers.

use trace_core::facts::RefKind;
use trace_core::model::{
    ByteSpan, DecisionStatus, Edge, EdgeId, EdgeKind, FileId, Location, Symbol, SymbolId, Unresolved,
};
use trace_core::source::SourceStore;
use trace_core::{Graph, Index, LanguageSupport, Tier};

use crate::report::{
    At, BoundsInfo, BridgeInfo, Card, DecisionInfo, EdgeCounts, EdgeRow, LanguageRow, UnresolvedRow,
};

/// Card for a symbol (summary = first doc line, <= 160 chars).
pub fn card(index: &Index, s: &Symbol) -> Card {
    let summary: String = s
        .doc
        .as_deref()
        .and_then(|d| d.lines().map(str::trim).find(|l| !l.is_empty()))
        .map(|l| l.trim_matches(|c| c == '"' || c == '\'').chars().take(160).collect())
        .unwrap_or_default();
    Card {
        id: s.uid.clone(),
        name: s.name.clone(),
        qualified_name: s.qualified_name.clone(),
        file: index.file(s.file).path.clone(),
        kind: s.kind.as_str(),
        language: s.language,
        line: s.span.start_line,
        end_line: s.span.end_line,
        start_byte: s.span.bytes.start,
        end_byte: s.span.bytes.end,
        summary,
        semantic: s.semantic,
    }
}

pub fn at(index: &Index, loc: &Location) -> At {
    At {
        file: index.file(loc.file).path.clone(),
        line: loc.line,
        start_byte: loc.bytes.start,
        end_byte: loc.bytes.end,
    }
}

/// Decision info for a site index.
pub(crate) fn decision_info(index: &Index, site: u32) -> Option<DecisionInfo> {
    let s = index.sites.get(site as usize)?;
    let d = index.decision(site)?;
    Some(DecisionInfo {
        site: s.id.0.clone(),
        category: s.category.as_str(),
        status: match d.status {
            trace_core::DecisionStatus::Decided => "decided",
            trace_core::DecisionStatus::Unknown => "unknown",
        },
        reason: d.reason.clone(),
    })
}

/// Both ends of bridge `b` (`Index::bridges[b]`).
pub(crate) fn bridge_info(index: &Index, b: u32) -> Option<BridgeInfo> {
    let r = index.bridges.get(b as usize)?;
    Some(BridgeInfo {
        kind: r.kind.as_str(),
        label: r.label.clone(),
        from_language: index.symbol(r.from).language,
        to_language: index.symbol(r.to).language,
        to_at: at(index, &r.to_at),
        assumptions: r.assumptions.clone(),
        contract: r.contract.map(|f| index.file(f).path.clone()),
        candidates: r.candidates,
    })
}

/// Output kind of an edge: `bridge:<kind>` for bridge edges, else the edge kind.
pub fn edge_kind(index: &Index, e: &Edge) -> &'static str {
    match e.bridge.and_then(|b| index.bridges.get(b as usize)) {
        Some(r) => r.kind.edge_label(),
        None => e.kind.as_str(),
    }
}

/// Edge row with the exact source line of its evidence location.
pub(crate) fn edge_row(graph: &Graph<'_>, sources: &SourceStore<'_>, e: &Edge) -> EdgeRow {
    let index = graph.index;
    let bridge = e.bridge.and_then(|b| bridge_info(index, b));
    EdgeRow {
        from: index.symbol(e.from).uid.clone(),
        to: index.symbol(e.to).uid.clone(),
        kind: edge_kind(index, e),
        tier: e.tier.as_str(),
        source: e.provider.label(),
        resolution: e.resolution.as_str(),
        at: at(index, &e.at),
        decision: e.site.and_then(|s| decision_info(index, s)),
        bridge,
        text: line_text(sources, &e.at),
        site: None,
    }
}

/// Unresolved-site row with the exact source line of the call site.
pub(crate) fn unresolved_row(index: &Index, sources: &SourceStore<'_>, u: &Unresolved) -> UnresolvedRow {
    UnresolvedRow {
        owner: u.owner.map(|o| index.symbol(o).uid.clone()),
        kind: u.kind.as_str(),
        callee: u.callee.clone(),
        at: at(index, &u.at),
        candidates: u.candidates.len(),
        text: line_text(sources, &u.at),
    }
}

pub(crate) fn bounds_info(reach: &trace_core::Reach, depth: u32) -> BoundsInfo {
    BoundsInfo {
        hit: reach.hit.names(),
        complete: reach.complete(),
        work: reach.work,
        depth,
    }
}

/// Merge the bounds of several traversals (union of hit bounds, summed work).
pub(crate) fn merge_bounds(parts: &[BoundsInfo], depth: u32) -> BoundsInfo {
    let mut hit: Vec<&'static str> = Vec::new();
    for name in ["depth", "work", "paths", "time"] {
        if parts.iter().any(|p| p.hit.contains(&name)) {
            hit.push(name);
        }
    }
    BoundsInfo {
        complete: hit.is_empty(),
        hit,
        work: parts.iter().map(|p| p.work).sum(),
        depth,
    }
}

/// For every node of a BFS `reach`, the edge that first reached it: among traversed edges
/// from a node at distance `d - 1` into a node at distance `d`, the strongest tier wins,
/// ties broken by edge order. Indexed by symbol id (`None` for unreached nodes / start).
pub(crate) fn first_edges(graph: &Graph<'_>, reach: &trace_core::Reach) -> Vec<Option<EdgeId>> {
    let n = graph.symbol_count();
    let mut dist = vec![u32::MAX; n];
    for &(id, d) in &reach.nodes {
        dist[id.idx()] = d;
    }
    let reverse = reach.direction == trace_core::Direction::Reverse;
    let mut best: Vec<Option<EdgeId>> = vec![None; n];
    for &eid in &reach.edges {
        let e = graph.edge(eid);
        let (near, far) = if reverse { (e.to, e.from) } else { (e.from, e.to) };
        let (dn, df) = (dist[near.idx()], dist[far.idx()]);
        if dn == u32::MAX || df == u32::MAX || df != dn + 1 {
            continue;
        }
        let slot = &mut best[far.idx()];
        let better = match slot {
            None => true,
            Some(cur) => {
                let c = graph.edge(*cur);
                (e.tier, eid) < (c.tier, *cur)
            }
        };
        if better {
            *slot = Some(eid);
        }
    }
    best
}

/// Weakest tier of a sequence of edges (proven for an empty sequence).
pub(crate) fn weakest_tier<'e>(edges: impl IntoIterator<Item = &'e Edge>) -> Tier {
    edges.into_iter().map(|e| e.tier).max().unwrap_or(Tier::Proven)
}

/// Edge counts per tier without materializing the graph (same rules as `Graph::new`).
pub(crate) fn edge_counts(index: &Index) -> EdgeCounts {
    let mut counts = EdgeCounts {
        proven: index.edges.len(),
        inferred: 0,
        possible: 0,
    };
    for (i, site) in index.sites.iter().enumerate() {
        // A dispatch decided by the receiver-type rule is a proven edge per target; its other
        // candidates are proven not to run (never materialized).
        if let Some(d) = index
            .decision(i as u32)
            .filter(|d| trace_core::graph::proven_by_receiver_rule(site, d))
        {
            counts.proven += d.targets.len();
            continue;
        }
        let chosen: &[SymbolId] = match index.decision(i as u32) {
            Some(d) if d.status == DecisionStatus::Decided => d.targets.as_slice(),
            _ => &[],
        };
        counts.inferred += chosen.len();
        counts.possible += site.candidates.iter().filter(|t| !chosen.contains(t)).count();
    }
    counts
}

/// Maximum characters of an exact line shown in rows.
pub(crate) const MAX_LINE_CHARS: usize = 400;

/// Tiers present in a result, ordered proven, inferred, possible (envelope `tiers_used`).
pub fn tiers_used<'t>(tiers: impl IntoIterator<Item = &'t str>) -> Vec<&'static str> {
    let mut seen = [false; 3];
    for t in tiers {
        match t {
            "proven" => seen[0] = true,
            "inferred" => seen[1] = true,
            "possible" => seen[2] = true,
            _ => {}
        }
    }
    ["proven", "inferred", "possible"]
        .into_iter()
        .zip(seen)
        .filter_map(|(name, hit)| hit.then_some(name))
        .collect()
}

/// Use kind of an incoming edge as listed by `uses` rows / `uses --deep` `other_references`:
/// `call` | `read` | `write` | `import` | `reexport` | `callback` | `override` |
/// `implements` | `declaration` | `bridge`.
pub(crate) fn use_kind(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::References | EdgeKind::PropertyGet => "read",
        EdgeKind::PassesCallback | EdgeKind::InferredCallback => "callback",
        EdgeKind::Writes | EdgeKind::InferredWrite => "write",
        EdgeKind::Imports => "import",
        EdgeKind::Reexports => "reexport",
        EdgeKind::Overrides => "override",
        EdgeKind::Implements => "implements",
        EdgeKind::Bridge => "bridge",
        EdgeKind::StubImplementation => "declaration",
        _ => "call",
    }
}

/// Use kind of a syntax reference (`RefKind`).
pub(crate) fn ref_use_kind(kind: RefKind) -> &'static str {
    match kind {
        RefKind::Read | RefKind::Decorator | RefKind::Type => "read",
        RefKind::Write => "write",
        RefKind::Argument => "callback",
        RefKind::Import => "import",
        RefKind::Export => "reexport",
    }
}

/// Innermost executing symbol (function, method, constructor, `<module>`, `<lambda>`)
/// whose span contains `byte`.
pub(crate) fn executing_symbol_at(index: &Index, file: FileId, byte: u32) -> Option<SymbolId> {
    index
        .symbols_of(file)
        .iter()
        .filter(|s| s.kind.is_executable() && s.span.bytes.contains(byte))
        .min_by_key(|s| s.span.bytes.len())
        .map(|s| s.id)
}

/// `(line, column, exact line text)` at `byte`, the text capped at [`MAX_LINE_CHARS`].
pub fn line_at(sources: &SourceStore<'_>, file: FileId, byte: u32) -> trace_core::Result<(u32, u32, String)> {
    let (line, column, text) = sources.line_at(file, byte)?;
    let text = truncate_chars(&text, MAX_LINE_CHARS).to_string();
    Ok((line, column, text))
}

/// Exact line text at `loc` (capped at [`MAX_LINE_CHARS`]); empty when the file cannot be
/// read or verified (rows are informational, the answer does not depend on them).
pub fn line_text(sources: &SourceStore<'_>, loc: &Location) -> String {
    line_at(sources, loc.file, loc.bytes.start)
        .map(|(_, _, text)| text)
        .unwrap_or_default()
}

/// Narrow an evidence span to the identifier `name` it ends with (a callee span
/// `self.json.dumps` -> `dumps`); unchanged when the source does not end with `name`.
/// [`narrow_to_name`] using the syntax facts first: the member-name span of the last call
/// inside `at` whose member is `name` (servers such as jdtls report the whole call
/// `writer.getStrictness()`, which does not end with the name), else the byte tail check.
pub(crate) fn narrow_to_member(
    index: &trace_core::Index,
    sources: &SourceStore<'_>,
    at: &Location,
    name: &str,
) -> ByteSpan {
    if let Some(facts) = index.file(at.file).facts.as_ref() {
        let found = facts
            .calls
            .iter()
            .filter(|c| c.member.as_deref() == Some(name))
            .map(crate::completeness::member_span)
            .filter(|s| at.bytes.encloses(*s) && s.len() == name.len() as u32)
            .max_by_key(|s| s.end);
        if let Some(span) = found {
            return span;
        }
    }
    narrow_to_name(sources, at, name)
}

pub(crate) fn narrow_to_name(sources: &SourceStore<'_>, at: &Location, name: &str) -> ByteSpan {
    let len = name.len() as u32;
    if name.is_empty() || at.bytes.len() <= len {
        return at.bytes;
    }
    let tail = ByteSpan::new(at.bytes.end - len, at.bytes.end);
    match sources.file(at.file) {
        Ok(f) if f.bytes.get(tail.range()) == Some(name.as_bytes()) => tail,
        _ => at.bytes,
    }
}

/// Language support row for reports.
pub(crate) fn language_row(s: &LanguageSupport) -> LanguageRow {
    LanguageRow {
        language: s.language,
        files: s.files,
        support: s.level,
        backend: s.backend.clone(),
        backend_available: s.backend_available,
        reason: s.reason.clone(),
        resolution: None,
    }
}

/// Longest prefix of `text` with at most `max` characters.
pub(crate) fn truncate_chars(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

#[cfg(test)]
#[path = "../tests/unit/cards.rs"]
mod tests;
