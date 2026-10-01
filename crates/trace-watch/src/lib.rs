//! trace-watch: `trace index --watch` (PLAN decision 13, revised by the owner: option B).
//!
//! The one freshness mechanism besides the in-process catch-up of every command. It runs in
//! the FOREGROUND of the user's terminal, like a dev server with auto-reload: it keeps the
//! language servers warm (persistent analyzer sessions), runs a debounced file watcher,
//! applies incremental updates (only changed files; dependents of an interface change are
//! resolved in the following batches) and persists every commit as a delta. One short line
//! per update. trace never starts a process that outlives the command: there is no detached
//! host, no IPC and no idle timeout.
//!
//! Coordination with other commands happens only through the cache: the watcher holds the
//! repository's watch lock (`RepoPaths::watch_lock_file`, an OS file lock) for its whole
//! life; a command that finds the graph older than the files while the lock is held waits for
//! the watcher to publish the update (index stamp) instead of answering stale
//! (`trace_analysis::Workspace::refresh`).
//!
//! Speed (I-10, I-11): the language servers of every analysed language start at launch, right
//! after the catch-up (`Workspace::warm_servers`), so the first edit is as fast as the next
//! ones; a new language starts its server after the update that brought it. Unchanged event
//! batches print nothing; stale dependents are resolved in the next batch without a line of
//! their own (JSON mode prints their report); while idle the delta journal is compacted
//! (`Workspace::compact_index`), so an edit never pays for rewriting the whole index.
//!
//! Shutdown: Ctrl+C ends the process; the language servers exit when their input closes (on
//! Windows they also get the console interrupt; on Unix they lead their own process groups so
//! trace can stop their whole trees); nothing is lost because
//! every commit is persisted. The loop also ends when the root disappears.
#![forbid(unsafe_code)]

mod schedule;
mod watcher;

use std::time::{Duration, Instant};

use trace_analysis::pipeline::{IndexMode, Quiet};
use trace_analysis::report::IndexReport;
use trace_analysis::{AnalysisError, OpenOptions, Workspace};
use trace_core::cache::CacheLock;
use trace_core::incremental::StalePolicy;
use trace_core::paths::RepoPaths;
use trace_core::{CoreError, Tier};

use crate::schedule::{Scheduler, Step};
use crate::watcher::{Timing, Watcher};

/// How the watcher's lines look (trace-cli renders them; this crate never depends on it).
pub trait Lines {
    /// One line for an applied update: the compact JSON report with `json`, else the text
    /// line.
    fn update_line(&mut self, report: &IndexReport, seconds: f64, json: bool) -> String;
}

/// Options of one `trace index --watch`.
#[derive(Clone, Copy, Debug, Default)]
pub struct WatchOptions {
    /// `--json`: one compact `IndexReport` per line on stdout (else text lines on stderr).
    pub json: bool,
    /// `TRACE_OFFLINE=1`: no automatic installs.
    pub offline: bool,
}

/// Open the watcher's workspace: persistent sessions, no automatic update at open (the
/// watcher catches up itself).
fn open_workspace(root: &std::path::Path, opts: WatchOptions) -> anyhow::Result<Workspace> {
    let opts = OpenOptions {
        include: Tier::Inferred,
        read_only: true,
        progress: false,
        persistent: true,
        offline: opts.offline,
        no_bridges: false,
        allow_build: false,
        env: Vec::new(),
    };
    Ok(Workspace::open(root, &opts)?)
}

/// `trace index --watch`: take the watch lock, open the workspace (persistent sessions),
/// start the watcher, catch up, then apply updates until the root disappears (or Ctrl+C
/// ends the process).
pub fn run_watch(root: &std::path::Path, opts: WatchOptions, lines: &mut dyn Lines) -> anyhow::Result<()> {
    let root = trace_core::inventory::canonical_root(root)?;
    let paths = RepoPaths::resolve(&root)?;
    paths.ensure_repo_dir()?;
    // One watcher per repository: the OS lock is held for the watcher's whole life and is
    // released by the OS when the process ends, however it ends.
    let _watch_lock = match CacheLock::acquire(&paths.watch_lock_file()) {
        Ok(lock) => lock,
        Err(CoreError::Locked(_)) => {
            anyhow::bail!(
                "trace index --watch is already running for this folder. Use that one, or stop it first."
            )
        }
        Err(e) => return Err(e.into()),
    };
    let mut ws = open_workspace(&root, opts)?;
    // The watcher starts before the catch-up, so no change made meanwhile is lost.
    let watcher = Watcher::start(&root, &ws.config);
    let timing = Timing::of(&ws.config.watch);
    let mut scheduler = Scheduler::new(timing);

    // Catch-up: only changed files, dependents of an interface change left to the batches.
    // A command updating the index meanwhile (build lock) is waited for.
    let started = Instant::now();
    let report = loop {
        match catch_up(&mut ws) {
            Ok(report) => break report,
            Err(AnalysisError::Core(CoreError::Locked(_))) if started.elapsed() < ws.wait_limit() => {
                std::thread::sleep(timing.tick);
            }
            Err(e) => return Err(e.into()),
        }
    };
    scheduler.refresh_stale(&ws);
    emit(lines.update_line(&report, started.elapsed().as_secs_f64(), opts.json), opts.json);
    // Start the language servers now, not at the first change.
    let mut warmed = analysed_languages(&ws);
    warm(&mut ws, opts);
    if !opts.json {
        let how = if watcher.uses_events() {
            "file events".to_string()
        } else {
            format!("polling every {}s", timing.poll.as_secs())
        };
        eprintln!("Watching {} ({how}). Press Ctrl+C to stop.", root.display());
    }

    // An update or dependents batch was committed since the last idle-time compaction check.
    let mut compact_due = true;
    loop {
        let wait = if scheduler.ready() {
            Duration::ZERO
        } else {
            timing.tick
        };
        match watcher.changes().recv_timeout(wait) {
            Ok(changes) if !changes.is_empty() => {
                scheduler.queue(&mut ws, changes);
            }
            Ok(_) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            // The watcher thread ended: nothing more can be observed.
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
        if !root.is_dir() {
            break;
        }
        if scheduler.ready() {
            let started = Instant::now();
            match scheduler.step(&mut ws) {
                Ok(Step::Updated(report)) => {
                    emit(lines.update_line(&report, started.elapsed().as_secs_f64(), opts.json), opts.json);
                    flush(&mut ws);
                    compact_due = true;
                    // A language the index did not have before: start its server now.
                    let now = analysed_languages(&ws);
                    if now != warmed {
                        warmed = now;
                        warm(&mut ws, opts);
                    }
                }
                Ok(Step::Dependents(report)) => {
                    // The edit's own update already had its line; JSON consumers get the report.
                    if opts.json {
                        emit(lines.update_line(&report, started.elapsed().as_secs_f64(), true), true);
                    }
                    flush(&mut ws);
                    compact_due = true;
                }
                Ok(Step::Idle) => {}
                // The same error every command would report; retried after new changes.
                Err(e) => eprintln!("Error: {e}"),
            }
            continue;
        }
        // Idle: compact the delta journal now instead of inside a later edit's update.
        if scheduler.idle() && compact_due {
            compact_due = false;
            if let Err(e) = ws.compact_index() {
                eprintln!("Error: {e}");
            }
        }
    }
    flush(&mut ws);
    if !opts.json {
        eprintln!("Stopped watching: {} no longer exists.", root.display());
    }
    Ok(())
}

/// The catch-up at start: an incremental update against the persisted index (the committed
/// index's report when it is already current).
fn catch_up(ws: &mut Workspace) -> Result<IndexReport, AnalysisError> {
    ws.reload_if_published();
    ws.set_stale_policy(StalePolicy::Defer {
        resolve: Default::default(),
    });
    let caught_up = ws.update_if_stale(&mut Quiet);
    ws.set_stale_policy(StalePolicy::ResolveAll);
    match caught_up? {
        Some(report) => Ok(report),
        None => ws.reindex(IndexMode::Incremental, &mut Quiet),
    }
}

/// Code languages the index analyses (semantic support): a change starts the new servers.
fn analysed_languages(ws: &Workspace) -> std::collections::BTreeSet<trace_core::Language> {
    ws.index()
        .map(|index| {
            index
                .support
                .iter()
                .filter(|s| s.level == trace_core::SupportLevel::Semantic)
                .map(|s| s.language)
                .collect()
        })
        .unwrap_or_default()
}

/// Start the language servers that are not running yet (`Workspace::warm_servers`), with one
/// progress line in text mode. A failure is printed like an update error: the next update
/// reports the same error.
fn warm(ws: &mut Workspace, opts: WatchOptions) {
    let started = Instant::now();
    match ws.warm_servers() {
        Ok(backends) if !backends.is_empty() && !opts.json => {
            eprintln!(
                "Language servers ready in {:.1}s ({}).",
                started.elapsed().as_secs_f64(),
                backends.join(", ")
            );
        }
        Ok(_) => {}
        Err(e) => eprintln!("Error: {e}"),
    }
}

/// Persist the cache statistics counted in memory.
fn flush(ws: &mut Workspace) {
    ws.flush();
}

/// An update line: JSON reports on stdout, text lines on stderr.
fn emit(line: String, json: bool) {
    if json {
        println!("{line}");
    } else {
        eprintln!("{line}");
    }
}
