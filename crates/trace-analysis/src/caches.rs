//! Cache hit-rate counters beside the index (always under the repository cache, never inside
//! an inspected root; JSON for auditability, atomic writes):
//!
//! * `<repo cache>/stats.json` — counters shown by `status`:
//!   `{"version": 1, "semantic_files": {"lookups", "hits"}, "updated_unix"}`. Counters are
//!   merged into the file (read, add, write) when a workspace flushes; concurrent processes
//!   may lose increments (best effort, statistics only). Unknown keys are ignored when read.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use trace_core::cache::{unix_now, write_atomic};
use trace_core::paths::RepoPaths;

const VERSION: u32 = 1;

/// `<repo cache>/stats.json`.
pub(crate) fn stats_path(paths: &RepoPaths) -> PathBuf {
    paths.repo_dir.join("stats.json")
}

/// Lookups and hits of one cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HitCounter {
    pub lookups: u64,
    pub hits: u64,
}

impl HitCounter {
    pub fn record(&mut self, lookups: u64, hits: u64) {
        self.lookups += lookups;
        self.hits += hits.min(lookups);
    }

    /// Hit rate in [0, 1]; `None` before the first lookup.
    pub fn rate(&self) -> Option<f64> {
        (self.lookups > 0).then(|| self.hits as f64 / self.lookups as f64)
    }

    fn add(&mut self, other: &HitCounter) {
        self.lookups += other.lookups;
        self.hits += other.hits;
    }

    pub fn is_empty(&self) -> bool {
        self.lookups == 0
    }
}

/// Cache hit-rate counters (module docs).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Stats {
    pub version: u32,
    /// Per-file semantic results reused (hits) among files of semantic partitions (lookups).
    pub semantic_files: HitCounter,
    pub updated_unix: f64,
}

impl Stats {
    /// Read the persisted counters (missing or corrupt -> zero).
    pub fn load(path: &Path) -> Stats {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Stats>(&b).ok())
            .unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.semantic_files.is_empty()
    }

    /// Add `delta` to the counters stored at `path`.
    pub(crate) fn merge_into(path: &Path, delta: &Stats) -> trace_core::Result<()> {
        if delta.is_empty() {
            return Ok(());
        }
        let mut total = Stats::load(path);
        total.version = VERSION;
        total.semantic_files.add(&delta.semantic_files);
        total.updated_unix = unix_now();
        let bytes =
            serde_json::to_vec_pretty(&total).map_err(|e| trace_core::CoreError::Serialize(e.to_string()))?;
        write_atomic(path, &bytes)
    }
}

#[cfg(test)]
#[path = "../tests/unit/caches.rs"]
mod tests;
