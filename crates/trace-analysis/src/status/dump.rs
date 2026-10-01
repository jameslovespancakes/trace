//! The unresolved-call dump (per language, grouped by reason and callee) behind the
//! `unresolved` example.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;
use trace_core::model::UnresolvedKind;
use trace_core::tiers::{DEFERRED, EXECUTION, INFERRED};
use trace_core::{Index, Language, SupportLevel};

use super::{
    resolution::call_namespace, resolution::decided_sites, resolution::elsewhere_calls,
    resolution::inactive_calls, resolution::is_local_call, resolution::match_calls, resolution::Answer,
};

/// Debug dump of the calls of one language (I-13, `examples/unresolved.rs`): the resolved
/// buckets and every unresolved call grouped by reason, then by callee.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LanguageDump {
    pub language: Option<Language>,
    /// Calls of analysed files (the denominator of the status resolution rate).
    pub calls: usize,
    /// An execution edge into the repository.
    pub repository: usize,
    /// The server located the target in installed library code.
    pub library: usize,
    /// External by the engine's by-name rule (no repository declaration carries the name).
    pub by_name: usize,
    /// External without a library location otherwise (local bindings, answers outside the
    /// index).
    pub external: usize,
    /// No proven target, but a decided site (a decision rule) at the call.
    pub inferred: usize,
    /// Everything else (listed in `groups`).
    pub unresolved: usize,
    /// Calls of files not analysed: pending languages / sub-projects, files outside the build
    /// on this machine and calls in inactive preprocessor regions (`inactive_code`) (listed
    /// in `groups` too, never in `calls`).
    pub pending: usize,
    pub outside_build: usize,
    /// Sorted by calls (descending), then reason.
    pub groups: Vec<ReasonGroup>,
}

/// Unresolved calls with one reason: an `UnresolvedKind` name (`no_semantic_target`,
/// `template_dependent`, `inactive_code`, ...), `ambiguous` (external_or_ambiguous with
/// repository candidates), `no_answer` (nothing recorded at the call), `pending`,
/// `outside_build`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ReasonGroup {
    pub reason: String,
    /// Every call of this reason (complete, also when `callees` is cut).
    pub calls: usize,
    /// The most frequent callees (at most the dump's `top`).
    pub callees: Vec<CalleeGroup>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CalleeGroup {
    /// Callee text (first 80 characters).
    pub callee: String,
    pub calls: usize,
    /// `path:line` of the first calls (at most the dump's `top`).
    pub at: Vec<String>,
}

/// [`LanguageDump`] per language (only `language` when given), from the persisted index
/// alone (no update, no server): calls are matched to recorded spans exactly like
/// [`resolution_health`], so the dump's totals equal the status numbers.
pub fn unresolved_dump(index: &Index, language: Option<Language>, top: usize) -> Vec<LanguageDump> {
    // Proven and inferred targets resolve a call (like `resolution_health`).
    let kinds = EXECUTION.union(DEFERRED).union(INFERRED);
    let mut answers: HashMap<u32, Vec<(u32, u32, Answer)>> = HashMap::new();
    for e in index.edges.iter().filter(|e| kinds.contains(e.kind)) {
        answers
            .entry(e.at.file.0)
            .or_default()
            .push((e.at.bytes.start, e.at.bytes.end, Answer::Edge));
    }
    // Unresolved records by position (value = index into `index.unresolved`).
    let mut records: HashMap<u32, Vec<(u32, u32, usize)>> = HashMap::new();
    for (ui, u) in index.unresolved.iter().enumerate() {
        records
            .entry(u.at.file.0)
            .or_default()
            .push((u.at.bytes.start, u.at.bytes.end, ui));
        if u.kind == UnresolvedKind::ExternalOrAmbiguous && u.candidates.is_empty() {
            answers.entry(u.at.file.0).or_default().push((
                u.at.bytes.start,
                u.at.bytes.end,
                Answer::External,
            ));
        }
    }
    for r in &index.library_receivers {
        answers
            .entry(r.at.file.0)
            .or_default()
            .push((r.at.bytes.start, r.at.bytes.end, Answer::Library));
    }
    for (fi, rec) in index.files.iter().enumerate() {
        if let Some(sem) = &rec.semantic {
            for call in &sem.library_calls {
                answers
                    .entry(fi as u32)
                    .or_default()
                    .push((call.at.start, call.at.end, Answer::Library));
            }
            answers
                .entry(fi as u32)
                .or_default()
                .extend(elsewhere_calls(rec.facts.as_ref(), &sem.resolved_elsewhere));
        }
    }
    let inactive = inactive_calls(index);
    // Sites with a decision naming targets.
    for (file, start, end) in decided_sites(index) {
        answers.entry(file).or_default().push((start, end, Answer::Inferred));
    }
    let mut declared: HashMap<Language, HashSet<&str>> = HashMap::new();
    for s in index.symbols.iter().filter(|s| !s.is_synthetic()) {
        declared
            .entry(call_namespace(s.language))
            .or_default()
            .insert(s.name.as_str());
    }
    // (language, reason) -> callee -> (calls, first locations)
    type Groups = BTreeMap<String, BTreeMap<String, (usize, Vec<String>)>>;
    let mut per: BTreeMap<Language, (LanguageDump, Groups)> = BTreeMap::new();
    let empty: Vec<(u32, u32, Answer)> = Vec::new();
    for (fi, rec) in index.files.iter().enumerate() {
        if language.is_some_and(|l| l != rec.language) {
            continue;
        }
        let Some(facts) = &rec.facts else { continue };
        if facts.calls.is_empty() {
            continue;
        }
        let (dump, groups) = per.entry(rec.language).or_insert_with(|| {
            (
                LanguageDump {
                    language: Some(rec.language),
                    ..LanguageDump::default()
                },
                Groups::new(),
            )
        });
        let mut note = |reason: &str, c: &trace_core::facts::CallSite| {
            let callee: String = c.callee.chars().take(80).collect();
            let slot = groups
                .entry(reason.to_string())
                .or_default()
                .entry(callee)
                .or_insert((0, Vec::new()));
            slot.0 += 1;
            if slot.1.len() < top {
                slot.1.push(format!("{}:{}", rec.path, c.line));
            }
        };
        let not_analysed = if rec.support == SupportLevel::Pending {
            Some("pending")
        } else if rec.support != SupportLevel::Semantic {
            None
        } else if rec.semantic.as_ref().is_some_and(|s| s.outside_build.is_some()) {
            Some("outside_build")
        } else {
            None
        };
        if let Some(reason) = not_analysed {
            for c in &facts.calls {
                note(reason, c);
            }
            if reason == "pending" {
                dump.pending += facts.calls.len();
            } else {
                dump.outside_build += facts.calls.len();
            }
            continue;
        }
        if rec.support != SupportLevel::Semantic || rec.semantic.is_none() {
            continue;
        }
        let file = fi as u32;
        let answered = match_calls(&facts.calls, answers.get(&file).map(Vec::as_slice).unwrap_or(&empty));
        let recorded = match_calls(&facts.calls, records.get(&file).map(Vec::as_slice).unwrap_or(&[]));
        let skipped = match_calls(&facts.calls, inactive.get(&file).map(Vec::as_slice).unwrap_or(&[]));
        let names = declared.get(&call_namespace(rec.language));
        for (ci, c) in facts.calls.iter().enumerate() {
            // Inactive preprocessor regions: outside the build on this machine (listed, never
            // in `calls`, like the status rate).
            if skipped.contains_key(&ci) {
                dump.outside_build += 1;
                note("inactive_code", c);
                continue;
            }
            dump.calls += 1;
            let in_repo = c
                .member
                .as_deref()
                .is_some_and(|m| names.is_some_and(|n| n.contains(m)));
            match answered.get(&ci) {
                Some(Answer::Edge) => dump.repository += 1,
                Some(Answer::Library) => dump.library += 1,
                Some(Answer::Elsewhere) => dump.external += 1,
                Some(Answer::External) if !in_repo && !is_local_call(facts, c) => dump.by_name += 1,
                Some(Answer::External) => dump.external += 1,
                Some(Answer::Inferred) => dump.inferred += 1,
                None => {
                    dump.unresolved += 1;
                    let reason = match recorded.get(&ci).and_then(|&ui| index.unresolved.get(ui)) {
                        Some(u) if u.kind == UnresolvedKind::ExternalOrAmbiguous => "ambiguous",
                        Some(u) => u.kind.as_str(),
                        None => "no_answer",
                    };
                    note(reason, c);
                }
            }
        }
    }
    per.into_values()
        .map(|(mut dump, groups)| {
            dump.groups = groups
                .into_iter()
                .map(|(reason, callees)| {
                    let calls: usize = callees.values().map(|(n, _)| *n).sum();
                    let mut callees: Vec<CalleeGroup> = callees
                        .into_iter()
                        .map(|(callee, (calls, at))| CalleeGroup { callee, calls, at })
                        .collect();
                    callees.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.callee.cmp(&b.callee)));
                    callees.truncate(top);
                    ReasonGroup {
                        reason,
                        calls,
                        callees,
                    }
                })
                .collect();
            dump.groups
                .sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.reason.cmp(&b.reason)));
            dump
        })
        .collect()
}
