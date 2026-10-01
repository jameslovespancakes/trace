//! Syntax phase: reuse the facts of unchanged files, extract the rest, and decide the
//! language of shared C / C++ headers from their includers.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use trace_core::facts::FileFacts;
use trace_core::incremental::{self, UpdatePlan};
use trace_core::languages::{self, Family};
use trace_core::model::{Diagnostic, Index, OmittedFile};
use trace_core::{Language, SCHEMA_VERSION};
use trace_semantic::backend::Backend;
use trace_semantic::ToolEnv;

use super::{
    pending::pending_reasons, progress::IndexProgress, records::syntax_diagnostics, setup::setup_phase,
    Build, FileState, Host,
};
use crate::Result;

/// True when versions, setup and semantic inputs all match `prev` for a no-op inventory
/// plan (module docs). Runs the automatic install and the preflight with the previous facts
/// (identical to the current ones: same files, same extractor): a setup failure is an error.
pub(super) fn is_unchanged(
    prev: &Index,
    plan: &UpdatePlan,
    backends: &[Box<dyn Backend>],
    host: &Host<'_>,
    tools: &ToolEnv,
    files: &[(&str, Language)],
    pending: &BTreeSet<Language>,
) -> Result<bool> {
    let versions_ok = prev.header.syntax_version == trace_syntax::EXTRACTOR_VERSION
        && prev.header.infer_version == trace_infer::INFER_VERSION
        && prev.header.bridge_version == trace_bridge::BRIDGE_VERSION
        && prev.header.schema == SCHEMA_VERSION;
    if !versions_ok || !plan.is_noop() {
        return Ok(false);
    }
    let facts =
        |p: &str| -> Option<&FileFacts> { prev.file_by_path(p).and_then(|id| prev.file(id).facts.as_ref()) };
    let sem = setup_phase(backends, host, tools, files, &facts, pending)?;
    let prepared: Vec<(Vec<Language>, &BTreeMap<String, String>)> = sem
        .assign
        .iter()
        .zip(&sem.prepared)
        .map(|((_, l), p)| (l.clone(), &p.pending_dirs))
        .collect();
    let reasons = pending_reasons(files, pending, &prepared, host.settings);
    let current: Vec<(String, Language)> = files.iter().map(|(p, l)| (p.to_string(), *l)).collect();
    let empty = HashSet::new();
    let semantic_ok = sem.assign.iter().zip(&sem.fingerprints).all(|((_, langs), fp)| {
        let product: Vec<(String, Language)> = current
            .iter()
            .filter(|(p, _)| !reasons.contains_key(p))
            .cloned()
            .collect();
        // Stale files the policy keeps stale are not work.
        incremental::semantic_requery(incremental::RequeryInput {
            prev: Some(prev),
            plan,
            partition: langs,
            current_files: &product,
            tool_fingerprint: fp,
            declared_names: &empty,
            interface_changed: &empty,
            policy: &host.stale,
        })
        .now
        .is_empty()
    });
    let records_ok = prev.files.iter().all(|f| {
        let pending = reasons.get(&f.path);
        f.pending.as_ref() == pending
            && match (pending, sem.prepared_for(f.language)) {
                (Some(_), _) => f.semantic.is_none(),
                (None, Some(_)) => f.semantic.is_some(),
                (None, None) => f.semantic.is_none(),
            }
    });
    Ok(semantic_ok && records_ok)
}

/// Phase 2: reuse or extract syntax facts. Returns the number of files parsed.
pub(super) fn syntax_phase(build: &mut Build<'_>, progress: &mut dyn IndexProgress) -> usize {
    let mut no_grammar: BTreeMap<Language, usize> = BTreeMap::new();
    let mut to_parse: Vec<usize> = Vec::new();
    for (i, f) in build.files.iter_mut().enumerate() {
        if trace_syntax::grammar(f.language).is_none() {
            *no_grammar.entry(f.language).or_insert(0) += 1;
            continue;
        }
        match incremental::reusable_record(
            build.reuse_prev,
            &f.path,
            &f.hash,
            trace_syntax::EXTRACTOR_VERSION,
        ) {
            Some(rec) if rec.facts.is_some() => {
                f.facts = rec.facts.clone();
                if let Some(facts) = &f.facts {
                    f.diagnostics.extend(syntax_diagnostics(&f.path, facts));
                }
            }
            _ => to_parse.push(i),
        }
    }
    progress.phase("syntax", 0, to_parse.len());
    build.load(&to_parse);
    for &i in &to_parse {
        let f = &mut build.files[i];
        match f.bytes_hash {
            Some(h) => f.hash = h,
            None => {
                f.skip = true;
                build.omitted.push(OmittedFile {
                    path: f.path.clone(),
                    reason: "unreadable".into(),
                });
            }
        }
    }
    let parse_ids: Vec<usize> = to_parse.iter().copied().filter(|&i| !build.files[i].skip).collect();
    let results = {
        let inputs: Vec<trace_syntax::SourceInput<'_>> = parse_ids
            .iter()
            .map(|&i| {
                let f = &build.files[i];
                trace_syntax::SourceInput {
                    path: &f.path,
                    language: f.language,
                    source: f.bytes.as_deref().unwrap_or(&[]),
                }
            })
            .collect();
        trace_syntax::extract_many(&inputs)
    };
    for (&i, result) in parse_ids.iter().zip(results) {
        let f = &mut build.files[i];
        match result {
            Ok(facts) => {
                f.diagnostics.extend(syntax_diagnostics(&f.path, &facts));
                f.facts = Some(facts);
            }
            Err(e) => {
                f.diagnostics
                    .push(Diagnostic::new("syntax_unavailable", Some(f.path.clone()), e.to_string()))
            }
        }
    }
    for (language, count) in &no_grammar {
        build.diagnostics.push(Diagnostic::new(
            "no_grammar",
            None,
            format!("{count} {language} file(s) inventoried only: no grammar compiled in"),
        ));
    }
    // Only languages present are compiled (and so can report a query failure).
    let present: BTreeSet<Language> = build.files.iter().map(|f| f.language).collect();
    for e in present.into_iter().filter_map(trace_syntax::grammar::grammar_error) {
        build
            .diagnostics
            .push(Diagnostic::new("grammar_error", None, e.to_string()));
    }
    progress.phase("syntax", parse_ids.len(), parse_ids.len());
    parse_ids.len()
}

/// Whether `path` is a C-family header whose language the repository decides (`.h`).
fn is_shared_header(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("h"))
}

/// C or C++ ([`Family::C`]).
fn in_c_family(language: Language) -> bool {
    languages::info(language).family == Some(Family::C)
}

/// The language of every `.h` header: its per-file syntax language (`facts.language`), then
/// the repository rule over the includers (`trace_syntax::header::repo_header_languages`).
/// The file record takes that language, so C++ headers are served with C++.
pub(super) fn apply_header_languages(files: &mut [FileState]) {
    let headers: Vec<(String, Language)> = files
        .iter()
        .filter(|f| !f.skip && in_c_family(f.language) && is_shared_header(&f.path))
        .map(|f| {
            let language = f.facts.as_ref().and_then(|x| x.language).unwrap_or(f.language);
            (f.path.clone(), language)
        })
        .collect();
    if headers.is_empty() {
        return;
    }
    let includers: Vec<(String, Language, Vec<String>)> = files
        .iter()
        .filter(|f| !f.skip && in_c_family(f.language))
        .filter_map(|f| {
            let facts = f.facts.as_ref()?;
            let included: Vec<String> = facts.imports.iter().map(|i| i.target.clone()).collect();
            (!included.is_empty()).then(|| (f.path.clone(), f.language, included))
        })
        .collect();
    let header_refs: Vec<(&str, Language)> = headers.iter().map(|(p, l)| (p.as_str(), *l)).collect();
    let languages = trace_syntax::header::repo_header_languages(
        &header_refs,
        &includers
            .iter()
            .map(|(p, l, i)| (p.as_str(), *l, i.clone()))
            .collect::<Vec<_>>(),
    );
    for f in files.iter_mut() {
        if let Some(&language) = languages.get(&f.path) {
            f.language = language;
        }
    }
}
