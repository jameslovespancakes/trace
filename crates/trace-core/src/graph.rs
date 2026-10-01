//! In-memory graph over an [`Index`]: all tiers materialized, CSR adjacency, tier views.
//!
//! Proven edges come from `Index::edges`. Inferred and possible edges are materialized from
//! `Index::sites` + `Index::decisions` (mapstore.py `view`, classify.py `augmented`):
//!
//! * decided targets                         -> `inferred_*` (tier inferred; provider
//!   `deterministic`, resolution `deterministic_unique`); a dispatch site decided by the
//!   receiver-type language rule
//!   ([`proven_by_receiver_rule`]) -> its activation kind, tier proven, provider
//!   `rule:receiver-type`, resolution `inheritance_rule`
//! * other candidates                        -> `possible_link` (tier possible)
//!
//! Decision targets are intersected with the site's candidates here as well (defence in
//! depth): a decision can never create an edge outside the site's candidate set.
//!
//! Bridges (`Index::bridges`) become `EdgeKind::Bridge` edges carrying the bridge's own tier
//! and `Edge::bridge = Some(i)`. They are traversed by every view whose tier admits them
//! unless disabled with [`Graph::set_bridges`] (library switch; the CLI always traverses them).
//! Tiers are never merged: every edge keeps its own kind, tier, provider and site.

use std::collections::HashMap;

use crate::error::Result;
use crate::model::{
    DecisionStatus, Edge, EdgeId, EdgeKind, FileId, Index, Provider, Resolution, Site, Symbol, SymbolId, Tier,
};
use crate::query::Direction;
use crate::tiers::{kinds_for, KindSet, FAMILY};

/// Upper bound of an override / implementation family (SPEC §7.10).
pub const MAX_FAMILY: usize = 64;

/// Override / implementation family of `start`: `start` first, then every symbol connected
/// to it by `overrides` / `implements` edges in either direction, transitively (BFS order,
/// ties by symbol id). At most `limit` members; the flag is true when members were cut off.
/// Family edges are proven facts stored in `Index::edges`.
pub(crate) fn family_closure(index: &Index, start: SymbolId, limit: usize) -> (Vec<SymbolId>, bool) {
    let mut adjacent: HashMap<SymbolId, Vec<SymbolId>> = HashMap::new();
    for e in index.edges.iter().filter(|e| FAMILY.contains(e.kind)) {
        adjacent.entry(e.from).or_default().push(e.to);
        adjacent.entry(e.to).or_default().push(e.from);
    }
    let mut members = vec![start];
    let mut truncated = false;
    let mut next = 0;
    while next < members.len() {
        let node = members[next];
        next += 1;
        let mut around = adjacent.get(&node).cloned().unwrap_or_default();
        around.sort_unstable();
        around.dedup();
        for other in around {
            if members.contains(&other) {
                continue;
            }
            if members.len() >= limit.max(1) {
                truncated = true;
                break;
            }
            members.push(other);
        }
    }
    (members, truncated)
}

/// How a member belongs to a target's family ([`target_family`], SPEC §7.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FamilyRelation {
    /// The queried symbol itself.
    Target,
    /// Linked by an `overrides` edge (either direction).
    Override,
    /// Linked by an `implements` edge (either direction): trait / interface / protocol /
    /// abstract member and its implementations, from language rules or the server
    /// (`textDocument/implementation`, type hierarchy).
    Implements,
    /// Linked by a `stub_implementation` edge: C/C++ prototype or forward declaration,
    /// Haskell type signature, TS `declare` / overload signature, `.pyi` stub.
    Declaration,
    /// Same name, callable, same declaring type (overloads; also property getter/setter
    /// pairs): parent type symbol equal, or both without a type parent and the same
    /// out-of-line container (`impl` type, Go receiver) in the same file.
    Overload,
}

impl FamilyRelation {
    pub const fn as_str(self) -> &'static str {
        match self {
            FamilyRelation::Target => "target",
            FamilyRelation::Override => "override",
            FamilyRelation::Implements => "implements",
            FamilyRelation::Declaration => "declaration",
            FamilyRelation::Overload => "overload",
        }
    }
}

/// One member of a target's family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FamilyMember {
    pub id: SymbolId,
    pub relation: FamilyRelation,
    /// The member it was reached from (`None` for the target).
    pub from: Option<SymbolId>,
}

/// The family of one target: the target first, then BFS order ([`target_family`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Family {
    pub members: Vec<FamilyMember>,
    /// Members were cut off at the limit (answers built on it are `unknown`, SPEC §10.1).
    pub truncated: bool,
}

impl Family {
    /// Member ids, target first.
    pub fn ids(&self) -> Vec<SymbolId> {
        self.members.iter().map(|m| m.id).collect()
    }

    pub fn contains(&self, id: SymbolId) -> bool {
        self.members.iter().any(|m| m.id == id)
    }

    pub fn relation(&self, id: SymbolId) -> Option<FamilyRelation> {
        self.members.iter().find(|m| m.id == id).map(|m| m.relation)
    }
}

/// Declaring type, else (file, out-of-line container) of a callable.
type OverloadKey = (Option<SymbolId>, Option<(FileId, String)>);

/// Overload key of a callable: its declaring type, else its out-of-line container in its
/// file (`None` for free functions, synthetic symbols and languages without overload
/// groups).
fn overload_key(index: &Index, s: &Symbol) -> Option<OverloadKey> {
    if !s.kind.is_callable() || s.is_synthetic() || !crate::languages::info(s.language).overload_groups {
        return None;
    }
    match s.parent {
        Some(p) if index.symbol(p).kind.is_type() => Some((Some(p), None)),
        Some(_) => None,
        None => s
            .container
            .as_ref()
            .filter(|c| !c.is_empty())
            .map(|c| (None, Some((s.file, c.clone())))),
    }
}

/// The complete family of `target` (SPEC §7.10): every symbol connected to it,
/// transitively, by `overrides` / `implements` edges (either direction),
/// `stub_implementation` links (declarations: prototypes, signatures, stubs) and overload
/// groups (same name, same declaring type). Target first, then BFS order with ties by symbol
/// id; at most `limit` members (`truncated` when more exist). Family edges are proven facts
/// in `Index::edges`; overload groups are computed from declarations.
///
/// This supersedes [`family_closure`] for `uses` (which only follows overrides /
/// implements); `family_closure` stays for traversal code that must not widen to overloads.
pub fn target_family(index: &Index, target: SymbolId, limit: usize) -> Family {
    let mut adjacent: HashMap<SymbolId, Vec<(SymbolId, FamilyRelation)>> = HashMap::new();
    for e in &index.edges {
        let relation = match e.kind {
            EdgeKind::Overrides => FamilyRelation::Override,
            EdgeKind::Implements => FamilyRelation::Implements,
            EdgeKind::StubImplementation => FamilyRelation::Declaration,
            _ => continue,
        };
        adjacent.entry(e.from).or_default().push((e.to, relation));
        adjacent.entry(e.to).or_default().push((e.from, relation));
    }
    let mut groups: HashMap<(OverloadKey, &str), Vec<SymbolId>> = HashMap::new();
    for s in &index.symbols {
        if let Some(key) = overload_key(index, s) {
            groups.entry((key, s.name.as_str())).or_default().push(s.id);
        }
    }
    let overloads = |id: SymbolId| -> Vec<SymbolId> {
        let s = index.symbol(id);
        overload_key(index, s)
            .and_then(|key| groups.get(&(key, s.name.as_str())))
            .map(|v| v.iter().copied().filter(|&o| o != id).collect())
            .unwrap_or_default()
    };
    let mut family = Family {
        members: vec![FamilyMember {
            id: target,
            relation: FamilyRelation::Target,
            from: None,
        }],
        truncated: false,
    };
    let mut next = 0;
    while next < family.members.len() {
        let node = family.members[next].id;
        next += 1;
        let mut around: Vec<(SymbolId, FamilyRelation)> = adjacent.get(&node).cloned().unwrap_or_default();
        around.extend(overloads(node).into_iter().map(|o| (o, FamilyRelation::Overload)));
        around.sort_unstable();
        around.dedup_by_key(|(id, _)| *id);
        for (other, relation) in around {
            if family.contains(other) {
                continue;
            }
            if family.members.len() >= limit.max(1) {
                family.truncated = true;
                break;
            }
            family.members.push(FamilyMember {
                id: other,
                relation,
                from: Some(node),
            });
        }
    }
    family
}

/// Edge counts per tier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct TierCounts {
    pub proven: usize,
    pub inferred: usize,
    pub possible: usize,
}

/// Read-only graph view of an index.
pub struct Graph<'a> {
    pub index: &'a Index,
    edges: Vec<Edge>,
    out_start: Vec<u32>,
    out_list: Vec<EdgeId>,
    in_start: Vec<u32>,
    in_list: Vec<EdgeId>,
    by_uid: HashMap<&'a str, SymbolId>,
    /// Kinds allowed in traversal views (all by default; `set_bridges(false)` removes
    /// `EdgeKind::Bridge`).
    mask: KindSet,
}

impl<'a> Graph<'a> {
    /// Materialize all tiers and build adjacency. O(V + E).
    pub fn new(index: &'a Index) -> Self {
        let site_edges: usize = index.sites.iter().map(|s| s.candidates.len()).sum();
        let mut edges: Vec<Edge> = Vec::with_capacity(index.edges.len() + site_edges);
        edges.extend(index.edges.iter().cloned());
        for (i, site) in index.sites.iter().enumerate() {
            materialize_site(&mut edges, i as u32, site, index);
        }
        for (i, b) in index.bridges.iter().enumerate() {
            edges.push(Edge {
                from: b.from,
                to: b.to,
                kind: EdgeKind::Bridge,
                tier: b.tier,
                provider: b.provider.clone(),
                resolution: b.resolution,
                at: b.from_at,
                site: None,
                bridge: Some(i as u32),
            });
        }
        let n = index.symbols.len();
        let (out_start, out_list) = csr(n, edges.iter().map(|e| e.from));
        let (in_start, in_list) = csr(n, edges.iter().map(|e| e.to));
        let by_uid = index.symbols.iter().map(|s| (s.uid.as_str(), s.id)).collect();
        Graph {
            index,
            edges,
            out_start,
            out_list,
            in_start,
            in_list,
            by_uid,
            mask: KindSet::ALL,
        }
    }

    /// Enable (default) or disable traversal of bridge edges in every view.
    pub fn set_bridges(&mut self, enabled: bool) {
        self.mask = if enabled {
            KindSet::ALL
        } else {
            KindSet(KindSet::ALL.0 & !EdgeKind::Bridge.bit())
        };
    }

    /// Whether bridge edges are traversed.
    pub fn bridges_enabled(&self) -> bool {
        self.mask.contains(EdgeKind::Bridge)
    }

    /// All materialized edges (every tier).
    #[inline]
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }
    #[inline]
    pub fn edge(&self, id: EdgeId) -> &Edge {
        &self.edges[id.idx()]
    }
    #[inline]
    pub fn symbol(&self, id: SymbolId) -> &'a Symbol {
        &self.index.symbols[id.idx()]
    }
    #[inline]
    pub fn file_path(&self, id: FileId) -> &'a str {
        &self.index.files[id.idx()].path
    }
    pub fn symbol_count(&self) -> usize {
        self.index.symbols.len()
    }

    /// Outgoing edges of `id` within the view at `include`.
    pub fn outgoing(&self, id: SymbolId, include: Tier) -> impl Iterator<Item = (EdgeId, &Edge)> + '_ {
        self.filtered(&self.out_start, &self.out_list, id, include)
    }

    /// Incoming edges of `id` within the view at `include`.
    pub fn incoming(&self, id: SymbolId, include: Tier) -> impl Iterator<Item = (EdgeId, &Edge)> + '_ {
        self.filtered(&self.in_start, &self.in_list, id, include)
    }

    /// Outgoing edges of any kind and tier (evidence listings include reference kinds).
    pub fn outgoing_all(&self, id: SymbolId) -> impl Iterator<Item = (EdgeId, &Edge)> + '_ {
        slice(&self.out_start, &self.out_list, id)
            .iter()
            .map(move |&e| (e, &self.edges[e.idx()]))
    }

    /// Incoming edges of any kind and tier.
    pub fn incoming_all(&self, id: SymbolId) -> impl Iterator<Item = (EdgeId, &Edge)> + '_ {
        slice(&self.in_start, &self.in_list, id)
            .iter()
            .map(move |&e| (e, &self.edges[e.idx()]))
    }

    /// Neighbours of `node` in `direction` within the view at `include`.
    pub(crate) fn neighbors(
        &self,
        node: SymbolId,
        direction: Direction,
        include: Tier,
    ) -> impl Iterator<Item = (EdgeId, SymbolId)> + '_ {
        let (start, list) = match direction {
            Direction::Forward => (&self.out_start, &self.out_list),
            Direction::Reverse => (&self.in_start, &self.in_list),
        };
        let kinds = KindSet(kinds_for(include).0 & self.mask.0);
        slice(start, list, node).iter().filter_map(move |&id| {
            let e = &self.edges[id.idx()];
            view_contains(kinds, include, e).then_some((
                id,
                match direction {
                    Direction::Forward => e.to,
                    Direction::Reverse => e.from,
                },
            ))
        })
    }

    fn filtered<'g>(
        &'g self,
        start: &'g [u32],
        list: &'g [EdgeId],
        id: SymbolId,
        include: Tier,
    ) -> impl Iterator<Item = (EdgeId, &'g Edge)> + 'g {
        let kinds = KindSet(kinds_for(include).0 & self.mask.0);
        slice(start, list, id)
            .iter()
            .map(move |&e| (e, &self.edges[e.idx()]))
            .filter(move |(_, e)| view_contains(kinds, include, e))
    }

    /// Override / implementation family of `start` ([`family_closure`], [`MAX_FAMILY`]).
    pub fn family(&self, start: SymbolId) -> (Vec<SymbolId>, bool) {
        family_closure(self.index, start, MAX_FAMILY)
    }

    /// Symbol by exact uid.
    pub fn symbol_by_uid(&self, uid: &str) -> Option<SymbolId> {
        self.by_uid.get(uid).copied()
    }

    /// Resolve a user reference (`path:Qualified.name`, `path:line`, exact uid, bare name).
    pub fn resolve(&self, reference: &str) -> Result<SymbolId> {
        crate::resolve::resolve(self.index, reference, |uid| self.symbol_by_uid(uid))
    }

    /// Edge counts per tier over all materialized edges (reference kinds count as proven).
    pub fn counts(&self) -> TierCounts {
        let mut c = TierCounts::default();
        for e in &self.edges {
            match e.tier {
                Tier::Proven => c.proven += 1,
                Tier::Inferred => c.inferred += 1,
                Tier::Possible => c.possible += 1,
            }
        }
        c
    }
}

#[inline]
fn view_contains(kinds: KindSet, include: Tier, edge: &Edge) -> bool {
    edge.tier <= include && kinds.contains(edge.kind)
}

/// Provider name of dispatch edges proven by the receiver's type (`rule:receiver-type`).
pub(crate) const RECEIVER_TYPE_RULE: &str = "receiver-type";

/// Whether `decision` decided dispatch site `site` by the receiver-type language rule: the
/// site's receiver type is proven ([`Site::receiver_exact`] on a dispatch site), its one
/// receiver-reached implementation (`flow_candidates`) is exactly the decided target. Such a
/// target is a proven edge (a language rule with a unique
/// match), never an inferred one.
pub fn proven_by_receiver_rule(site: &Site, decision: &crate::model::Decision) -> bool {
    site.category == crate::model::SiteCategory::Dispatch
        && site.receiver_exact
        && site.via.is_none()
        && decision.status == DecisionStatus::Decided
        && site.flow_candidates.len() == 1
        && decision.targets == site.flow_candidates
        && site.candidates.contains(&site.flow_candidates[0])
}

/// Append the inferred / possible edges of one site (proven for the receiver-type rule).
fn materialize_site(edges: &mut Vec<Edge>, i: u32, site: &Site, index: &Index) {
    let decision = index.decision(i);
    if let Some(d) = decision.filter(|d| proven_by_receiver_rule(site, d)) {
        let kind = if crate::tiers::EXECUTION.contains(site.activation) {
            site.activation
        } else {
            EdgeKind::Calls
        };
        for &t in &d.targets {
            edges.push(Edge {
                from: site.owner,
                to: t,
                kind,
                tier: kind.tier(),
                provider: Provider::Rule(RECEIVER_TYPE_RULE.into()),
                resolution: Resolution::InheritanceRule,
                at: site.at,
                site: Some(i),
                bridge: None,
            });
        }
        // The other implementations are proven not to run (the receiver's type is known).
        return;
    }
    let in_candidates = |t: &SymbolId| site.candidates.contains(t);
    let mut chosen: Vec<SymbolId> = match decision {
        Some(d) if d.status == DecisionStatus::Decided => {
            d.targets.iter().copied().filter(in_candidates).collect()
        }
        _ => Vec::new(),
    };
    chosen.sort_unstable();
    chosen.dedup();
    let make = |to: SymbolId, kind: EdgeKind, provider: Provider, resolution: Resolution| Edge {
        from: site.owner,
        to,
        kind,
        tier: kind.tier(),
        provider,
        resolution,
        at: site.at,
        site: Some(i),
        bridge: None,
    };
    let inferred_kind = if site.operation == Some(crate::model::SiteOperation::FieldWrite) {
        EdgeKind::InferredWrite
    } else {
        EdgeKind::inferred_for(site.category)
    };
    for &t in &chosen {
        edges.push(make(t, inferred_kind, Provider::Deterministic, Resolution::DeterministicUnique));
    }
    // Field writes are non-call uses: undecided ones never become (executable) possible
    // links.
    if inferred_kind == EdgeKind::InferredWrite {
        return;
    }
    let mut last = None;
    for &t in &site.candidates {
        if chosen.contains(&t) || last == Some(t) {
            continue;
        }
        last = Some(t);
        edges.push(make(t, EdgeKind::PossibleLink, Provider::Candidates, Resolution::GeneratedCandidate));
    }
}

fn slice<'s>(start: &[u32], list: &'s [EdgeId], id: SymbolId) -> &'s [EdgeId] {
    let i = id.idx();
    if i + 1 >= start.len() {
        return &[];
    }
    &list[start[i] as usize..start[i + 1] as usize]
}

/// Compressed sparse rows: `start[v]..start[v+1]` indexes `list` with the edges of `v`,
/// preserving edge order within a node. Keys out of range are ignored (validated indexes
/// never contain them).
fn csr(n: usize, keys: impl Iterator<Item = SymbolId> + Clone) -> (Vec<u32>, Vec<EdgeId>) {
    let mut start = vec![0u32; n + 1];
    for k in keys.clone() {
        if k.idx() < n {
            start[k.idx() + 1] += 1;
        }
    }
    for i in 0..n {
        start[i + 1] += start[i];
    }
    let mut fill = start.clone();
    let mut list = vec![EdgeId(0); start[n] as usize];
    for (e, k) in keys.enumerate() {
        if k.idx() >= n {
            continue;
        }
        let slot = &mut fill[k.idx()];
        list[*slot as usize] = EdgeId(e as u32);
        *slot += 1;
    }
    (start, list)
}

#[cfg(test)]
#[path = "../tests/unit/graph.rs"]
mod tests;
