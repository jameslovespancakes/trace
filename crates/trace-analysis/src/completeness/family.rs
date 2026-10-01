//! The target family: its names, and the family candidate / flow edges of `uses` (call sites
//! whose every candidate, or every value-flow target, is a family member).

use std::collections::{BTreeSet, HashSet};

use trace_core::model::{
    DecisionStatus, Edge, EdgeKind, Provider, Resolution, SiteCategory, SiteOperation, SymbolId,
    UnresolvedKind,
};
use trace_core::tiers::FAMILY;
use trace_core::{Index, SymbolKind, Tier};

use crate::cards::executing_symbol_at;

/// Names searched for a family: member names plus, for constructors (`__init__`,
/// `constructor`, constructor declarations), the class name used at call sites.
pub(crate) fn family_names(index: &Index, family: &[SymbolId]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for &m in family {
        let s = index.symbol(m);
        if s.is_synthetic() {
            continue;
        }
        names.insert(s.name.clone());
        let constructor =
            s.kind == SymbolKind::Constructor || matches!(s.name.as_str(), "__init__" | "constructor");
        if constructor {
            if let Some(p) = s.parent.map(|p| index.symbol(p)).filter(|p| p.kind.is_type()) {
                names.insert(p.name.clone());
            }
        }
    }
    names
}

/// Reason of [`family_candidate_edges`] rows reached by value flow.
pub(crate) const FAMILY_FLOW_REASON: &str = "value flow reaches only members of this override family";
/// Reason of [`family_candidate_edges`] rows whose candidates are all family members.
pub(crate) const FAMILY_OVERLOAD_REASON: &str = "every candidate is a member of this family";

/// Query-time family rows (SPEC §10.3), tier inferred, never stored in the index (`deps`
/// and `context` never see them). One synthetic `inferred_call` edge per site, provider
/// deterministic, `to` = the first candidate:
/// * (a) value flow (resolution `deterministic_unique`, shown as `family_flow`): a
///   `no_target` / `flow` call site, not composed, candidate set not truncated, no decision
///   already choosing a family member, every flow candidate a family member and at least one
///   of them with specific (not field-name-only) evidence — e.g. a fixture instance whose
///   attribute holds one override in one test and another override elsewhere;
/// * (b) candidates (resolution `family_overloads`): an undecided `no_target` / `flow` /
///   `dispatch` call site whose candidates minus field-only ones are non-empty, not
///   truncated, all family members, and with no proven edge at the same span (overloads
///   `Clone()` / `Clone(Settings)` of one type);
/// * (c) server ambiguity (resolution `family_overloads`): an `external_or_ambiguous`
///   unresolved entry with at least 2 in-index candidates, all family members (the server
///   named several members of the family, e.g. overloads).
///
/// `include = proven` yields nothing.
pub(crate) fn family_candidate_edges(index: &Index, family: &HashSet<SymbolId>, include: Tier) -> Vec<Edge> {
    let mut out = family_flow_edges(index, family, include);
    if include == Tier::Proven || family.is_empty() {
        return out;
    }
    let proven_at: HashSet<(u32, u32)> = index
        .edges
        .iter()
        .filter(|e| e.tier == Tier::Proven && !FAMILY.contains(e.kind))
        .map(|e| (e.at.file.0, e.at.bytes.end))
        .collect();
    let mut seen: HashSet<(u32, u32)> = out.iter().map(|e| (e.at.file.0, e.at.bytes.end)).collect();
    let edge = |from: SymbolId, to: SymbolId, at: trace_core::Location, site: Option<u32>| Edge {
        from,
        to,
        kind: EdgeKind::InferredCall,
        tier: Tier::Inferred,
        provider: Provider::Deterministic,
        resolution: Resolution::FamilyOverloads,
        at,
        site,
        bridge: None,
    };
    for (i, s) in index.sites.iter().enumerate() {
        if !matches!(s.category, SiteCategory::NoTarget | SiteCategory::Flow | SiteCategory::Dispatch)
            || !matches!(
                s.operation,
                None | Some(SiteOperation::Call) | Some(SiteOperation::OverrideDispatch)
            )
            || s.via.is_some()
            || s.truncated_candidates
        {
            continue;
        }
        if index
            .decision(i as u32)
            .is_some_and(|d| d.status == DecisionStatus::Decided)
        {
            continue;
        }
        let candidates: Vec<SymbolId> = s
            .candidates
            .iter()
            .copied()
            .filter(|c| !s.field_only.contains(c))
            .collect();
        if candidates.is_empty() || !candidates.iter().all(|c| family.contains(c)) {
            continue;
        }
        let key = (s.at.file.0, s.at.bytes.end);
        if proven_at.contains(&key) || !seen.insert(key) {
            continue;
        }
        out.push(edge(s.owner, candidates[0], s.at, Some(i as u32)));
    }
    for u in &index.unresolved {
        if u.kind != UnresolvedKind::ExternalOrAmbiguous
            || u.candidates.len() < 2
            || !u.candidates.iter().all(|c| family.contains(c))
        {
            continue;
        }
        let key = (u.at.file.0, u.at.bytes.end);
        if proven_at.contains(&key) || seen.contains(&key) {
            continue;
        }
        let Some(from) = u
            .owner
            .or_else(|| executing_symbol_at(index, u.at.file, u.at.bytes.start))
        else {
            continue;
        };
        seen.insert(key);
        out.push(edge(from, u.candidates[0], u.at, None));
    }
    out
}

/// The `resolution` label of a [`family_candidate_edges`] row.
pub(crate) fn family_resolution(e: &Edge) -> &'static str {
    if e.resolution == Resolution::FamilyOverloads {
        "family_overloads"
    } else {
        "family_flow"
    }
}

/// Rule (a) of [`family_candidate_edges`]: sites whose value flow reaches only family
/// members.
pub(crate) fn family_flow_edges(index: &Index, family: &HashSet<SymbolId>, include: Tier) -> Vec<Edge> {
    let mut out = Vec::new();
    if include == Tier::Proven || family.is_empty() {
        return out;
    }
    for (i, s) in index.sites.iter().enumerate() {
        if !matches!(s.category, SiteCategory::NoTarget | SiteCategory::Flow)
            || !matches!(s.operation, None | Some(SiteOperation::Call))
            || s.via.is_some()
            || s.truncated_candidates
            || s.flow_candidates.is_empty()
        {
            continue;
        }
        if !s.flow_candidates.iter().all(|c| family.contains(c)) {
            continue;
        }
        if s.flow_candidates.iter().all(|c| s.field_only.contains(c)) {
            continue;
        }
        let decided_member = index.decision(i as u32).is_some_and(|d| {
            d.status == DecisionStatus::Decided && d.targets.iter().any(|t| family.contains(t))
        });
        if decided_member {
            continue;
        }
        out.push(Edge {
            from: s.owner,
            to: s.flow_candidates[0],
            kind: EdgeKind::InferredCall,
            tier: Tier::Inferred,
            provider: Provider::Deterministic,
            resolution: Resolution::DeterministicUnique,
            at: s.at,
            site: Some(i as u32),
            bridge: None,
        });
    }
    out
}

// ------------------------------------------------------------------ occurrences

/// A declaration named through a value inside a function body: its parent is a callable and
/// its qualified name has a qualifier segment between the parent and the name
/// (`internal.colorscheme.picker.set_selection` for `picker.set_selection = function` in
/// `internal.colorscheme`).
pub(super) fn stores_into_value(index: &Index, id: SymbolId) -> bool {
    let s = index.symbol(id);
    let Some(p) = s.parent.map(|p| index.symbol(p)) else {
        return false;
    };
    if !p.kind.is_callable() || s.is_synthetic() {
        return false;
    }
    let direct = format!("{}.{}", p.qualified_name, s.name);
    s.qualified_name != direct
        && s.qualified_name.starts_with(&format!("{}.", p.qualified_name))
        && s.qualified_name.ends_with(&format!(".{}", s.name))
}
