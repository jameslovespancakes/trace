//! The LSP client: process start, `initialize`, pipelined requests with timeouts and
//! retries, documents, server requests answered from the entry's policy, kept messages,
//! failure detection and shutdown.

use crate::registry::{ConfigurationMissing, ReadySpec, ServerRequestPolicy};
use crate::SemanticError;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use trace_core::SetupError;

use super::contexts::*;
use super::readiness::*;
use super::transport::*;
use super::*;

/// Log messages, notifications and diagnostics kept (each).
pub(super) const MAX_KEPT: usize = 2000;

/// Bytes kept per list (log messages, notifications, diagnostics).
pub(super) const MAX_KEPT_BYTES: usize = 32 * 1024 * 1024;

/// Longest kept log / show message text (longer ones are cut at a character boundary).
pub(super) const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// Ended progress tokens remembered (oldest forgotten first).
pub(super) const MAX_ENDED_TOKENS: usize = 4096;

/// Distinct notification methods remembered for `notification` readiness.
pub(super) const MAX_SEEN_METHODS: usize = 1024;

/// Documents named by a crash log.
pub(super) const MAX_RECENT_DOCS: usize = 8;

/// Notifications kept as the latest one per document, outside the rolling bound of the other
/// notifications (clangd's inactive preprocessor regions: the engine reads them per document
/// after the queries, so a big repository must not roll them out).
pub(super) const LATEST_PER_DOCUMENT: [&str; 1] = ["textDocument/inactiveRegions"];

/// Documents whose latest per-document notification is kept.
pub(super) const MAX_LATEST_DOCS: usize = 100_000;

/// Bound for the graceful shutdown handshake and the process exit wait.
pub(super) const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// `publishDiagnostics` received this long after the start are not kept.
pub const DIAGNOSTICS_WINDOW: Duration = Duration::from_secs(300);

/// Pause before re-sending a request the server cancelled (times the attempt number).
pub(super) const CANCELLED_PAUSE: Duration = Duration::from_millis(500);

pub(super) const REQUEST_CANCELLED: i64 = -32800;

pub(super) const CONTENT_MODIFIED: i64 = -32801;

pub(super) const SERVER_CANCELLED: i64 = -32802;

pub(super) const INTERNAL_ERROR: i64 = -32603;

pub(super) const METHOD_NOT_FOUND: i64 = -32601;

/// Why the connection became unusable.
#[derive(Clone, Debug)]
pub(super) enum Failure {
    /// The process exited or its pipe broke (detail for the log).
    Exited(String),
    /// Framing / JSON error.
    Broken(String),
}

/// An in-flight request.
pub(super) struct Pending {
    pub(super) index: usize,
    pub(super) method: String,
    pub(super) params: Value,
    pub(super) sent: Instant,
    pub(super) attempts: u8,
}

/// A queued request: (index, method, params, attempts, not before).
pub(super) type Queued = (usize, String, Value, u8, Option<Instant>);

/// A running, initialized language server session.
pub struct LspClient {
    pub(super) child: Child,
    pub(super) stdin: Option<ChildStdin>,
    pub(super) incoming: Receiver<Incoming>,
    pub(super) reader: Option<JoinHandle<()>>,
    pub(super) opts: ClientOptions,
    pub(super) next_id: i64,
    pub(super) workspace_uri: String,
    pub(super) capabilities: Value,
    pub(super) metrics: LspMetrics,
    pub(super) log_path: PathBuf,
    pub(super) started: Instant,
    /// Deadline of `initialize` + readiness.
    pub(super) ready_deadline: Instant,
    /// When `initialized` was sent.
    pub(super) initialized_at: Instant,
    pub(super) log_messages: Kept<(u8, String)>,
    pub(super) notifications: Kept<(String, Value)>,
    /// [`LATEST_PER_DOCUMENT`] notifications: (method, uri) -> (arrival order, size, params),
    /// at most [`MAX_LATEST_DOCS`] entries and [`MAX_KEPT_BYTES`] bytes.
    pub(super) latest_per_document: BTreeMap<(String, String), (u64, usize, Value)>,
    pub(super) latest_seq: u64,
    pub(super) latest_bytes: usize,
    pub(super) diagnostics: Kept<(String, Value)>,
    /// Whether `publishDiagnostics` are still kept (shared with the reader thread, which drops
    /// them before queueing once this is false).
    pub(super) keep_diagnostics: Arc<AtomicBool>,
    /// Notification methods seen (for `notification` readiness), at most
    /// [`MAX_SEEN_METHODS`].
    pub(super) seen: HashSet<String>,
    /// A log/show message contained the `log` readiness text.
    pub(super) log_matched: bool,
    pub(super) quiescent: bool,
    /// When quiescence was last reported (reset by a non-quiescent status).
    pub(super) quiescent_since: Option<Instant>,
    /// jdtls `language/status` `ServiceReady` (or `Error`) seen.
    pub(super) service_ready: bool,
    pub(super) readiness: Readiness,
    /// Open documents and their versions (never a second `didOpen`).
    pub(super) open_docs: HashMap<String, i32>,
    /// Project contexts of open documents, in the order they are tried (Roslyn).
    pub(super) contexts: HashMap<String, Vec<Value>>,
    /// Documents opened, changed or queried last (named by a crash log), oldest first.
    pub(super) recent_docs: VecDeque<String>,
    pub(super) failure: Option<Failure>,
    pub(super) crash_logged: bool,
    pub(super) closed: bool,
}

impl Drop for LspClient {
    fn drop(&mut self) {
        self.close(false);
    }
}

/// Whether a server capability is advertised (`true` or an options object).
pub fn capability_enabled(capabilities: &Value, name: &str) -> bool {
    match capabilities.get(name) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Object(_)) => true,
        _ => false,
    }
}

/// Answer `workspace/configuration` from the controlled settings. An empty section returns
/// the whole settings object; a section is looked up literally first (a key `a|b` or
/// `a.b` at the top level), then - unless it contains `|` - by its `.` path; a missing
/// section is answered per `missing` (`{}` or `null`).
pub(crate) fn configuration(
    settings: &Value,
    params: Option<&Value>,
    missing: ConfigurationMissing,
) -> Value {
    let items = params.and_then(|p| p.get("items")).and_then(Value::as_array);
    let Some(items) = items else {
        return Value::Array(Vec::new());
    };
    let absent = || match missing {
        ConfigurationMissing::Object => json!({}),
        ConfigurationMissing::Null => Value::Null,
    };
    Value::Array(
        items
            .iter()
            .map(|item| {
                let section = item.get("section").and_then(Value::as_str).unwrap_or("");
                if section.is_empty() {
                    return settings.clone();
                }
                if let Some(literal) = settings.get(section) {
                    return literal.clone();
                }
                if section.contains('|') {
                    return absent();
                }
                let mut value = settings;
                for part in section.split('.') {
                    match value.get(part) {
                        Some(v) => value = v,
                        None => return absent(),
                    }
                }
                value.clone()
            })
            .collect(),
    )
}

/// Answer `window/showMessageRequest`: the first offered action whose title the policy
/// lists as `true`, else `null` (no action taken).
pub(crate) fn message_action(params: Option<&Value>, policy: &ServerRequestPolicy) -> Value {
    let actions = params
        .and_then(|p| p.get("actions"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    actions
        .iter()
        .find(|a| {
            a.get("title")
                .and_then(Value::as_str)
                .is_some_and(|t| policy.message_actions.get(t) == Some(&true))
        })
        .cloned()
        .unwrap_or(Value::Null)
}

impl LspClient {
    /// Spawn, initialize (`initialize` + `initialized` + `didChangeConfiguration` +
    /// `after_initialized`) and wait for readiness.
    pub fn start(
        cmd: &ServerCommand,
        workspace: &Path,
        opts: ClientOptions,
    ) -> Result<LspClient, SemanticError> {
        if !cmd.program.is_absolute() {
            return Err(SemanticError::ExecutableUnavailable(cmd.program.display().to_string()));
        }
        let workspace_uri = path_to_uri(workspace)?;
        let log_path = opts
            .stderr_log
            .clone()
            .unwrap_or_else(|| workspace.join("lsp.stderr.log"));
        let stderr = File::create(&log_path)?;
        let mut command = Command::new(&cmd.program);
        command
            .args(&cmd.args)
            .current_dir(&cmd.cwd)
            .env_clear()
            .envs(cmd.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr));
        crate::procs::isolate(&mut command);
        let mut child = command.spawn().map_err(|source| SemanticError::Launch {
            program: cmd.program.display().to_string(),
            source,
        })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            crate::procs::stop_tree(&mut child, Duration::ZERO);
            return Err(SemanticError::Protocol("language server pipes unavailable".into()));
        };
        let (tx, rx) = mpsc::channel();
        let keep_diagnostics = Arc::new(AtomicBool::new(true));
        let reader_keeps = Arc::clone(&keep_diagnostics);
        let reader = match thread::Builder::new()
            .name("trace-lsp-reader".into())
            .spawn(move || read_loop(stdout, tx, &reader_keeps))
        {
            Ok(handle) => handle,
            Err(e) => {
                crate::procs::stop_tree(&mut child, Duration::ZERO);
                return Err(e.into());
            }
        };
        let now = Instant::now();
        let ready_deadline = now + opts.ready_timeout.max(Duration::from_millis(1));
        let mut client = LspClient {
            child,
            stdin: Some(stdin),
            incoming: rx,
            reader: Some(reader),
            opts,
            next_id: 1,
            workspace_uri,
            capabilities: Value::Null,
            metrics: LspMetrics::default(),
            log_path,
            started: now,
            ready_deadline,
            initialized_at: now,
            log_messages: Kept::new(MAX_KEPT, MAX_KEPT_BYTES, true),
            notifications: Kept::new(MAX_KEPT, MAX_KEPT_BYTES, true),
            latest_per_document: BTreeMap::new(),
            latest_seq: 0,
            latest_bytes: 0,
            diagnostics: Kept::new(MAX_KEPT, MAX_KEPT_BYTES, false),
            keep_diagnostics,
            seen: HashSet::new(),
            log_matched: false,
            quiescent: false,
            quiescent_since: None,
            service_ready: false,
            readiness: Readiness::default(),
            open_docs: HashMap::new(),
            contexts: HashMap::new(),
            recent_docs: VecDeque::new(),
            failure: None,
            crash_logged: false,
            closed: false,
        };
        let started = client.initialize(workspace).and_then(|()| client.wait_ready());
        if let Err(e) = started {
            client.close(false);
            return Err(e);
        }
        Ok(client)
    }

    /// Server capabilities from `initialize`.
    pub fn capabilities(&self) -> &Value {
        &self.capabilities
    }

    pub fn notify(&mut self, method: &str, params: Value) -> Result<(), SemanticError> {
        let params = (!params.is_null()).then_some(&params);
        self.send(&NotificationFrame {
            jsonrpc: "2.0",
            method,
            params,
        })
    }

    /// One request with timeout.
    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, SemanticError> {
        let mut results = self.request_many(vec![(method.to_string(), params)])?;
        results
            .pop()
            .unwrap_or_else(|| Err(SemanticError::Protocol("missing response".into())))
    }

    /// Pipelined requests; results in input order. Individual RPC errors are returned per
    /// item; timeouts/deadline/exit abort the whole batch.
    pub fn request_many(
        &mut self,
        calls: Vec<(String, Value)>,
    ) -> Result<Vec<Result<Value, SemanticError>>, SemanticError> {
        let limit = self.opts.session_deadline;
        let timeout = self.opts.request_timeout;
        if self.opts.answer_policy.project_contexts {
            return self.batch_in_contexts(calls, limit, timeout);
        }
        self.batch(calls, limit, timeout)
    }

    /// Remember `uri` as the document handled last (crash logs name the last few).
    pub(super) fn note_document(&mut self, uri: &str) {
        if self.recent_docs.back().is_some_and(|last| last == uri) {
            return;
        }
        self.recent_docs.retain(|d| d != uri);
        self.recent_docs.push_back(uri.to_string());
        while self.recent_docs.len() > MAX_RECENT_DOCS {
            self.recent_docs.pop_front();
        }
    }

    /// `textDocument/didOpen` with exact text (BOM stripped as LSP requires; positions are
    /// mapped with [`trace_core::text::LineIndex`], which accounts for the BOM). A document
    /// that is already open gets a full-text `didChange` instead (never a second open).
    pub fn open(&mut self, uri: &str, language_id: &str, text: &str) -> Result<(), SemanticError> {
        if let Some(version) = self.open_docs.get(uri).copied() {
            return self.change(uri, version + 1, text);
        }
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
        let params = DidOpen {
            text_document: TextDocumentItem {
                uri,
                language_id,
                version: 1,
                text,
            },
        };
        self.note_document(uri);
        self.send(&NotificationFrame {
            jsonrpc: "2.0",
            method: "textDocument/didOpen",
            params: Some(&params),
        })?;
        self.open_docs.insert(uri.to_string(), 1);
        Ok(())
    }

    /// Full-text `textDocument/didChange` of an open document (BOM stripped like `open`).
    /// A change of a document that is not open is refused with a protocol error (callers
    /// open first).
    pub fn change(&mut self, uri: &str, version: i32, text: &str) -> Result<(), SemanticError> {
        if !self.open_docs.contains_key(uri) {
            return Err(SemanticError::Protocol(format!("didChange for a closed document {uri}")));
        }
        self.note_document(uri);
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
        let params = json!({
            "textDocument": {"uri": uri, "version": version},
            "contentChanges": [{"text": text}]
        });
        self.notify("textDocument/didChange", params)?;
        self.open_docs.insert(uri.to_string(), version);
        Ok(())
    }

    /// Whether `uri` is open, with its version.
    pub fn open_version(&self, uri: &str) -> Option<i32> {
        self.open_docs.get(uri).copied()
    }

    /// `textDocument/didClose` (a no-op for documents that are not open).
    pub fn close_document(&mut self, uri: &str) -> Result<(), SemanticError> {
        if self.open_docs.remove(uri).is_none() {
            return Ok(());
        }
        self.contexts.remove(uri);
        self.notify("textDocument/didClose", json!({"textDocument": {"uri": uri}}))
    }

    /// `workspace/didChangeWatchedFiles` for workspace files changed on disk.
    pub fn files_changed(&mut self, changes: &[(String, FileChange)]) -> Result<(), SemanticError> {
        if changes.is_empty() {
            return Ok(());
        }
        let changes: Vec<Value> = changes
            .iter()
            .map(|(uri, kind)| json!({"uri": uri, "type": *kind as u8}))
            .collect();
        self.notify("workspace/didChangeWatchedFiles", json!({ "changes": changes }))
    }

    /// Replace the session deadline (persistent sessions: one deadline per update).
    pub fn set_deadline(&mut self, deadline: Instant) {
        self.opts.session_deadline = deadline;
    }

    /// Whether the connection is still usable (no exit, broken frame or write failure seen).
    pub fn is_healthy(&mut self) -> bool {
        self.failure.is_none() && matches!(self.child.try_wait(), Ok(None))
    }

    pub fn metrics(&self) -> &LspMetrics {
        &self.metrics
    }

    /// `window/logMessage` + `window/showMessage` (type, text), bounded (module docs).
    pub fn log_messages(&self) -> &[(u8, String)] {
        self.log_messages.as_slice()
    }

    /// Other notifications (method, params), bounded (module docs; no `$/progress` reports).
    pub fn notifications(&self) -> &[(String, Value)] {
        self.notifications.as_slice()
    }

    /// Params of the kept notifications named `method`, oldest first; for a
    /// [`LATEST_PER_DOCUMENT`] method the latest one of every document.
    pub fn notifications_named(&self, method: &str) -> Vec<Value> {
        if LATEST_PER_DOCUMENT.contains(&method) {
            let mut latest: Vec<(u64, &Value)> = self
                .latest_per_document
                .iter()
                .filter(|((m, _), _)| m == method)
                .map(|(_, (seq, _, params))| (*seq, params))
                .collect();
            latest.sort_by_key(|(seq, _)| *seq);
            return latest.into_iter().map(|(_, params)| params.clone()).collect();
        }
        self.notifications
            .as_slice()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, params)| params.clone())
            .collect()
    }

    /// Keep `params` as the latest `method` notification of its document (bounded).
    pub(super) fn keep_latest(&mut self, method: String, params: Value) {
        let uri = params
            .get("textDocument")
            .and_then(|d| d.get("uri"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let size = value_size(&params) + uri.len() + method.len();
        let key = (method, uri);
        if let Some((_, old, _)) = self.latest_per_document.remove(&key) {
            self.latest_bytes -= old;
        }
        if size > MAX_MESSAGE_BYTES
            || self.latest_per_document.len() >= MAX_LATEST_DOCS
            || self.latest_bytes + size > MAX_KEPT_BYTES
        {
            return;
        }
        self.latest_seq += 1;
        self.latest_bytes += size;
        self.latest_per_document.insert(key, (self.latest_seq, size, params));
    }

    /// `textDocument/publishDiagnostics` (uri, params) until the process was warmed up or the
    /// first [`DIAGNOSTICS_WINDOW`] passed, bounded.
    pub fn diagnostics(&self) -> &[(String, Value)] {
        self.diagnostics.as_slice()
    }

    /// The server's stderr log.
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    /// Graceful shutdown (bounded), then kill.
    pub fn shutdown(mut self) -> Result<LspMetrics, SemanticError> {
        self.close(true);
        Ok(std::mem::take(&mut self.metrics))
    }

    // ---------------------------------------------------------------------------------
    // Internals
    // ---------------------------------------------------------------------------------

    pub(super) fn initialize(&mut self, workspace: &Path) -> Result<(), SemanticError> {
        let uri = self.workspace_uri.clone();
        let mut params = json!({
            "processId": std::process::id(),
            "clientInfo": {"name": "trace", "version": trace_core::TRACE_VERSION},
            "rootUri": uri,
            "rootPath": workspace.display().to_string(),
            "workspaceFolders": [{"uri": uri, "name": "workspace"}],
            "initializationOptions": self.opts.initialization_options,
            "capabilities": {
                "general": {"positionEncodings": ["utf-16"]},
                "workspace": {
                    "configuration": true,
                    "workspaceFolders": true,
                    "symbol": {"dynamicRegistration": false},
                    "didChangeConfiguration": {"dynamicRegistration": false},
                    "didChangeWatchedFiles": {"dynamicRegistration": true, "relativePatternSupport": false}
                },
                "textDocument": {
                    "synchronization": {"dynamicRegistration": false, "didSave": false},
                    "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                    "callHierarchy": {"dynamicRegistration": false},
                    "typeHierarchy": {"dynamicRegistration": false},
                    "definition": {"dynamicRegistration": false, "linkSupport": true},
                    "references": {"dynamicRegistration": false},
                    "implementation": {"dynamicRegistration": false, "linkSupport": true},
                    "signatureHelp": {"dynamicRegistration": false, "signatureInformation": {"parameterInformation": {"labelOffsetSupport": true}}},
                    "hover": {"dynamicRegistration": false, "contentFormat": ["plaintext", "markdown"]}
                },
                "window": {"workDoneProgress": true, "showMessage": {"messageActionItem": {"additionalPropertiesSupport": false}}},
                "experimental": {"serverStatusNotification": true}
            },
            "trace": "off"
        });
        // clangd sends `textDocument/inactiveRegions` only to clients declaring it (no
        // command-line flag exists); the engine reads them per document (inactive code).
        if self.opts.answer_policy.inactive_regions {
            params["capabilities"]["textDocument"]["inactiveRegionsCapabilities"] =
                json!({"inactiveRegions": true});
        }
        // Server start-up (JVM, build import, stub indexing) is bounded by the readiness
        // timeout, not by one query timeout.
        let limit = self.ready_deadline;
        let timeout = limit.saturating_duration_since(Instant::now());
        let result = match self.batch(vec![("initialize".to_string(), params)], limit, timeout) {
            Ok(mut results) => results
                .pop()
                .unwrap_or_else(|| Err(SemanticError::Protocol("missing response".into())))?,
            Err(SemanticError::Timeout { .. }) | Err(SemanticError::Deadline) => {
                return Err(self.timeout_error())
            }
            Err(e) => return Err(e),
        };
        let capabilities = match result {
            Value::Object(mut map) => map.remove("capabilities"),
            _ => None,
        }
        .ok_or_else(|| SemanticError::Protocol("initialize result without capabilities".into()))?;
        if let Some(encoding) = capabilities.get("positionEncoding").and_then(Value::as_str) {
            if encoding != "utf-16" {
                return Err(SemanticError::Capability(format!(
                    "position encoding {encoding} (UTF-16 required)"
                )));
            }
        }
        self.capabilities = capabilities;
        self.notify("initialized", json!({}))?;
        self.initialized_at = Instant::now();
        let settings = json!({"settings": self.opts.settings});
        self.notify("workspace/didChangeConfiguration", settings)?;
        let after = std::mem::take(&mut self.opts.after_initialized);
        for (method, params) in &after {
            self.notify(method, params.clone())?;
        }
        self.opts.after_initialized = after;
        Ok(())
    }

    pub(super) fn batch(
        &mut self,
        calls: Vec<(String, Value)>,
        limit: Instant,
        timeout: Duration,
    ) -> Result<Vec<Result<Value, SemanticError>>, SemanticError> {
        let mut results: Vec<Option<Result<Value, SemanticError>>> = (0..calls.len()).map(|_| None).collect();
        let mut queue: VecDeque<Queued> = calls
            .into_iter()
            .enumerate()
            .map(|(i, (method, params))| (i, method, params, 0u8, None))
            .collect();
        let mut pending: HashMap<i64, Pending> = HashMap::new();
        let window = self.opts.max_in_flight.max(1);
        let policy = self.opts.answer_policy;
        loop {
            let mut deferred: Option<Instant> = None;
            let mut rotations = queue.len();
            while pending.len() < window && rotations > 0 {
                rotations -= 1;
                let Some((index, method, params, attempts, not_before)) = queue.pop_front() else {
                    break;
                };
                let now = Instant::now();
                if let Some(t) = not_before {
                    if t > now {
                        deferred = Some(deferred.map_or(t, |d: Instant| d.min(t)));
                        queue.push_back((index, method, params, attempts, not_before));
                        continue;
                    }
                }
                if now >= limit {
                    return Err(self.limit_error(limit, &method));
                }
                if let Some(uri) = document_uri(&params).map(str::to_string) {
                    self.note_document(&uri);
                }
                let id = self.next_id;
                self.next_id += 1;
                self.send(&RequestFrame {
                    jsonrpc: "2.0",
                    id,
                    method: &method,
                    params: (!params.is_null()).then_some(&params),
                })?;
                self.metrics.requests += 1;
                pending.insert(
                    id,
                    Pending {
                        index,
                        method,
                        params,
                        sent: now,
                        attempts,
                    },
                );
            }
            if pending.is_empty() {
                match deferred {
                    None if queue.is_empty() => break,
                    None => continue,
                    Some(t) => {
                        // Only paused retries remain: serve the server until the pause ends.
                        let until = t.min(limit);
                        let _ = self.next_response(until)?;
                        if Instant::now() >= limit {
                            let method = queue.front().map(|q| q.1.clone()).unwrap_or_default();
                            return Err(self.limit_error(limit, &method));
                        }
                        continue;
                    }
                }
            }
            let now = Instant::now();
            let (expiry, oldest) = pending
                .iter()
                .map(|(id, p)| (p.sent + timeout, *id))
                .min()
                .unwrap_or((now, 0));
            if now >= limit || now >= expiry {
                let method = pending.get(&oldest).map(|p| p.method.clone()).unwrap_or_default();
                return Err(if now >= limit {
                    self.limit_error(limit, &method)
                } else {
                    SemanticError::Timeout { method }
                });
            }
            let wake = deferred.map_or(expiry, |d| d.min(expiry)).min(limit);
            let Some(response) = self.next_response(wake)? else {
                continue;
            };
            let Value::Object(mut response) = response else {
                continue;
            };
            let Some(id) = response.get("id").and_then(Value::as_i64) else {
                continue;
            };
            // Late responses to abandoned requests are ignored.
            let Some(p) = pending.remove(&id) else {
                continue;
            };
            *self.metrics.seconds_by_method.entry(p.method.clone()).or_insert(0.0) +=
                p.sent.elapsed().as_secs_f64();
            let outcome = match response.remove("error") {
                Some(error) => {
                    let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
                    if p.attempts == 0 && (code == CONTENT_MODIFIED || code == SERVER_CANCELLED) {
                        // The server state moved while answering; ask once more.
                        queue.push_back((p.index, p.method, p.params, 1, None));
                        continue;
                    }
                    if code == REQUEST_CANCELLED && p.attempts < policy.retry_cancelled {
                        let attempts = p.attempts + 1;
                        let pause = CANCELLED_PAUSE * u32::from(attempts);
                        queue.push_back((
                            p.index,
                            p.method,
                            p.params,
                            attempts,
                            Some(Instant::now() + pause),
                        ));
                        continue;
                    }
                    if code == INTERNAL_ERROR && policy.internal_error_is_unresolved {
                        Ok(Value::Null)
                    } else {
                        Err(SemanticError::Rpc {
                            method: p.method,
                            code,
                            message: error
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        })
                    }
                }
                None => Ok(response.remove("result").unwrap_or(Value::Null)),
            };
            results[p.index] = Some(outcome);
        }
        Ok(results
            .into_iter()
            .map(|r| r.unwrap_or_else(|| Err(SemanticError::Protocol("missing response".into()))))
            .collect())
    }

    pub(super) fn limit_error(&self, limit: Instant, method: &str) -> SemanticError {
        if limit == self.opts.session_deadline {
            SemanticError::Deadline
        } else {
            SemanticError::Timeout {
                method: method.to_string(),
            }
        }
    }

    /// Next response message (server requests and notifications are handled inline);
    /// `None` when `until` passes first.
    pub(super) fn next_response(&mut self, until: Instant) -> Result<Option<Value>, SemanticError> {
        loop {
            self.check_failure()?;
            let now = Instant::now();
            if now >= until {
                return Ok(None);
            }
            match self.incoming.recv_timeout(until - now) {
                Ok(Incoming::Message(message)) => {
                    if let Some(response) = self.dispatch(message)? {
                        return Ok(Some(response));
                    }
                }
                Ok(Incoming::Eof) | Err(RecvTimeoutError::Disconnected) => {
                    let detail = self.exit_detail("language server closed its output");
                    self.failure = Some(Failure::Exited(detail));
                }
                Ok(Incoming::Broken(reason)) => {
                    self.failure = Some(Failure::Broken(reason));
                }
                Err(RecvTimeoutError::Timeout) => return Ok(None),
            }
        }
    }

    /// The connection's failure as an error: an exited process is a crash of the language
    /// server (setup error with the log, the exit detail appended to the log once).
    pub(super) fn check_failure(&mut self) -> Result<(), SemanticError> {
        match self.failure.clone() {
            None => Ok(()),
            Some(Failure::Exited(detail)) => {
                if !self.crash_logged {
                    self.crash_logged = true;
                    let documents = self.last_documents();
                    if let Ok(mut log) = std::fs::OpenOptions::new()
                        .append(true)
                        .create(true)
                        .open(&self.log_path)
                    {
                        let _ = writeln!(log, "\ntrace: {detail}");
                        if !documents.is_empty() {
                            let _ = writeln!(
                                log,
                                "trace: documents analysed last (the last one first): {}",
                                documents.join(", ")
                            );
                        }
                    }
                }
                Err(SemanticError::Setup(SetupError::ServerCrashed {
                    language: self.opts.language,
                    log: self.log_path.clone(),
                }))
            }
            Some(Failure::Broken(reason)) => Err(SemanticError::Protocol(reason)),
        }
    }

    /// The documents handled last, newest first, as file paths where they are files.
    pub(super) fn last_documents(&self) -> Vec<String> {
        self.recent_docs
            .iter()
            .rev()
            .map(|uri| {
                uri_to_path(uri)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| uri.clone())
            })
            .collect()
    }

    pub(super) fn exit_detail(&mut self, reason: &str) -> String {
        // The reader saw EOF; give the process a moment to report its status.
        let until = Instant::now() + Duration::from_millis(500);
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return format!("{reason}; {status}"),
                Ok(None) if Instant::now() < until => thread::sleep(Duration::from_millis(20)),
                _ => return reason.to_string(),
            }
        }
    }

    pub(super) fn dispatch(&mut self, message: Value) -> Result<Option<Value>, SemanticError> {
        let method = message.get("method").and_then(Value::as_str).map(str::to_owned);
        let Some(method) = method else {
            let is_response = message.get("id").is_some();
            return Ok(is_response.then_some(message));
        };
        match message.get("id") {
            Some(id) => {
                let id = id.clone();
                match self.server_request(&method, message.get("params")) {
                    Ok(result) => self.send(&ResponseFrame {
                        jsonrpc: "2.0",
                        id: &id,
                        result: Some(&result),
                        error: None,
                    })?,
                    Err((code, text)) => self.send(&ResponseFrame {
                        jsonrpc: "2.0",
                        id: &id,
                        result: None,
                        error: Some(json!({"code": code, "message": text})),
                    })?,
                }
            }
            None => self.notification(method, message),
        }
        Ok(None)
    }

    /// Track one server notification (readiness signals, kept logs and diagnostics).
    pub(super) fn notification(&mut self, method: String, mut message: Value) {
        let now = Instant::now();
        let params = message.get_mut("params").map(Value::take).unwrap_or(Value::Null);
        match method.as_str() {
            "experimental/serverStatus" => {
                let quiescent = params.get("quiescent").and_then(Value::as_bool).unwrap_or(false);
                if quiescent && !self.quiescent {
                    self.quiescent_since = Some(now);
                } else if !quiescent {
                    self.quiescent_since = None;
                }
                self.quiescent = quiescent;
            }
            // jdtls: `Starting` / `ProjectStatus` while the project is imported,
            // `ServiceReady` once queries see the whole project (`Error`: the import failed;
            // nothing more will come, `check_loaded` reports it).
            "language/status" => {
                if matches!(params.get("type").and_then(Value::as_str), Some("ServiceReady" | "Error")) {
                    self.service_ready = true;
                }
            }
            "window/logMessage" | "window/showMessage" => {
                let kind = params.get("type").and_then(Value::as_u64).unwrap_or(4);
                let text = params.get("message").and_then(Value::as_str).unwrap_or_default();
                if let ReadySpec::Log { contains } = &self.opts.ready {
                    if text.contains(contains.as_str()) {
                        self.log_matched = true;
                    }
                }
                let text = cut_text(text, MAX_MESSAGE_BYTES);
                self.log_messages
                    .push((u8::try_from(kind).unwrap_or(4), text.to_string()), text.len() + 16);
                return;
            }
            "textDocument/publishDiagnostics" => {
                if now >= self.started + DIAGNOSTICS_WINDOW {
                    self.keep_diagnostics.store(false, Ordering::Relaxed);
                }
                if self.keep_diagnostics.load(Ordering::Relaxed) {
                    let uri = params
                        .get("uri")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let size = value_size(&params) + uri.len();
                    self.diagnostics.push((uri, params), size);
                }
                return;
            }
            _ => {}
        }
        self.readiness.observe(&method, Some(&params), now);
        if self.seen.len() < MAX_SEEN_METHODS || self.seen.contains(&method) {
            self.seen.insert(method.clone());
        }
        if LATEST_PER_DOCUMENT.contains(&method.as_str()) {
            self.keep_latest(method, params);
            return;
        }
        // Progress reports are only observed (readiness); their volume is never kept.
        let report = method == "$/progress"
            && params
                .get("value")
                .and_then(|v| v.get("kind"))
                .and_then(Value::as_str)
                == Some("report");
        if !report {
            let size = value_size(&params) + method.len();
            let params = if size > MAX_MESSAGE_BYTES {
                Value::Null
            } else {
                params
            };
            self.notifications.push((method, params), size.min(MAX_MESSAGE_BYTES));
        }
    }

    /// Answer a server-to-client request. Nothing here modifies files or runs anything.
    pub(super) fn server_request(
        &self,
        method: &str,
        params: Option<&Value>,
    ) -> Result<Value, (i64, String)> {
        let policy = &self.opts.server_requests;
        match method {
            "workspace/configuration" => {
                Ok(configuration(&self.opts.settings, params, policy.configuration_missing))
            }
            "window/showMessageRequest" => Ok(message_action(params, policy)),
            "workspace/workspaceFolders" => Ok(json!([{"uri": self.workspace_uri, "name": "workspace"}])),
            "workspace/applyEdit" => Ok(json!({
                "applied": false,
                "failureReason": "trace is a read-only analysis client"
            })),
            "window/showDocument" => Ok(json!({"success": false})),
            "window/workDoneProgress/create"
            | "client/registerCapability"
            | "client/unregisterCapability"
            | "workspace/semanticTokens/refresh"
            | "workspace/inlayHint/refresh"
            | "workspace/inlineValue/refresh"
            | "workspace/codeLens/refresh"
            | "workspace/diagnostic/refresh"
            | "workspace/foldingRange/refresh" => Ok(Value::Null),
            other => Err((METHOD_NOT_FOUND, format!("{other} is not supported by the trace client"))),
        }
    }

    pub(super) fn send<T: Serialize>(&mut self, frame: &T) -> Result<(), SemanticError> {
        self.check_failure()?;
        let body = serde_json::to_vec(frame)?;
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(SemanticError::Protocol("language server input already closed".into()));
        };
        if let Err(e) = write_frame(stdin, &body) {
            let detail = self.exit_detail(&format!("write to language server failed: {e}"));
            self.failure = Some(Failure::Exited(detail));
            self.check_failure()?;
        }
        Ok(())
    }

    /// Bounded shutdown: optional `shutdown` handshake, then the whole process tree is stopped
    /// (module docs: Unix after `exit` and a bounded wait, Windows while the server runs).
    pub(super) fn close(&mut self, graceful: bool) {
        if self.closed {
            return;
        }
        self.closed = true;
        let running = matches!(self.child.try_wait(), Ok(None));
        let polite = graceful && running;
        let mut answered = false;
        if polite && self.failure.is_none() {
            let until = Instant::now() + SHUTDOWN_GRACE;
            answered = self
                .batch(vec![("shutdown".into(), Value::Null)], until, SHUTDOWN_GRACE)
                .is_ok();
        }
        if polite && !cfg!(windows) {
            if answered {
                let _ = self.notify("exit", Value::Null);
            }
            drop(self.stdin.take());
            let wait_until = Instant::now() + SHUTDOWN_GRACE;
            while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < wait_until {
                thread::sleep(Duration::from_millis(20));
            }
        }
        // Windows: the tree is stopped before the server's input closes (a server exiting at
        // the end of its input would leave its descendants out of taskkill's reach).
        let grace = if polite {
            crate::procs::STOP_GRACE
        } else {
            Duration::ZERO
        };
        crate::procs::stop_tree(&mut self.child, grace);
        drop(self.stdin.take());
        if let Some(reader) = self.reader.take() {
            // The reader ends at EOF; never block indefinitely on a stuck pipe.
            let until = Instant::now() + Duration::from_secs(2);
            while !reader.is_finished() && Instant::now() < until {
                thread::sleep(Duration::from_millis(10));
            }
            if reader.is_finished() {
                let _ = reader.join();
            }
        }
    }
}
