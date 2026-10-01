//! The persistent worker session ([`TsSession`]): `main.mjs --serve` with JSON lines on
//! stdin / stdout; only changed files are re-sent and only queried files re-visited.

use crate::backend::{run_summary, Backend, BackendOutput, SemanticFile, SemanticRequest};
use crate::mapping::DeclTable;
use crate::references::{LiveReferences, ReferenceQuery};
use crate::session::BackendSession;
use crate::snapshot::Snapshot;
use crate::tools::clean_env;
use crate::SemanticError;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};
use trace_core::setup_error::SetupError;
use trace_core::{Hash32, Language};

use super::*;

/// A persistent worker (`main.mjs --serve`): the compiler program stays warm; updates send
/// only changed and deleted files; analyses visit only the queried files.
pub struct TsSession {
    pub(super) language: Language,
    pub(super) fingerprint: String,
    pub(super) child: Child,
    pub(super) stdin: Option<ChildStdin>,
    pub(super) lines: Receiver<std::io::Result<String>>,
    pub(super) stderr_log: PathBuf,
    /// Text hash of every file the worker holds.
    pub(super) sent: HashMap<String, Hash32>,
    pub(super) version: Option<String>,
    pub(super) deadline: Duration,
    /// Held for the session's lifetime (the workspace root the worker's paths live under).
    pub(super) snapshot: Snapshot,
}

impl TsSession {
    pub(super) fn open(
        backend: &TypeScript,
        request: &SemanticRequest<'_>,
    ) -> Result<TsSession, SemanticError> {
        let files = worker_files(request);
        let language = partition_language(&files);
        let launch = TypeScript::launch_inputs(request.tools, language)?;
        // The worker reads nothing from disk under the workspace (sources are sent as text),
        // so the stable workspace holds no copies; its state dir keeps the log.
        let snapshot = Snapshot::create(&request.repo.workspaces_dir, backend.id(), &request.repo.root, &[])?;
        let state = snapshot.outside_dir();
        fs::create_dir_all(&state)?;
        let stderr_log = state.join("ts-worker-session.stderr.log");
        let mut command = Command::new(&launch.node);
        command
            .arg(&launch.worker)
            .arg("--serve")
            .arg(&launch.sdk)
            .current_dir(&snapshot.dir)
            .env_clear()
            .envs(clean_env(&[], &[]))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(File::create(&stderr_log)?));
        crate::procs::isolate(&mut command);
        let mut child = command.spawn().map_err(|source| SemanticError::Launch {
            program: launch.node.display().to_string(),
            source,
        })?;
        let stdin = child.stdin.take();
        let Some(stdout) = child.stdout.take() else {
            crate::procs::stop_tree(&mut child, Duration::ZERO);
            return Err(SemanticError::Setup(SetupError::ServerCrashed {
                language,
                log: stderr_log,
            }));
        };
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut session = TsSession {
            language,
            fingerprint: backend.fingerprint(request.tools, request.prepared),
            child,
            stdin,
            lines: rx,
            stderr_log,
            sent: HashMap::new(),
            version: launch.version,
            deadline: request.tools.session_deadline,
            snapshot,
        };
        let deadline = Instant::now() + session.deadline;
        let mut invalid = HashSet::new();
        let input = worker_input(&session.snapshot.dir, request, &files, &mut invalid);
        let message = serde_json::json!({"op": "open", "input": input});
        drop(input);
        let reply = session.call(&message, deadline)?;
        drop(message);
        session.expect_ok(&reply)?;
        session.sent = files
            .iter()
            .filter(|f| !invalid.contains(f.path))
            .map(|f| (f.path.to_string(), f.hash))
            .collect();
        Ok(session)
    }

    pub(super) fn crashed(&self) -> SemanticError {
        SemanticError::Setup(SetupError::ServerCrashed {
            language: self.language,
            log: self.stderr_log.clone(),
        })
    }

    /// Send one request line and wait for its answer line (bounded by `deadline`).
    pub(super) fn call(
        &mut self,
        message: &serde_json::Value,
        deadline: Instant,
    ) -> Result<String, SemanticError> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        let written = match self.stdin.as_mut() {
            Some(stdin) => stdin.write_all(&line).and_then(|()| stdin.flush()).is_ok(),
            None => false,
        };
        if !written {
            return Err(self.crashed());
        }
        let wait = deadline.saturating_duration_since(Instant::now());
        match self.lines.recv_timeout(wait) {
            Ok(Ok(answer)) => {
                if answer.len() as u64 > MAX_WORKER_OUTPUT {
                    return Err(SemanticError::Worker("TypeScript worker output too large".into()));
                }
                Ok(answer)
            }
            Ok(Err(e)) => Err(SemanticError::Io(e)),
            Err(RecvTimeoutError::Timeout) => {
                crate::procs::stop_tree(&mut self.child, Duration::ZERO);
                Err(SemanticError::Setup(SetupError::ServerTimeout {
                    language: self.language,
                    minutes: deadline_minutes(self.deadline),
                    log: self.stderr_log.clone(),
                }))
            }
            Err(RecvTimeoutError::Disconnected) => Err(self.crashed()),
        }
    }

    /// `{"ok":true}` or the worker's error.
    pub(super) fn expect_ok(&self, answer: &str) -> Result<(), SemanticError> {
        let value: serde_json::Value = serde_json::from_str(answer)?;
        if value.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
            return Ok(());
        }
        Err(worker_failure(&value))
    }

    /// Parse an analysis / references answer (or the worker's error).
    pub(super) fn output(&self, answer: &str) -> Result<WorkerOutput, SemanticError> {
        if answer.starts_with("{\"ok\":false") {
            let value: serde_json::Value = serde_json::from_str(answer)?;
            return Err(worker_failure(&value));
        }
        serde_json::from_str(answer)
            .map_err(|e| SemanticError::Worker(format!("malformed TypeScript worker output: {e}")))
    }

    /// Bring the worker's files up to date with `files`: only changed / new texts and the
    /// deleted paths are sent. Returns the non-UTF-8 files (left out).
    pub(super) fn sync<'a>(
        &mut self,
        files: &[&SemanticFile<'a>],
        deadline: Instant,
    ) -> Result<HashSet<&'a str>, SemanticError> {
        let mut invalid = HashSet::new();
        let mut current: HashMap<String, Hash32> = HashMap::with_capacity(files.len());
        let mut changed: Vec<WorkerChange<'a>> = Vec::new();
        for f in files {
            match std::str::from_utf8(f.source) {
                Ok(text) => {
                    current.insert(f.path.to_string(), f.hash);
                    if self.sent.get(f.path) != Some(&f.hash) {
                        changed.push(WorkerChange { path: f.path, text });
                    }
                }
                Err(_) => {
                    invalid.insert(f.path);
                }
            }
        }
        let mut deleted: Vec<String> = self
            .sent
            .keys()
            .filter(|p| !current.contains_key(p.as_str()))
            .cloned()
            .collect();
        deleted.sort_unstable();
        if !changed.is_empty() || !deleted.is_empty() {
            let message = serde_json::json!({"op": "update", "changed": changed, "deleted": deleted});
            let answer = self.call(&message, deadline)?;
            self.expect_ok(&answer)?;
        }
        self.sent = current;
        Ok(invalid)
    }
}

impl BackendSession for TsSession {
    fn backend_id(&self) -> &str {
        "typescript"
    }

    fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    fn processes(&self) -> usize {
        1
    }

    fn update(&mut self, request: &SemanticRequest<'_>) -> Result<BackendOutput, SemanticError> {
        let started = Instant::now();
        let deadline = started + self.deadline;
        let files = worker_files(request);
        let invalid = self.sync(&files, deadline)?;
        let names = declared_names(&files);
        let query: Vec<&str> = files
            .iter()
            .filter(|f| request.query.contains(f.path) && !invalid.contains(f.path))
            .map(|f| f.path)
            .collect();
        let answer =
            self.call(&serde_json::json!({"op": "analyze", "query": query, "names": names}), deadline)?;
        let output = self.output(&answer)?;
        drop(answer);
        let decls = DeclTable::new(files.iter().map(|f| (f.path, f.source, f.facts)));
        let queried: Vec<&SemanticFile<'_>> = files
            .iter()
            .copied()
            .filter(|f| request.query.contains(f.path))
            .collect();
        let results = map_worker_output(&output, &queried, &decls, &invalid, &self.fingerprint);
        Ok(BackendOutput {
            files: results,
            run: run_summary(
                "typescript",
                &[Language::JavaScript, Language::TypeScript, Language::Tsx],
                files.len(),
                queried.len(),
                1,
                started,
                self.version.clone(),
            ),
        })
    }

    fn references(
        &mut self,
        request: &SemanticRequest<'_>,
        query: &ReferenceQuery,
    ) -> Result<Option<LiveReferences>, SemanticError> {
        let files = worker_files(request);
        if !files.iter().any(|f| f.path == query.path) {
            return Ok(None);
        }
        let deadline = Instant::now() + self.deadline;
        self.sync(&files, deadline)?;
        let message = serde_json::json!({
            "op": "references",
            "query": {"file": query.path, "start_byte": query.byte}
        });
        let answer = self.call(&message, deadline)?;
        let output = self.output(&answer)?;
        Ok(Some(map_worker_references(&output, &files, query, "typescript")))
    }

    fn close(mut self: Box<Self>) {
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        let _ = self.call(&serde_json::json!({"op": "shutdown"}), deadline);
        drop(self.stdin.take());
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    // Whatever the worker started (the native compiler) ends with it.
                    crate::procs::stop_leftovers(&mut self.child);
                    break;
                }
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                _ => {
                    crate::procs::stop_tree(&mut self.child, Duration::ZERO);
                    break;
                }
            }
        }
    }
}

impl Drop for TsSession {
    fn drop(&mut self) {
        // A session dropped without `close` never leaves a worker (or its compiler) behind.
        if let Ok(None) = self.child.try_wait() {
            crate::procs::stop_tree(&mut self.child, Duration::ZERO);
        }
    }
}
