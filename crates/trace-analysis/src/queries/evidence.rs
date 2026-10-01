//! The `evidence` block of `uses`: every
//! incoming and outgoing link of the symbol across ALL tiers and kinds (incl. `references`
//! and `passes_callback`), each with tier, provider, resolution, location, the exact source
//! line and — for inferred and possible links — the site decision (status and the rule's
//! reason). Plus the sites owned by the symbol with their candidates and
//! decisions, and unresolved call sites owned by the symbol. Unknown means not resolved,
//! never assumed safe.
//!
//! Bridge links (`bridge:<kind>`) carry both ends (`EdgeRow::bridge`: label, languages, the
//! other side's evidence span, assumptions, contract file, candidate count). Family links
//! (`overrides` / `implements`) are listed like every other kind.
//!
//! Rows are ordered by tier (proven, inferred, possible), then location, then the other
//! symbol's uid, so provenance listings are stable across runs.

use trace_core::model::{DecisionStatus, Edge};
use trace_core::source::SourceStore;
use trace_core::{Graph, Index, SymbolId};

use crate::cards::{at, card, decision_info, edge_row, unresolved_row};
use crate::report::{DecisionInfo, EvidenceRow, SiteRow, UsesEvidence};

/// Provenance of `id`'s own links (module docs).
pub(crate) fn evidence_of(graph: &Graph<'_>, sources: &SourceStore<'_>, id: SymbolId) -> UsesEvidence {
    let index = graph.index;
    let row = |e: &Edge, other: SymbolId| EvidenceRow {
        edge: edge_row(graph, sources, e),
        other: card(index, index.symbol(other)),
    };
    let mut outgoing: Vec<(&Edge, EvidenceRow)> =
        graph.outgoing_all(id).map(|(_, e)| (e, row(e, e.to))).collect();
    let mut incoming: Vec<(&Edge, EvidenceRow)> =
        graph.incoming_all(id).map(|(_, e)| (e, row(e, e.from))).collect();
    let order = |a: &(&Edge, EvidenceRow), b: &(&Edge, EvidenceRow)| {
        (a.0.tier, a.0.at.file, a.0.at.bytes.start, &a.1.other.id, a.0.kind).cmp(&(
            b.0.tier,
            b.0.at.file,
            b.0.at.bytes.start,
            &b.1.other.id,
            b.0.kind,
        ))
    };
    outgoing.sort_by(order);
    incoming.sort_by(order);
    let sites = site_rows(index, id);
    let unresolved = index
        .unresolved
        .iter()
        .filter(|u| u.owner == Some(id))
        .map(|u| unresolved_row(index, sources, u))
        .collect();
    UsesEvidence {
        incoming: incoming.into_iter().map(|(_, r)| r).collect(),
        outgoing: outgoing.into_iter().map(|(_, r)| r).collect(),
        sites,
        unresolved,
    }
}

/// Sites owned by `owner`, with candidates and decisions.
fn site_rows(index: &Index, owner: SymbolId) -> Vec<SiteRow> {
    index
        .sites
        .iter()
        .enumerate()
        .filter(|(_, s)| s.owner == owner)
        .map(|(i, s)| {
            let i = i as u32;
            let decision = decision_info(index, i).unwrap_or_else(|| DecisionInfo {
                site: s.id.0.clone(),
                category: s.category.as_str(),
                status: "unknown",
                reason: Some("no decision recorded".into()),
            });
            let decided_targets = index
                .decision(i)
                .filter(|d| d.status == DecisionStatus::Decided)
                .map(|d| d.targets.iter().map(|&t| index.symbol(t).uid.clone()).collect())
                .unwrap_or_default();
            SiteRow {
                id: s.id.0.clone(),
                category: s.category.as_str(),
                operation: s.operation.map(|o| o.as_str()),
                callee: s.callee.clone(),
                at: at(index, &s.at),
                candidates: s.candidates.iter().map(|&c| index.symbol(c).uid.clone()).collect(),
                truncated_candidates: s.truncated_candidates,
                decision,
                decided_targets,
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "../../tests/unit/queries/evidence.rs"]
mod tests;
