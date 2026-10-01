//! A workspace = one inspected root + its cache + loaded index + configuration, plus the warm
//! state a long-running process (`trace index --watch`) keeps between updates: search index,
//! uid table, context ranking cache and persistent semantic analyzer sessions.
//!
//! Settings: `trace index --allow-build` and `--env <path>` ([`OpenOptions`]) are stored in
//! the repository's settings (`<repo cache>/settings.json`, never in the repository) before
//! any pipeline run; every later command reads them. An `--env` path no ecosystem accepts is
//! `EnvNotFound` (`No Python environment found at ...`, naming the product language with the
//! most files among those that declare dependencies).
//!
//! Freshness policy: `open` loads `index.bin`; if missing or corrupt/incompatible it builds
//! (full); otherwise it re-scans the inventory (metadata shortcut: unchanged size+mtime
//! reuse the stored hash, anything else is re-hashed) and runs an incremental update when
//! the inventory fingerprint differs. Every update analyses all product code, test files
//! included, or stops with the setup error (no fallback). Pending languages and sub-projects
//! (only tests / fixtures / examples, DESIGN §1.13) are set up by the first query that needs
//! one of their files ([`Workspace::ensure_ready`], [`Workspace::resolve_ready`]). `status`
//! uses [`OpenOptions::read_only`] and never updates. Progress lines go to stderr only when
//! asked; install lines of the automatic install always.
//!
//! Protected locations (never inspected, never an ancestor of the root):
//! `trace_core::paths::forbidden_roots()`.
//!
//! Files: opening, freshness and updates here; `resolve` (index access, selectors,
//! suggestions), `settings` (`--allow-build` / `--env` repository settings).

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use trace_core::cache::{load_index, save_index, CacheLock};
use trace_core::config::Settings;
use trace_core::paths::{forbidden_roots, is_within, RepoPaths};
use trace_core::repo_settings::RepoSettings;
use trace_core::{CoreError, FileId, Index, Language, SupportLevel, SymbolId, Tier, TRACE_VERSION};
use trace_semantic::SemanticSessions;

use crate::caches::{stats_path, Stats};
use crate::pipeline::{self, IndexMode, IndexProgress, PipelineInput, Quiet, Scanned, StderrProgress};
use crate::report::{Envelope, IndexInfo, IndexReport};
use crate::search::SearchIndex;
use crate::{AnalysisError, Result};

mod resolve;
mod settings;

pub use resolve::Relaxed;
use settings::save_settings;

/// What stands between the loaded index and the files on disk ([`Workspace::refresh`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Freshness {
    Current,
    /// Files changed since the index was stamped (or there is no index).
    FilesChanged,
    /// The files match; dependents of an earlier interface change are still stale.
    StaleDependents,
}

/// How to open a workspace.
#[derive(Clone, Debug)]
pub struct OpenOptions {
    pub include: Tier,
    /// Do not build or update the index (status).
    pub read_only: bool,
    /// Emit progress lines on stderr.
    pub progress: bool,
    /// Long-running process (`index --watch`): keep analyzer sessions alive.
    pub persistent: bool,
    /// `TRACE_OFFLINE=1`: no automatic installs of default-language servers.
    pub offline: bool,
    /// Do not traverse cross-language bridge edges (library switch; the CLI always
    /// traverses them).
    pub no_bridges: bool,
    /// `trace index --allow-build`: remember the approval for this repository.
    pub allow_build: bool,
    /// `trace index --env <path>`: remember these environments for this repository.
    pub env: Vec<PathBuf>,
}

/// An opened workspace.
pub struct Workspace {
    pub paths: RepoPaths,
    pub config: Settings,
    pub index: Option<Index>,
    /// What this command did to keep the index fresh: `none` | `incremental` | `full`.
    pub updated: &'static str,
    pub include: Tier,
    started: Instant,
    /// The loaded index matches the sources (false after a lock fallback or read-only open).
    fresh: bool,
    /// Why a previous cache was discarded (reported by the next build and by status).
    invalidated: Option<String>,
    progress: bool,
    /// `TRACE_OFFLINE=1`: automatic installs are off.
    offline: bool,
    search: OnceCell<SearchIndex>,
    uids: OnceCell<HashMap<String, SymbolId>>,
    /// Counter increments not yet merged into `stats.json`.
    stats: RefCell<Stats>,
    sessions: Option<SemanticSessions>,
    /// Modification time of `index.bin` when loaded or last written by this process.
    index_stamp: Option<SystemTime>,
    /// Traverse bridge edges (false with `OpenOptions::no_bridges`); applied to every graph.
    bridges: bool,
    /// What updates do with stale dependents (`index --watch` defers them; commands resolve
    /// all).
    stale_policy: trace_core::incremental::StalePolicy,
}

impl Workspace {
    /// Open `root` (canonicalized), ensuring a fresh index unless read-only.
    pub fn open(root: &Path, opts: &OpenOptions) -> Result<Workspace> {
        check_forbidden(root)?;
        let paths = RepoPaths::resolve(root)?;
        check_forbidden(&paths.root)?;
        let config = Settings::load(&paths.home)?;
        // Loaded once per command: engine code without a settings parameter reads these.
        trace_core::config::install(&config);
        save_settings(&paths, &config, opts)?;
        let (index, invalidated) = load_existing(&paths);
        let sessions = if opts.persistent {
            Some(SemanticSessions::new(&paths)?)
        } else {
            None
        };
        let mut ws = Workspace {
            index_stamp: trace_core::cache::index_stamp(&paths.index_file),
            paths,
            config,
            index,
            updated: "none",
            include: opts.include,
            started: Instant::now(),
            fresh: false,
            invalidated,
            progress: opts.progress,
            offline: opts.offline,
            search: OnceCell::new(),
            uids: OnceCell::new(),
            stats: RefCell::new(Stats::default()),
            sessions,
            bridges: !opts.no_bridges,
            stale_policy: trace_core::incremental::StalePolicy::default(),
        };
        if !opts.read_only {
            ws.refresh()?;
        }
        Ok(ws)
    }

    /// How the following updates treat stale dependents (DESIGN §1.14.4): `index --watch`
    /// defers them to its following batches; the default resolves all.
    pub fn set_stale_policy(&mut self, policy: trace_core::incremental::StalePolicy) {
        self.stale_policy = policy;
    }

    /// Watcher hints for the live analyzer sessions (`index --watch`): the repository-relative paths
    /// that changed, or `None` when events were lost (mirror workspaces then sync only these
    /// paths instead of walking the tree).
    pub fn hint_changes(&mut self, changed: Option<Vec<String>>) {
        if let Some(sessions) = self.sessions.as_mut() {
            sessions.hint_changes(changed);
        }
    }

    fn progress_sink(&self) -> Box<dyn IndexProgress> {
        if self.progress {
            Box::new(StderrProgress::terminal())
        } else {
            Box::new(Quiet)
        }
    }

    /// Upper bound of a wait for another process that updates the index, then the `locked`
    /// error (`analysis.wait_limit_secs`).
    pub fn wait_limit(&self) -> Duration {
        Duration::from_secs(self.config.analysis.wait_limit_secs)
    }

    /// Re-check freshness (metadata scan) and bring the index up to date before a query
    /// answers (PLAN decision 13: never stale). When `trace index --watch` runs for this
    /// repository (it holds the watch lock), wait for it to publish the update; when another
    /// process is updating the index (build lock), wait for it; otherwise update in-process.
    /// Stale dependents the watcher left are waited for as long as it keeps publishing
    /// batches (`analysis.stale_grace_secs`), never re-queried cold here while its warm
    /// servers do it. A newly published index is reloaded. Waiting is bounded by
    /// `analysis.wait_limit_secs`.
    pub fn refresh(&mut self) -> Result<()> {
        let a = &self.config.analysis;
        let (watch_grace, stale_grace) =
            (Duration::from_millis(a.watch_grace_ms), Duration::from_secs(a.stale_grace_secs));
        let (wait_limit, wait_poll) = (self.wait_limit(), Duration::from_millis(a.wait_poll_ms));
        let started = Instant::now();
        let mut noted = false;
        // Last time the watcher published an index (its stale-dependent batches publish too).
        let mut progress_at = started;
        loop {
            let before = self.index_stamp;
            self.reload_if_published();
            if self.index_stamp != before {
                progress_at = Instant::now();
            }
            let watching = self.paths.watching();
            if watching {
                // Changed files: the watcher's debounce and update (`watch_grace`). Only stale
                // dependents left: the watcher resolves them in its next batch, warm - wait
                // as long as it keeps publishing instead of re-querying them cold here.
                let wait = match self.freshness()? {
                    Freshness::Current => false,
                    Freshness::FilesChanged => started.elapsed() < watch_grace,
                    Freshness::StaleDependents => {
                        started.elapsed() < wait_limit && progress_at.elapsed() < stale_grace
                    }
                };
                if wait {
                    self.note_wait(
                        &mut noted,
                        "Waiting for trace index --watch to update the changed files...",
                    );
                    std::thread::sleep(wait_poll);
                    continue;
                }
            }
            let mut progress = self.progress_sink();
            match self.update_if_stale(progress.as_mut()) {
                Ok(_) => return Ok(()),
                Err(AnalysisError::Core(CoreError::Locked(path))) => {
                    if started.elapsed() >= wait_limit {
                        return Err(AnalysisError::Core(CoreError::Locked(path)));
                    }
                    let what = if watching {
                        "Waiting for trace index --watch to update the changed files..."
                    } else {
                        "Waiting for another trace process to finish updating this index..."
                    };
                    self.note_wait(&mut noted, what);
                    std::thread::sleep(wait_poll);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// One progress line while waiting for another process (terminal only, once).
    fn note_wait(&self, noted: &mut bool, what: &str) {
        if self.progress && !*noted {
            eprintln!("{what}");
        }
        *noted = true;
    }

    /// Load the index another process (`trace index --watch`, a concurrent command)
    /// published since this workspace loaded or wrote it.
    pub fn reload_if_published(&mut self) {
        let now = trace_core::cache::index_stamp(&self.paths.index_file);
        if now.is_none() || now == self.index_stamp {
            return;
        }
        if let (Some(index), _) = load_existing(&self.paths) {
            self.replace_index(index);
        }
    }

    /// Whether the loaded index matches the files on disk, and whether stale dependents are
    /// left.
    fn freshness(&self) -> Result<Freshness> {
        let Some(prev) = self.index.as_ref() else {
            return Ok(Freshness::FilesChanged);
        };
        let scanned = pipeline::scan(&self.paths, &self.config, Some(prev), IndexMode::Incremental)?;
        Ok(if !pipeline::is_current(prev, &scanned) {
            Freshness::FilesChanged
        } else if !prev.stale.is_empty() {
            Freshness::StaleDependents
        } else {
            Freshness::Current
        })
    }

    /// Incremental update when the inventory differs from the loaded index (full build when
    /// there is none). `None` when the index was already current.
    pub fn update_if_stale(&mut self, progress: &mut dyn IndexProgress) -> Result<Option<IndexReport>> {
        let scanned = match &self.index {
            Some(prev) => {
                let scanned = pipeline::scan(&self.paths, &self.config, Some(prev), IndexMode::Incremental)?;
                // Stale dependents left by `index --watch` count as changed unless this
                // update defers them too.
                let stale_resolved = prev.stale.iter().all(|f| !self.stale_policy.resolves(f));
                if pipeline::is_current(prev, &scanned) && stale_resolved {
                    self.fresh = true;
                    return Ok(None);
                }
                Some(scanned)
            }
            None => None,
        };
        let full = self.index.is_none();
        let report = self.run_pipeline(IndexMode::Incremental, scanned, progress)?;
        if report.mode != "unchanged" {
            self.updated = if full { "full" } else { "incremental" };
        }
        Ok(Some(report))
    }

    /// Run the pipeline explicitly (`trace index`): every product file, test files included.
    pub fn reindex(&mut self, mode: IndexMode, progress: &mut dyn IndexProgress) -> Result<IndexReport> {
        let full = self.index.is_none() || mode == IndexMode::Rebuild;
        let report = self.run_pipeline(mode, None, progress)?;
        if report.mode != "unchanged" {
            self.updated = if full { "full" } else { "incremental" };
        }
        Ok(report)
    }

    /// Files not analysed yet in the loaded index (pending languages and sub-projects).
    pub fn pending_files(&self) -> usize {
        self.index.as_ref().map_or(0, pipeline::pending_files)
    }

    /// Set up the pending languages / sub-projects of `files` (DESIGN §1.13): they are
    /// remembered in the repository settings and the index is updated with them analysed
    /// (the automatic install runs first for a default language). `Ok(true)` when anything
    /// was set up (symbol ids may have changed: resolve again), `Ok(false)` when none of
    /// `files` is pending. A setup failure is the error (nothing is answered from syntax) and
    /// the settings stay as they were.
    pub fn ensure_ready(&mut self, files: &[FileId]) -> Result<bool> {
        let Some(index) = self.index.as_ref() else {
            return Ok(false);
        };
        let mut languages: BTreeSet<Language> = BTreeSet::new();
        let mut dirs: BTreeSet<String> = BTreeSet::new();
        for &file in files {
            if file.idx() >= index.files.len() {
                continue;
            }
            let rec = index.file(file);
            if rec.support != SupportLevel::Pending {
                continue;
            }
            match rec.pending.as_deref().and_then(pipeline::pending_dir) {
                Some(dir) => {
                    dirs.insert(dir.to_string());
                }
                None => {
                    languages.insert(rec.language);
                }
            }
        }
        if languages.is_empty() && dirs.is_empty() {
            return Ok(false);
        }
        let before = RepoSettings::load(&self.paths)?;
        let mut settings = before.clone();
        settings.ready_languages.extend(languages);
        settings.ready_dirs.extend(dirs);
        settings.save(&self.paths)?;
        let mut progress = self.progress_sink();
        match self.run_pipeline(IndexMode::Incremental, None, progress.as_mut()) {
            Ok(report) => {
                if report.mode != "unchanged" && self.updated == "none" {
                    self.updated = "incremental";
                }
                Ok(true)
            }
            Err(e) => {
                // Best effort: the error is what the user sees either way.
                let _ = before.save(&self.paths);
                Err(e)
            }
        }
    }

    /// [`Workspace::resolve`] for a query's selector or target: when the symbol lies in a
    /// pending file, its language / sub-project is set up first and the reference resolved
    /// again against the updated index.
    pub(crate) fn resolve_ready(&mut self, reference: &str) -> Result<SymbolId> {
        let id = self.resolve(reference)?;
        let file = self.index()?.symbol(id).file;
        if self.ensure_ready(&[file])? {
            self.resolve(reference)
        } else {
            Ok(id)
        }
    }

    /// [`Workspace::resolve_scope`] with the pending-file setup of
    /// [`Workspace::resolve_ready`] (`deps` / `path` endpoints).
    pub(crate) fn resolve_scope_ready(&mut self, reference: &str) -> Result<SymbolId> {
        let id = self.resolve_scope(reference)?;
        let file = self.index()?.symbol(id).file;
        if self.ensure_ready(&[file])? {
            self.resolve_scope(reference)
        } else {
            Ok(id)
        }
    }

    fn run_pipeline(
        &mut self,
        mode: IndexMode,
        scanned: Option<Scanned>,
        progress: &mut dyn IndexProgress,
    ) -> Result<IndexReport> {
        let prev = self.index.take();
        let result = pipeline::run(
            PipelineInput {
                paths: &self.paths,
                config: &self.config,
                prev,
                mode,
                sessions: self.sessions.as_mut(),
                scanned,
                invalidated: self.invalidated.clone(),
                offline: self.offline,
                stale: self.stale_policy.clone(),
            },
            progress,
        );
        match result {
            Ok(out) => {
                let r = &out.report;
                self.stats
                    .get_mut()
                    .semantic_files
                    .record((r.semantic_reused + r.semantic_requeried) as u64, r.semantic_reused as u64);
                if out.changed {
                    self.replace_index(out.index);
                } else {
                    self.index = Some(out.index);
                }
                self.fresh = true;
                Ok(out.report)
            }
            Err(e) => {
                // The pipeline consumed the previous index: reload what is persisted (the
                // index is never written when a run fails).
                let (index, _) = load_existing(&self.paths);
                self.index = index;
                self.index_stamp = trace_core::cache::index_stamp(&self.paths.index_file);
                Err(e)
            }
        }
    }

    /// Install a freshly built index and drop everything derived from the previous one.
    fn replace_index(&mut self, index: Index) {
        self.index = Some(index);
        self.search = OnceCell::new();
        self.uids = OnceCell::new();
        self.invalidated = None;
        self.index_stamp = trace_core::cache::index_stamp(&self.paths.index_file);
    }

    /// Why a previous cache was discarded, if it was.
    pub(crate) fn invalidated(&self) -> Option<&str> {
        self.invalidated.as_deref()
    }

    /// Envelope for a report of `command`.
    pub fn envelope(&self, command: &'static str) -> Envelope {
        let (files, symbols, fingerprint) = self
            .index
            .as_ref()
            .map(|i| (i.files.len(), i.symbols.len(), i.header.inventory_fingerprint.short()))
            .unwrap_or((0, 0, String::new()));
        Envelope {
            command,
            schema: crate::report::SCHEMA,
            trace_version: TRACE_VERSION,
            root: self.paths.root_display(),
            include: self.include.as_str(),
            index: IndexInfo {
                fresh: self.fresh,
                updated: self.updated,
                files,
                symbols,
                fingerprint,
            },
            seconds: self.started.elapsed().as_secs_f64(),
            // Filled per command (`Envelope::with_rows`-style helpers in `cards`).
            tiers_used: Vec::new(),
            bridges: self.bridges,
            completeness: None,
        }
    }

    /// Idle-time compaction of the delta journal (`index --watch`, I-11): when the journal
    /// is large enough ([`trace_core::cache::journal_wants_compaction`]), rewrite the base
    /// from the loaded index under the build lock, unless another process published a newer
    /// index meanwhile (or holds the lock: tried again later). Updates then keep appending
    /// small segments and never pay for a whole rewrite. `Ok(true)` when compacted.
    pub fn compact_index(&mut self) -> Result<bool> {
        if !trace_core::cache::journal_wants_compaction(&self.paths.index_file) {
            return Ok(false);
        }
        let Some(index) = self.index.as_ref() else {
            return Ok(false);
        };
        let _lock = match CacheLock::acquire(&self.paths.lock_file) {
            Ok(lock) => lock,
            Err(CoreError::Locked(_)) => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        if trace_core::cache::index_stamp(&self.paths.index_file) != self.index_stamp {
            return Ok(false);
        }
        save_index(&self.paths.index_file, index)?;
        self.index_stamp = trace_core::cache::index_stamp(&self.paths.index_file);
        Ok(true)
    }

    /// `trace index --watch` at launch (I-10): start the language servers of every analysed
    /// language now and wait until they are ready, so the first edit is as fast as the next
    /// ones. Backends whose sessions already run are skipped. Returns the backends started.
    pub fn warm_servers(&mut self) -> Result<Vec<String>> {
        let (Some(index), Some(sessions)) = (self.index.as_ref(), self.sessions.as_mut()) else {
            return Ok(Vec::new());
        };
        pipeline::warm_sessions(&self.paths, &self.config, index, sessions)
    }

    /// Persist the cache statistics counted in memory (best effort: a failed merge only
    /// loses counters).
    pub fn flush(&mut self) {
        let delta = std::mem::take(self.stats.get_mut());
        let _ = Stats::merge_into(&stats_path(&self.paths), &delta);
    }
}

/// Load the persisted index; `(None, Some(reason))` when it exists but cannot be used.
fn load_existing(paths: &RepoPaths) -> (Option<Index>, Option<String>) {
    if !paths.index_file.exists() {
        return (None, None);
    }
    match load_index(&paths.index_file, &paths.root_display()) {
        Ok(index) => (Some(index), None),
        Err(e) => (None, Some(e.to_string())),
    }
}

/// Refuse roots that are, contain, or are inside a protected location
/// (`trace_core::paths::forbidden_roots`, compared lexically on canonical forms; the
/// protected locations themselves are never touched).
pub fn check_forbidden(root: &Path) -> Result<()> {
    check_against(root, &forbidden_roots())
}

fn check_against(root: &Path, forbidden: &[PathBuf]) -> Result<()> {
    let canonical = std::fs::canonicalize(root)
        .map(trace_core::inventory::strip_verbatim)
        .unwrap_or_else(|_| root.to_path_buf());
    if forbidden
        .iter()
        .any(|f| is_within(&canonical, f) || is_within(f, &canonical))
    {
        return Err(CoreError::Excluded(root.to_path_buf()).into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/workspace/mod.rs"]
mod tests;
