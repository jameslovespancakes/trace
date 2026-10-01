//! Metals readiness: the build import and indexing wait over Metals' own log
//! (`.metals/metals.log`, [`MetalsLog`]) with import failures reported from it.

use crate::languages::jvm::{classify_load, load_texts, LoadOutcome};
use crate::languages::Prepared;
use crate::registry::Registry;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use trace_core::setup_error::SetupError;
use trace_core::Language;
use trace_env::jvm::BuildSystem;

use super::*;

/// Poll step of the readiness wait (the client serves the server meanwhile).
pub(super) const POLL_STEP: Duration = Duration::from_millis(250);

/// A progress token Metals never uses: waiting for it only serves the server for one step.
pub(super) const POLL_TOKEN: &str = "trace-metals-readiness-poll";

/// After the index: how long the first compile of the open files may take to begin, and the
/// quiet time after the last compile progress ended.
pub(super) const COMPILE_GRACE: Duration = Duration::from_secs(10);

pub(super) const COMPILE_SETTLE: Duration = Duration::from_secs(3);

/// After changes of a warm session: the same, shorter (most changes compile nothing).
pub(super) const SETTLE_GRACE: Duration = Duration::from_millis(500);

pub(super) const SETTLE_QUIET: Duration = Duration::from_millis(300);

/// The metals.log line that starts a Metals process.
pub(super) const METALS_STARTED: &str = "Started: Metals version";

/// Metals' status message once the workspace is indexed, and its metals.log line.
pub(super) const INDEXING_COMPLETE: &str = "Indexing complete!";

pub(super) const INDEXED_WORKSPACE: &str = "time: indexed workspace";

/// Metals' messages of a failed build import (lowercase).
pub(super) const IMPORT_FAILED: &[&str] = &[
    "import project failed",
    "import project partially failed",
    "reloading your project failed",
    "bloopinstall failed",
    "failed to import build",
];

/// A failed build-tool command of the import (`sbt command failed: ... bloopInstall`), named
/// by the import task it ran (sbt / Mill Bloop export, Scala CLI IDE setup; lowercase).
pub(super) const IMPORT_TASKS: &[&str] = &["bloopinstall", "bloop/install", "setup-ide"];

/// metals.log entries kept per process (the latest), bytes per entry, bytes read per poll.
pub(super) const MAX_LOG_ENTRIES: usize = 4000;

pub(super) const MAX_ENTRY_BYTES: usize = 16 * 1024;

pub(super) const MAX_LOG_READ: u64 = 16 * 1024 * 1024;

/// Name of the previous process's metals.log (moved away before each start).
pub(super) const PREVIOUS_LOG: &str = "metals.previous.log";

/// The readiness bound of the Metals entry (its `ready_timeout_secs`).
pub(super) fn metals_ready_limit() -> Duration {
    let secs = Registry::builtin()
        .entry("lsp:metals")
        .map_or(trace_core::config::current().semantic.ready_timeout_secs, |e| e.ready_timeout_secs());
    Duration::from_secs(secs.max(1))
}

/// The readiness of one Metals process (module docs), bounded by `limit`:
/// 1. the build import and the workspace index - a failed import is the build / dependency
///    error at once (never the readiness timeout);
/// 2. the first compile of the open files (Bloop), so answers never depend on how far it got;
/// 3. the load checks over everything the server said so far.
///
/// `workspace` is the workspace root Metals runs in (its `.metals/metals.log`).
pub(super) fn wait_for_metals(
    client: &mut crate::lsp::LspClient,
    prepared: &Prepared,
    workspace: &Path,
    limit: Duration,
) -> Result<(), SetupError> {
    let language = Language::Scala;
    let build = scala_data(prepared).map_or(BuildSystem::Sbt, |d| d.build);
    let client_log = client.log_path().to_path_buf();
    let to_setup = |e: crate::SemanticError| match e {
        crate::SemanticError::Setup(setup) => setup,
        _ => SetupError::ServerCrashed {
            language,
            log: client_log.clone(),
        },
    };
    let mut metals_log = MetalsLog::new(workspace.join(".metals").join("metals.log"));
    let deadline = Instant::now() + limit;
    loop {
        metals_log.poll();
        if let Some(failure) = metals_log
            .failed
            .clone()
            .or_else(|| import_failure_message(client.log_messages()))
        {
            let texts = session_texts(client, &metals_log);
            return Err(import_error(build, &failure, &texts, metals_log.details(&client_log)));
        }
        if metals_log.indexed || indexing_complete(client.log_messages()) {
            break;
        }
        if Instant::now() >= deadline {
            return Err(SetupError::ServerTimeout {
                language,
                minutes: u32::try_from(limit.as_secs().div_ceil(60).max(1)).unwrap_or(u32::MAX),
                log: metals_log.details(&client_log),
            });
        }
        client.wait_progress(POLL_TOKEN, POLL_STEP).map_err(to_setup)?;
    }
    client
        .wait_progress_after_open(COMPILE_GRACE, COMPILE_SETTLE)
        .map_err(to_setup)?;
    metals_log.poll();
    if let Some(failure) = metals_log
        .failed
        .clone()
        .or_else(|| import_failure_message(client.log_messages()))
    {
        let texts = session_texts(client, &metals_log);
        return Err(import_error(build, &failure, &texts, metals_log.details(&client_log)));
    }
    classify_metals(
        prepared,
        client.log_messages(),
        client.notifications(),
        client.diagnostics(),
        &metals_log.details(&client_log),
    )
    .map(|_| ())
}

/// Whether a message says the build import failed.
pub(super) fn is_import_failure(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    IMPORT_FAILED.iter().any(|s| lower.contains(s))
        || (lower.contains("command failed:") && IMPORT_TASKS.iter().any(|t| lower.contains(t)))
}

/// The first log / show message of the client saying the import failed.
pub(super) fn import_failure_message(log_messages: &[(u8, String)]) -> Option<String> {
    log_messages
        .iter()
        .find(|(_, t)| is_import_failure(t))
        .map(|(_, t)| first_line(t))
}

/// Whether Metals reported the finished workspace index to the client.
pub(super) fn indexing_complete(log_messages: &[(u8, String)]) -> bool {
    log_messages.iter().any(|(_, t)| t.contains(INDEXING_COMPLETE))
}

/// Every text of the process that may explain a failed import: the client's load texts and
/// the metals.log entries of this process (sbt's output is logged there).
pub(super) fn session_texts(client: &crate::lsp::LspClient, log: &MetalsLog) -> Vec<String> {
    let mut texts = load_texts(client.log_messages(), client.notifications(), client.diagnostics());
    texts.extend(log.entries.iter().cloned());
    texts
}

/// The error of a failed build import: the dependency error when the build tool could not
/// find the project's artifacts offline, the missing-parts error when it was Metals' own sbt
/// plugin, else the build error with Metals' failure message.
pub(super) fn import_error(build: BuildSystem, failure: &str, texts: &[String], log: PathBuf) -> SetupError {
    let (_, rest) = split_tool_artifact_messages(texts.to_vec());
    let plugin_missing = rest.iter().any(|t| {
        t.to_ascii_lowercase().contains("sbt-bloop")
            && classify_load(std::slice::from_ref(t)) == LoadOutcome::DepsMissing
    });
    if plugin_missing {
        return parts_error(&["the Bloop build server".to_string()]);
    }
    if classify_load(&rest) == LoadOutcome::DepsMissing {
        return SetupError::DepsMissing {
            language: Language::Scala,
            hint: build.hint().to_string(),
        };
    }
    SetupError::BuildFailed {
        language: Language::Scala,
        what: format!("{} import failed: {failure}", build.tool()),
        log,
    }
}

/// A metals.log line that starts an entry: `yyyy.mm.dd hh:mm:ss LEVEL message` (other lines
/// continue the previous entry: stack traces, multi-line messages).
pub(super) fn starts_log_entry(line: &str) -> bool {
    let b = line.as_bytes();
    b.len() > 11
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'.'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'.'
        && b[8..10].iter().all(u8::is_ascii_digit)
        && b[10] == b' '
}

/// The message of a metals.log entry line (date, time and level dropped).
pub(super) fn log_entry_message(line: &str) -> &str {
    line.splitn(4, ' ').nth(3).map_or(line, str::trim)
}

/// `text` cut to at most `max` bytes at a character boundary.
pub(super) fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The workspace's `.metals/metals.log`, read incrementally (only complete lines; bounded):
/// the entries of the latest Metals process, whether its import failed and whether its
/// workspace index finished.
pub(super) struct MetalsLog {
    pub(super) path: PathBuf,
    pub(super) offset: u64,
    pub(super) pending: Vec<u8>,
    pub(super) entries: VecDeque<String>,
    /// The first import-failure message of the process.
    pub(super) failed: Option<String>,
    /// The workspace index finished.
    pub(super) indexed: bool,
}

impl MetalsLog {
    pub(super) fn new(path: PathBuf) -> MetalsLog {
        MetalsLog {
            path,
            offset: 0,
            pending: Vec::new(),
            entries: VecDeque::new(),
            failed: None,
            indexed: false,
        }
    }

    /// The log errors point to: metals.log when Metals wrote one, else the client's log.
    pub(super) fn details(&self, fallback: &Path) -> PathBuf {
        if self.path.is_file() {
            self.path.clone()
        } else {
            fallback.to_path_buf()
        }
    }

    /// Read what Metals appended since the last poll.
    pub(super) fn poll(&mut self) {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut file) = std::fs::File::open(&self.path) else {
            return;
        };
        let Ok(len) = file.metadata().map(|m| m.len()) else {
            return;
        };
        if len < self.offset {
            // Rotated by Metals: continue in the new file.
            self.offset = 0;
            self.pending.clear();
        }
        if len.saturating_sub(self.offset) > MAX_LOG_READ {
            // Only the latest part of a huge log (the first line read may be partial: it is a
            // continuation line then and ignored).
            self.offset = len - MAX_LOG_READ;
            self.pending.clear();
        }
        if len == self.offset || file.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        if (&mut file).take(len - self.offset).read_to_end(&mut buf).is_err() {
            return;
        }
        self.offset += u64::try_from(buf.len()).unwrap_or(0);
        self.pending.extend_from_slice(&buf);
        let Some(last) = self.pending.iter().rposition(|b| *b == b'\n') else {
            if self.pending.len() > MAX_ENTRY_BYTES {
                self.pending.clear();
            }
            return;
        };
        let complete: Vec<u8> = self.pending.drain(..=last).collect();
        let text = String::from_utf8_lossy(&complete);
        for line in text.lines() {
            self.line(line.trim_end_matches('\r'));
        }
    }

    pub(super) fn line(&mut self, line: &str) {
        if !starts_log_entry(line) {
            if let Some(entry) = self.entries.back_mut() {
                if entry.len() + line.len() < MAX_ENTRY_BYTES {
                    entry.push('\n');
                    entry.push_str(line);
                }
            }
            return;
        }
        let message = log_entry_message(line);
        if message.starts_with(METALS_STARTED) {
            // A new process: only its lines count.
            self.entries.clear();
            self.failed = None;
            self.indexed = false;
        }
        if self.failed.is_none() && is_import_failure(message) {
            self.failed = Some(first_line(message));
        }
        if message.contains(INDEXED_WORKSPACE) {
            self.indexed = true;
        }
        if self.entries.len() >= MAX_LOG_ENTRIES {
            self.entries.pop_front();
        }
        self.entries.push_back(cut(message, MAX_ENTRY_BYTES).to_string());
    }
}
