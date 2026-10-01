//! `uses --deep`: the deep sections (transitive callers, result uses, similar code and
//! tests; sections per NEXT.md item 8).
//!
//! Family: the target is expanded to its complete family (`trace_core::graph::target_family`:
//! overrides, implementations, declarations — prototypes, signatures, `.pyi` stubs — and
//! overloads, <= 64 members); call sites of members other than the target carry `via` = the
//! member uid.
//!
//! Sections (one row per call site, each with the exact line `text`):
//! * CALLERS — distance-1 call sites of the target and its family (reason "directly calls
//!   changed code", or the family reasons of `completeness::family_candidate_edges`);
//!   several edges at one site keep the strongest tier. Non-proven links at member-binding
//!   sites (`completeness::Bindings`) are not callers, exactly as they are not `uses` rows.
//! * OTHER REFERENCES — non-call uses (reference kinds: reads, writes, imports, re-exports,
//!   callbacks), `relation` = the use kind; plus the declarations that change with the
//!   target (the target itself and linked stub / prototype declarations).
//! * TRANSITIVE — callers at distance >= 2 (`Graph::reach_many(Reverse)` from target and
//!   family, capped at 200 + total). Each row's first-edge chain is followed back to the
//!   family member it reaches (`target`, `via`); `through` = the distance-1 caller just
//!   before that member (`None` when the chain breaks).
//! * RESULT USES — every direct caller edge of the target with a call position:
//!   `trace_syntax::uses::result_use` on the caller file; phrases grouped by kind:
//!   `"<phrase> (<n>x, e.g. line <l>: <code>)"`, at most 8 per caller.
//! * SIMILAR CODE — [`crate::queries::similar`] matches of the target (Jaccard >= 0.6, top 5).
//! * TESTS — [`crate::queries::tests_index`] for the target, sorted direct first, max 50.
//! * UNKNOWN — unresolved sites inside the target, undecided sites whose candidates include
//!   the target, pending languages present (not analysed yet), traversal bounds hit.
//! * `totals` — callers (distinct symbols), call_sites, other_references, transitive, tests.
//!
//! Completeness is the `uses` rows' name completeness (reported once, in the envelope).
//!
//! Before collecting callers, a target in a pending file sets up its language
//! ([`Workspace::resolve_ready`]).

use std::collections::{BTreeMap, HashMap, HashSet};

use trace_core::graph::{target_family, MAX_FAMILY};
use trace_core::model::{DecisionStatus, Edge, FileId, SymbolId};
use trace_core::source::SourceStore;
use trace_core::tiers::USES;
use trace_core::{Bounds, Direction, Graph, Index, SupportLevel};
use trace_syntax::uses::{ResultUse, UseKind};

use crate::cards::{at, bounds_info, card, edge_kind, first_edges, line_at, unresolved_row, use_kind};
use crate::report::{
    BoundsInfo, CallerRow, DeepImpact, ImpactTotals, ResultUseRow, ResultUses, SimilarCode, UnknownInfo,
};
use crate::workspace::Workspace;
use crate::Result;

pub(crate) const MAX_TRANSITIVE: usize = 200;
pub(crate) const RESULT_USE_TARGETS: usize = 5;
pub(crate) const SIMILAR_TARGETS: usize = 5;
pub(crate) const SIMILAR_LIMIT: usize = 5;
pub(crate) const TEST_TARGETS: usize = 10;
pub(crate) const MAX_TESTS: usize = 50;
pub(crate) const MAX_USE_PHRASES: usize = 8;

const REASON_DIRECT: &str = "directly calls changed code";
const REASON_REFERENCE: &str = "uses changed code without calling it";
const REASON_DECLARATION: &str = "declares changed code";

/// The deep sections for `symbol` with traversal depth `depth` (module docs).
pub fn impact(ws: &mut Workspace, symbol: &str, depth: u32) -> Result<DeepImpact> {
    // A symbol in a pending file sets up its language first (symbol ids may change).
    let unique = [ws.resolve_ready(symbol)?];
    // Family of the target (target excluded): overrides, implementations, declarations
    // (stubs, prototypes, signatures) and overloads.
    let family: Vec<SymbolId> = target_family(ws.index()?, unique[0], MAX_FAMILY)
        .ids()
        .into_iter()
        .filter(|m| !unique.contains(m))
        .collect();
    let ws: &Workspace = ws;
    let graph = ws.graph()?;
    let sources = ws.sources()?;
    impact_of(ws, &graph, &sources, &unique, &family, depth)
}

fn impact_of(
    ws: &Workspace,
    graph: &Graph<'_>,
    sources: &SourceStore<'_>,
    targets: &[SymbolId],
    family: &[SymbolId],
    depth: u32,
) -> Result<DeepImpact> {
    let index = graph.index;
    let include = ws.include;
    let target_set: HashSet<SymbolId> = targets.iter().copied().collect();
    let members: Vec<SymbolId> = targets.iter().chain(family).copied().collect();
    let member_set: HashSet<SymbolId> = members.iter().copied().collect();
    let via = |m: SymbolId| (!target_set.contains(&m)).then(|| index.symbol(m).uid.clone());

    // Sites whose candidates / value flow are all family members (query-time rules).
    let flow_edges = if family.is_empty() {
        Vec::new()
    } else {
        crate::completeness::family_candidate_edges(index, &member_set, include)
    };
    // Non-proven links at member-binding sites are not uses (SPEC §10.1).
    let bindings = crate::completeness::Bindings::new(index, &members);
    let rejected = |e: &Edge, m: SymbolId| -> bool {
        if e.tier == trace_core::Tier::Proven {
            return false;
        }
        let Some(facts) = &index.file(e.at.file).facts else {
            return false;
        };
        let span = crate::cards::narrow_to_name(sources, &e.at, &index.symbol(m).name);
        bindings.excludes(e.at.file, span, &crate::completeness::access_at(facts, span), m)
    };
    // Direct call sites: one per (file, start, caller), strongest tier.
    let mut sites: BTreeMap<(FileId, u32, SymbolId), (SymbolId, &Edge)> = BTreeMap::new();
    for e in &flow_edges {
        if rejected(e, e.to) {
            continue;
        }
        sites.insert((e.at.file, e.at.bytes.start, e.from), (e.to, e));
    }
    for &m in &members {
        for (_, e) in graph.incoming(m, include) {
            // A stub declaration is part of the family, not a call site.
            if target_set.contains(&e.from)
                || (e.kind == trace_core::EdgeKind::StubImplementation && member_set.contains(&e.from))
                || rejected(e, m)
            {
                continue;
            }
            let key = (e.at.file, e.at.bytes.start, e.from);
            match sites.get(&key) {
                Some((_, cur)) if cur.tier <= e.tier => {}
                _ => {
                    sites.insert(key, (m, e));
                }
            }
        }
    }
    let mut callers: Vec<CallerRow> = Vec::new();
    let mut direct: HashSet<SymbolId> = HashSet::new();
    for (&(_, _, from), &(m, e)) in &sites {
        direct.insert(from);
        let from_flow = flow_edges.iter().any(|f| std::ptr::eq(f, e));
        let reason = match from_flow.then(|| crate::completeness::family_resolution(e)) {
            Some("family_overloads") => crate::completeness::FAMILY_OVERLOAD_REASON,
            Some(_) => crate::completeness::FAMILY_FLOW_REASON,
            None => REASON_DIRECT,
        };
        callers.push(caller_row(graph, sources, from, m, e, 1, reason.into(), via(m))?);
    }
    let order = |a: &CallerRow, b: &CallerRow| {
        let key = |r: &CallerRow| {
            (r.call_site.as_ref().map(|c| (c.file.clone(), c.line, c.start_byte)), r.card.id.clone())
        };
        key(a).cmp(&key(b))
    };
    callers.sort_by(order);

    // Other references (non-call uses), one row per use site.
    let mut uses: BTreeMap<(FileId, u32, SymbolId), (SymbolId, &Edge)> = BTreeMap::new();
    for &m in &members {
        for (_, e) in graph.incoming_all(m) {
            if !USES.contains(e.kind) || e.tier > include || e.from == m || rejected(e, m) {
                continue;
            }
            uses.entry((e.at.file, e.at.bytes.start, e.from)).or_insert((m, e));
        }
    }
    let mut other_references: Vec<CallerRow> = Vec::new();
    for (&(_, _, from), &(m, e)) in &uses {
        let mut row = caller_row(graph, sources, from, m, e, 1, REASON_REFERENCE.into(), via(m))?;
        row.relation = use_kind(e.kind);
        other_references.push(row);
    }
    // Declarations of the changed entity itself: the target and every stub / prototype
    // declaration linked to a member (`stub_implementation`: `.pyi` stubs, C/C++ header
    // prototypes) change with it (overriding members are listed in the family).
    for &m in &members {
        let s = index.symbol(m);
        if s.is_synthetic() {
            continue;
        }
        let linked = members.iter().any(|&o| {
            o != m
                && index.edges.iter().any(|e| {
                    e.kind == trace_core::EdgeKind::StubImplementation
                        && ((e.from == m && e.to == o) || (e.from == o && e.to == m))
                })
        });
        if !target_set.contains(&m) && !linked {
            continue;
        }
        let (line, text) = match line_at(sources, s.file, s.name_span.start) {
            Ok((line, _, text)) => (line, text),
            Err(trace_core::CoreError::SourceChanged(path)) => {
                return Err(trace_core::CoreError::SourceChanged(path).into())
            }
            Err(_) => (s.span.start_line, String::new()),
        };
        let loc = trace_core::Location {
            file: s.file,
            bytes: s.name_span,
            line,
        };
        other_references.push(CallerRow {
            card: card(index, s),
            tier: trace_core::Tier::Proven.as_str(),
            relation: "declaration",
            distance: 0,
            target: s.uid.clone(),
            reason: REASON_DECLARATION.into(),
            call_site: Some(at(index, &loc)),
            text,
            via: via(m),
            through: None,
        });
    }
    other_references.sort_by(order);

    // Transitive callers (distance >= 2) from target and family together.
    let mut bound_parts: Vec<BoundsInfo> = Vec::new();
    let mut transitive: Vec<CallerRow> = Vec::new();
    let mut transitive_total = 0;
    if !members.is_empty() {
        let bounds = Bounds::default().with_depth(depth);
        let reach = graph.reach_many(&members, Direction::Reverse, include, &bounds)?;
        bound_parts.push(bounds_info(&reach, depth));
        let first = first_edges(graph, &reach);
        let mut found: Vec<(u32, SymbolId, trace_core::EdgeId)> = reach
            .nodes
            .iter()
            .filter(|(id, d)| *d >= 2 && !member_set.contains(id) && !direct.contains(id))
            .filter_map(|&(id, d)| first[id.idx()].map(|eid| (d, id, eid)))
            .collect();
        found.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| index.symbol(a.1).uid.cmp(&index.symbol(b.1).uid))
        });
        transitive_total = found.len();
        found.truncate(MAX_TRANSITIVE);
        for (d, id, eid) in found {
            let e = graph.edge(eid);
            let (reached, member_via, through) = match walk_chain(graph, &first, &member_set, id, e.to, d) {
                Some((member, before)) => (member, via(member), before.map(|b| index.symbol(b).uid.clone())),
                None => (e.to, None, None),
            };
            let reason = format!("reaches changed code in {d} steps");
            let mut row = caller_row(graph, sources, id, reached, e, d, reason, member_via)?;
            row.through = through;
            transitive.push(row);
        }
    }
    let bounds = crate::cards::merge_bounds(&bound_parts, depth);

    let result_uses =
        result_uses(graph, sources, include, &targets[..targets.len().min(RESULT_USE_TARGETS)])?;
    let similar_code = crate::queries::similar::similar_many(
        ws,
        &targets[..targets.len().min(SIMILAR_TARGETS)],
        SIMILAR_LIMIT,
    )?
    .into_iter()
    .map(|(t, matches)| SimilarCode {
        target: index.symbol(t).uid.clone(),
        matches,
    })
    .collect();
    let tests = crate::queries::tests_index::tests_with_graph(
        graph,
        include,
        &targets[..targets.len().min(TEST_TARGETS)],
        MAX_TESTS,
    )?;

    let caller_ids: HashSet<&str> = callers.iter().map(|r| r.card.id.as_str()).collect();
    let totals = ImpactTotals {
        callers: caller_ids.len(),
        call_sites: callers.len(),
        other_references: other_references.len(),
        transitive: transitive_total,
        tests: tests.len(),
    };
    let unknown = unknown_info(index, sources, &target_set, bounds);
    Ok(DeepImpact {
        callers,
        other_references,
        transitive,
        transitive_total,
        totals,
        result_uses,
        similar_code,
        tests,
        unknown,
    })
}

/// Follow the first edges from `start` (the far end of `id`'s first edge) back to the
/// family member the chain reaches within `d` steps. Returns the member and the node just
/// before it: the distance-1 caller the chain passes (`None` if that is `id` itself). `None`
/// when the chain breaks before reaching a member.
fn walk_chain(
    graph: &Graph<'_>,
    first: &[Option<trace_core::EdgeId>],
    members: &HashSet<SymbolId>,
    id: SymbolId,
    start: SymbolId,
    d: u32,
) -> Option<(SymbolId, Option<SymbolId>)> {
    let mut node = start;
    let mut before = id;
    let mut steps = 1;
    while !members.contains(&node) && steps < d {
        match first[node.idx()] {
            Some(next) => {
                before = node;
                node = graph.edge(next).to;
            }
            None => break,
        }
        steps += 1;
    }
    members
        .contains(&node)
        .then_some((node, (before != id).then_some(before)))
}

#[allow(clippy::too_many_arguments)]
fn caller_row(
    graph: &Graph<'_>,
    sources: &SourceStore<'_>,
    caller: SymbolId,
    target: SymbolId,
    e: &Edge,
    distance: u32,
    reason: String,
    via: Option<String>,
) -> Result<CallerRow> {
    let index = graph.index;
    let relation = edge_kind(index, e);
    let text = match line_at(sources, e.at.file, e.at.bytes.start) {
        Ok((_, _, text)) => text,
        Err(trace_core::CoreError::SourceChanged(path)) => {
            return Err(trace_core::CoreError::SourceChanged(path).into())
        }
        Err(_) => String::new(),
    };
    Ok(CallerRow {
        card: card(index, index.symbol(caller)),
        tier: e.tier.as_str(),
        relation,
        distance,
        target: index.symbol(target).uid.clone(),
        reason,
        call_site: Some(at(index, &e.at)),
        text,
        via,
        through: None,
    })
}

fn unknown_info(
    index: &Index,
    sources: &SourceStore<'_>,
    targets: &HashSet<SymbolId>,
    bounds: BoundsInfo,
) -> UnknownInfo {
    let pending_languages = index
        .support
        .iter()
        .filter(|s| s.files > 0 && s.level == SupportLevel::Pending)
        .map(|s| s.language)
        .collect();
    let unresolved_inside = index
        .unresolved
        .iter()
        .filter(|u| u.owner.is_some_and(|o| targets.contains(&o)))
        .map(|u| unresolved_row(index, sources, u))
        .collect();
    let undecided_sites_into_targets = index
        .sites
        .iter()
        .enumerate()
        .filter(|(i, s)| {
            s.candidates.iter().any(|c| targets.contains(c))
                && index
                    .decision(*i as u32)
                    .is_none_or(|d| d.status != DecisionStatus::Decided)
        })
        .count();
    UnknownInfo {
        unresolved_inside,
        undecided_sites_into_targets,
        pending_languages,
        bounds,
    }
}

/// RESULT USES for `targets`: one row per (caller, call position).
fn result_uses(
    graph: &Graph<'_>,
    sources: &SourceStore<'_>,
    include: trace_core::Tier,
    targets: &[SymbolId],
) -> Result<Vec<ResultUses>> {
    let index = graph.index;
    let mut out = Vec::with_capacity(targets.len());
    for &t in targets {
        let mut seen: HashSet<(SymbolId, trace_core::FileId, u32)> = HashSet::new();
        let mut rows = Vec::new();
        let mut edges: Vec<&trace_core::Edge> = graph.incoming(t, include).map(|(_, e)| e).collect();
        edges.sort_by(|a, b| {
            (a.at.file, a.at.bytes.start, a.tier).cmp(&(b.at.file, b.at.bytes.start, b.tier))
        });
        for e in edges {
            if !seen.insert((e.from, e.at.file, e.at.bytes.start)) {
                continue;
            }
            let rec = index.file(e.at.file);
            let file = sources.file(e.at.file)?;
            let Some(ru) = trace_syntax::uses::result_use(rec.language, &file.bytes, e.at.bytes.start) else {
                continue;
            };
            rows.push(ResultUseRow {
                caller: index.symbol(e.from).uid.clone(),
                tier: e.tier.as_str(),
                call: ru.call.clone(),
                line: ru.line,
                uses: use_phrases(&ru),
            });
        }
        out.push(ResultUses {
            target: index.symbol(t).uid.clone(),
            callers: rows,
        });
    }
    Ok(out)
}

/// Group observed uses into phrases, at most [`MAX_USE_PHRASES`].
pub(crate) fn use_phrases(ru: &ResultUse) -> Vec<String> {
    let mut order: Vec<&UseKind> = Vec::new();
    let mut groups: HashMap<&UseKind, Vec<(u32, &str)>> = HashMap::new();
    for u in &ru.uses {
        if !groups.contains_key(&u.kind) {
            order.push(&u.kind);
        }
        groups.entry(&u.kind).or_default().push((u.line, u.code.as_str()));
    }
    let stored = || -> String {
        if ru.stored_in.is_empty() {
            format!("{} a structure", UseKind::StoredIn.phrase())
        } else {
            format!("{} {}", UseKind::StoredIn.phrase(), ru.stored_in.join(", "))
        }
    };
    let mut phrases: Vec<String> = order
        .into_iter()
        .map(|kind| {
            let sites = &groups[kind];
            match kind {
                UseKind::IgnoresResult => kind.phrase().to_string(),
                UseKind::StoredIn => stored(),
                _ => {
                    let (line, code) = sites[0];
                    let example = if code.is_empty() {
                        format!("line {line}")
                    } else {
                        format!("line {line}: {code}")
                    };
                    format!("{} ({}x, e.g. {example})", kind.phrase(), sites.len())
                }
            }
        })
        .collect();
    if phrases.is_empty() && !ru.stored_in.is_empty() {
        phrases.push(stored());
    }
    phrases.truncate(MAX_USE_PHRASES);
    phrases
}

#[cfg(test)]
#[path = "../../tests/unit/queries/impact.rs"]
mod tests;
