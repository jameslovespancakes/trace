//! Progress of a run: the [`IndexProgress`] callback (phase lines, install lines), the quiet and
//! stderr implementations, and the `TRACE_PROFILE` phase lines ([`Profile`]).

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use trace_core::model::Index;
use trace_infer::flow::FlowStats;

/// Progress callback.
pub trait IndexProgress {
    fn phase(&mut self, name: &str, done: usize, total: usize);
    /// One install progress line of the automatic install (PLAN decision 10), e.g.
    /// `Installing the Python language server (Pyright 1.1.414)...`. Always shown on stderr
    /// (an install is never silent).
    fn install_line(&mut self, line: &str) {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "{line}");
        let _ = err.flush();
    }
}

/// No phase progress (install lines are still printed, [`IndexProgress::install_line`]).
pub struct Quiet;

impl IndexProgress for Quiet {
    fn phase(&mut self, _name: &str, _done: usize, _total: usize) {}
}

/// `indexing: <phase> <done>/<total>` on stderr: rewritten in place on a terminal
/// (throttled, cleared when dropped), one line per phase otherwise.
pub struct StderrProgress {
    tty: bool,
    phase: String,
    last: Option<Instant>,
    width: usize,
}

impl StderrProgress {
    const INTERVAL: Duration = Duration::from_millis(120);

    pub fn new(tty: bool) -> Self {
        StderrProgress {
            tty,
            phase: String::new(),
            last: None,
            width: 0,
        }
    }

    /// In-place progress when stderr is a terminal.
    pub fn terminal() -> Self {
        Self::new(std::io::stderr().is_terminal())
    }
}

impl IndexProgress for StderrProgress {
    fn phase(&mut self, name: &str, done: usize, total: usize) {
        let new_phase = self.phase != name;
        if new_phase {
            self.phase.clear();
            self.phase.push_str(name);
        }
        let mut err = std::io::stderr().lock();
        if self.tty {
            let due = self.last.is_none_or(|t| t.elapsed() >= Self::INTERVAL);
            if !(new_phase || done >= total || due) {
                return;
            }
            let line = format!("indexing: {name} {done}/{total}");
            let pad = self.width.saturating_sub(line.chars().count());
            let _ = write!(err, "\r{line}{:pad$}", "");
            let _ = err.flush();
            self.width = line.chars().count();
            self.last = Some(Instant::now());
        } else if new_phase {
            let _ = writeln!(err, "indexing: {name} {done}/{total}");
        }
    }

    fn install_line(&mut self, line: &str) {
        let mut err = std::io::stderr().lock();
        if self.tty && self.width > 0 {
            // Clear the in-place phase line first; the install line stays on screen.
            let _ = write!(err, "\r{:width$}\r", "", width = self.width);
            self.width = 0;
        }
        let _ = writeln!(err, "{line}");
        let _ = err.flush();
    }
}

impl Drop for StderrProgress {
    fn drop(&mut self) {
        if self.tty && self.width > 0 {
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r{:width$}\r", "", width = self.width);
            let _ = err.flush();
        }
    }
}

/// `TRACE_PROFILE=1` phase lines (SPEC §5.1a).
pub struct Profile {
    pub(super) enabled: bool,
    pub(super) started: Instant,
    pub(super) files: usize,
    pub(super) symbols: usize,
    pub(super) edges: usize,
    pub(super) sites: usize,
    pub(super) bridges: usize,
    pub(crate) flow: FlowStats,
}

impl Profile {
    pub fn from_env() -> Profile {
        let enabled = trace_core::env::profile();
        Profile {
            enabled,
            started: Instant::now(),
            files: 0,
            symbols: 0,
            edges: 0,
            sites: 0,
            bridges: 0,
            flow: FlowStats::default(),
        }
    }

    pub(crate) fn index(&mut self, index: &Index) {
        self.files = index.files.len();
        self.symbols = index.symbols.len();
        self.edges = index.edges.len();
        self.sites = index.sites.len();
        self.bridges = index.bridges.len();
    }

    pub(crate) fn line(&self, phase: &str, secs: f64) {
        if !self.enabled {
            return;
        }
        let f = &self.flow;
        let mut err = std::io::stderr().lock();
        let _ = writeln!(
            err,
            "profile: {phase} {secs:.3}s files={} symbols={} edges={} slots={} values={} evals={} sites={} bridges={} contexts={} saturated={} skipped={} elapsed={:.3}s",
            self.files,
            self.symbols,
            self.edges,
            f.slots,
            f.values,
            f.evals,
            self.sites,
            self.bridges,
            f.contexts,
            f.saturated_slots,
            f.skipped,
            self.started.elapsed().as_secs_f64(),
        );
        let _ = err.flush();
    }
}
