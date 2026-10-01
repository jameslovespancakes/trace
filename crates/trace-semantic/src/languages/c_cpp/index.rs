//! clangd's background index: progress read from its shards and the wait until indexing
//! ended or stalled ([`wait_background_index`]).

use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};
use trace_core::setup_error::{timeout_minutes, SetupError};
use trace_core::Language;

use super::*;

/// clangd's work-done progress token of its background index.
pub const BACKGROUND_INDEX_TOKEN: &str = "backgroundIndexProgress";

/// How long clangd may take to begin its background index after the first documents opened
/// (it loads the compile database on the first file).
pub const INDEX_GRACE: Duration = Duration::from_secs(10);

/// No end of the background index and no new or grown shard for this long: the index
/// stalled (clangd was seen keeping its progress open after the last shard was written).
pub const INDEX_STALL: Duration = Duration::from_secs(120);

/// Poll step of the background index wait (server messages are processed meanwhile).
pub(super) const INDEX_POLL: Duration = Duration::from_millis(500);

/// A work-done token clangd never uses: waiting for it only processes server messages for
/// [`INDEX_POLL`] (`LspClient::wait_progress` returns when it did not begin in its grace).
pub(super) const POLL_TOKEN: &str = "trace/background-index-poll";

/// Shard directory entries read per poll at most.
pub(super) const MAX_SHARD_ENTRIES: usize = 500_000;

/// Bounds of the background index wait.
#[derive(Clone, Copy, Debug)]
pub struct IndexWait {
    /// The index may begin this late (nothing begun and nothing written: nothing to index).
    pub grace: Duration,
    /// No progress change and no shard change for this long: stalled.
    pub stall: Duration,
    /// The whole wait: `ServerTimeout` after it.
    pub limit: Duration,
}

/// The background index's work-done progress as the kept notifications show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexProgress {
    NotBegun,
    Running,
    Ended,
}

/// What the wait does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexVerdict {
    Wait,
    /// The index ended, or never began and wrote nothing within the grace.
    Ready,
    /// No progress change and no shard change for the stall time: go on with the index as it is.
    Stalled,
    /// The limit passed while the index still made progress.
    TimedOut,
}

/// The index shard directory's size signal: files, bytes and the newest modification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShardStats {
    pub files: u64,
    pub bytes: u64,
    pub newest: Option<SystemTime>,
}

/// Shard statistics of clangd's index directory (missing directory: all zero; bounded).
pub fn shard_stats(dir: &Path) -> ShardStats {
    let mut stats = ShardStats::default();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return stats;
    };
    for entry in rd.filter_map(Result::ok).take(MAX_SHARD_ENTRIES) {
        let Ok(meta) = entry.metadata() else { continue };
        stats.files += 1;
        stats.bytes = stats.bytes.saturating_add(meta.len());
        if let Ok(m) = meta.modified() {
            stats.newest = Some(stats.newest.map_or(m, |n| n.max(m)));
        }
    }
    stats
}

/// The last begin / end of [`BACKGROUND_INDEX_TOKEN`] among the kept notifications (begin and
/// end notifications are kept; progress reports are not).
pub fn background_index_progress(notifications: &[(String, Value)]) -> IndexProgress {
    let mut state = IndexProgress::NotBegun;
    for (method, params) in notifications {
        if method != "$/progress" {
            continue;
        }
        if params.get("token").and_then(Value::as_str) != Some(BACKGROUND_INDEX_TOKEN) {
            continue;
        }
        match params
            .get("value")
            .and_then(|v| v.get("kind"))
            .and_then(Value::as_str)
        {
            Some("begin") => state = IndexProgress::Running,
            Some("end") => state = IndexProgress::Ended,
            _ => {}
        }
    }
    state
}

/// One background index wait: when it started, the last activity (a progress change or a
/// shard change) and what was seen last.
#[derive(Clone, Debug)]
pub struct IndexWatch {
    pub(super) started: Instant,
    pub(super) last_activity: Instant,
    pub(super) progress: IndexProgress,
    pub(super) shards: ShardStats,
    /// A shard was written or changed during the wait.
    pub(super) grew: bool,
}

impl IndexWatch {
    pub fn new(now: Instant, shards: ShardStats) -> IndexWatch {
        IndexWatch {
            started: now,
            last_activity: now,
            progress: IndexProgress::NotBegun,
            shards,
            grew: false,
        }
    }

    /// Record one poll and decide.
    pub fn observe(
        &mut self,
        now: Instant,
        progress: IndexProgress,
        shards: ShardStats,
        wait: &IndexWait,
    ) -> IndexVerdict {
        if progress != self.progress {
            self.progress = progress;
            self.last_activity = now;
        }
        if shards != self.shards {
            self.shards = shards;
            self.last_activity = now;
            self.grew = true;
        }
        if progress == IndexProgress::Ended {
            return IndexVerdict::Ready;
        }
        if now >= self.started + wait.limit {
            return IndexVerdict::TimedOut;
        }
        if progress == IndexProgress::NotBegun && !self.grew {
            return if now >= self.started + wait.grace {
                IndexVerdict::Ready
            } else {
                IndexVerdict::Wait
            };
        }
        if now >= self.last_activity + wait.stall {
            IndexVerdict::Stalled
        } else {
            IndexVerdict::Wait
        }
    }
}

/// Wait for clangd's background index (bounded; module docs). Server messages are processed
/// while waiting; a stall is recorded next to the server log.
pub(super) fn wait_background_index(
    client: &mut crate::lsp::LspClient,
    shards: &Path,
    wait: &IndexWait,
    language: Language,
) -> Result<(), SetupError> {
    let log = client.log_path().to_path_buf();
    let failed = |e: crate::SemanticError| match e {
        crate::SemanticError::Setup(setup) => setup,
        _ => SetupError::ServerCrashed {
            language,
            log: log.clone(),
        },
    };
    let mut watch = IndexWatch::new(Instant::now(), shard_stats(shards));
    loop {
        // The poll token never begins: this only processes what the server sent meanwhile.
        client.wait_progress(POLL_TOKEN, INDEX_POLL).map_err(failed)?;
        let progress = background_index_progress(client.notifications());
        let stats = shard_stats(shards);
        match watch.observe(Instant::now(), progress, stats, wait) {
            IndexVerdict::Wait => {}
            IndexVerdict::Ready => return Ok(()),
            IndexVerdict::Stalled => {
                let note = format!(
                    "clangd background index: no progress for {} s ({} shards); queries use the index as it is",
                    wait.stall.as_secs(),
                    stats.files
                );
                let _ = append_line(&log.with_file_name("clangd-index.log"), &note);
                return Ok(());
            }
            IndexVerdict::TimedOut => {
                return Err(SetupError::ServerTimeout {
                    language,
                    minutes: timeout_minutes(wait.limit),
                    log: log.clone(),
                })
            }
        }
    }
}
