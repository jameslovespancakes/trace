//! Site decisions: [`decide`] settles every edge-case site from its own evidence, one
//! decision per site. A decided site's targets become inferred edges (proven for the
//! receiver-type rule, `trace_core::graph::proven_by_receiver_rule`); an undecided site's
//! options stay possible (`check:`). The rules, in order, over the site's [`options`]:
//!
//! * a callback into a library call: decided when the library runs the passed function with
//!   an inferred fact ([`library_runs`]), else undecided;
//! * a dispatch site decided by its receiver evidence ([`dispatch_by_receiver`]);
//! * a [`receiver_exact`] flow site: every option;
//! * value flow reaching exactly one option ([`FLOW_UNIQUE_REASON`]);
//! * a dispatch through a library-declared member without receiver evidence: undecided;
//! * exactly one option (unless it is field-name evidence only); an implicit data-model site
//!   whose options are one class's protocol; else undecided.

use trace_core::model::{Decision, DecisionStatus, Site, SiteCategory, SymbolId};
use trace_core::Index;

/// One decision per site of `index`, in site order.
pub fn decide(index: &Index) -> Vec<Decision> {
    index
        .sites
        .iter()
        .enumerate()
        .map(|(i, site)| decide_site(site, i as u32, &|opts| one_protocol(index, opts)))
        .collect()
}

/// Indices (into `site.candidates`) of the options that may be decided: every candidate
/// except test-only ones.
pub fn options(site: &Site) -> Vec<usize> {
    (0..site.candidates.len())
        .filter(|&i| !site.test_only.contains(&site.candidates[i]))
        .collect()
}

fn undecided(site_index: u32, reason: String) -> Decision {
    Decision {
        site: site_index,
        status: DecisionStatus::Unknown,
        targets: Vec::new(),
        reason: Some(reason),
    }
}

/// Reason of a library callback site the library behaviour does not decide: no derived
/// fact, declared function type or table row says the library runs the passed function
/// (`never_calls`, a non-function / top parameter type, a fact below the precision gate, no
/// evidence). The site stays possible (`check:`).
pub(crate) const LIBRARY_UNKNOWN_REASON: &str = "library parameter is not known to run it";

/// Reason prefix of a library callback site decided by library behaviour (inferred, never
/// proven): the library runs the passed function (derived with the gate passed, a declared
/// function type, or a table row).
pub(crate) const LIBRARY_RUNS_REASON: &str = "library runs the passed function";

/// Library gate (DESIGN §1.10 item 6, §1.10a) of a callback site whose receiving call is a
/// library call (`Site::library`): `Some(true)` when the library behaviour runs the passed
/// function with an inferred fact, `Some(false)` when it does not (or not with an inferred
/// fact: below the precision gate, `never_calls`, no evidence), `None` for sites without
/// library behaviour (in-index callees keep the flow and language rules).
pub(crate) fn library_runs(site: &Site) -> Option<bool> {
    if site.category != SiteCategory::Callback {
        return None;
    }
    let b = site.library.as_ref()?;
    Some(b.inferred && crate::behaviour::runs_name(&b.effect))
}

/// Reason of a dispatch site decided by the receiver-type language rule (the receiver's
/// type is proven; materialized as a proven edge, `trace_core::graph::proven_by_receiver_rule`).
pub(crate) const RECEIVER_TYPE_PROVEN_REASON: &str =
    "the receiver's type is proven: exactly one implementation runs";

/// Reason of a dispatch site decided by receiver evidence (the receiver's type narrows the
/// implementations, or value flow reaches specific receivers): inferred, one edge per
/// implementation the receivers reach.
pub(crate) const RECEIVER_TYPES_REASON: &str = "the receiver's types reach these implementations";

/// Reason of an undecided dispatch through a member declared in a library: nothing is known
/// about the receiver, and the library may provide implementations of its own.
pub(crate) const LIBRARY_DISPATCH_REASON: &str = "library-declared member: receiver type unknown";

/// The decision a dispatch site's receiver evidence makes (I-01): the proven implementation
/// (`Site::receiver_exact`, one `flow_candidates` entry) -> ([t], proven reason); the
/// implementations the receiver evidence reaches (`flow_candidates` among the options,
/// strong, complete candidate set) -> (those, inferred reason); else `None`.
pub(crate) fn dispatch_by_receiver(site: &Site) -> Option<(Vec<SymbolId>, &'static str)> {
    if site.category != SiteCategory::Dispatch {
        return None;
    }
    let opts: Vec<SymbolId> = options(site).into_iter().map(|i| site.candidates[i]).collect();
    if site.receiver_exact {
        return match site.flow_candidates.as_slice() {
            [t] if opts.contains(t) => Some((vec![*t], RECEIVER_TYPE_PROVEN_REASON)),
            _ => None,
        };
    }
    if site.truncated_candidates {
        return None;
    }
    let reached: Vec<SymbolId> = opts
        .iter()
        .copied()
        .filter(|t| site.flow_candidates.contains(t) && !site.field_only.contains(t))
        .collect();
    (!reached.is_empty()).then_some((reached, RECEIVER_TYPES_REASON))
}

/// Every option is a method of one class (one receiver type's data-model protocol).
fn one_protocol(index: &Index, opts: &[SymbolId]) -> bool {
    let Some((first, rest)) = opts.split_first() else {
        return false;
    };
    let class = owning_class(index, *first);
    class.is_some() && rest.iter().all(|&m| owning_class(index, m) == class)
}

const UNIQUE_REASON: &str = "exactly one candidate";

/// Reason of [`unique_flow_target`] decisions.
pub(crate) const FLOW_UNIQUE_REASON: &str = "value flow reaches exactly one candidate";

/// An unresolved call whose name pool has several options but whose value flow (the
/// receiver's tracked values, e.g. a parameter injected by name) reaches exactly one of them
/// with specific (not field-name-only) evidence and a complete (unbounded) candidate set:
/// that option, decided like a unique candidate (inferred, never proven).
fn unique_flow_target(site: &Site, opts: &[SymbolId]) -> Option<SymbolId> {
    if site.category != SiteCategory::NoTarget || opts.len() < 2 || site.truncated_candidates {
        return None;
    }
    let mut strong = site
        .flow_candidates
        .iter()
        .filter(|t| opts.contains(t) && !site.field_only.contains(t));
    match (strong.next(), strong.next()) {
        (Some(&t), None) => Some(t),
        _ => None,
    }
}

/// Owning class of a method candidate (lexical parent that is a type).
fn owning_class(index: &Index, id: SymbolId) -> Option<SymbolId> {
    index.symbol(id).parent.filter(|&p| index.symbol(p).kind.is_type())
}

/// Whether the site is decided by its receiver contexts alone: a flow call whose every
/// receiver context resolves to at most one strong target ([`Site::receiver_exact`]), with
/// options and no field-only option. Dispatch sites use the flag for a proven receiver type
/// instead ([`dispatch_by_receiver`]).
pub fn receiver_exact(site: &Site) -> bool {
    site.category != SiteCategory::Dispatch
        && site.receiver_exact
        && site.field_only.is_empty()
        && !options(site).is_empty()
}

/// The decision of one site (`site_index` = its position in `Index::sites`); `one_protocol`
/// answers whether options are one class's data-model protocol (the only rule that reads the
/// index).
fn decide_site(site: &Site, site_index: u32, one_protocol: &dyn Fn(&[SymbolId]) -> bool) -> Decision {
    let opts: Vec<SymbolId> = options(site).into_iter().map(|i| site.candidates[i]).collect();
    let decided = |targets: Vec<SymbolId>, reason: &str| Decision {
        site: site_index,
        status: DecisionStatus::Decided,
        targets,
        reason: Some(reason.into()),
    };
    // Library callback sites: decided only when the library runs the passed function with
    // an inferred fact; otherwise possible (`check:`).
    if !opts.is_empty() {
        match (library_runs(site), site.library.as_ref()) {
            (Some(true), Some(b)) => {
                let reason = format!("{LIBRARY_RUNS_REASON}: {}", crate::behaviour::describe(b));
                return decided(opts.clone(), &reason);
            }
            (Some(_), _) => return undecided(site_index, LIBRARY_UNKNOWN_REASON.into()),
            _ => {}
        }
    }
    if let Some((targets, reason)) = dispatch_by_receiver(site) {
        return decided(targets, reason);
    }
    if receiver_exact(site) {
        return decided(opts.clone(), "one target per receiver context");
    }
    if let Some(t) = unique_flow_target(site, &opts) {
        return decided(vec![t], FLOW_UNIQUE_REASON);
    }
    // A member declared in a library may run the library's own implementations: one
    // repository candidate is no unique target without receiver evidence.
    if site.category == SiteCategory::Dispatch
        && site.declared_target.is_none()
        && site.declared_library.is_some()
        && !opts.is_empty()
    {
        return undecided(site_index, LIBRARY_DISPATCH_REASON.into());
    }
    match opts.as_slice() {
        [only] if site.field_only.contains(only) => undecided(site_index, "only field-name evidence".into()),
        [only] => decided(vec![*only], UNIQUE_REASON),
        [] if !site.candidates.is_empty() => undecided(site_index, "only test-origin candidates".into()),
        [_, _, ..]
            if site.category == SiteCategory::Implicit
                && one_protocol(&opts)
                && !opts.iter().any(|o| site.field_only.contains(o)) =>
        {
            decided(opts.clone(), "one receiver type's data-model protocol")
        }
        _ => undecided(site_index, "abstain: not unique".into()),
    }
}

#[cfg(test)]
#[path = "../tests/unit/decide.rs"]
mod tests;
