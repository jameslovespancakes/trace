//! Tier views: which edge kinds each tier level traverses (the CLI uses `inferred`, and
//! `possible` with `--deep`).
//!
//! The kinds per tier:
//!
//! | include   | kinds                                               |
//! |-----------|-----------------------------------------------------|
//! | proven    | EXECUTION                                           |
//! | inferred  | EXECUTION + DEFERRED + INFERRED                     |
//! | possible  | EXECUTION + DEFERRED + INFERRED + POSSIBLE          |
//!
//! Reference kinds (`references`, `passes_callback`, `writes`, `imports`, `reexports`) and
//! family kinds (`overrides`, `implements`) are never traversed by execution views; they are
//! read by `uses` (rows, `--deep` family expansion and other references, evidence).
//! `bridge` edges are traversed by every view whose tier admits the bridge's own tier
//! (`Graph::set_bridges(false)` removes them; library switch).

use crate::model::{Edge, EdgeKind, Tier};

/// Bit set over [`EdgeKind`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct KindSet(pub u32);

impl KindSet {
    pub const EMPTY: KindSet = KindSet(0);
    /// Every kind.
    pub const ALL: KindSet = KindSet(u32::MAX);

    pub const fn of(kinds: &[EdgeKind]) -> KindSet {
        let mut bits = 0u32;
        let mut i = 0;
        while i < kinds.len() {
            bits |= kinds[i].bit();
            i += 1;
        }
        KindSet(bits)
    }
    #[inline]
    pub const fn contains(self, kind: EdgeKind) -> bool {
        self.0 & kind.bit() != 0
    }
    #[inline]
    pub const fn union(self, other: KindSet) -> KindSet {
        KindSet(self.0 | other.0)
    }
    pub fn iter(self) -> impl Iterator<Item = EdgeKind> {
        EdgeKind::ALL.into_iter().filter(move |k| self.contains(*k))
    }
}

pub const EXECUTION: KindSet = KindSet::of(&[
    EdgeKind::Calls,
    EdgeKind::Constructor,
    EdgeKind::InvokedCallback,
    EdgeKind::PropertyGet,
    EdgeKind::Awaits,
    EdgeKind::Iterates,
    EdgeKind::StubImplementation,
]);

/// Created generators/coroutines run when consumed: included only from `inferred` upward.
pub const DEFERRED: KindSet = KindSet::of(&[EdgeKind::CreatesGenerator, EdgeKind::CreatesCoroutine]);

pub const INFERRED: KindSet = KindSet::of(&[
    EdgeKind::InferredDispatch,
    EdgeKind::InferredCall,
    EdgeKind::InferredCallback,
    EdgeKind::InferredImplicit,
]);

pub(crate) const POSSIBLE: KindSet = KindSet::of(&[EdgeKind::PossibleLink]);

pub const REFERENCE: KindSet = KindSet::of(&[
    EdgeKind::References,
    EdgeKind::PassesCallback,
    EdgeKind::Writes,
    EdgeKind::Imports,
    EdgeKind::Reexports,
]);

/// Non-call uses decided from value flow (tier inferred; never traversed by execution
/// views, listed with the proven [`REFERENCE`] kinds as other references).
pub(crate) const INFERRED_REFERENCE: KindSet = KindSet::of(&[EdgeKind::InferredWrite]);

/// Every non-call use kind (proven and inferred).
pub const USES: KindSet = REFERENCE.union(INFERRED_REFERENCE);

/// Override / implementation families (impact, references and path include the family of
/// a target by default; never traversed as execution).
pub const FAMILY: KindSet = KindSet::of(&[EdgeKind::Overrides, EdgeKind::Implements]);

/// Cross-language bridges (tier per edge).
pub(crate) const BRIDGE: KindSet = KindSet::of(&[EdgeKind::Bridge]);

/// Kinds traversed by a view at `include` (bridges included; each bridge edge still needs
/// `edge.tier <= include`).
pub const fn kinds_for(include: Tier) -> KindSet {
    match include {
        Tier::Proven => EXECUTION.union(BRIDGE),
        Tier::Inferred => EXECUTION.union(DEFERRED).union(INFERRED).union(BRIDGE),
        Tier::Possible => EXECUTION
            .union(DEFERRED)
            .union(INFERRED)
            .union(POSSIBLE)
            .union(BRIDGE),
    }
}

/// Whether `edge` is part of the view at `include`.
#[inline]
pub fn in_view(edge: &Edge, include: Tier) -> bool {
    edge.tier <= include && kinds_for(include).contains(edge.kind)
}

#[cfg(test)]
#[path = "../tests/unit/tiers.rs"]
mod tests;
