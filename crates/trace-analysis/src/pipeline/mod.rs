//! The index pipeline (SPEC §5; DESIGN §0): inventory -> syntax -> automatic install ->
//! setup (preflight) -> semantic -> post-semantic update (link, families, library knowledge,
//! bridges, inference, decisions) -> persist. Holds `index.lock` for the whole run.
//!
//! There is no syntax-only mode (PLAN decision 3): every code language of the repository is
//! analysed by its language server, or the run stops with ONE [`trace_core::SetupError`] that
//! lists everything missing across all languages (PLAN decision 15). Syntax trees are used
//! next to the servers (exact positions, symbols, derivation), never instead of them. The
//! index is not written when setup or analysis fails.
//!
//! 1. inventory: `trace_core::inventory::scan` (user `inventory.exclude` globs keep folders out
//!    of everything below) + `hash_entries` (reuse hashes of unchanged size+mtime unless
//!    `Rebuild`); `incremental::plan` against the previous index.
//! 2. syntax: reuse facts for unchanged files (`reusable_record`), `extract_many` for the rest
//!    (sources read once, verified against the inventory hash); languages without a grammar
//!    are inventoried (`SupportLevel::Inventoried`) with a diagnostic. The language of `.h`
//!    headers comes from the syntax facts and the repository's includers
//!    (`trace_syntax::header::repo_header_languages`).
//! 3. languages: product languages are analysed now. A code language whose every file is test
//!    / fixture / example code while another language has more product files, or used only in
//!    packaging files (package recipes, installer / release scripts, packaging folders:
//!    [`is_packaging_location`]) while another language has product files, is PENDING
//!    ([`pending_languages`]): not set up at index time, its files carry
//!    `SupportLevel::Pending` and a reason, and the first query that needs one of them sets it
//!    up (`Workspace::ensure_ready`, remembered in `RepoSettings::ready_languages`). Code
//!    languages no registry entry serves are setup failures.
//! 4. automatic install (PLAN decision 10): default languages whose server or server runtime
//!    is missing are installed (`trace_semantic::install::auto_install`, one progress line per
//!    tool through [`IndexProgress::install_line`]); off with `TRACE_OFFLINE=1`,
//!    `TRACE_NO_AUTO_INSTALL=1` or `semantic.auto_install = false`.
//! 5. setup: `trace_semantic::setup::preflight_all` over one registry entry per product
//!    language (platform, toolchain, server + runtime, dependencies, build approval); every
//!    failure of every language combined into one error. Sub-projects a preflight reports in
//!    `Prepared::pending_dirs` are pending like pending languages.
//! 6. semantic: every backend partition (test files included: they are analysed at index time)
//!    goes through `trace_semantic::SemanticSessions::run` (per-file cache keyed by content and
//!    dependency hashes, process pool, persistent sessions in `trace index --watch`); only the
//!    files `incremental::semantic_requery` selects are asked, the others keep their previous
//!    results. A server failure is the setup error it carries, any other backend error is
//!    `SetupError::ServerCrashed` with a log, and a requested file without a result means the
//!    server (or its shard) died: `ServerCrashed` too.
//! 7. post-semantic (`update::apply`, one code path for full and incremental runs): the
//!    pipeline hands over every file record, the previous index (incremental runs) and the
//!    [`IndexDelta`] (added / modified / removed, requeried, interface changes, symbol changes).
//! 8. persist (`update::persist`).
//!
//! With `TRACE_PROFILE=1` every phase boundary prints one stderr line (whether or not
//! stderr is a terminal): `profile: <phase> <secs>s files=<n> symbols=<n> edges=<n>
//! slots=<n> values=<n> evals=<n> sites=<n> bridges=<n>` (SPEC §5.1a).
//!
//! After persisting, the inventory fingerprint is recomputed; if sources changed during the
//! run the report carries a `sources_changed_during_index` diagnostic (next run updates).
//!
//! An incremental run whose inventory, versions, setup and semantic inputs are all unchanged
//! reports mode `unchanged` and keeps the index as is (the setup is still checked: a server
//! or dependency removed since the last run stops it).
//!
//! Files: [`run`] and the per-file build state here; `progress` (progress lines, profile),
//! `scan` (hashed inventory), `pending` (pending languages and sub-projects), `setup`
//! (automatic install, preflight), `syntax`, `semantic`, `records` (file records, header,
//! delta, report); [`update`] (post-semantic update and persist) and [`library`] (the library
//! phase inside the update).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

use rayon::prelude::*;
use trace_core::cache::CacheLock;
use trace_core::config::Settings;
use trace_core::facts::FileFacts;
use trace_core::incremental::{self, StalePolicy};
use trace_core::model::{Diagnostic, Index, OmittedFile};
use trace_core::paths::{ensure_outside, RepoPaths};
use trace_core::repo_settings::RepoSettings;
use trace_core::semantics::FileSemantics;
use trace_core::{Hash32, Language};
use trace_semantic::backend::Backend;
use trace_semantic::{Prepared, SemanticSessions, ToolEnv};

use crate::report::{IndexReport, PhaseSeconds};
use crate::Result;

pub mod library;
mod pending;
mod progress;
mod records;
mod scan;
mod semantic;
mod setup;
mod syntax;
pub mod update;

pub(crate) use pending::pending_dir;
pub use pending::{pending_files, pending_languages};
use pending::{pending_now, pending_reasons};
pub use progress::{IndexProgress, Profile, Quiet, StderrProgress};
pub use records::outside_build_files;
use records::{build_report, index_delta, needs_full_update, read_source, records_and_header, support_rows};
pub(crate) use scan::scan;
pub(crate) use scan::{is_current, Scanned};
pub(crate) use semantic::warm_sessions;
use semantic::{semantic_phase, SemanticCounts, SemanticOptions};
use setup::{auto_install, product_languages, setup_phase};
use syntax::{apply_header_languages, is_unchanged, syntax_phase};

/// What `trace index` was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexMode {
    /// Update from the cached index (default).
    Incremental,
    /// Ignore every cache (facts, semantics, hashes); full build.
    Rebuild,
}

/// Inputs of [`run`].
pub(crate) struct PipelineInput<'a> {
    pub paths: &'a RepoPaths,
    pub config: &'a Settings,
    /// The previously persisted index, if any. Handed to the post-semantic update for an
    /// incremental run, returned unchanged in [`PipelineOutput::index`] when nothing changed.
    pub prev: Option<Index>,
    pub mode: IndexMode,
    /// Persistent analyzer sessions of a long-running process (`trace index --watch`);
    /// `None` = one-shot run (per-file semantic cache only, no live processes).
    pub sessions: Option<&'a mut SemanticSessions>,
    /// A scan already made by the caller (freshness check), reused as phase 1.
    pub scanned: Option<Scanned>,
    /// Why a previous cache was discarded (recorded as `cache_invalidated`).
    pub invalidated: Option<String>,
    /// `TRACE_OFFLINE=1`: no automatic installs.
    pub offline: bool,
    /// Stale dependents of an interface change: resolved now (commands, `trace index`) or
    /// deferred to the following batches (`trace index --watch`).
    pub stale: StalePolicy,
}

/// Result of [`run`].
pub(crate) struct PipelineOutput {
    /// The current index: the new one, or the previous one when nothing changed
    /// (`changed == false`, report mode `unchanged`).
    pub index: Index,
    pub changed: bool,
    pub report: IndexReport,
}

/// One source of this run with its per-phase results.
struct FileState {
    path: String,
    language: Language,
    size: u64,
    mtime_ns: u64,
    /// Hash of the bytes the facts were extracted from.
    hash: Hash32,
    facts: Option<FileFacts>,
    semantic: Option<FileSemantics>,
    diagnostics: Vec<Diagnostic>,
    /// Exact bytes, loaded at most once and dropped after the semantic phase.
    bytes: Option<Vec<u8>>,
    /// blake3 of `bytes` (may differ from `hash` if the file changed while indexing).
    bytes_hash: Option<Hash32>,
    /// Unreadable while indexing: omitted from the index.
    skip: bool,
    /// Why the file is not analysed yet (pending language or sub-project).
    pending: Option<String>,
}

/// Mutable state of one build.
struct Build<'a> {
    paths: &'a RepoPaths,
    reuse_prev: Option<&'a Index>,
    files: Vec<FileState>,
    diagnostics: Vec<Diagnostic>,
    omitted: Vec<OmittedFile>,
    /// A source's bytes differed from its inventory hash while indexing.
    changed_during: bool,
}

/// One registry entry per product language, with its preflight result.
struct SemanticPlan<'b> {
    tools: &'b ToolEnv,
    assign: Vec<(&'b dyn Backend, Vec<Language>)>,
    /// Preflight result per `assign` entry (DESIGN §1.7).
    prepared: Vec<Prepared>,
    /// Tool fingerprint per `assign` entry (includes `Prepared::fingerprint`).
    fingerprints: Vec<String>,
}

impl SemanticPlan<'_> {
    /// Version of the server of `backend` (registry pin).
    fn version_of(&self, backend: &str) -> Option<String> {
        self.tools
            .registry
            .entry(backend)
            .map(|e| e.server.version.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    /// The prepared result of the backend serving `language`.
    fn prepared_for(&self, language: Language) -> Option<&Prepared> {
        self.assign
            .iter()
            .position(|(_, l)| l.contains(&language))
            .and_then(|i| self.prepared.get(i))
    }
}

/// Run the whole pipeline under the repository lock.
pub(crate) fn run(input: PipelineInput<'_>, progress: &mut dyn IndexProgress) -> Result<PipelineOutput> {
    let PipelineInput {
        paths,
        config,
        prev,
        mode,
        sessions,
        scanned,
        invalidated,
        offline,
        stale: stale_policy,
    } = input;
    ensure_outside(&paths.repo_dir, &[&paths.root])?;
    let _lock = CacheLock::acquire(&paths.lock_file)?;
    let rebuild = mode == IndexMode::Rebuild;
    let mut secs = PhaseSeconds::default();
    let mut profile = Profile::from_env();

    // 1. Inventory.
    let t = Instant::now();
    progress.phase("inventory", 0, 1);
    let scanned = match scanned {
        Some(s) if !rebuild => s,
        _ => scan(paths, config, prev.as_ref(), mode)?,
    };
    let plan = incremental::plan(prev.as_ref(), &scanned.sources, &scanned.configs);
    let files: Vec<FileState> = scanned
        .sources
        .iter()
        .filter_map(|e| {
            Some(FileState {
                path: e.entry.path.clone(),
                language: e.entry.language?,
                size: e.entry.size,
                mtime_ns: e.entry.mtime_ns,
                hash: e.hash,
                facts: None,
                semantic: None,
                diagnostics: Vec::new(),
                bytes: None,
                bytes_hash: None,
                skip: false,
                pending: None,
            })
        })
        .collect();
    secs.inventory = t.elapsed().as_secs_f64();
    progress.phase("inventory", 1, 1);
    profile.files = files.len();
    profile.line("inventory", secs.inventory);

    let mut tools = ToolEnv::discover(config, &paths.home, &paths.root)?;
    let settings = RepoSettings::load(paths)?;
    let host = Host {
        paths,
        config,
        settings: &settings,
        platform: trace_env::os::Platform::current(),
        vars: trace_env::os::EnvVars::from_process(),
        auto_install: trace_core::config::auto_install_enabled(config, offline),
        stale: stale_policy.clone(),
    };

    // Unchanged shortcut: the same files, versions and configs as the previous index. The
    // setup is still checked (with the previous facts, which are the current ones).
    let reuse_ok = prev
        .as_ref()
        .is_some_and(|p| !rebuild && p.header.syntax_version == trace_syntax::EXTRACTOR_VERSION);
    if reuse_ok && plan.is_noop() {
        let t = Instant::now();
        let unchanged = match prev.as_ref() {
            Some(p) => {
                let files: Vec<(&str, Language)> =
                    p.files.iter().map(|f| (f.path.as_str(), f.language)).collect();
                let pending = pending_now(&files, &settings);
                if auto_install(&host, &tools, &product_languages(&files, &pending), progress)? {
                    tools = ToolEnv::discover(config, &paths.home, &paths.root)?;
                }
                let backends = trace_semantic::registry(&tools);
                is_unchanged(p, &plan, &backends, &host, &tools, &files, &pending)?
            }
            None => false,
        };
        if unchanged {
            if let Some(prev) = prev {
                secs.semantic = t.elapsed().as_secs_f64();
                profile.index(&prev);
                profile.line("unchanged", secs.semantic);
                let report =
                    build_report(paths, &prev, "unchanged", &plan, 0, SemanticCounts::default(), secs);
                return Ok(PipelineOutput {
                    index: prev,
                    changed: false,
                    report,
                });
            }
        }
    }

    let reuse_prev = prev.as_ref().filter(|_| reuse_ok);
    let mut build = Build {
        paths,
        reuse_prev,
        files,
        diagnostics: Vec::new(),
        omitted: scanned.omitted.clone(),
        changed_during: false,
    };
    if let Some(reason) = &invalidated {
        build.diagnostics.push(Diagnostic::new(
            "cache_invalidated",
            None,
            format!("previous index discarded ({reason}); rebuilt from sources"),
        ));
    }

    // 2. Syntax (+ header languages from the facts and the includers).
    let t = Instant::now();
    let reparsed = syntax_phase(&mut build, progress);
    apply_header_languages(&mut build.files);
    // Interface rule + new same-named declarations (DESIGN §1.14.5, rules 3-4).
    let (interfaces, declared) = {
        let dirty: HashSet<&str> = plan.dirty().collect();
        let dirty_facts: Vec<(&str, Option<&FileFacts>)> = build
            .files
            .iter()
            .filter(|f| dirty.contains(f.path.as_str()))
            .map(|f| (f.path.as_str(), f.facts.as_ref()))
            .collect();
        (
            incremental::interface_changed(build.reuse_prev, &plan, dirty_facts.iter().copied()),
            incremental::changed_declaration_names(build.reuse_prev, &plan, dirty_facts.iter().copied()),
        )
    };
    secs.syntax = t.elapsed().as_secs_f64();
    profile.line("syntax", secs.syntax);

    // 3-5. Languages, automatic install, setup: all failures of all languages in ONE error.
    let t = Instant::now();
    let pairs: Vec<(String, Language)> = current_files(&build.files);
    let file_refs: Vec<(&str, Language)> = pairs.iter().map(|(p, l)| (p.as_str(), *l)).collect();
    let pending = pending_now(&file_refs, &settings);
    if auto_install(&host, &tools, &product_languages(&file_refs, &pending), progress)? {
        // The new tools are in the MANIFEST now: resolve them again before the preflight.
        tools = ToolEnv::discover(config, &paths.home, &paths.root)?;
    }
    let backends: Vec<Box<dyn Backend>> = trace_semantic::registry(&tools);
    let sem = {
        let facts_by_path: HashMap<&str, &FileFacts> = build
            .files
            .iter()
            .filter_map(|f| f.facts.as_ref().map(|x| (f.path.as_str(), x)))
            .collect();
        let facts = |p: &str| -> Option<&FileFacts> { facts_by_path.get(p).copied() };
        setup_phase(&backends, &host, &tools, &file_refs, &facts, &pending)?
    };
    {
        let prepared: Vec<(Vec<Language>, &BTreeMap<String, String>)> = sem
            .assign
            .iter()
            .zip(&sem.prepared)
            .map(|((_, l), p)| (l.clone(), &p.pending_dirs))
            .collect();
        let reasons = pending_reasons(&file_refs, &pending, &prepared, &settings);
        for f in &mut build.files {
            f.pending = reasons.get(f.path.as_str()).cloned();
        }
    }
    let library_roots: Vec<trace_env::LibraryRoot> = sem
        .prepared
        .iter()
        .flat_map(|p| p.library_roots.iter().cloned())
        .collect();
    let installed_packages = trace_library::installed::InstalledPackages::from_roots(&library_roots);
    let setup_secs = t.elapsed().as_secs_f64();
    profile.line("setup", setup_secs);

    // 6. Semantic.
    let t = Instant::now();
    let mut one_shot: SemanticSessions;
    let (sessions, persistent) = match sessions {
        Some(live) => (live, true),
        None => {
            one_shot = SemanticSessions::new(paths).unwrap_or_else(|_| SemanticSessions::in_memory());
            (&mut one_shot, false)
        }
    };
    let (runs, counts, requery) = semantic_phase(
        &mut build,
        &plan,
        &sem,
        &scanned.configs,
        SemanticOptions {
            rebuild,
            sessions,
            persistent,
            interfaces: &interfaces,
            declared: &declared,
            policy: &stale_policy,
        },
        progress,
    )?;
    secs.semantic = setup_secs + t.elapsed().as_secs_f64();
    profile.line("semantic", secs.semantic);

    // 7. Link, families, library knowledge, bridges, inference, decisions: the one
    // post-semantic code path ([`update`]), full or incremental per the delta.
    let support = support_rows(&build.files, &runs, &sem);
    let files = std::mem::take(&mut build.files);
    let (records, configs, header) = records_and_header(files, &build, prev.as_ref(), &scanned);
    let full = reuse_prev.is_none_or(|p| needs_full_update(p, &plan));
    let delta = index_delta(if full { None } else { reuse_prev }, &plan, &records, &requery, &interfaces);
    let Build {
        diagnostics,
        omitted,
        changed_during,
        ..
    } = build;
    let library = trace_library::Library::open(&paths.home)?.with_roots(library_roots);
    let (mut index, delta) = update::apply(
        update::PostSemantic {
            config,
            prev: if delta.full { None } else { prev },
            header,
            files: records,
            removed: plan.removed.clone(),
            configs,
            omitted,
            delta,
            support,
            backend_runs: runs,
            diagnostics,
            library: &library,
            installed: &installed_packages,
            secs: &mut secs,
        },
        progress,
        &mut profile,
    )?;

    // 8. Persist.
    let t = Instant::now();
    progress.phase("persist", 0, 1);
    let rescanned = scan(paths, config, Some(&index), IndexMode::Incremental).ok();
    if changed_during || rescanned.is_some_and(|s| s.fingerprint != index.header.inventory_fingerprint) {
        index.diagnostics.push(Diagnostic::new(
            "sources_changed_during_index",
            None,
            "sources changed while indexing; the next command updates the index".to_string(),
        ));
    }
    update::persist(paths, &index, &delta)?;
    secs.persist = t.elapsed().as_secs_f64();
    progress.phase("persist", 1, 1);
    profile.line("persist", secs.persist);
    profile.line("total", secs.total());

    let report = build_report(
        paths,
        &index,
        if rebuild { "rebuild" } else { "incremental" },
        &plan,
        reparsed,
        counts,
        secs,
    );
    Ok(PipelineOutput {
        index,
        changed: true,
        report,
    })
}

impl Build<'_> {
    /// Load bytes for the given files in parallel (read-only, safe path checks).
    fn load(&mut self, ids: &[usize]) {
        let root = &self.paths.root;
        let wanted: HashSet<usize> = ids.iter().copied().collect();
        let changed = self
            .files
            .par_iter_mut()
            .enumerate()
            .filter(|(i, f)| wanted.contains(i) && f.bytes.is_none() && !f.skip)
            .map(|(_, f)| match read_source(root, &f.path) {
                Ok(b) => {
                    let h = Hash32::of(&b);
                    f.bytes = Some(b);
                    f.bytes_hash = Some(h);
                    h != f.hash
                }
                Err(e) => {
                    f.diagnostics
                        .push(Diagnostic::new("unreadable", Some(f.path.clone()), e.to_string()));
                    false
                }
            })
            .reduce(|| false, |a, b| a || b);
        self.changed_during |= changed;
    }
}

fn current_files(files: &[FileState]) -> Vec<(String, Language)> {
    files
        .iter()
        .filter(|f| !f.skip)
        .map(|f| (f.path.clone(), f.language))
        .collect()
}

/// Everything the setup phase reads besides the files and the tools.
struct Host<'e> {
    paths: &'e RepoPaths,
    config: &'e Settings,
    settings: &'e RepoSettings,
    platform: trace_env::os::Platform,
    vars: trace_env::os::EnvVars,
    /// Automatic installs of default languages (off with `TRACE_OFFLINE=1`, TRACE_NO_AUTO_INSTALL=1,
    /// `semantic.auto_install = false`).
    auto_install: bool,
    /// What updates do with stale dependents.
    stale: StalePolicy,
}

#[cfg(test)]
#[path = "../../tests/unit/pipeline/mod.rs"]
mod tests;
