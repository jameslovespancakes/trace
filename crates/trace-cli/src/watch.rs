//! `trace index --watch` (owner decision 13, option B): the foreground watcher of
//! `trace_watch::run_watch` with the CLI's update lines: text mode prints
//! `updated <n> files (+a ~c -r) in <s>s` lines to stderr (one per edit: unchanged event
//! batches and the stale-dependent batch after an edit print none), JSON mode one compact
//! `IndexReport` per update and per dependents batch to stdout. The language servers start at
//! launch (one `Language servers ready ...` line). It stops on Ctrl+C or when the root
//! disappears. There is no second watcher and no second update path.

use std::path::Path;

use trace_analysis::report::IndexReport;

use crate::cli::GlobalOpts;
use crate::render::watch_line;

/// The CLI's lines for the watcher.
struct CliLines;

impl trace_watch::Lines for CliLines {
    fn update_line(&mut self, report: &IndexReport, seconds: f64, json: bool) -> String {
        if json {
            serde_json::to_string(report).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
        } else {
            watch_line(report, seconds)
        }
    }
}

/// The watch options of the global flags and `TRACE_OFFLINE`.
fn options(global: &GlobalOpts) -> trace_watch::WatchOptions {
    trace_watch::WatchOptions {
        json: global.json,
        offline: crate::app::offline(),
    }
}

pub fn run(global: &GlobalOpts, root: &Path) -> anyhow::Result<()> {
    trace_watch::run_watch(root, options(global), &mut CliLines)
}

#[cfg(test)]
#[path = "../tests/unit/watch.rs"]
mod tests;
