//! Records and report: file records, support levels, the index header and delta, and the
//! index report of a run.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

use trace_core::cache::unix_now;
use trace_core::delta::IndexDelta;
use trace_core::facts::FileFacts;
use trace_core::incremental::{self, UpdatePlan};
use trace_core::model::{BackendRun, Diagnostic, FileRecord, Index, IndexHeader};
use trace_core::paths::RepoPaths;
use trace_core::semantics::FileSemantics;
use trace_core::{inventory, Hash32, Language, LanguageSupport, SupportLevel, SCHEMA_VERSION, TRACE_VERSION};

use super::{
    pending::pending_files, pending::pending_language_reason, scan::Scanned, semantic::SemanticCounts, Build,
    FileState, SemanticPlan,
};
use crate::cards::{edge_counts, language_row};
use crate::report::{IndexReport, PhaseSeconds};

/// Support level of one file record.
pub(super) fn file_support(language: Language, semantic: bool, pending: bool) -> SupportLevel {
    if semantic {
        SupportLevel::Semantic
    } else if pending && language.is_code() {
        SupportLevel::Pending
    } else {
        SupportLevel::Inventoried
    }
}

/// Phase 7 inputs: file records, fingerprinted configuration files and the header of the
/// new index (assembled by `crate::pipeline::update::apply`).
pub(super) fn records_and_header(
    files: Vec<FileState>,
    build: &Build<'_>,
    prev: Option<&Index>,
    scanned: &Scanned,
) -> (Vec<FileRecord>, Vec<(String, Hash32)>, IndexHeader) {
    let records: Vec<FileRecord> = files
        .into_iter()
        .filter(|f| !f.skip)
        .map(|f| {
            let support = file_support(f.language, f.semantic.is_some(), f.pending.is_some());
            FileRecord {
                path: f.path,
                language: f.language,
                hash: f.hash,
                size: f.size,
                mtime_ns: f.mtime_ns,
                support,
                facts: f.facts,
                semantic: f.semantic,
                first_symbol: 0,
                symbol_count: 0,
                diagnostics: f.diagnostics,
                pending: f.pending.filter(|_| support == SupportLevel::Pending),
            }
        })
        .collect();
    let configs: Vec<(String, Hash32)> = scanned
        .configs
        .iter()
        .map(|c| (c.entry.path.clone(), c.hash))
        .collect();
    let fingerprint = inventory::inventory_fingerprint(
        records.iter().map(|r| (r.path.as_str(), &r.hash)),
        configs.iter().map(|(p, h)| (p.as_str(), h)),
    );
    let (full_builds, incremental_updates) = match (prev, build.reuse_prev) {
        (Some(p), Some(_)) => (p.header.full_builds, p.header.incremental_updates + 1),
        (Some(p), None) => (p.header.full_builds + 1, 0),
        (None, _) => (1, 0),
    };
    let header = IndexHeader {
        schema: SCHEMA_VERSION,
        trace_version: TRACE_VERSION.to_string(),
        root: build.paths.root_display(),
        built_unix: unix_now(),
        syntax_version: trace_syntax::EXTRACTOR_VERSION,
        infer_version: trace_infer::INFER_VERSION,
        bridge_version: trace_bridge::BRIDGE_VERSION,
        inventory_fingerprint: fingerprint,
        full_builds,
        incremental_updates,
    };
    (records, configs, header)
}

/// Whether the post-semantic update must run in full: other inference / bridge / schema
/// versions, or changed configuration files (they can change every file's semantics).
pub(super) fn needs_full_update(prev: &Index, plan: &UpdatePlan) -> bool {
    plan.configs_changed
        || prev.header.infer_version != trace_infer::INFER_VERSION
        || prev.header.bridge_version != trace_bridge::BRIDGE_VERSION
        || prev.header.schema != SCHEMA_VERSION
}

/// What changed against the previous index (DESIGN §1.14.5): `None` -> a full update.
/// `requeried` = the files the partitions re-queried now plus every file whose facts,
/// semantics, pending state, support or language differ from `prev` (the journal carries
/// exactly the added | modified | requeried blocks); `stale` = the union of the partitions'
/// `Requery::stale` (it keeps still-stale previous entries the policy left stale).
pub(super) fn index_delta(
    prev: Option<&Index>,
    plan: &UpdatePlan,
    records: &[FileRecord],
    requery: &incremental::Requery,
    interfaces: &HashSet<String>,
) -> IndexDelta {
    let mut requeried = requery.now.clone();
    if let Some(prev) = prev {
        for rec in records {
            let differs = match prev.file_by_path(&rec.path).map(|id| prev.file(id)) {
                None => rec.semantic.is_some(),
                Some(old) => {
                    old.semantic != rec.semantic
                        || old.facts != rec.facts
                        || old.pending != rec.pending
                        || old.support != rec.support
                        || old.language != rec.language
                }
            };
            if differs {
                requeried.insert(rec.path.clone());
            }
        }
    }
    incremental::index_delta(incremental::DeltaInput {
        prev,
        plan,
        full: prev.is_none(),
        records,
        requeried: &requeried,
        interface_changed: interfaces,
        stale: &requery.stale,
    })
}

pub(super) fn build_report(
    paths: &RepoPaths,
    index: &Index,
    mode: &'static str,
    plan: &UpdatePlan,
    reparsed: usize,
    counts: SemanticCounts,
    seconds: PhaseSeconds,
) -> IndexReport {
    IndexReport {
        command: "index",
        schema: crate::report::SCHEMA,
        trace_version: TRACE_VERSION,
        root: paths.root_display(),
        mode,
        files: index.files.len(),
        added: plan.added.len(),
        changed: plan.changed.len(),
        removed: plan.removed.len(),
        reparsed,
        semantic_requeried: counts.queried,
        semantic_reused: counts.reused,
        pending_files: pending_files(index),
        outside_build_files: outside_build_files(index),
        symbols: index.symbols.len(),
        edges: edge_counts(index),
        sites: index.sites.len(),
        bridges: index.bridges.len(),
        backends: index.backend_runs.clone(),
        languages: index.support.iter().map(language_row).collect(),
        diagnostics: index.diagnostics.len() + index.files.iter().map(|f| f.diagnostics.len()).sum::<usize>(),
        seconds,
    }
}

/// Files the server reported as not part of the build on this machine (Go build tags,
/// `cfg(target_os)`, platform-only sources): counted and listed, never answered from syntax.
pub fn outside_build_files(index: &Index) -> usize {
    index
        .files
        .iter()
        .filter(|f| f.semantic.as_ref().is_some_and(|s| s.outside_build.is_some()))
        .count()
}

/// Read a repository-relative source through the safe path check (read-only).
pub(super) fn read_source(root: &Path, rel: &str) -> std::result::Result<Vec<u8>, trace_core::CoreError> {
    let path = inventory::safe_source_path(root, rel)?;
    fs::read(&path).map_err(|e| trace_core::CoreError::io(&path, e))
}

pub(super) fn cached_semantics(prev: Option<&Index>, path: &str) -> Option<FileSemantics> {
    let prev = prev?;
    prev.file(prev.file_by_path(path)?).semantic.clone()
}

pub(super) fn syntax_diagnostics(path: &str, facts: &FileFacts) -> Option<Diagnostic> {
    (facts.error_count > 0).then(|| {
        Diagnostic::new(
            "syntax_error",
            Some(path.to_string()),
            format!("{} syntax error node(s); facts extracted from valid regions", facts.error_count),
        )
    })
}

/// Per-language support rows (SPEC §8.7): `semantic` (the server that analysed it),
/// `pending` (with the reason), `inventoried` (no grammar / not code).
pub(super) fn support_rows(
    files: &[FileState],
    runs: &[BackendRun],
    sem: &SemanticPlan<'_>,
) -> Vec<LanguageSupport> {
    #[derive(Default)]
    struct Tally {
        files: u32,
        semantic: u32,
        pending: u32,
        reason: Option<String>,
    }
    let mut per_language: BTreeMap<Language, Tally> = BTreeMap::new();
    for f in files.iter().filter(|f| !f.skip) {
        let t = per_language.entry(f.language).or_default();
        t.files += 1;
        if f.semantic.is_some() {
            t.semantic += 1;
        }
        if f.pending.is_some() && f.language.is_code() {
            t.pending += 1;
            if t.reason.is_none() {
                t.reason = f.pending.clone();
            }
        }
    }
    per_language
        .into_iter()
        .map(|(language, t)| {
            let assigned = sem
                .assign
                .iter()
                .find(|(_, langs)| langs.contains(&language))
                .map(|(b, _)| b.id().to_string());
            let entry_id = assigned.clone().or_else(|| {
                sem.tools
                    .registry
                    .backends
                    .iter()
                    .find(|e| e.languages.contains(&language))
                    .map(|e| e.id.clone())
            });
            let (level, available, reason) = if !language.is_code() {
                let reason = if language.is_contract() {
                    "contract file (read for cross-language links)"
                } else {
                    "no grammar compiled in"
                };
                (SupportLevel::Inventoried, false, reason.to_string())
            } else if t.semantic == 0 && t.pending > 0 {
                (
                    SupportLevel::Pending,
                    false,
                    t.reason.clone().unwrap_or_else(|| pending_language_reason(language)),
                )
            } else if let Some(id) = &assigned {
                let version = runs
                    .iter()
                    .find(|r| &r.backend == id)
                    .and_then(|r| r.tool_version.clone())
                    .or_else(|| sem.version_of(id));
                let mut reason = match version {
                    Some(v) => format!("{id} {v}"),
                    None => id.clone(),
                };
                if t.pending > 0 {
                    reason.push_str(&format!(
                        "; {} file{} of sub-projects pending",
                        t.pending,
                        if t.pending == 1 { "" } else { "s" }
                    ));
                }
                (SupportLevel::Semantic, true, reason)
            } else {
                (SupportLevel::Inventoried, false, "no grammar compiled in".to_string())
            };
            LanguageSupport {
                language,
                files: t.files,
                level,
                backend: entry_id,
                backend_available: available,
                reason,
            }
        })
        .collect()
}
