//! Worker output mapped onto syntax declarations (module docs: symbols, owners, edges,
//! unresolved entries, uses, callback parameters, library calls, live references).

use crate::backend::SemanticFile;
use crate::engine::{activation_kind, slice_text, use_edge_kind};
use crate::mapping::{DeclRef, DeclTable};
use crate::references::{LiveReference, LiveReferences, ReferenceQuery};
use std::collections::{BTreeMap, HashMap, HashSet};
use trace_core::facts::{Activation, CallSite, FileFacts, RefKind};
use trace_core::model::{
    ByteSpan, Diagnostic, EdgeKind, ExecutionModel, Provider, Resolution, UnresolvedKind,
};
use trace_core::semantics::{FileSemantics, SemEdge, SemUnresolved, SemValueRef};
use trace_core::text::LineIndex;

use super::*;

#[derive(Default)]
pub(super) struct Out {
    pub(super) edges: Vec<SemEdge>,
    pub(super) unresolved: Vec<SemUnresolved>,
    pub(super) value_refs: Vec<SemValueRef>,
    pub(super) counts: BTreeMap<&'static str, u32>,
}

impl Out {
    pub(super) fn count(&mut self, kind: &'static str) {
        *self.counts.entry(kind).or_insert(0) += 1;
    }
}

/// Map worker JSON onto syntax declarations; fresh results for every partition file.
pub(crate) fn map_worker_output<'a>(
    output: &WorkerOutput,
    files: &[&SemanticFile<'a>],
    decls: &DeclTable<'a>,
    invalid: &HashSet<&str>,
    fingerprint: &str,
) -> HashMap<String, FileSemantics> {
    let mut out: BTreeMap<&'a str, Out> = files.iter().map(|f| (f.path, Out::default())).collect();
    let facts_of: HashMap<&str, &'a FileFacts> = files.iter().map(|f| (f.path, f.facts)).collect();

    // Worker symbol id -> syntax declaration.
    let owners: HashMap<&str, DeclRef<'a>> = output
        .symbols
        .iter()
        .filter_map(|(id, s)| {
            let path = decls.path_key(&s.file)?;
            decls
                .at_span(path, ByteSpan::new(s.start_byte, s.end_byte), &s.name)
                .map(|r| (id.as_str(), r))
        })
        .collect();
    // Owner of evidence in `path`: the syntax call's executing owner, else the worker owner.
    let owner_of = |path: &'a str, call: Option<&CallSite>, worker: Option<&str>| {
        let facts = facts_of.get(path).copied();
        let syntax = call.and_then(|c| facts.and_then(|f| f.executing_owner(c.owner)));
        match syntax {
            Some(decl) => Some(DeclRef { path, decl }),
            None => worker
                .and_then(|id| owners.get(id).copied())
                .filter(|o| o.path.eq_ignore_ascii_case(path)),
        }
    };

    let mut covered: HashSet<(&str, u32)> = HashSet::new();
    for edge in &output.edges {
        let ev = &edge.evidence;
        let Some(path) = decls.path_key(&ev.file).filter(|p| out.contains_key(p)) else {
            continue;
        };
        // A getter read is a property access, not a call: it covers no syntax call.
        let getter = edge.kind == "property_get";
        if !getter {
            covered.insert((path, ev.start_byte));
        }
        let facts = facts_of.get(path).copied();
        let call = if getter {
            None
        } else {
            facts.and_then(|f| call_at(f, ev.start_byte, ev.end_byte))
        };
        let o = out.get_mut(path).expect("partition file");
        let Some(owner) = owner_of(path, call, Some(edge.from.as_str())) else {
            o.count("unmapped_owner");
            continue;
        };
        let kind = match edge.kind.as_str() {
            "calls" => EdgeKind::Calls,
            "constructor" => EdgeKind::Constructor,
            "iterates" => EdgeKind::Iterates,
            "creates_generator" => EdgeKind::CreatesGenerator,
            "property_get" => EdgeKind::PropertyGet,
            _ => {
                o.count("unknown_edge_kind");
                continue;
            }
        };
        let (at, line) = evidence_location(call, ev);
        match owners.get(edge.to.as_str()).copied() {
            Some(target) => {
                let kind = refine(kind, call, decls.decl(target).execution);
                o.edges.push(SemEdge {
                    owner: owner.decl,
                    target: decls.uid(target),
                    kind,
                    at,
                    line,
                    resolution: Resolution::ResolvedSignature,
                });
            }
            None => {
                o.count("unmapped_target");
                o.unresolved.push(SemUnresolved {
                    owner: Some(owner.decl),
                    kind: UnresolvedKind::ExternalOrAmbiguous,
                    at,
                    line,
                    callee: callee_text(call, decls, owner.path, at),
                    candidates: Vec::new(),
                });
            }
        }
    }
    for entry in &output.unresolved {
        let ev = &entry.evidence;
        let Some(path) = decls.path_key(&ev.file).filter(|p| out.contains_key(p)) else {
            continue;
        };
        covered.insert((path, ev.start_byte));
        let facts = facts_of.get(path).copied();
        let call = facts.and_then(|f| call_at(f, ev.start_byte, ev.end_byte));
        let o = out.get_mut(path).expect("partition file");
        let Some(owner) = owner_of(path, call, entry.owner.as_deref()) else {
            o.count("unmapped_owner");
            continue;
        };
        let (at, line) = evidence_location(call, ev);
        let kind = if entry.kind == "unresolved_or_external_signature" || entry.kind.is_empty() {
            UnresolvedKind::UnresolvedSignature
        } else {
            UnresolvedKind::ExternalOrAmbiguous
        };
        o.unresolved.push(SemUnresolved {
            owner: Some(owner.decl),
            kind,
            at,
            line,
            callee: callee_text(call, decls, owner.path, at),
            candidates: Vec::new(),
        });
    }

    // Non-call uses.
    for use_ in &output.uses {
        let ev = &use_.evidence;
        let Some(path) = decls.path_key(&ev.file).filter(|p| out.contains_key(p)) else {
            continue;
        };
        let Some(facts) = facts_of.get(path).copied() else { continue };
        let o = out.get_mut(path).expect("partition file");
        let Some(target) = owners.get(use_.to.as_str()).copied() else {
            o.count("unmapped_use");
            continue;
        };
        let at = ByteSpan::new(ev.start_byte, ev.end_byte.max(ev.start_byte));
        // The declaration's own name is not a use of it.
        if target.path == path && decls.decl(target).name_span.contains(at.start) {
            continue;
        }
        let syntax = facts.references.iter().find(|r| r.span.start == at.start);
        let kind = match syntax {
            Some(r) => r.kind,
            None => match use_.kind.as_str() {
                "write" => RefKind::Write,
                "import" => RefKind::Import,
                "reexport" => RefKind::Export,
                _ => RefKind::Read,
            },
        };
        let owner = match syntax {
            Some(r) => facts.executing_owner(r.owner),
            None => innermost_executing(facts, at.start).or_else(|| {
                use_.from
                    .as_deref()
                    .and_then(|id| owners.get(id).copied())
                    .filter(|r| r.path == path)
                    .map(|r| r.decl)
            }),
        };
        let Some(owner) = owner else {
            o.count("unmapped_owner");
            continue;
        };
        let uid = decls.uid(target);
        if matches!(kind, RefKind::Read | RefKind::Decorator | RefKind::Argument) {
            o.value_refs.push(SemValueRef {
                at,
                line: ev.line,
                target: uid.clone(),
            });
        }
        let edge_kind = match use_edge_kind(kind) {
            Some(k) => k,
            None if decls.decl(target).kind.is_callable() => EdgeKind::PassesCallback,
            None => EdgeKind::References,
        };
        o.edges.push(SemEdge {
            owner,
            target: uid,
            kind: edge_kind,
            at,
            line: ev.line,
            resolution: Resolution::Definition,
        });
    }

    // Blind sites: syntax calls (module level too) without any worker entry.
    for f in files {
        let o = out.get_mut(f.path).expect("partition file");
        if invalid.contains(f.path) {
            o.count("invalid_utf8");
        }
        for c in &f.facts.calls {
            let Some(owner) = f.facts.executing_owner(c.owner) else {
                continue;
            };
            if covered.contains(&(f.path, c.span.start)) {
                continue;
            }
            o.unresolved.push(SemUnresolved {
                owner: Some(owner),
                kind: UnresolvedKind::NoSemanticTarget,
                at: c.callee_span,
                line: c.line,
                callee: c.callee.clone(),
                candidates: Vec::new(),
            });
        }
    }

    out.into_iter()
        .map(|(path, mut o)| {
            o.edges.sort_by(|a, b| {
                (a.at.start, a.at.end, a.owner, &a.target, a.kind)
                    .cmp(&(b.at.start, b.at.end, b.owner, &b.target, b.kind))
            });
            o.edges.dedup_by(|a, b| {
                a.at == b.at && a.owner == b.owner && a.target == b.target && a.kind == b.kind
            });
            o.unresolved.sort_by_key(|u| (u.at.start, u.at.end, u.owner));
            o.unresolved
                .dedup_by(|a, b| a.at == b.at && a.owner == b.owner && a.kind == b.kind);
            o.value_refs
                .sort_by(|a, b| (a.at.start, a.at.end, &a.target).cmp(&(b.at.start, b.at.end, &b.target)));
            o.value_refs.dedup();
            let (library_files, library_calls) = worker_library_calls(output, path);
            let diagnostics = o
                .counts
                .iter()
                .map(|(&kind, &n)| {
                    Diagnostic::new(kind, Some(path.to_string()), format!("{n} {}", describe(kind)))
                })
                .collect();
            (
                path.to_string(),
                FileSemantics {
                    provider: Provider::TypeScript,
                    tool_fingerprint: fingerprint.to_string(),
                    edges: o.edges,
                    unresolved: o.unresolved,
                    value_refs: o.value_refs,
                    diagnostics,
                    implementations: Vec::new(),
                    resolved_elsewhere: Vec::new(),
                    callback_params: worker_callback_params(output, path),
                    library_files,
                    library_calls,
                    outside_build: None,
                    expanded: Vec::new(),
                    library_dispatch: Vec::new(),
                    library_bases: Vec::new(),
                },
            )
        })
        .collect()
}

/// Function-type answers of the worker for `path` (sorted by argument span).
pub(super) fn worker_callback_params(
    output: &WorkerOutput,
    path: &str,
) -> Vec<trace_core::semantics::SemCallbackParam> {
    let mut out: Vec<trace_core::semantics::SemCallbackParam> = output
        .callback_params
        .iter()
        .filter(|p| p.source == path)
        .map(|p| trace_core::semantics::SemCallbackParam {
            call: ByteSpan::new(p.call.start, p.call.end),
            arg: ByteSpan::new(p.arg.start, p.arg.end),
            param_name: p.param_name.clone(),
            param_type: p.param_type.clone(),
            verdict: p.verdict,
            route: if p.route.is_empty() {
                "checker".to_string()
            } else {
                p.route.clone()
            },
            library_symbol: p.library_symbol.clone(),
        })
        .collect();
    out.sort_by_key(|p| (p.arg.start, p.arg.end, p.call.start));
    out
}

/// Library calls of the worker for `path`, with the library files they use re-indexed per file.
pub(super) fn worker_library_calls(
    output: &WorkerOutput,
    path: &str,
) -> (Vec<trace_core::semantics::LibraryFile>, Vec<trace_core::semantics::SemLibraryCall>) {
    let mut files: Vec<trace_core::semantics::LibraryFile> = Vec::new();
    let mut calls = Vec::new();
    for c in output.library_calls.iter().filter(|c| c.source == path) {
        let Some(file) = output.library_files.get(c.file as usize) else {
            continue;
        };
        let index = match files.iter().position(|f| f == file) {
            Some(i) => i,
            None => {
                files.push(file.clone());
                files.len() - 1
            }
        };
        calls.push(trace_core::semantics::SemLibraryCall {
            at: ByteSpan::new(c.at.start, c.at.end),
            line: c.line,
            file: index as u32,
            decl_line: c.decl_line,
            decl_column: c.decl_column,
            symbol: c.symbol.clone(),
        });
    }
    calls.sort_by_key(|c| (c.at.start, c.at.end));
    (files, calls)
}

/// References-mode output -> repository-relative spans (unknown files are dropped).
pub(super) fn map_worker_references(
    output: &WorkerOutput,
    files: &[&SemanticFile<'_>],
    query: &ReferenceQuery,
    backend: &str,
) -> LiveReferences {
    let by_path: HashMap<&str, &[u8]> = files.iter().map(|f| (f.path, f.source)).collect();
    let mut lines: HashMap<&str, LineIndex> = HashMap::new();
    let mut references = Vec::new();
    let mut dropped = 0usize;
    for r in &output.references {
        let Some((path, source)) = by_path.get_key_value(r.file.as_str()) else {
            dropped += 1;
            continue;
        };
        if r.end_byte as usize > source.len() || r.start_byte > r.end_byte {
            dropped += 1;
            continue;
        }
        let index = lines.entry(*path).or_insert_with(|| LineIndex::new(source));
        let is_declaration = r.is_declaration || (*path == query.path.as_str() && r.start_byte == query.byte);
        if is_declaration && !query.include_declaration {
            continue;
        }
        references.push(LiveReference {
            path: path.to_string(),
            at: ByteSpan::new(r.start_byte, r.end_byte),
            line: index.line1(r.start_byte),
            is_declaration,
        });
    }
    references.sort_by(|a, b| (&a.path, a.at.start).cmp(&(&b.path, b.at.start)));
    references.dedup_by(|a, b| a.path == b.path && a.at == b.at);
    LiveReferences {
        references,
        dropped,
        complete: dropped == 0 && output.references_complete.unwrap_or(false),
        backend: backend.to_string(),
    }
}

pub(super) fn describe(kind: &str) -> &'static str {
    match kind {
        "invalid_utf8" => {
            "file is not valid UTF-8 and was not given to the compiler; its calls stay unresolved"
        }
        "unmapped_owner" => "compiler results owned by anonymous or unmapped callables were dropped",
        "unmapped_target" => {
            "resolved targets are not indexed declarations; recorded as external_or_ambiguous"
        }
        "unmapped_use" => "resolved uses whose declaration is not an indexed declaration were ignored",
        "unknown_edge_kind" => "compiler edges of unknown kind were ignored",
        _ => "occurrences",
    }
}

/// Innermost executing declaration (callable, synthetic scope or `<module>`) whose span
/// contains `point`.
pub(super) fn innermost_executing(facts: &FileFacts, point: u32) -> Option<u32> {
    facts
        .declarations
        .iter()
        .enumerate()
        .filter(|(_, d)| d.kind.is_executable() && d.span.bytes.contains(point))
        .min_by_key(|(_, d)| d.span.bytes.len())
        .map(|(i, _)| i as u32)
        .or(facts.module_decl)
}

/// The syntax call whose expression starts at the worker's call evidence.
pub(super) fn call_at(facts: &FileFacts, start: u32, end: u32) -> Option<&CallSite> {
    let mut starting = facts.calls.iter().filter(|c| c.span.start == start);
    let first = starting.next()?;
    if first.span.end == end {
        return Some(first);
    }
    Some(starting.find(|c| c.span.end == end).unwrap_or(first))
}

pub(super) fn evidence_location(call: Option<&CallSite>, ev: &WorkerEvidence) -> (ByteSpan, u32) {
    match call {
        Some(c) => (c.callee_span, c.line),
        None => (ByteSpan::new(ev.start_byte, ev.end_byte.max(ev.start_byte)), ev.line),
    }
}

pub(super) fn callee_text(
    call: Option<&CallSite>,
    decls: &DeclTable<'_>,
    path: &str,
    at: ByteSpan,
) -> String {
    match call {
        Some(c) => c.callee.clone(),
        None => decls.source(path).map(|s| slice_text(s, at)).unwrap_or_default(),
    }
}

/// The worker only models generator activation; async targets use the syntax activation.
pub(super) fn refine(kind: EdgeKind, call: Option<&CallSite>, execution: ExecutionModel) -> EdgeKind {
    if kind != EdgeKind::Calls {
        return kind;
    }
    match execution {
        ExecutionModel::Coroutine | ExecutionModel::AsyncGenerator => {
            activation_kind(call.map_or(Activation::Plain, |c| c.activation), execution)
        }
        _ => kind,
    }
}
