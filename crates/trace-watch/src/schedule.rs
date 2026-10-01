//! The update queue of `trace index --watch` (owner speed; DESIGN §1.14.2-§1.14.3).
//!
//! [`Scheduler::queue`] records every change the watcher reports (a source file by path;
//! anything else - a directory, a configuration file, an event overflow - asks for an
//! inventory rescan). [`Scheduler::step`] runs one batch: the incremental update of the
//! queued changes (`Workspace::update_if_stale`: only changed files, the interface rule,
//! dependents left stale), else one batch of stale dependents (`Workspace::reindex` with the
//! batch to resolve). A change stops being queued when an update that started after it was
//! committed.
//!
//! Costs (I-11, I-12): a batch whose files all hash unchanged (a save without changes, a
//! touched file, a directory event) finds the index current and does nothing: no server
//! work, no line ([`Step::Idle`]). Stale dependents are resolved in ONE engine run of up to
//! `watch.stale_batch` files: every batch pays a whole pipeline pass (setup check, link,
//! inference, persistence), so many small batches cost more than one cold re-query of the
//! same files. Their batch is reported as [`Step::Dependents`] (no update line in text
//! mode: the edit's own update already had its line). Every commit is persisted, so other commands (which wait while the watcher holds
//! the repository's watch lock) read the new graph as soon as it is published.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};
use std::time::Instant;

use trace_analysis::pipeline::{IndexMode, Quiet};
use trace_analysis::report::IndexReport;
use trace_analysis::{AnalysisError, Workspace};
use trace_core::incremental::StalePolicy;
use trace_core::CoreError;

use crate::watcher::{Changes, Timing};

/// What one [`Scheduler::step`] did.
#[derive(Debug)]
pub enum Step {
    /// Nothing to do, or the queued changes left the index as it was (unchanged files).
    Idle,
    /// The incremental update of queued changes (one update line).
    Updated(IndexReport),
    /// A batch of stale dependents of an earlier update was resolved.
    Dependents(IndexReport),
}

/// Queued changes by file, the stale set and the generation counter.
pub(crate) struct Scheduler {
    /// Batch size and retry delays (`watch.*`). The stale batch is in practice the whole
    /// stale set of an edit in one engine run (module docs); its bound keeps a new edit from
    /// waiting behind more than one large batch.
    timing: Timing,
    /// Changed files waiting for an update (path -> generation of the last change).
    queued: BTreeMap<String, u64>,
    /// Generation of the newest change that asks for an inventory rescan.
    rescan: Option<u64>,
    generation: u64,
    /// Stale dependents of the committed index.
    stale: BTreeSet<String>,
    /// No batch before this instant (retry backoff after a failure).
    retry_at: Option<Instant>,
}

/// Repository-relative, `/`-separated form of `abs` under `root`.
fn relative(root: &Path, abs: &Path) -> Option<String> {
    let rel = abs.strip_prefix(root).ok()?;
    let mut parts: Vec<&str> = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(name) => parts.push(name.to_str()?),
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

impl Scheduler {
    pub fn new(timing: Timing) -> Scheduler {
        Scheduler {
            timing,
            queued: BTreeMap::new(),
            rescan: None,
            generation: 0,
            stale: BTreeSet::new(),
            retry_at: None,
        }
    }

    /// Queue the watcher's changes: every changed source by path; a rescan for anything that
    /// is not a source file. The live analyzer sessions get the changed paths as hints.
    pub fn queue(&mut self, ws: &mut Workspace, changes: Changes) {
        if changes.is_empty() {
            return;
        }
        self.generation += 1;
        let g = self.generation;
        self.retry_at = None;
        if changes.overflow {
            self.rescan = Some(g);
        }
        let root = ws.paths.root.clone();
        let hints: Option<Vec<String>> = if changes.overflow {
            None
        } else {
            changes.paths.iter().map(|abs| relative(&root, abs)).collect()
        };
        ws.hint_changes(hints);
        for abs in &changes.paths {
            let Some(rel) = relative(&root, abs) else {
                self.rescan = Some(g);
                continue;
            };
            match trace_core::languages::from_path(Path::new(&rel)) {
                Some(_) if !abs.is_dir() => {
                    self.queued.insert(rel, g);
                }
                // Directories, configuration and build files, files without a known
                // extension: the inventory decides what changed.
                _ => self.rescan = Some(g),
            }
        }
    }

    /// One batch: the incremental update of queued changes (stale dependents of an interface
    /// change are left stale), else one batch of stale dependents. [`Step::Idle`] when there
    /// was nothing to do or nothing changed.
    pub fn step(&mut self, ws: &mut Workspace) -> anyhow::Result<Step> {
        if self.rescan.is_some() || !self.queued.is_empty() {
            let started = self.generation;
            // A command may have published an update itself meanwhile.
            ws.reload_if_published();
            ws.set_stale_policy(StalePolicy::Defer {
                resolve: BTreeSet::new(),
            });
            let result = ws.update_if_stale(&mut Quiet);
            ws.set_stale_policy(StalePolicy::ResolveAll);
            let report = match result {
                Ok(report) => report,
                Err(e) => return self.failed(e),
            };
            self.committed(ws, started);
            return Ok(match report {
                Some(report) if report.mode != "unchanged" => Step::Updated(report),
                _ => Step::Idle,
            });
        }
        let batch: BTreeSet<String> = self.stale.iter().take(self.timing.stale_batch).cloned().collect();
        if batch.is_empty() {
            return Ok(Step::Idle);
        }
        ws.reload_if_published();
        ws.set_stale_policy(StalePolicy::Defer { resolve: batch });
        let result = ws.reindex(IndexMode::Incremental, &mut Quiet);
        ws.set_stale_policy(StalePolicy::ResolveAll);
        match result {
            Ok(report) => {
                self.refresh_stale(ws);
                Ok(Step::Dependents(report))
            }
            Err(e) => self.failed(e),
        }
    }

    /// A failed batch: retried soon when another process held the build lock (nothing to
    /// report), else after `watch.retry_after_ms` or the next change (the error is reported).
    fn failed(&mut self, e: AnalysisError) -> anyhow::Result<Step> {
        if matches!(e, AnalysisError::Core(CoreError::Locked(_))) {
            self.retry_at = Some(Instant::now() + self.timing.retry_locked);
            return Ok(Step::Idle);
        }
        self.retry_at = Some(Instant::now() + self.timing.retry_after);
        Err(e.into())
    }

    /// An update that started at generation `started` was committed: queued changes up to it
    /// are done; the stale set is taken from the new index.
    fn committed(&mut self, ws: &Workspace, started: u64) {
        self.queued.retain(|_, g| *g > started);
        if self.rescan.is_some_and(|g| g <= started) {
            self.rescan = None;
        }
        self.retry_at = None;
        self.refresh_stale(ws);
    }

    /// Take the stale dependents from the committed index.
    pub(crate) fn refresh_stale(&mut self, ws: &Workspace) {
        if let Ok(index) = ws.index() {
            self.stale = index.stale.clone();
        }
    }

    /// No pending work at all.
    pub fn idle(&self) -> bool {
        self.queued.is_empty() && self.rescan.is_none() && self.stale.is_empty()
    }

    /// Whether work may run now (not idle, and not right after a failure).
    pub fn ready(&self) -> bool {
        !self.idle() && self.retry_at.is_none_or(|t| Instant::now() >= t)
    }
}

#[cfg(test)]
#[path = "../tests/unit/schedule.rs"]
mod tests;
