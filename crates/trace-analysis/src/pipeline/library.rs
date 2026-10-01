//! Library phase (DESIGN §1.11; owner derive): build one behaviour request per call site that
//! passes a function (named or anonymous) to a library callee, per library call whose callee
//! may carry a channel effect (any server-resolved library callee with an argument: the
//! derivation answers, no name filter), and per spelling-only candidate of an unresolvable
//! builtin; ask trace-library what the callee does. Expanded macros (`FileSemantics::expanded`)
//! add their language-level exports. Runs after link + families and before bridges (bridges
//! read its channel effects) and inference.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rayon::prelude::*;
use trace_core::delta::IndexDelta;
use trace_core::facts::{ArgSlot, Consumer, FileFacts};
use trace_core::model::UnresolvedKind;
use trace_core::semantics::FileSemantics;
use trace_core::{Index, Language};
use trace_library::{
    ArgSel, BehaviourRequest, BehaviourSource, CallBehaviour, Channel, Effect, Library, LibraryKnowledge,
    RequestArg,
};

/// Behaviour requests per index at most (DESIGN §4.13 task 7).
pub(crate) const MAX_REQUESTS: usize = 10_000;

/// Reason of the behaviours read from expanded macro code (also identifies them).
pub(crate) const EXPANSION_REASON: &str = "exported by the expanded macro code";

/// What the library phase reads of one file.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FileView<'a> {
    pub path: &'a str,
    pub language: Language,
    pub facts: Option<&'a FileFacts>,
    pub semantics: Option<&'a FileSemantics>,
}

/// Every file of an assembled index as a [`FileView`].
pub(crate) fn views(index: &Index) -> Vec<FileView<'_>> {
    index
        .files
        .iter()
        .map(|f| FileView {
            path: &f.path,
            language: f.language,
            facts: f.facts.as_ref(),
            semantics: f.semantic.as_ref(),
        })
        .collect()
}

/// Positional index / keyword of an argument slot.
fn slot_position(slot: &ArgSlot) -> (Option<u32>, Option<String>) {
    match slot {
        ArgSlot::Positional { index, exact: true } => (Some(*index), None),
        ArgSlot::Keyword(k) => (None, Some(k.clone())),
        _ => (None, None),
    }
}

/// Behaviour requests of `files` (sorted by path; bounded by [`MAX_REQUESTS`]).
pub(crate) fn requests<'a>(files: &[FileView<'a>]) -> Vec<BehaviourRequest<'a>> {
    let mut sorted: Vec<&FileView<'a>> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(b.path));
    let mut out = Vec::new();
    for view in sorted {
        let (Some(facts), Some(sem)) = (view.facts, view.semantics) else {
            continue;
        };
        file_requests(view, facts, sem, &mut out);
        if out.len() >= MAX_REQUESTS {
            out.truncate(MAX_REQUESTS);
            break;
        }
    }
    out
}

fn file_requests<'a>(
    view: &FileView<'a>,
    facts: &'a FileFacts,
    sem: &'a FileSemantics,
    out: &mut Vec<BehaviourRequest<'a>>,
) {
    let library_calls: HashMap<u32, &'a trace_core::semantics::SemLibraryCall> =
        sem.library_calls.iter().map(|c| (c.at.start, c)).collect();
    let blind: BTreeSet<u32> = sem
        .unresolved
        .iter()
        .filter(|u| u.candidates.is_empty() && u.kind != UnresolvedKind::UnresolvedSignature)
        .map(|u| u.at.start)
        .collect();
    // Function-valued arguments per receiving call (callee start).
    let mut args: BTreeMap<u32, Vec<RequestArg>> = BTreeMap::new();
    for cb in &facts.callbacks {
        args.entry(cb.call_callee_span.start).or_default().push(RequestArg {
            span: cb.arg_span,
            index: cb.index,
            keyword: cb.keyword.clone(),
        });
    }
    for anon in &facts.anonymous {
        let Some(decl) = facts.declarations.get(anon.decl as usize) else {
            continue;
        };
        if let Consumer::Argument { call, slot } = &anon.consumer {
            let Some(c) = facts.calls.get(*call as usize) else { continue };
            let (index, keyword) = slot_position(slot);
            args.entry(c.callee_span.start).or_default().push(RequestArg {
                span: decl.span.bytes,
                index,
                keyword,
            });
        }
    }
    for list in args.values_mut() {
        list.sort_by_key(|a| (a.span.start, a.span.end));
        list.dedup();
    }
    let mut seen: BTreeSet<u32> = BTreeSet::new();
    for (ci, call) in facts.calls.iter().enumerate() {
        let start = call.callee_span.start;
        if !seen.insert(start) {
            continue;
        }
        let library_call = library_calls.get(&start).copied();
        let call_args = args.get(&start).cloned().unwrap_or_default();
        let wanted = match library_call {
            // A library callee: every call passing a function, and every call with an argument
            // (channel effects: the derivation decides, no name filter).
            Some(_) => !call_args.is_empty() || call.arg_count > 0,
            // No location at all: spelling-only table candidates of unresolvable builtins.
            None => !call_args.is_empty() && blind.contains(&start),
        };
        if !wanted {
            continue;
        }
        let detail = facts.call_detail(ci);
        let positional_args = detail
            .map(|d| {
                d.arguments
                    .iter()
                    .filter(|a| matches!(a.slot, ArgSlot::Positional { .. }))
                    .count() as u32
            })
            .unwrap_or(call.arg_count);
        let keywords: Vec<&'a str> = detail
            .map(|d| {
                d.arguments
                    .iter()
                    .filter_map(|a| match &a.slot {
                        ArgSlot::Keyword(k) => Some(k.as_str()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let target = library_call.and_then(|l| {
            sem.library_files
                .get(l.file as usize)
                .map(|f| (f, l.decl_line, l.decl_column))
        });
        out.push(BehaviourRequest {
            file: view.path,
            language: view.language,
            callee: call.callee_span,
            spelling: call.member.as_deref().unwrap_or(call.callee.as_str()),
            qualifier: call.receiver.as_deref(),
            positional_args,
            keywords,
            target,
            symbol: library_call.and_then(|l| l.symbol.as_deref()),
            callback_params: sem
                .callback_params
                .iter()
                .filter(|p| p.call == call.callee_span)
                .collect(),
            args: call_args,
        });
    }
}

/// Behaviours of the expanded macros of `files` (language-level exports of the expansion).
fn expansions(files: &[FileView<'_>], library: &Library) -> BTreeMap<(String, u32), CallBehaviour> {
    let mut out = BTreeMap::new();
    for view in files {
        let Some(sem) = view.semantics else { continue };
        for expanded in &sem.expanded {
            let names = trace_library::channels::expansion_exports(view.language, &expanded.text);
            if names.is_empty() {
                continue;
            }
            out.insert(
                (view.path.to_string(), expanded.span.start),
                CallBehaviour {
                    symbol: None,
                    effects: names
                        .into_iter()
                        .map(|n| Effect::Exports {
                            channel: Channel::Ffi,
                            name: ArgSel::Kw(n),
                        })
                        .collect(),
                    source: BehaviourSource::Derived,
                    inferred: library.gate().passed(view.language),
                    reason: EXPANSION_REASON.to_string(),
                },
            );
        }
    }
    out
}

/// Library classes of the header bases of `files` the server located outside the index
/// (`LibraryKnowledge::classes`; one read per library declaration).
fn library_classes(
    files: &[FileView<'_>],
    library: &Library,
) -> BTreeMap<(String, u32), trace_library::library_class::LibraryClass> {
    // Every base site and the library declaration it names; each declaration is read once
    // (declarations in parallel: a class is a function of the installed files).
    let mut sites: Vec<(&str, u32, (String, u32, u32))> = Vec::new();
    let mut declarations: BTreeMap<(String, u32, u32), &trace_core::semantics::LibraryFile> = BTreeMap::new();
    for view in files {
        let Some(sem) = view.semantics else { continue };
        for base in &sem.library_bases {
            let Some(file) = sem.library_files.get(base.file as usize) else {
                continue;
            };
            let key = (file.path.clone(), base.decl_line, base.decl_column);
            declarations.entry(key.clone()).or_insert(file);
            sites.push((view.path, base.at.start, key));
        }
    }
    let read: HashMap<(String, u32, u32), trace_library::library_class::LibraryClass> = declarations
        .into_par_iter()
        .filter_map(|(key, file)| library.library_class(file, key.1, key.2).map(|class| (key, class)))
        .collect();
    let mut out = BTreeMap::new();
    for (path, start, key) in sites {
        if let Some(class) = read.get(&key) {
            out.insert((path.to_string(), start), class.clone());
        }
    }
    out
}

/// Sites = distinct request call sites + expanded-macro entries (identified by their reason).
fn finish(mut knowledge: LibraryKnowledge, request_sites: u32) -> LibraryKnowledge {
    let expansion_sites = knowledge
        .by_call
        .values()
        .filter(|b| b.reason == EXPANSION_REASON)
        .count() as u32;
    knowledge.recount(request_sites + expansion_sites);
    knowledge
}

/// Library knowledge of every call site of `files`.
pub(crate) fn knowledge(files: &[FileView<'_>], library: &Library) -> LibraryKnowledge {
    let start = std::time::Instant::now();
    // `TRACE_PROFILE=1`: one `profile-library: <step> <secs>s` line per step.
    let profile = trace_core::env::profile();
    let mut last = start;
    let mut step = |name: &str| {
        if profile {
            let now = std::time::Instant::now();
            eprintln!("profile-library: {name} {:.3}s", (now - last).as_secs_f64());
            last = now;
        }
    };
    let reqs = requests(files);
    step("requests");
    let mut k = library.knowledge(&reqs);
    step("knowledge");
    for (key, b) in expansions(files, library) {
        k.by_call.entry(key).or_insert(b);
    }
    step("expansions");
    k.classes = library_classes(files, library);
    step("classes");
    k.providers_key = library.injected_key();
    k.providers = library.injected_providers();
    step("providers");
    let mut k = finish(k, trace_library::unique_sites(&reqs));
    k.stats.seconds = start.elapsed().as_secs_f64();
    k
}

/// Drops every `by_call` entry of delta.removed / changed / requeried files from `prev`,
/// builds requests only for those files, merges; stats recomputed from the merged map. Must
/// equal [`knowledge`] over all files (equivalence guard): the request bound is applied to the
/// full request list first, so the changed files get exactly the requests a full run gives
/// them. Library summaries themselves are per machine and never recomputed for an edit.
pub(crate) fn knowledge_delta(
    files: &[FileView<'_>],
    library: &Library,
    prev: LibraryKnowledge,
    delta: &IndexDelta,
) -> LibraryKnowledge {
    if delta.full {
        return knowledge(files, library);
    }
    let start = std::time::Instant::now();
    let present: BTreeSet<&str> = files.iter().map(|v| v.path).collect();
    let mut by_call = prev.by_call;
    by_call.retain(|(file, _), _| present.contains(file.as_str()) && !delta.file_changed(file));
    let all = requests(files);
    let changed: Vec<BehaviourRequest<'_>> =
        all.iter().filter(|r| delta.file_changed(r.file)).cloned().collect();
    let (fresh, cache_hits) = library.behaviours(&changed);
    by_call.extend(fresh);
    let changed_views: Vec<FileView<'_>> =
        files.iter().filter(|v| delta.file_changed(v.path)).copied().collect();
    for (key, b) in expansions(&changed_views, library) {
        by_call.entry(key).or_insert(b);
    }
    let mut classes = prev.classes;
    classes.retain(|(file, _), _| present.contains(file.as_str()) && !delta.file_changed(file));
    classes.extend(library_classes(&changed_views, library));
    // Providers of installed plugins: read again only when the installation changed.
    let providers_key = library.injected_key();
    let providers = if prev.providers_key == providers_key {
        prev.providers
    } else {
        library.injected_providers()
    };
    let mut k = LibraryKnowledge {
        by_call,
        stats: prev.stats,
        classes,
        providers,
        providers_key,
    };
    k = finish(k, trace_library::unique_sites(&all));
    k.stats.cache_hits = cache_hits;
    k.stats.seconds = start.elapsed().as_secs_f64();
    k
}

#[cfg(test)]
#[path = "../../tests/unit/pipeline/library.rs"]
mod tests;
