//! The file watcher of `trace index --watch` (owner speed; the only watcher implementation):
//! `notify` events on the root, filtered by the inventory rules
//! (hidden names, excluded directories - also events on an excluded directory itself, e.g. a
//! build tool creating `build/` -, the user's `inventory.exclude` globs, sensitive names,
//! gitignored - the same `ignore` rules), so build output, tool caches and excluded folders
//! never start an update; debounced (`watch.debounce_ms`) after the last relevant event and at
//! most `watch.max_delay_ms` under a continuous stream ([`Timing`]). A watcher error switches to
//! polling the inventory every `watch.poll_ms` (same rules; changed paths from size / modification time). Build and
//! configuration files are watched like sources (the root is watched recursively).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use trace_core::config::{Settings, WatchSettings};
use trace_core::inventory::{is_excluded_dir, is_sensitive_name};

/// The watch timings (`watch` settings).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    /// Quiet period after the last relevant event before the changes are delivered.
    pub debounce: Duration,
    /// Upper bound on how long a continuous stream of events can postpone delivery.
    pub max_delay: Duration,
    /// Polling interval (fallback mode).
    pub poll: Duration,
    /// How long the watch loop waits for changes before it looks at the root and idle work
    /// again.
    pub tick: Duration,
    /// Stale dependents resolved per batch.
    pub stale_batch: usize,
    /// A failed update is not retried before this long (new changes retry at once).
    pub retry_after: Duration,
    /// Another process holds the build lock (a command updating in-process): retry this soon.
    pub retry_locked: Duration,
}

impl Timing {
    pub fn of(watch: &WatchSettings) -> Timing {
        Timing {
            debounce: Duration::from_millis(watch.debounce_ms),
            max_delay: Duration::from_millis(watch.max_delay_ms),
            poll: Duration::from_millis(watch.poll_ms),
            tick: Duration::from_millis(watch.tick_ms),
            stale_batch: watch.stale_batch,
            retry_after: Duration::from_millis(watch.retry_after_ms),
            retry_locked: Duration::from_millis(watch.retry_locked_ms),
        }
    }
}

/// Changed paths after the debounce.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Absolute paths (as reported by the OS) of changed files and directories.
    pub paths: BTreeSet<PathBuf>,
    /// Rescan everything (events were lost or carried no paths).
    pub overflow: bool,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && !self.overflow
    }

    /// Fold `other` into these changes.
    pub fn merge(&mut self, other: Changes) {
        self.paths.extend(other.paths);
        self.overflow |= other.overflow;
    }
}

/// Changes seen but not yet delivered.
#[derive(Default)]
struct Buffer {
    changes: Changes,
    /// When the first undelivered change arrived.
    since: Option<Instant>,
    /// When the last relevant event arrived.
    last: Option<Instant>,
}

/// The watcher: a thread delivering debounced [`Changes`] on a channel.
pub struct Watcher {
    rx: crossbeam_channel::Receiver<Changes>,
    /// Whether file events are used (false: polling).
    events: bool,
    _thread: std::thread::JoinHandle<()>,
}

fn lock(buffer: &Mutex<Buffer>) -> std::sync::MutexGuard<'_, Buffer> {
    buffer.lock().unwrap_or_else(|p| p.into_inner())
}

impl Watcher {
    /// Watch `root` (canonical) with the inventory rules of `config`.
    pub fn start(root: &Path, config: &Settings) -> Watcher {
        let (tx, rx) = crossbeam_channel::unbounded();
        let buffer = Mutex::new(Buffer::default());
        let (event_tx, event_rx) = mpsc::channel::<notify::Result<Event>>();
        let notify_watcher = start_notify(root, event_tx);
        let events = notify_watcher.is_some();
        let thread = {
            let root = root.to_path_buf();
            let config = config.clone();
            std::thread::spawn(move || {
                // The notify watcher lives as long as this thread.
                let _keep = notify_watcher;
                if events {
                    event_loop(&root, &config, &event_rx, &buffer, &tx);
                } else {
                    poll_loop(&root, &config, &buffer, &tx);
                }
            })
        };
        Watcher {
            rx,
            events,
            _thread: thread,
        }
    }

    /// Debounced changes.
    pub fn changes(&self) -> &crossbeam_channel::Receiver<Changes> {
        &self.rx
    }

    /// Whether OS file events are used (false: inventory polling).
    pub(crate) fn uses_events(&self) -> bool {
        self.events
    }
}

/// A recursive `notify` watcher on `root`; `None` when file events are unavailable.
fn start_notify(root: &Path, tx: mpsc::Sender<notify::Result<Event>>) -> Option<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(tx).ok()?;
    watcher.watch(root, RecursiveMode::Recursive).ok()?;
    Some(watcher)
}

/// Deliver the buffer when it is due (quiet for [`Timing::debounce`] or pending for
/// [`Timing::max_delay`]). Returns false when the receiver is gone.
fn deliver_if_due(
    buffer: &Mutex<Buffer>,
    tx: &crossbeam_channel::Sender<Changes>,
    now: Instant,
    timing: Timing,
) -> bool {
    let due = {
        let b = lock(buffer);
        match (b.since, b.last) {
            (Some(since), Some(last)) => {
                now.duration_since(last) >= timing.debounce || now.duration_since(since) >= timing.max_delay
            }
            _ => false,
        }
    };
    if !due {
        return true;
    }
    let changes = {
        let mut b = lock(buffer);
        b.since = None;
        b.last = None;
        std::mem::take(&mut b.changes)
    };
    changes.is_empty() || tx.send(changes).is_ok()
}

fn note(buffer: &Mutex<Buffer>, changes: Changes, now: Instant) {
    if changes.is_empty() {
        return;
    }
    let mut b = lock(buffer);
    b.changes.merge(changes);
    b.since.get_or_insert(now);
    b.last = Some(now);
}

fn event_loop(
    root: &Path,
    config: &Settings,
    rx: &mpsc::Receiver<notify::Result<Event>>,
    buffer: &Mutex<Buffer>,
    tx: &crossbeam_channel::Sender<Changes>,
) {
    let mut filter = PathFilter::new(root).with_exclude(&config.inventory.exclude);
    let timing = Timing::of(&config.watch);
    loop {
        let wait = if lock(buffer).since.is_some() {
            timing.debounce / 3
        } else {
            timing.poll
        };
        match rx.recv_timeout(wait) {
            Ok(Ok(event)) => {
                let reload = filter.note_ignore_change(&event);
                let changes = filter.changes_of(&event);
                let changes = if reload {
                    Changes {
                        overflow: true,
                        ..changes
                    }
                } else {
                    changes
                };
                note(buffer, changes, Instant::now());
            }
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => {
                // Events are lost from here on: rescan once, then poll.
                note(
                    buffer,
                    Changes {
                        paths: BTreeSet::new(),
                        overflow: true,
                    },
                    Instant::now(),
                );
                let _ = deliver_now(buffer, tx);
                return poll_loop(root, config, buffer, tx);
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if !deliver_if_due(buffer, tx, Instant::now(), timing) {
            return;
        }
    }
}

fn deliver_now(buffer: &Mutex<Buffer>, tx: &crossbeam_channel::Sender<Changes>) -> bool {
    let changes = {
        let mut b = lock(buffer);
        b.since = None;
        b.last = None;
        std::mem::take(&mut b.changes)
    };
    changes.is_empty() || tx.send(changes).is_ok()
}

/// (size, modification time) of every inventoried file, by absolute path.
fn snapshot(root: &Path, config: &Settings) -> Option<BTreeMap<PathBuf, (u64, u64)>> {
    let inv = trace_core::inventory::scan(root, &config.inventory).ok()?;
    Some(
        inv.sources
            .iter()
            .chain(inv.configs.iter())
            .map(|e| (e.abs.clone(), (e.size, e.mtime_ns)))
            .collect(),
    )
}

fn poll_loop(
    root: &Path,
    config: &Settings,
    buffer: &Mutex<Buffer>,
    tx: &crossbeam_channel::Sender<Changes>,
) {
    let mut before = snapshot(root, config).unwrap_or_default();
    let poll = Timing::of(&config.watch).poll;
    loop {
        std::thread::sleep(poll);
        if !root.is_dir() {
            let _ = tx.send(Changes {
                paths: BTreeSet::new(),
                overflow: true,
            });
            return;
        }
        let Some(now) = snapshot(root, config) else {
            continue;
        };
        let mut changes = Changes::default();
        for (path, meta) in &now {
            if before.get(path) != Some(meta) {
                changes.paths.insert(path.clone());
            }
        }
        for path in before.keys() {
            if !now.contains_key(path) {
                changes.paths.insert(path.clone());
            }
        }
        before = now;
        note(buffer, changes, Instant::now());
        if !deliver_now(buffer, tx) {
            return;
        }
    }
}

/// Pre-filter for file events (the inventory decides what really changed).
pub(crate) struct PathFilter {
    root: PathBuf,
    gitignore: Option<Gitignore>,
    /// The user's `inventory.exclude` globs (`None`: none, or invalid - the inventory scan
    /// reports an invalid glob itself).
    exclude: Option<Gitignore>,
}

impl PathFilter {
    pub fn new(root: &Path) -> Self {
        PathFilter {
            root: root.to_path_buf(),
            gitignore: load_gitignore(root),
            exclude: None,
        }
    }

    /// Also ignore events below the user's exclusion globs (`inventory.exclude`).
    pub(crate) fn with_exclude(mut self, globs: &[String]) -> Self {
        self.exclude = trace_core::inventory::exclusion_matcher(&self.root, globs)
            .ok()
            .flatten();
        self
    }

    fn relative<'a>(&self, path: &'a Path) -> Option<&'a Path> {
        path.strip_prefix(&self.root).ok()
    }

    /// Reload ignore rules when an ignore file changed; true if one did.
    pub(crate) fn note_ignore_change(&mut self, event: &Event) -> bool {
        let touched = event.paths.iter().any(|p| {
            self.relative(p).is_some_and(|rel| {
                rel == Path::new(".gitignore")
                    || rel == Path::new(".ignore")
                    || rel == Path::new(".git/info/exclude")
                    || rel.file_name().is_some_and(|n| n == ".gitignore" || n == ".ignore")
            })
        });
        if touched {
            self.gitignore = load_gitignore(&self.root);
        }
        touched
    }

    /// The relevant paths of `event` (an event without paths, or one asking for a rescan, is
    /// an overflow).
    pub(crate) fn changes_of(&self, event: &Event) -> Changes {
        if matches!(event.kind, EventKind::Access(_)) {
            return Changes::default();
        }
        if event.paths.is_empty() || event.need_rescan() {
            return Changes {
                paths: BTreeSet::new(),
                overflow: true,
            };
        }
        Changes {
            paths: event
                .paths
                .iter()
                .filter(|p| self.relevant_path(p))
                .cloned()
                .collect(),
            overflow: false,
        }
    }

    fn relevant_path(&self, path: &Path) -> bool {
        let Some(rel) = self.relative(path) else {
            // Outside the canonical root form (e.g. different prefix): let the inventory decide.
            return true;
        };
        let parts: Vec<Component<'_>> = rel.components().collect();
        let Some((last, dirs)) = parts.split_last() else {
            return true; // the root itself
        };
        for c in dirs {
            match c {
                Component::Normal(name) => match name.to_str() {
                    Some(name) if !is_excluded_dir(name) => {}
                    _ => return false,
                },
                _ => return false,
            }
        }
        let Component::Normal(name) = last else {
            return false;
        };
        let Some(name) = name.to_str() else {
            return false;
        };
        if name.starts_with('.') || is_sensitive_name(name) {
            return false;
        }
        // An excluded directory itself (created / touched by a build tool): nothing below it
        // is ever inventoried. A file with such a name (an extensionless script) stays relevant.
        if is_excluded_dir(name) && path.is_dir() {
            return false;
        }
        if self
            .exclude
            .as_ref()
            .is_some_and(|m| m.matched_path_or_any_parents(rel, path.is_dir()).is_ignore())
        {
            return false;
        }
        match &self.gitignore {
            Some(gi) => !gi.matched_path_or_any_parents(rel, path.is_dir()).is_ignore(),
            None => true,
        }
    }
}

/// Root-level `.gitignore`, `.ignore` and `.git/info/exclude` (nested ignore files are
/// honoured by the inventory scan itself).
fn load_gitignore(root: &Path) -> Option<Gitignore> {
    let mut builder = GitignoreBuilder::new(root);
    let mut any = false;
    for name in [".gitignore", ".ignore", ".git/info/exclude"] {
        let path = root.join(name);
        if path.is_file() && builder.add(&path).is_none() {
            any = true;
        }
    }
    if !any {
        return None;
    }
    builder.build().ok()
}

#[cfg(test)]
#[path = "../tests/unit/watcher.rs"]
mod tests;
