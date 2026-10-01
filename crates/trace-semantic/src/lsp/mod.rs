//! Minimal LSP client over stdio (port of codepath_v3/protocols/lsp.py, plus pipelining,
//! readiness per registry entry and the setup-error mapping of DESIGN §4.2).
//!
//! * Framing: `Content-Length: N\r\n\r\n<N bytes JSON>`; other headers ignored; frames larger
//!   than 32 MB are a protocol error.
//! * A reader thread parses frames and sends them over an `mpsc` channel; the client routes
//!   responses by id and answers server requests from the entry's
//!   [`ServerRequestPolicy`]: `workspace/configuration` from the controlled settings (a
//!   section is looked up literally first - Roslyn's `csharp|...` names are never split -,
//!   then by `.` path; a missing section is answered `null` or `{}` per
//!   `configuration_missing`), `window/showMessageRequest` with the first offered action the
//!   policy lists as `true` (every other request is answered `null`),
//!   `window/workDoneProgress/create`, `client/registerCapability`,
//!   `workspace/workspaceFolders`, refresh requests -> result/null; `workspace/applyEdit` and
//!   `window/showDocument` are refused (trace is read-only).
//! * Kept for the backend's `Server::check_loaded` / `warm_up` ([`LspClient::log_messages`],
//!   [`LspClient::notifications`], [`LspClient::diagnostics`]), every list bounded by count
//!   (2000) and bytes (32 MB, one message at most 1 MB, longer texts cut at a character
//!   boundary): `window/logMessage` + `window/showMessage` (type, text) and other
//!   notifications (method, params; `$/progress` reports are only observed, never kept) keep
//!   their first entries (up to half of each bound) for good and roll the rest (the oldest go
//!   first), so start-up messages and the latest ones both stay readable;
//!   `textDocument/publishDiagnostics` (uri, params) are kept only until the process is warmed
//!   up ([`LspClient::mark_warmed`]: the last hook that reads them ran) or for the first
//!   [`client::DIAGNOSTICS_WINDOW`], and later ones are dropped by the reader thread before they are
//!   queued. The reader also cuts over-long log texts before queueing, ended progress tokens
//!   are remembered up to 4096 (oldest forgotten first) and notification methods seen up to
//!   1024: nothing a server sends grows without a bound for the life of a session.
//! * Every request has a timeout (`request_timeout`) and each update a hard deadline
//!   (`session_deadline`); exceeding either is an error (never a hang). Answers with
//!   `ContentModified` / `ServerCancelled` are asked once more; `RequestCancelled` (-32800,
//!   Metals while compiling) up to `AnswerPolicy::retry_cancelled` times with a growing pause;
//!   `InternalError` (-32603) is an unresolved (`null`) answer when
//!   `AnswerPolicy::internal_error_is_unresolved` (jdtls on jars without sources).
//! * [`LspClient::request_many`] pipelines up to `max_in_flight` requests.
//! * Initialization: `processId`, `rootUri` + one workspace folder (the workspace),
//!   capabilities `general.positionEncodings=["utf-16"]`, `workspace.configuration=true`,
//!   hierarchical document symbols, call hierarchy, definition with link support,
//!   references, implementation, `window.workDoneProgress`,
//!   `experimental.serverStatusNotification=true`. The negotiated encoding must be UTF-16.
//!   After `initialized` and `workspace/didChangeConfiguration` the entry's
//!   `after_initialized` notifications are sent (Roslyn `solution/open`).
//! * Readiness ([`ReadySpec`], one wait per start, bounded by the entry's
//!   `ready_timeout_secs`, which also bounds `initialize`):
//!   `none` (requests block until complete: Pyright, TS worker, gopls);
//!   `progress` (the backend's `ready_grace_ms` / `ready_settle_ms`: wait up to the grace
//!   for a first work-done progress `begin` - Intelephense's `indexingStarted` counts too -,
//!   then until every begun token ended; in both cases the last progress change, or
//!   `initialized`, must be the settle time ago, which also serves start-up requests such as bash-language-server's configuration
//!   round trip); `quiescent` (rust-analyzer `experimental/serverStatus {quiescent: true}`
//!   stable for [`readiness::QUIESCENCE_SETTLE`]); `language_status` (jdtls `language/status`
//!   `ServiceReady` - or `Error`, which `check_loaded` turns into the load error -, then all
//!   progress ended and quiet for [`readiness::LANGUAGE_STATUS_SETTLE`]); `notification{method}`
//!   (Roslyn `workspace/projectInitializationComplete`); `request{method, params}` (a client request
//!   whose answer means ready); `log{contains}` (a log/show message
//!   containing the text, then progress ended + [`readiness::PROGRESS_SETTLE`]); `symbol_poll` (R:
//!   `workspace/symbol` with `ClientOptions::symbol_poll_query` every [`readiness::SYMBOL_POLL_INTERVAL`]
//!   until the count is non-zero and equal twice in a row).
//!   A readiness timeout is `SetupError::ServerTimeout` (no partial answers); the process
//!   exiting (or its pipe breaking on write) is `SetupError::ServerCrashed` with the stderr
//!   log (the exit status is appended to that log). [`LspClient::ready`] reports
//!   `Some(true)` when a signal was waited for, `None` for `none` and progress entries whose
//!   server sent no progress (`BackendRun::ready`).
//! * Documents: `didOpen` is sent at most once per URI (a second open becomes a full-text
//!   `didChange`: Roslyn crashes on duplicate opens); `didChange` / `didClose` only for open
//!   documents; on-disk workspace changes are announced with
//!   `workspace/didChangeWatchedFiles` ([`LspClient::files_changed`]). After changes,
//!   [`LspClient::settle_after_changes`] waits for quiescence / running progress again.
//! * Project contexts (Roslyn, `AnswerPolicy::project_contexts`): a file compiled by several
//!   projects / target frameworks is answered per context. Before the first request on an
//!   open document the client asks `textDocument/_vs_getProjectContexts` (pipelined, once per
//!   document), orders the contexts deterministically ([`contexts::order_project_contexts`]: newest
//!   target framework first - `netX.Y` / `netcoreappX.Y` > `netstandardX.Y` > .NET Framework
//!   `netNN` -, then by label and id; miscellaneous-files contexts last) and puts the first
//!   into every request's `textDocument._vs_projectContext`. An empty answer (`null` / `[]`,
//!   e.g. a call inside an `#if` region the first context does not compile) is asked again in
//!   the file's next contexts, in order (at most 8), and the first non-empty answer is used.
//! * Crashes: the log names the documents opened, changed or queried last (at most 8), so a
//!   server that dies on one file says which.
//! * Shutdown: `shutdown` request (bounded), then the process tree is stopped
//!   ([`crate::procs::stop_tree`]): on Unix after the `exit` notification and up to 5 s for the
//!   server to exit (its process group then catches what it left running), on Windows while
//!   the server still runs (taskkill follows parent links only from a live process). Servers
//!   start in a stoppable tree ([`crate::procs::isolate`]) with piped stdin / stdout; stderr
//!   goes to `<workspace>/lsp.stderr.log` (or [`ClientOptions::stderr_log`] for pooled
//!   processes): a server never gets trace's own standard streams.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;
use trace_core::Language;

use crate::languages::AnswerPolicy;
use crate::registry::{ReadySpec, ServerRequestPolicy};

mod client;
mod contexts;
mod readiness;
mod transport;

pub use client::{capability_enabled, LspClient};
pub use readiness::combine_ready;
pub use transport::{path_to_uri, uri_to_path};

/// How to launch a language server.
#[derive(Clone, Debug)]
pub struct ServerCommand {
    /// Trusted absolute executable.
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Controlled environment (see [`crate::tools::clean_env`]).
    pub env: Vec<(String, String)>,
    /// Working directory: the workspace.
    pub cwd: PathBuf,
}

/// Client options (build with [`ClientOptions::new`], then set what the entry needs).
#[derive(Clone, Debug)]
pub struct ClientOptions {
    /// Language named by setup errors (crash, timeout).
    pub language: Language,
    pub request_timeout: Duration,
    pub session_deadline: Instant,
    pub max_in_flight: usize,
    /// `initializationOptions`.
    pub initialization_options: Value,
    /// Settings answered to `workspace/configuration` and sent via `didChangeConfiguration`.
    pub settings: Value,
    /// Server stderr log file (default `<workspace>/lsp.stderr.log`). Pooled processes use
    /// one log per process.
    pub stderr_log: Option<PathBuf>,
    /// Readiness signal waited for before the first query (module docs).
    pub ready: ReadySpec,
    /// Bound of `initialize` + the readiness wait (`ready_timeout_secs`).
    pub ready_timeout: Duration,
    /// `progress` readiness: the wait for a first begin, the quiet period after the last end.
    pub progress_grace: Duration,
    pub progress_settle: Duration,
    /// How server-to-client requests are answered.
    pub server_requests: ServerRequestPolicy,
    /// Notifications sent right after `initialized` (placeholders already expanded).
    pub after_initialized: Vec<(String, Value)>,
    /// Retries and error mapping of this server's answers.
    pub answer_policy: AnswerPolicy,
    /// `symbol_poll` readiness: the `workspace/symbol` query (a declared name).
    pub symbol_poll_query: Option<String>,
}

impl ClientOptions {
    /// Options with no readiness wait, default policies and an empty settings object.
    pub fn new(
        language: Language,
        request_timeout: Duration,
        session_deadline: Instant,
        max_in_flight: usize,
    ) -> ClientOptions {
        ClientOptions {
            language,
            request_timeout,
            session_deadline,
            max_in_flight,
            initialization_options: Value::Null,
            settings: Value::Object(Default::default()),
            stderr_log: None,
            ready: ReadySpec::None,
            ready_timeout: Duration::from_secs(trace_core::config::current().semantic.ready_timeout_secs),
            progress_grace: Duration::ZERO,
            progress_settle: Duration::ZERO,
            server_requests: ServerRequestPolicy::default(),
            after_initialized: Vec::new(),
            answer_policy: AnswerPolicy::default(),
            symbol_poll_query: None,
        }
    }
}

/// `FileChangeType` of `workspace/didChangeWatchedFiles`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileChange {
    Created = 1,
    Changed = 2,
    Deleted = 3,
}

/// Per-method counts and cumulative seconds.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct LspMetrics {
    pub requests: u64,
    pub seconds_by_method: std::collections::BTreeMap<String, f64>,
}

#[cfg(test)]
#[path = "../../tests/unit/lsp/mod.rs"]
mod tests;
