//! Pooled, persistent language-server sessions (shared by Pyright, rust-analyzer and the
//! generic LSP backend).
//!
//! * One stable workspace per backend ([`Snapshot`]: snapshot or mirror mode, DESIGN §1.13);
//!   every process of the pool serves the same workspace (cwd, workspace root), with its own
//!   stderr log (`lsp-<n>.stderr.log`). Processes start lazily, in parallel, and each waits
//!   for its server's readiness signal ([`crate::lsp`]); after readiness the launcher
//!   validates the loaded server ([`Launcher::loaded`], `Server::check_loaded`), whose
//!   notes are returned once with the next update ([`SessionUpdate::diagnostics`]).
//! * File sharding (`ShardMode::Files`, [`assign`]): an update that queries `q` files uses
//!   `min(max_processes, ceil(q / workers.min_files_per_process))` processes (Pyright:
//!   `max_processes` = [`crate::tools::ToolEnv::processes`], bounded by the memory budget,
//!   [`pool_size`]; servers that index the whole workspace per process: 1). Queried files
//!   are grouped by parent directory; whole directories go to the least-loaded process by
//!   estimated request count ([`weight`]); a directory heavier than the balanced target is
//!   split into contiguous runs; in persistent sessions a file stays with the process that
//!   already has it open. Each process opens only its shard's documents (Pyright,
//!   `keep_documents_open = false`: closed again before the queries) and runs
//!   [`engine::analyze`] on them; per-file results are disjoint, so the merge is a union.
//! * Request sharding (`ShardMode::Requests`, DESIGN §1.14.1): for single-threaded servers
//!   (bash-language-server answers one request at a time) N processes serve the same
//!   workspace, every process opens every queried document, and one analysis distributes
//!   its requests round-robin over them ([`Sharded`]); N = [`request_shards`]:
//!   `min(8, cores / 2)`, the entry's `processes`, the memory budget and the work.
//! * Warm updates ([`LspSession::update_reusing`]): the workspace is synced (watcher hints
//!   when given); every process receives `workspace/didChangeWatchedFiles`, `didChange` for
//!   documents it keeps open and `didClose` for removed ones, then one ordered cheap request
//!   per changed source file (`documentSymbol`: LSP servers handle requests after the
//!   notifications before them, so its answer proves the edit was applied), then the
//!   backend's `Server::settle_changes` (servers that re-index asynchronously after an
//!   edit - Intelephense - are waited for, bounded) before the re-query ([`apply_changes`]);
//!   servers with a readiness signal settle again. A changed
//!   build file of this backend's languages ([`crate::session::is_build_file_for`]) restarts
//!   this session's processes only (re-import; the new processes wait for the import-finished
//!   signal); other backends stay warm.
//! * Declaration reuse: answers of unchanged units ([`crate::cache::FileReuse`]) are handed to
//!   the engine, which asks only the rest and returns the raw answers of every file
//!   ([`SessionUpdate::answers`]).
//! * Originals are re-verified for every analysed file written to the workspace since the
//!   last verification plus every queried file ([`Snapshot::verify_paths`]).
//! * Memory (Pyright): K = the largest process count whose estimated pool memory
//!   ([`estimate_mb`], a monotone model fitted to measured pools) fits
//!   `semantic.memory_budget_mb` (`ToolEnv::pool_memory_mb`, 0 = unbounded), never below 1
//!   ([`pool_size`]); a Node heap cap equal to the budget is added only when even one process
//!   is estimated above it ([`heap_cap_mb`]). Results are identical for every K.
//! * Live find-references (SPEC section 8.9): [`references_once`] and
//!   [`LspSession::references`] (sync, then one request on the first process).

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use trace_core::facts::FileFacts;
use trace_core::model::{Diagnostic, Provider};
use trace_core::{Hash32, Language};

use crate::backend::{run_summary, BackendOutput, SemanticFile, SemanticRequest};
use crate::cache::{FileAnswers, FileReuse};
use crate::engine::{self, lsp_text, Analysis, Options, Session, UriResolver};
use crate::languages::WorkspaceMode;
use crate::lsp::{capability_enabled, ClientOptions, FileChange, LspClient, ServerCommand};
use crate::mapping::DeclTable;
use crate::mirror::MirrorRules;
use crate::references::{LiveReferences, ReferenceQuery};
use crate::registry::ShardMode;
use crate::session::{BackendSession, SessionUpdate};
use crate::snapshot::{Snapshot, SnapshotFile, SyncDelta, WorkspaceOptions};
use crate::tools::ToolEnv;
use crate::SemanticError;

/// Changed files confirmed with an ordered request per update (the rest are covered by the
/// order of the same stream).
const MAX_CONFIRMED: usize = 64;

/// Extra `textDocument/references` rounds on a freshly started process (until two
/// consecutive answers agree) and the pause between them.
const REFERENCE_REPEATS: usize = 4;
const REFERENCE_REPEAT_PAUSE: Duration = Duration::from_millis(300);

/// Backend-specific launch details.
pub(crate) trait Launcher: Send + Sync {
    /// Write controlled configuration into a newly opened workspace (e.g.
    /// `pyrightconfig.json`, generated files, approved build steps).
    fn prepare(&self, snapshot: &Snapshot, files: &[&SemanticFile<'_>]) -> Result<(), SemanticError>;
    /// Command line and client options of pool process `index`. `heap_cap_mb`: a runtime
    /// heap limit for Node-based servers ([`heap_cap_mb`]); other launchers ignore it.
    fn command(
        &self,
        snapshot: &Snapshot,
        index: usize,
        tools: &ToolEnv,
        deadline: Instant,
        heap_cap_mb: Option<u64>,
    ) -> Result<(ServerCommand, ClientOptions), SemanticError>;
    /// After readiness: validate the loaded server (load failures are setup errors); notes
    /// for the run's diagnostics.
    fn loaded(&self, client: &LspClient) -> Result<Vec<Diagnostic>, SemanticError> {
        let _ = client;
        Ok(Vec::new())
    }
    /// Once per process, after its first documents are opened and before any query
    /// (`Server::warm_up`).
    fn warm_up(
        &self,
        client: &mut LspClient,
        files: &[&SemanticFile<'_>],
        snapshot: &Snapshot,
    ) -> Result<(), SemanticError> {
        let _ = (client, files, snapshot);
        Ok(())
    }
}

/// Static description of the backend a session serves.
#[derive(Clone, Debug)]
pub(crate) struct Profile {
    pub id: String,
    pub languages: Vec<Language>,
    pub provider: Provider,
    pub fingerprint: String,
    pub tool_version: Option<String>,
    /// Pyright algorithm switches (constructor bridge, stub rule) and the Pyright memory
    /// model of the pool ([`pool_size`], [`heap_cap_mb`]).
    pub python: bool,
    pub max_processes: usize,
    /// Configuration globs copied into snapshot workspaces.
    pub config_names: &'static [&'static str],
    /// Keep queried documents open (servers that only analyse open documents). When false,
    /// documents are opened and closed again before the queries (the server reads them
    /// from the workspace on disk).
    pub keep_documents_open: bool,
    /// File or request sharding (registry `shard`).
    pub shard: ShardMode,
    /// Workspace mode (preflight `Prepared.workspace`).
    pub workspace: WorkspaceMode,
    /// Mirror globs (registry `workspace.include_ignored` / `exclude`).
    pub rules: MirrorRules,
    /// The workspace name of an analysed file when the server needs another one than its
    /// repository path (Pyright: extensionless Python files as `<path>.py`); `None` = its
    /// own path ([`WorkspaceOptions::server_name`]).
    pub server_name: Option<fn(&str) -> Option<String>>,
}

impl Profile {
    /// Language named by setup errors.
    fn language(&self) -> Language {
        self.languages.first().copied().unwrap_or(Language::Python)
    }
}

/// One language-server process of the pool.
struct Worker {
    client: LspClient,
    /// Open documents and their versions.
    opened: HashMap<String, i32>,
    /// [`Profile::keep_documents_open`].
    keep_open: bool,
    /// [`Launcher::warm_up`] ran for this process.
    warmed: bool,
    /// The backend's hooks (`server_text`: the document text the server gets).
    hooks: &'static dyn crate::languages::Server,
}

impl Worker {
    /// [`Launcher::warm_up`] once per process, after its first documents were opened.
    fn warm(
        &mut self,
        launcher: &dyn Launcher,
        files: &[&SemanticFile<'_>],
        snapshot: &Snapshot,
    ) -> Result<(), SemanticError> {
        if !self.warmed && !files.is_empty() {
            launcher.warm_up(&mut self.client, files, snapshot)?;
            self.warmed = true;
            // The load checks and the warm-up read the diagnostics kept so far; later ones
            // are not kept (bounded memory for the life of the session).
            self.client.mark_warmed();
        }
        Ok(())
    }

    /// Open `files` (valid UTF-8 only; closed again unless the profile keeps them open).
    fn open_all(&mut self, files: &[&SemanticFile<'_>], snapshot: &Snapshot) -> Result<(), SemanticError> {
        for f in files {
            if self.opened.contains_key(f.path) {
                continue;
            }
            if let Some(text) = lsp_text(f.source) {
                let uri = snapshot.uri_of(f.path)?;
                let text = self.hooks.server_text(f.language, text);
                self.client
                    .open(&uri, trace_core::languages::info(f.language).lsp_id, &text)?;
                self.opened.insert(f.path.to_string(), 1);
            }
        }
        if !self.keep_open {
            // The workspace on disk holds the same text: closing makes the server read it
            // from disk and stops background checking of open documents (Pyright), while
            // the brief open registers every queried file with the server.
            for f in files {
                if self.opened.remove(f.path).is_some() {
                    self.client.close_document(&snapshot.uri_of(f.path)?)?;
                }
            }
        }
        Ok(())
    }

    /// Open the shard's documents and analyze them.
    #[allow(clippy::too_many_arguments)]
    fn analyze<'a>(
        &mut self,
        launcher: &dyn Launcher,
        shard: &[&SemanticFile<'a>],
        decls: &DeclTable<'a>,
        snapshot: &Snapshot,
        opts: &Options<'_>,
        deadline: Instant,
    ) -> Result<Analysis, SemanticError> {
        self.client.set_deadline(deadline);
        self.open_all(shard, snapshot)?;
        self.warm(launcher, shard, snapshot)?;
        engine::analyze(&mut self.client, shard, decls, snapshot, opts)
    }
}

/// Document updates of one server process (implemented by [`LspClient`]).
pub(crate) trait ChangeSink {
    fn watched(&mut self, changes: &[(String, FileChange)]) -> Result<(), SemanticError>;
    fn version(&self, uri: &str) -> Option<i32>;
    fn change(&mut self, uri: &str, version: i32, text: &str) -> Result<(), SemanticError>;
    fn close(&mut self, uri: &str) -> Result<(), SemanticError>;
    /// One ordered cheap request per URI (`documentSymbol`); its answers prove every
    /// notification before them was applied. A per-request error still proves the order.
    fn confirm(&mut self, uris: &[String]) -> Result<(), SemanticError>;
    /// After the confirmation: wait until the server took the changed documents `changed`
    /// into account (`Server::settle_changes`; bounded, a timeout is a setup error).
    fn settle(&mut self, changed: &[String]) -> Result<(), SemanticError>;
}

/// One server process of a pool as a [`ChangeSink`], with its backend's hooks.
pub(crate) struct ServerSink<'a> {
    pub client: &'a mut LspClient,
    pub hooks: &'static dyn crate::languages::Server,
    pub prepared: &'a crate::languages::Prepared,
}

impl ChangeSink for ServerSink<'_> {
    fn watched(&mut self, changes: &[(String, FileChange)]) -> Result<(), SemanticError> {
        self.client.files_changed(changes)
    }
    fn version(&self, uri: &str) -> Option<i32> {
        self.client.open_version(uri)
    }
    fn change(&mut self, uri: &str, version: i32, text: &str) -> Result<(), SemanticError> {
        self.client.change(uri, version, text)
    }
    fn close(&mut self, uri: &str) -> Result<(), SemanticError> {
        self.client.close_document(uri)
    }
    fn confirm(&mut self, uris: &[String]) -> Result<(), SemanticError> {
        if uris.is_empty() || !capability_enabled(self.client.capabilities(), "documentSymbolProvider") {
            return Ok(());
        }
        let calls = uris
            .iter()
            .map(|uri| ("textDocument/documentSymbol".to_string(), json!({"textDocument": {"uri": uri}})))
            .collect();
        self.client.request_many(calls).map(|_| ())
    }
    fn settle(&mut self, changed: &[String]) -> Result<(), SemanticError> {
        self.hooks
            .settle_changes(&mut crate::languages::SettleContext {
                prepared: self.prepared,
                client: &mut *self.client,
                changed,
            })
            .map_err(SemanticError::Setup)
    }
}

/// Tell one server process about workspace changes (module docs): watched-file events,
/// full-text changes of its open documents, closes of removed ones, the ordered
/// confirmation of the changed source files, then the backend's settle hook over every
/// changed source file.
pub(crate) fn apply_changes(
    sink: &mut dyn ChangeSink,
    changes: &[(String, FileChange)],
    changed_sources: &[(String, Option<&str>)],
    removed: &[String],
) -> Result<(), SemanticError> {
    sink.watched(changes)?;
    let mut confirm: Vec<String> = Vec::new();
    let changed: Vec<String> = changed_sources
        .iter()
        .filter(|(_, text)| text.is_some())
        .map(|(uri, _)| uri.clone())
        .collect();
    for (uri, text) in changed_sources {
        if let Some(version) = sink.version(uri) {
            match text {
                Some(text) => sink.change(uri, version + 1, text)?,
                None => sink.close(uri)?,
            }
        }
        if text.is_some() && confirm.len() < MAX_CONFIRMED {
            confirm.push(uri.clone());
        }
    }
    for uri in removed {
        if sink.version(uri).is_some() {
            sink.close(uri)?;
        }
    }
    sink.confirm(&confirm)?;
    sink.settle(&changed)
}

/// Requests of one analysis distributed round-robin over several processes of the same
/// workspace (request sharding); answers come back in request order.
pub(crate) struct Sharded<'s> {
    parts: Vec<&'s mut (dyn Session + Send)>,
}

impl<'s> Sharded<'s> {
    /// `parts` must not be empty.
    pub(crate) fn new(parts: Vec<&'s mut (dyn Session + Send)>) -> Result<Self, SemanticError> {
        if parts.is_empty() {
            return Err(SemanticError::Worker("request sharding without processes".into()));
        }
        Ok(Sharded { parts })
    }
}

impl Session for Sharded<'_> {
    fn capabilities(&self) -> &Value {
        self.parts[0].capabilities()
    }

    /// Every process of the shard reports the notifications of the documents it analysed.
    fn notifications_named(&self, method: &str) -> Vec<Value> {
        self.parts
            .iter()
            .flat_map(|p| p.notifications_named(method))
            .collect()
    }

    fn request_many(
        &mut self,
        calls: Vec<(String, Value)>,
    ) -> Result<Vec<Result<Value, SemanticError>>, SemanticError> {
        let n = self.parts.len();
        if n == 1 {
            return self.parts[0].request_many(calls);
        }
        let total = calls.len();
        let mut buckets: Vec<Vec<(usize, (String, Value))>> = (0..n).map(|_| Vec::new()).collect();
        for (i, call) in calls.into_iter().enumerate() {
            buckets[i % n].push((i, call));
        }
        type Part = Result<(Vec<usize>, Vec<Result<Value, SemanticError>>), SemanticError>;
        let parts: Vec<Part> = thread::scope(|scope| {
            let handles: Vec<_> = self
                .parts
                .iter_mut()
                .zip(buckets)
                .map(|(part, bucket)| {
                    scope.spawn(move || -> Part {
                        let (indices, calls): (Vec<usize>, Vec<(String, Value)>) = bucket.into_iter().unzip();
                        if calls.is_empty() {
                            return Ok((indices, Vec::new()));
                        }
                        part.request_many(calls).map(|results| (indices, results))
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join().unwrap_or_else(|_| {
                        Err(SemanticError::Worker("request shard thread panicked".into()))
                    })
                })
                .collect()
        });
        let mut out: Vec<Option<Result<Value, SemanticError>>> = (0..total).map(|_| None).collect();
        for part in parts {
            let (indices, results) = part?;
            for (i, result) in indices.into_iter().zip(results) {
                out[i] = Some(result);
            }
        }
        Ok(out
            .into_iter()
            .map(|r| r.unwrap_or_else(|| Err(SemanticError::Protocol("missing response".into()))))
            .collect())
    }
}

/// A workspace plus a pool of language-server processes.
pub(crate) struct LspSession {
    profile: Profile,
    launcher: Box<dyn Launcher>,
    tools: ToolEnv,
    /// Declared before `snapshot`: processes are stopped before the workspace lock is released.
    workers: Vec<Worker>,
    snapshot: Snapshot,
    /// Queried file -> process that has it open (file sharding).
    assigned: HashMap<String, usize>,
    /// Workspace files written since their originals were last verified.
    unverified: BTreeSet<String>,
    /// Notes of `Launcher::loaded`, returned with the next update.
    notes: Vec<Diagnostic>,
}

impl LspSession {
    /// Open the backend's workspace for `request`'s partition; processes start on the first
    /// update.
    pub fn open(
        profile: Profile,
        launcher: Box<dyn Launcher>,
        request: &SemanticRequest<'_>,
    ) -> Result<LspSession, SemanticError> {
        let files = partition(request, &profile.languages);
        let copies = snapshot_files(request, &files, profile.config_names);
        let options = WorkspaceOptions {
            workspaces_dir: &request.repo.workspaces_dir,
            backend: &profile.id,
            target_root: &request.repo.root,
            mode: profile.workspace,
            stamp: &profile.fingerprint,
            rules: profile.rules.clone(),
            language: profile.language(),
            server_name: profile.server_name,
        };
        let snapshot = Snapshot::open(&options, &copies)?;
        launcher.prepare(&snapshot, &files)?;
        let unverified = snapshot.paths().map(str::to_string).collect();
        Ok(LspSession {
            profile,
            launcher,
            tools: request.tools.clone(),
            workers: Vec::new(),
            snapshot,
            assigned: HashMap::new(),
            unverified,
            notes: Vec::new(),
        })
    }

    /// Sync the workspace with `request` and analyze `request.query` (no reuse).
    pub fn update(&mut self, request: &SemanticRequest<'_>) -> Result<BackendOutput, SemanticError> {
        Ok(self.update_reusing(request, &HashMap::new(), None)?.output)
    }

    /// Sync the workspace (watcher `hints` when given), apply the changes to the running
    /// processes (restart on a build-file edit), analyze `request.query` with the answers of
    /// unchanged units in `reuse`.
    pub fn update_reusing(
        &mut self,
        request: &SemanticRequest<'_>,
        reuse: &HashMap<String, FileReuse>,
        hints: Option<&[String]>,
    ) -> Result<SessionUpdate, SemanticError> {
        let started = Instant::now();
        let files = partition(request, &self.profile.languages);
        let delta = self.sync(request, &files, hints)?;

        let mut queried: Vec<&SemanticFile<'_>> = files
            .iter()
            .copied()
            .filter(|f| request.query.contains(f.path))
            .collect();
        queried.sort_by(|a, b| a.path.cmp(b.path));
        let requests_before = self.requests();
        let mut analysis = Analysis::default();
        if !queried.is_empty() {
            let deadline = started + self.tools.session_deadline;
            let (wanted, heap) = self.pool_plan(&queried, &files);
            self.spawn(wanted, heap)?;
            if !delta.is_empty() {
                // Readiness servers (rust-analyzer, jdtls, progress servers) settle after
                // the changes; a no-op for servers without a signal.
                for worker in &mut self.workers {
                    worker.client.set_deadline(deadline);
                    worker.client.settle_after_changes()?;
                }
            }
            let decls = DeclTable::new(files.iter().map(|f| (f.path, f.source, f.facts)));
            let fingerprint = self.profile.fingerprint.clone();
            let opts = Options {
                provider: self.profile.provider.clone(),
                tool_fingerprint: &fingerprint,
                python: self.profile.python,
                syntax_answers: crate::engine::syntax_answers_enabled(),
                hooks: crate::languages::server_for(&self.profile.id),
                prepared: request.prepared,
                calls_by_definition: crate::engine::calls_by_definition(&self.profile.id),
                reuse: (!reuse.is_empty()).then_some(reuse),
            };
            analysis = match self.profile.shard {
                ShardMode::Requests => self.analyze_sharded_requests(&queried, &decls, &opts, deadline)?,
                ShardMode::Files => self.analyze_sharded_files(&queried, &decls, &opts, deadline)?,
            };
            if self.profile.python {
                crate::stubs::apply_rules(&mut analysis.files, &decls, &analysis.incomplete);
            }
        }
        let verify: BTreeSet<&str> = self
            .unverified
            .iter()
            .map(String::as_str)
            .chain(queried.iter().map(|f| f.path))
            .collect();
        self.snapshot.verify_paths(verify)?;
        self.unverified.clear();

        let requests = self.requests().saturating_sub(requests_before);
        let mut run = run_summary(
            &self.profile.id,
            &self.profile.languages,
            files.len(),
            queried.len(),
            requests,
            started,
            self.profile.tool_version.clone(),
        );
        run.ready = crate::lsp::combine_ready(self.workers.iter().map(|w| w.client.ready()));
        let answers: HashMap<String, FileAnswers> = analysis.answers;
        Ok(SessionUpdate {
            output: BackendOutput {
                files: analysis.files,
                run,
            },
            answers,
            units: analysis.units,
            diagnostics: std::mem::take(&mut self.notes),
        })
    }

    /// File sharding: each process analyzes its shard, concurrently.
    fn analyze_sharded_files<'a>(
        &mut self,
        queried: &[&SemanticFile<'a>],
        decls: &DeclTable<'a>,
        opts: &Options<'_>,
        deadline: Instant,
    ) -> Result<Analysis, SemanticError> {
        let paths: Vec<&str> = queried.iter().map(|f| f.path).collect();
        let weights: Vec<u64> = queried.iter().map(|f| weight(f.facts)).collect();
        let shards = assign(&paths, &weights, self.workers.len(), &self.assigned);
        for (k, shard) in shards.iter().enumerate() {
            for &i in shard {
                self.assigned.insert(paths[i].to_string(), k);
            }
        }
        let snapshot = &self.snapshot;
        let launcher: &dyn Launcher = self.launcher.as_ref();
        let workers = &mut self.workers;
        let results: Vec<Result<Analysis, SemanticError>> = thread::scope(|scope| {
            let handles: Vec<_> = workers
                .iter_mut()
                .zip(&shards)
                .filter(|(_, shard)| !shard.is_empty())
                .map(|(worker, shard)| {
                    let shard_files: Vec<&SemanticFile<'_>> = shard.iter().map(|&i| queried[i]).collect();
                    scope.spawn(move || {
                        worker.analyze(launcher, &shard_files, decls, snapshot, opts, deadline)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err(SemanticError::Worker("analyzer thread panicked".into())))
                })
                .collect()
        });
        let mut analysis = Analysis::default();
        for result in results {
            analysis.merge(result?);
        }
        Ok(analysis)
    }

    /// Request sharding: every process opens every queried document; one analysis spreads
    /// its requests over all processes.
    fn analyze_sharded_requests<'a>(
        &mut self,
        queried: &[&SemanticFile<'a>],
        decls: &DeclTable<'a>,
        opts: &Options<'_>,
        deadline: Instant,
    ) -> Result<Analysis, SemanticError> {
        let snapshot = &self.snapshot;
        let launcher: &dyn Launcher = self.launcher.as_ref();
        for worker in &mut self.workers {
            worker.client.set_deadline(deadline);
            worker.open_all(queried, snapshot)?;
            worker.warm(launcher, queried, snapshot)?;
        }
        let parts: Vec<&mut (dyn Session + Send)> = self
            .workers
            .iter_mut()
            .map(|w| &mut w.client as &mut (dyn Session + Send))
            .collect();
        let mut sharded = Sharded::new(parts)?;
        engine::analyze(&mut sharded, queried, decls, snapshot, opts)
    }

    /// Sync the workspace, restart on a build-file edit, notify running processes.
    fn sync(
        &mut self,
        request: &SemanticRequest<'_>,
        files: &[&SemanticFile<'_>],
        hints: Option<&[String]>,
    ) -> Result<SyncDelta, SemanticError> {
        let copies = snapshot_files(request, files, self.profile.config_names);
        let delta = self.snapshot.sync_hinted(&copies, hints)?;
        for path in &delta.removed {
            self.assigned.remove(path);
            self.unverified.remove(path);
        }
        self.unverified.extend(
            delta
                .added
                .iter()
                .chain(&delta.changed)
                .filter(|p| self.snapshot.contains(p))
                .cloned(),
        );
        if delta.is_empty() {
            return Ok(delta);
        }
        let build_edit = delta
            .paths()
            .any(|p| crate::session::is_build_file_for(&self.profile.languages, p));
        if build_edit && !self.workers.is_empty() {
            // The project model changed: this backend re-imports (fresh processes wait for
            // the import-finished signal); generated files and approved build steps rerun.
            self.stop_workers();
            self.launcher.prepare(&self.snapshot, files)?;
            return Ok(delta);
        }
        self.notify_changes(&delta, files, request.prepared)?;
        Ok(delta)
    }

    /// Processes to use for the queried files and the Node heap cap (module docs).
    fn pool_plan(&self, queried: &[&SemanticFile<'_>], files: &[&SemanticFile<'_>]) -> (usize, Option<u64>) {
        if self.profile.shard == ShardMode::Requests {
            let work: u64 = queried.iter().map(|f| weight(f.facts)).sum();
            let auto_shards = trace_core::config::current().auto().request_shards;
            return (
                request_shards(self.profile.max_processes, auto_shards, self.tools.pool_memory_mb, work),
                None,
            );
        }
        if !self.profile.python {
            return (desired_processes(queried.len(), self.profile.max_processes), None);
        }
        let callables = callable_count(files);
        let budget = self.tools.pool_memory_mb;
        (
            pool_size(queried.len(), self.profile.max_processes, files.len(), callables, budget),
            heap_cap_mb(files.len(), callables, budget),
        )
    }

    /// Warm live find-references (SPEC section 8.9): sync the workspace with `request`, then
    /// one `textDocument/references` at `query` on the first process. `Ok(None)` when the
    /// queried file is not in this partition, is not valid UTF-8, or the server has no
    /// references provider.
    pub fn references(
        &mut self,
        request: &SemanticRequest<'_>,
        query: &ReferenceQuery,
    ) -> Result<Option<LiveReferences>, SemanticError> {
        let started = Instant::now();
        let files = partition(request, &self.profile.languages);
        let Some(file) = files.iter().copied().find(|f| f.path == query.path) else {
            return Ok(None);
        };
        let Some(text) = lsp_text(file.source) else {
            return Ok(None);
        };
        let delta = self.sync(request, &files, None)?;
        let fresh = self.workers.is_empty();
        let heap = if self.profile.python {
            heap_cap_mb(files.len(), callable_count(&files), self.tools.pool_memory_mb)
        } else {
            None
        };
        self.spawn(1, heap)?;
        let deadline = started + self.tools.session_deadline;
        let uri = self.snapshot.uri_of(file.path)?;
        let keep_open = self.profile.keep_documents_open;
        let settle = !delta.is_empty();
        let sources: HashMap<&str, &[u8]> = files.iter().map(|f| (f.path, f.source)).collect();
        let snapshot = &self.snapshot;
        let launcher: &dyn Launcher = self.launcher.as_ref();
        let worker = &mut self.workers[0];
        worker.client.set_deadline(deadline);
        if !capability_enabled(worker.client.capabilities(), "referencesProvider") {
            return Ok(None);
        }
        if settle {
            worker.client.settle_after_changes()?;
        }
        let opened_now = !worker.opened.contains_key(file.path);
        if opened_now {
            let text = worker.hooks.server_text(file.language, text);
            worker
                .client
                .open(&uri, trace_core::languages::info(file.language).lsp_id, &text)?;
            worker.opened.insert(file.path.to_string(), 1);
        }
        worker.warm(launcher, &files, snapshot)?;
        let mut found = crate::references::query_references(
            &mut worker.client,
            snapshot,
            &sources,
            query,
            &self.profile.id,
        );
        // A process that just started may answer before it tracks the workspace files
        // (Pyright: the declaration only). Repeat until two consecutive answers agree.
        if fresh {
            for _ in 0..REFERENCE_REPEATS {
                let Ok(Some(previous)) = &found else { break };
                let previous = previous.clone();
                std::thread::sleep(REFERENCE_REPEAT_PAUSE);
                found = crate::references::query_references(
                    &mut worker.client,
                    snapshot,
                    &sources,
                    query,
                    &self.profile.id,
                );
                if matches!(&found, Ok(Some(now)) if *now == previous) {
                    break;
                }
            }
        }
        if opened_now && !keep_open && worker.opened.remove(file.path).is_some() {
            worker.client.close_document(&uri)?;
        }
        let found = found?;
        let verify: BTreeSet<&str> = self
            .unverified
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(file.path))
            .collect();
        self.snapshot.verify_paths(verify)?;
        self.unverified.clear();
        Ok(found)
    }

    /// Graceful shutdown of every process (in parallel, bounded); the workspace stays.
    pub fn close(mut self) {
        self.stop_workers();
    }

    fn stop_workers(&mut self) {
        let workers = std::mem::take(&mut self.workers);
        self.assigned.clear();
        thread::scope(|scope| {
            for worker in workers {
                scope.spawn(move || {
                    let _ = worker.client.shutdown();
                });
            }
        });
    }

    /// Requests sent by all processes so far.
    fn requests(&self) -> u64 {
        self.workers.iter().map(|w| w.client.metrics().requests).sum()
    }

    /// Start processes (in parallel) until the pool has `wanted`; each is validated by the
    /// launcher after readiness.
    fn spawn(&mut self, wanted: usize, heap_cap_mb: Option<u64>) -> Result<(), SemanticError> {
        let first = self.workers.len();
        if first >= wanted {
            return Ok(());
        }
        let deadline = Instant::now() + self.tools.session_deadline;
        let commands = (first..wanted)
            .map(|i| {
                self.launcher
                    .command(&self.snapshot, i, &self.tools, deadline, heap_cap_mb)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let dir: &Path = &self.snapshot.dir;
        let started: Vec<Result<LspClient, SemanticError>> = thread::scope(|scope| {
            let handles: Vec<_> = commands
                .into_iter()
                .map(|(command, options)| scope.spawn(move || LspClient::start(&command, dir, options)))
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err(SemanticError::Worker("server start thread panicked".into())))
                })
                .collect()
        });
        let mut clients = Vec::with_capacity(started.len());
        for client in started {
            clients.push(client?);
        }
        for client in clients {
            let notes = self.launcher.loaded(&client)?;
            if self.workers.is_empty() {
                self.notes.extend(notes);
            }
            self.workers.push(Worker {
                client,
                opened: HashMap::new(),
                keep_open: self.profile.keep_documents_open,
                warmed: false,
                hooks: crate::languages::server_for(&self.profile.id),
            });
        }
        Ok(())
    }

    /// Tell every process about workspace changes ([`apply_changes`]).
    fn notify_changes(
        &mut self,
        delta: &SyncDelta,
        files: &[&SemanticFile<'_>],
        prepared: &crate::languages::Prepared,
    ) -> Result<(), SemanticError> {
        if self.workers.is_empty() {
            return Ok(());
        }
        let source_of: HashMap<&str, &[u8]> = files.iter().map(|f| (f.path, f.source)).collect();
        let mut changes: Vec<(String, FileChange)> = Vec::new();
        for (paths, kind) in [
            (&delta.added, FileChange::Created),
            (&delta.changed, FileChange::Changed),
            (&delta.removed, FileChange::Deleted),
        ] {
            for path in paths {
                changes.push((self.snapshot.uri_of(path)?, kind));
            }
        }
        // The text the server gets (`Server::server_text`, same length as the file).
        let language_of: HashMap<&str, trace_core::Language> =
            files.iter().map(|f| (f.path, f.language)).collect();
        let hooks = crate::languages::server_for(&self.profile.id);
        let mut server_sources: Vec<(String, Option<std::borrow::Cow<'_, str>>)> = Vec::new();
        for path in delta.changed.iter().chain(&delta.added) {
            let Some(source) = source_of.get(path.as_str()).copied() else { continue };
            let text = match (lsp_text(source), language_of.get(path.as_str())) {
                (Some(text), Some(&language)) => Some(hooks.server_text(language, text)),
                (text, _) => text.map(std::borrow::Cow::Borrowed),
            };
            server_sources.push((self.snapshot.uri_of(path)?, text));
        }
        let changed_sources: Vec<(String, Option<&str>)> = server_sources
            .iter()
            .map(|(uri, text)| (uri.clone(), text.as_deref()))
            .collect();
        let removed: Vec<String> = delta
            .removed
            .iter()
            .map(|p| self.snapshot.uri_of(p))
            .collect::<Result<_, _>>()?;
        for worker in &mut self.workers {
            let mut sink = ServerSink {
                client: &mut worker.client,
                hooks: worker.hooks,
                prepared,
            };
            apply_changes(&mut sink, &changes, &changed_sources, &removed)?;
            // Keep the pool's view of open documents in step with the client's.
            worker.opened.retain(|path, version| {
                let Ok(uri) = crate::lsp::path_to_uri(&self.snapshot.path_of(path)) else {
                    return false;
                };
                match worker.client.open_version(&uri) {
                    Some(v) => {
                        *version = v;
                        true
                    }
                    None => false,
                }
            });
        }
        Ok(())
    }
}

impl BackendSession for LspSession {
    fn backend_id(&self) -> &str {
        &self.profile.id
    }

    fn fingerprint(&self) -> &str {
        &self.profile.fingerprint
    }

    fn processes(&self) -> usize {
        self.workers.len()
    }

    fn update(&mut self, request: &SemanticRequest<'_>) -> Result<BackendOutput, SemanticError> {
        LspSession::update(self, request)
    }

    fn update_reusing(
        &mut self,
        request: &SemanticRequest<'_>,
        reuse: &HashMap<String, FileReuse>,
        hints: Option<&[String]>,
    ) -> Result<SessionUpdate, SemanticError> {
        LspSession::update_reusing(self, request, reuse, hints)
    }

    fn references(
        &mut self,
        request: &SemanticRequest<'_>,
        query: &ReferenceQuery,
    ) -> Result<Option<LiveReferences>, SemanticError> {
        LspSession::references(self, request, query)
    }

    fn close(self: Box<Self>) {
        LspSession::close(*self);
    }
}

/// One-shot run: open, update once, close.
pub(crate) fn run_once(
    profile: Profile,
    launcher: Box<dyn Launcher>,
    request: &SemanticRequest<'_>,
) -> Result<BackendOutput, SemanticError> {
    let mut session = LspSession::open(profile, launcher, request)?;
    let output = session.update(request);
    session.close();
    output
}

/// One-shot live find-references (SPEC §8.9): one process over the backend's workspace,
/// one `textDocument/references` request at `query`, mapped back to repository bytes.
/// `Ok(None)` = unsupported (file outside the partition, no references provider).
pub(crate) fn references_once(
    profile: Profile,
    launcher: Box<dyn Launcher>,
    request: &SemanticRequest<'_>,
    query: &ReferenceQuery,
) -> Result<Option<LiveReferences>, SemanticError> {
    let mut session = LspSession::open(profile, launcher, request)?;
    let found = session.references(request, query);
    session.close();
    found
}

/// Files of the partition's languages.
pub(crate) fn partition<'r, 'a>(
    request: &'r SemanticRequest<'a>,
    languages: &[Language],
) -> Vec<&'r SemanticFile<'a>> {
    request
        .files
        .iter()
        .filter(|f| languages.contains(&f.language))
        .collect()
}

/// Workspace contents: the partition files plus configuration files matching the entry's
/// `workspace.configs` globs.
pub(crate) fn snapshot_files<'a>(
    request: &SemanticRequest<'a>,
    files: &[&SemanticFile<'a>],
    config_names: &[&str],
) -> Vec<SnapshotFile<'a>> {
    let mut copies: Vec<SnapshotFile<'a>> = files
        .iter()
        .map(|f| SnapshotFile {
            path: f.path,
            hash: f.hash,
            bytes: f.source,
        })
        .collect();
    for &(path, bytes) in request.configs {
        if config_names.iter().any(|p| crate::mirror::glob_match(p, path)) {
            copies.push(SnapshotFile {
                path,
                hash: Hash32::of(bytes),
                bytes,
            });
        }
    }
    copies
}

/// Processes of a request-sharded pool: `auto_shards` (`Auto::request_shards` =
/// `min(workers.max_request_shards, cores / 2)`), at most the entry's `processes` (when set),
/// the memory budget (`budget_mb / memory.request_shard_mb`, 0 = unbounded) and one per
/// `workers.requests_per_shard` estimated requests; at least 1.
pub fn request_shards(entry_processes: usize, auto_shards: usize, budget_mb: u64, work: u64) -> usize {
    let settings = trace_core::config::current();
    let mut n = auto_shards;
    if entry_processes > 0 {
        n = n.min(entry_processes);
    }
    if budget_mb > 0 {
        n = n.min(usize::try_from(budget_mb / settings.memory.request_shard_mb).unwrap_or(usize::MAX));
    }
    let by_work = usize::try_from(work.div_ceil(settings.workers.requests_per_shard)).unwrap_or(usize::MAX);
    n.min(by_work).max(1)
}

/// Estimated resident memory in MB of a Pyright pool of `k` processes (at least 1) over a
/// partition of `files` Python files declaring `callables` functions and methods.
///
/// Model (monotone in every argument): one process needs `280 + files / 20 + 0.069 *
/// callables` MB; a pool of `k` needs `1 + 0.45 (k - 1)` times that (every process loads
/// most of the program its shard imports; 280 = `memory.server_base_mb`, 45 % =
/// `memory.server_extra_percent`). Fitted to the lspbench
/// measurements of Pyright 1.1.414 pools (`artifacts/lspbench/RESULTS.md`):
///
/// | partition (files / callables) | measured k=1 / 2 / 4 (MB) | model k=1 / 2 / 4 (MB) |
/// |---|---|---|
/// | 332 / 3.8k   | 560 / 820 / 1,250    | 558 / 809 / 1,311    |
/// | 674 / 31.8k  | 2,500 / 3,600 / 4,400 | 2,507 / 3,635 / 5,891 |
/// | 2,452 / 55.8k | 4,100 / 6,500 / 9,200 | 4,252 / 6,165 / 9,992 |
///
/// The model errs on the high side for large pools (the budget is a bound, not a target).
pub fn estimate_mb(files: usize, callables: usize, k: usize) -> u64 {
    let memory = &trace_core::config::current().memory;
    let one = memory
        .server_base_mb
        .saturating_add(files as u64 / 20)
        .saturating_add((callables as u64).saturating_mul(69) / 1000);
    let k = k.max(1) as u64;
    one.saturating_mul(100 + memory.server_extra_percent.saturating_mul(k - 1)) / 100
}

/// Pyright pool size: the largest `k <= min(max_processes, ceil(queried / 10))` whose
/// [`estimate_mb`] fits `budget_mb` (0 = unbounded), else 1.
pub fn pool_size(
    queried: usize,
    max_processes: usize,
    files: usize,
    callables: usize,
    budget_mb: u64,
) -> usize {
    let upper = desired_processes(queried, max_processes);
    if budget_mb == 0 {
        return upper;
    }
    (1..=upper)
        .rev()
        .find(|&k| estimate_mb(files, callables, k) <= budget_mb)
        .unwrap_or(1)
}

/// Node heap cap for Pyright processes: the budget, only when even one process is
/// estimated above it (`None` otherwise, and for an unbounded budget).
pub fn heap_cap_mb(files: usize, callables: usize, budget_mb: u64) -> Option<u64> {
    (budget_mb > 0 && estimate_mb(files, callables, 1) > budget_mb).then_some(budget_mb)
}

/// Callable declarations (functions, methods, constructors) of a partition.
pub fn callable_count(files: &[&SemanticFile<'_>]) -> usize {
    files
        .iter()
        .map(|f| f.facts.declarations.iter().filter(|d| d.kind.is_callable()).count())
        .sum()
}

/// Processes worth starting for `queried` files.
pub fn desired_processes(queried: usize, max_processes: usize) -> usize {
    queried
        .div_ceil(trace_core::config::current().workers.min_files_per_process)
        .clamp(1, max_processes.max(1))
}

/// Estimated request count of a file (prepare + outgoing per callable, one definition per
/// call, callback argument and value reference).
pub fn weight(facts: &FileFacts) -> u64 {
    let callables = facts.declarations.iter().filter(|d| d.kind.is_callable()).count() as u64;
    1 + 2 * callables
        + facts.calls.len() as u64
        + facts.callbacks.len() as u64
        + facts.references.len() as u64
}

/// Parent directory of a `/`-separated relative path (`""` at the root).
fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// Deterministic, directory-grouped sharding of `paths` (path order) over `workers`
/// processes. Files in `sticky` stay with their process. The other files are grouped by
/// parent directory (groups in order of first appearance); each group goes whole to the
/// least-loaded process (lowest index on ties); a group heavier than the balanced target
/// load `ceil(total / workers)` is split into contiguous runs of at most the target, each
/// run to the then least-loaded process. Returns indices into `paths` per process (sorted).
pub fn assign(
    paths: &[&str],
    weights: &[u64],
    workers: usize,
    sticky: &HashMap<String, usize>,
) -> Vec<Vec<usize>> {
    let workers = workers.max(1);
    let mut shards: Vec<Vec<usize>> = vec![Vec::new(); workers];
    let mut loads = vec![0u64; workers];
    let total: u64 = weights.iter().sum();
    let target = total.div_ceil(workers as u64).max(1);
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut group_of: HashMap<&str, usize> = HashMap::new();
    for (i, path) in paths.iter().enumerate() {
        match sticky.get(*path) {
            Some(&k) if k < workers => {
                shards[k].push(i);
                loads[k] += weights[i];
            }
            _ => {
                let g = *group_of.entry(parent_dir(path)).or_insert_with(|| {
                    groups.push(Vec::new());
                    groups.len() - 1
                });
                groups[g].push(i);
            }
        }
    }
    let least = |loads: &[u64]| (0..loads.len()).min_by_key(|&k| (loads[k], k)).unwrap_or(0);
    for group in groups {
        let weight: u64 = group.iter().map(|&i| weights[i]).sum();
        if weight <= target {
            let k = least(&loads);
            loads[k] += weight;
            shards[k].extend(group);
            continue;
        }
        let mut run: Vec<usize> = Vec::new();
        let mut run_weight = 0u64;
        for i in group {
            if !run.is_empty() && run_weight + weights[i] > target {
                let k = least(&loads);
                loads[k] += run_weight;
                shards[k].append(&mut run);
                run_weight = 0;
            }
            run.push(i);
            run_weight += weights[i];
        }
        if !run.is_empty() {
            let k = least(&loads);
            loads[k] += run_weight;
            shards[k].append(&mut run);
        }
    }
    for shard in &mut shards {
        shard.sort_unstable();
    }
    shards
}

#[cfg(test)]
#[path = "../tests/unit/pool.rs"]
mod tests;
