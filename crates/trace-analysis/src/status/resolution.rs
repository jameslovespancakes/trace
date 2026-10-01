//! Resolution health per language: resolved / unresolved in-repository calls, server state
//! and missing servers.

use std::collections::{BTreeMap, HashMap, HashSet};

use trace_core::facts::FileFacts;
use trace_core::languages::same_module_namespace;
use trace_core::model::{DecisionStatus, UnresolvedKind};
use trace_core::tiers::{DEFERRED, EXECUTION, INFERRED};
use trace_core::{Index, Language, SupportLevel};
use trace_semantic::setup::SetupRow;

use crate::report::ResolutionHealth;

/// Language namespace of the in-repository rate, named by its first language in language
/// order (JS / TS / TSX share one, C / C++ one: `languages::same_module_namespace`).
pub(super) fn call_namespace(language: Language) -> Language {
    Language::ALL
        .into_iter()
        .find(|&l| same_module_namespace(l, language))
        .unwrap_or(language)
}

pub(super) fn rate_of(resolved: usize, unresolved: usize) -> Option<f64> {
    let total = resolved + unresolved;
    (total > 0).then(|| (resolved as f64 / total as f64 * 1000.0).round() / 1000.0)
}

/// Server state of `language` from the last backend run serving it (module docs).
fn server_state(index: &Index, language: Language) -> Option<&'static str> {
    let run = index
        .backend_runs
        .iter()
        .rev()
        .find(|r| r.languages.contains(&language))?;
    if !run.ok {
        Some("server_failed")
    } else if run.ready == Some(false) {
        Some("server_not_ready")
    } else {
        None
    }
}

/// `server_missing` for languages whose setup row says the server is not installed (and
/// that have no other server state); languages without call sites get a row of their own so
/// the state is never lost.
pub(crate) fn mark_missing_servers(resolution: &mut Vec<ResolutionHealth>, setup: &[SetupRow]) {
    for row in setup.iter().filter(|r| r.error_type == Some("server_missing")) {
        match resolution.iter_mut().find(|r| r.language == row.language) {
            Some(r) => {
                if r.server.is_none() {
                    r.server = Some("server_missing");
                }
            }
            None => resolution.push(ResolutionHealth {
                language: row.language,
                support: SupportLevel::Semantic,
                resolved: 0,
                unresolved: 0,
                rate: None,
                warning: None,
                in_repo_resolved: 0,
                in_repo_unresolved: 0,
                in_repo_rate: None,
                server: Some("server_missing"),
                by_name: 0,
            }),
        }
    }
    resolution.sort_by_key(|r| r.language);
}

/// How a recorded span answered a call (resolution buckets of [`resolution_health`] and
/// [`unresolved_dump`]), weakest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum Answer {
    /// Only a decided inference site with targets (`Index::decisions`: the inferred edges
    /// are materialised from them, never stored in `Index::edges`).
    Inferred,
    /// Only an `external_or_ambiguous` entry without candidates (no location recorded).
    External,
    /// Resolved elsewhere by a language / machine rule at the call's member identifier
    /// (`FileSemantics::resolved_elsewhere`: a conversion to a type outside the index, an
    /// external program found on this machine): external, never "by name".
    Elsewhere,
    /// A library call (the server located the target in installed library code).
    Library,
    /// An execution edge into the repository.
    Edge,
}

/// The calls of one file that recorded spans resolve (module docs): per call index, the
/// strongest answer. `spans` = (start, end, answer), any order, duplicates allowed.
pub(super) fn resolved_calls(
    calls: &[trace_core::facts::CallSite],
    spans: &[(u32, u32, Answer)],
) -> HashMap<usize, Answer> {
    match_calls(calls, spans)
}

/// The call each recorded span `(start, end, value)` designates (module docs: the call whose
/// callee ends at `end`, else the innermost call containing a whole-invocation span whose
/// callee starts or ends inside it); per call index the greatest value.
pub(super) fn match_calls<T: Copy + Ord + std::hash::Hash>(
    calls: &[trace_core::facts::CallSite],
    spans: &[(u32, u32, T)],
) -> HashMap<usize, T> {
    let mut out: HashMap<usize, T> = HashMap::new();
    if calls.is_empty() || spans.is_empty() {
        return out;
    }
    let mut by_end: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, c) in calls.iter().enumerate() {
        by_end.entry(c.callee_span.end).or_default().push(i);
    }
    // Calls sorted by the start of their whole expression (innermost-containing search).
    let mut by_start: Vec<usize> = (0..calls.len()).collect();
    by_start.sort_by_key(|&i| (calls[i].span.start, std::cmp::Reverse(calls[i].span.end)));
    fn claim<T: Copy + Ord>(i: usize, answer: T, out: &mut HashMap<usize, T>) {
        let slot = out.entry(i).or_insert(answer);
        if answer > *slot {
            *slot = answer;
        }
    }
    let mut seen: HashSet<(u32, u32, T)> = HashSet::new();
    for &(start, end, answer) in spans {
        if !seen.insert((start, end, answer)) {
            continue;
        }
        // 1. The span ends where callees end (member identifier, or the whole callee).
        if let Some(exact) = by_end.get(&end) {
            for &i in exact {
                claim(i, answer, &mut out);
            }
            continue;
        }
        // 2. A whole-invocation span: the innermost call containing it whose callee starts
        //    or ends inside it (bounded backward scan over the calls starting before it).
        let inside = |p: u32| start <= p && p <= end;
        let upto = by_start.partition_point(|&i| calls[i].span.start <= start);
        let found = by_start[..upto].iter().rev().take(64).copied().find(|&i| {
            let c = &calls[i];
            c.span.end >= end && (inside(c.callee_span.start) || inside(c.callee_span.end))
        });
        if let Some(i) = found {
            claim(i, answer, &mut out);
        }
    }
    out
}

/// The `resolved_elsewhere` spans of a file that end where one of its callees ends (calls the
/// engine resolved elsewhere at their member identifier), as [`Answer::Elsewhere`] spans.
/// Other listed spans are references (a receiver module, an argument), never a call.
pub(super) fn elsewhere_calls(
    facts: Option<&FileFacts>,
    resolved_elsewhere: &[trace_core::model::ByteSpan],
) -> Vec<(u32, u32, Answer)> {
    let Some(facts) = facts else {
        return Vec::new();
    };
    if resolved_elsewhere.is_empty() {
        return Vec::new();
    }
    let ends: HashSet<u32> = facts.calls.iter().map(|c| c.callee_span.end).collect();
    resolved_elsewhere
        .iter()
        .filter(|s| ends.contains(&s.end))
        .map(|s| (s.start, s.end, Answer::Elsewhere))
        .collect()
}

/// (file, start, end) of every site with a decision naming targets: the inferred (or
/// receiver-rule proven) edges materialised from the decisions at query time.
pub(super) fn decided_sites(index: &Index) -> Vec<(u32, u32, u32)> {
    index
        .decisions
        .iter()
        .filter(|d| d.status == DecisionStatus::Decided && !d.targets.is_empty())
        .filter_map(|d| index.sites.get(d.site as usize))
        .map(|s| (s.at.file.0, s.at.bytes.start, s.at.bytes.end))
        .collect()
}

/// Spans of `inactive_code` entries per file (calls in preprocessor regions the build does
/// not compile on this machine).
pub(super) fn inactive_calls(index: &Index) -> HashMap<u32, Vec<(u32, u32, bool)>> {
    let mut out: HashMap<u32, Vec<(u32, u32, bool)>> = HashMap::new();
    for u in index
        .unresolved
        .iter()
        .filter(|u| u.kind == UnresolvedKind::InactiveCode)
    {
        out.entry(u.at.file.0)
            .or_default()
            .push((u.at.bytes.start, u.at.bytes.end, true));
    }
    out
}

/// Whether the member identifier of `c` is a local binding syntax proves (the engine's
/// local-call rule: such calls are recorded external without a request).
pub(super) fn is_local_call(facts: &FileFacts, c: &trace_core::facts::CallSite) -> bool {
    let start = match &c.member {
        Some(m) if c.callee.ends_with(m.as_str()) && m.len() as u32 <= c.callee_span.len() => {
            c.callee_span.end - m.len() as u32
        }
        _ => c.callee_span.start,
    };
    facts.is_local(trace_core::model::ByteSpan::new(start, c.callee_span.end))
}

/// Resolved vs unresolved call sites per language over analysed files, raw and
/// in-repository, plus the calls resolved by name only (module docs). A semantic language
/// whose in-repository rate is below `warn` gets the `low_resolution` warning.
pub(crate) fn resolution_health(index: &Index, warn: f64) -> Vec<ResolutionHealth> {
    // Proven and inferred targets resolve a call; `possible` candidates never do.
    let kinds = EXECUTION.union(DEFERRED).union(INFERRED);
    let mut spans: HashMap<u32, Vec<(u32, u32, Answer)>> = HashMap::new();
    for e in &index.edges {
        if kinds.contains(e.kind) {
            spans
                .entry(e.at.file.0)
                .or_default()
                .push((e.at.bytes.start, e.at.bytes.end, Answer::Edge));
        }
    }
    for u in &index.unresolved {
        if u.kind == UnresolvedKind::ExternalOrAmbiguous && u.candidates.is_empty() {
            spans
                .entry(u.at.file.0)
                .or_default()
                .push((u.at.bytes.start, u.at.bytes.end, Answer::External));
        }
    }
    for r in &index.library_receivers {
        spans
            .entry(r.at.file.0)
            .or_default()
            .push((r.at.bytes.start, r.at.bytes.end, Answer::Library));
    }
    for (fi, rec) in index.files.iter().enumerate() {
        if let Some(sem) = &rec.semantic {
            for call in &sem.library_calls {
                spans
                    .entry(fi as u32)
                    .or_default()
                    .push((call.at.start, call.at.end, Answer::Library));
            }
            spans
                .entry(fi as u32)
                .or_default()
                .extend(elsewhere_calls(rec.facts.as_ref(), &sem.resolved_elsewhere));
        }
    }
    for (file, start, end) in decided_sites(index) {
        spans.entry(file).or_default().push((start, end, Answer::Inferred));
    }
    let inactive = inactive_calls(index);
    // Declaration names of the repository per language namespace.
    let mut declared: HashMap<Language, HashSet<&str>> = HashMap::new();
    for s in index.symbols.iter().filter(|s| !s.is_synthetic()) {
        declared
            .entry(call_namespace(s.language))
            .or_default()
            .insert(s.name.as_str());
    }
    // (resolved, unresolved, in-repo resolved, in-repo unresolved, by name)
    let mut per: BTreeMap<Language, (usize, usize, usize, usize, usize)> = BTreeMap::new();
    for (fi, rec) in index.files.iter().enumerate() {
        let analysed = rec.support == SupportLevel::Semantic
            && rec.semantic.as_ref().is_some_and(|s| s.outside_build.is_none());
        if !analysed {
            continue;
        }
        let Some(facts) = &rec.facts else { continue };
        if facts.calls.is_empty() {
            continue;
        }
        let names = declared.get(&call_namespace(rec.language));
        let answered =
            resolved_calls(&facts.calls, spans.get(&(fi as u32)).map(Vec::as_slice).unwrap_or(&[]));
        let skipped = match_calls(&facts.calls, inactive.get(&(fi as u32)).map(Vec::as_slice).unwrap_or(&[]));
        let slot = per.entry(rec.language).or_insert((0, 0, 0, 0, 0));
        for (ci, c) in facts.calls.iter().enumerate() {
            // Code the build does not compile on this machine: not in the rate (like files
            // outside the build).
            if skipped.contains_key(&ci) {
                continue;
            }
            let answer = answered.get(&ci).copied();
            let resolved = answer.is_some();
            let in_repo = c
                .member
                .as_deref()
                .is_some_and(|m| names.is_some_and(|n| n.contains(m)));
            if answer == Some(Answer::External) && !in_repo && !is_local_call(facts, c) {
                slot.4 += 1;
            }
            match (resolved, in_repo) {
                (true, true) => {
                    slot.0 += 1;
                    slot.2 += 1;
                }
                (true, false) => slot.0 += 1,
                (false, true) => {
                    slot.1 += 1;
                    slot.3 += 1;
                }
                (false, false) => slot.1 += 1,
            }
        }
    }
    per.into_iter()
        .map(|(language, (resolved, unresolved, in_repo_resolved, in_repo_unresolved, by_name))| {
            let support = index
                .support
                .iter()
                .find(|s| s.language == language)
                .map(|s| s.level)
                .unwrap_or(SupportLevel::Semantic);
            let rate = rate_of(resolved, unresolved);
            let in_repo_rate = rate_of(in_repo_resolved, in_repo_unresolved);
            let warning = (support == SupportLevel::Semantic && in_repo_rate.is_some_and(|r| r < warn))
                .then_some("low_resolution");
            ResolutionHealth {
                language,
                support,
                resolved,
                unresolved,
                rate,
                warning,
                in_repo_resolved,
                in_repo_unresolved,
                in_repo_rate,
                server: server_state(index, language),
                by_name,
            }
        })
        .collect()
}
