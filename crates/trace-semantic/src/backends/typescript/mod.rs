//! TypeScript / JavaScript backend (port of codepath_v3/backends/typescript/adapter.py).
//!
//! Runs the embedded TypeScript worker (`assets/ts-worker/main.mjs` + modules) on trace's own
//! Node runtime (`ToolEnv::runtime_exe("node")`, never the user's):
//! * one-shot: `<node> <assets>/ts-worker/main.mjs <state>/ts-worker.input.json <sdk>` (cwd =
//!   the workspace, clean env, timeout = session deadline, stdout/stderr in the backend's
//!   state dir `ts-worker.stdout.json|stderr.log`);
//! * persistent session ([`TsSession`], `BackendSession`): `<node> main.mjs --serve <sdk>`,
//!   JSON lines on stdin/stdout (`open` with the whole input once, then per update only the
//!   changed / deleted files and `analyze` with the queried files; `references`; `shutdown`).
//!   The compiler process and its program stay warm for the lifetime of `trace index --watch`, so a one-file
//!   edit re-checks and re-visits only what changed.
//!
//! Input (`WorkerInput`): `{"workspace", "repo_root", "files": [{"path", "source"}],
//!   "configs": [{"path", "text"}], "modules": [{"virtual", "real"}],
//!   "bundled_types": [{"virtual", "real"}], "names": [<declared names>], "query": [<paths>]}`
//! (plus `"references": {"file", "start_byte"}` in references mode). The preflight
//! (`languages/typescript.rs`, [`TsSetup`]) supplies the project's configuration texts
//! (tsconfig*/jsconfig*/package.json, read, never executed; TypeScript 7 has no plugins), the
//! installed `node_modules` (mapped read-only at their places in the workspace) and whether
//! the project lacks its own `@types/node` (then trace's pinned `@types/node` from the
//! TypeScript tool is mapped in: it describes the Node runtime like typeshed describes
//! Python's). The worker opens the project's configs (solution-style references followed),
//! analyses files no config lists in a controlled project, and visits only `query`.
//!
//! Output mapping (worker JSON `{symbols, edges, unresolved, uses, diagnostics, metrics}`):
//! * worker symbol `id = "ts:<file>:<start>:<end>"` -> `DeclTable::at_span` with its name
//!   (function expressions / arrows are named from their binding like trace-syntax; unbound
//!   ones, `<anonymous@...>`, map by span onto the syntax `<lambda>`); `ts:<file>:module`
//!   (name `<module>`) -> the file's `<module>` declaration;
//! * owners: the executing owner of the syntax call at the worker's evidence
//!   (`FileFacts::executing_owner`, so module-level and callback code is attributed exactly
//!   like syntax attributes it), else the mapped worker owner, else `unmapped_owner`;
//! * edges `calls|constructor|iterates|creates_generator|property_get` -> `SemEdge`
//!   (resolution `resolved_signature`); the evidence is the syntax call starting at the
//!   worker's call expression (callee span), and calls to `async` targets are refined with
//!   the syntax activation (`awaits` / `creates_coroutine`);
//! * edges whose target is not an indexed declaration -> `external_or_ambiguous`
//!   (diagnostic count `unmapped_target`);
//! * `unresolved_or_external_signature` -> `unresolved_signature`; `external_signature` (the
//!   signature is declared outside the repository: an installed dependency, the bundled Node
//!   types or the compiler's standard library; also `require("x")`) ->
//!   `external_or_ambiguous` without candidates (proven external);
//! * uses `{from, to, kind, evidence}` -> `references` / `writes` / `imports` / `reexports`
//!   edges (the syntax `Reference::kind` at the same span wins; `argument` ->
//!   `passes_callback` for callable targets), owned by the syntax executing owner at the
//!   use; reads also become value references (flow input);
//! * syntax calls of a queried file (module level included) with no worker edge or
//!   unresolved entry covering them -> `no_semantic_target`.
//!
//! Results are produced for the queried files only. Live find-references
//! ([`Backend::references`]) runs the worker in references mode: every identifier with the
//! target's text whose checker symbol shares a declaration with the target symbol (aliases
//! followed), in every project that contains the queried file.
//! Sources are passed as UTF-8 text WITHOUT stripping a BOM (U+FEFF stays a character, as in
//! adapter.py), so worker byte offsets equal raw file byte offsets. Files that are not valid
//! UTF-8 are left out of the worker input (diagnostic `invalid_utf8`).
//! A worker that exits or fails is `ServerCrashed` (details: its stderr log); one that does
//! not finish within the session deadline is `ServerTimeout`. There is no fallback.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use trace_core::facts::FileFacts;
use trace_core::fingerprint::PartsHasher;
use trace_core::setup_error::SetupError;
use trace_core::Language;

use crate::assets::ts_worker_hash;
use crate::backend::{
    hash_executable_into, hash_file_into, package_version, run_summary, Backend, BackendOutput, SemanticFile,
    SemanticRequest,
};
use crate::mapping::DeclTable;
use crate::references::{LiveReferences, ReferenceQuery};
use crate::session::BackendSession;
use crate::snapshot::{Snapshot, SnapshotFile};
use crate::tools::{clean_env, ToolEnv};
use crate::SemanticError;

mod mapping;
mod protocol;
mod session;

use self::mapping::*;
use self::protocol::*;
use self::session::*;

/// Largest accepted worker output.
const MAX_WORKER_OUTPUT: u64 = 512 * 1024 * 1024;
/// How long `close` waits for a graceful worker exit.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// The preflight's TypeScript inputs (`Prepared.data` of the `typescript` backend).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TsSetup {
    /// Every `node_modules` next to a `package.json` (+ the `--env` one as the root's).
    pub node_modules: Vec<trace_env::NodeModules>,
    /// Project configuration texts (repository-relative path, text): tsconfig*.json,
    /// jsconfig*.json and package.json of the directories holding the analysed files and
    /// their ancestors.
    pub configs: Vec<(String, String)>,
    /// The project has no own `@types/node`: map trace's bundled Node types.
    pub bundle_node_types: bool,
    /// ... and no `undici-types` (the bundled types' dependency) at the root.
    pub bundle_undici_types: bool,
}

impl TsSetup {
    /// The setup the preflight stored in `prepared`, or the default.
    pub fn of(prepared: &crate::languages::Prepared) -> TsSetup {
        prepared
            .data
            .as_ref()
            .and_then(|d| d.downcast_ref::<TsSetup>())
            .cloned()
            .unwrap_or_default()
    }
}

pub struct TypeScript;

/// Trusted launch inputs of the worker.
struct WorkerLaunch {
    node: PathBuf,
    worker: PathBuf,
    sdk: PathBuf,
    version: Option<String>,
}

impl TypeScript {
    fn sdk(tools: &ToolEnv) -> Option<PathBuf> {
        tools.package_dir("typescript")
    }

    /// Node runtime, worker script and compiler SDK (all trusted), or `ServerMissing`.
    fn launch_inputs(tools: &ToolEnv, language: Language) -> Result<WorkerLaunch, SemanticError> {
        let missing = || SemanticError::Setup(SetupError::ServerMissing { language });
        let node = tools.runtime_exe("node").ok_or_else(missing)?;
        let sdk = Self::sdk(tools)
            .filter(|p| p.join("dist/api/sync/api.js").is_file())
            .ok_or_else(missing)?;
        let worker = tools.assets.ts_worker.clone();
        tools.ensure_trusted(&node)?;
        tools.ensure_trusted(&worker)?;
        tools.ensure_trusted(&sdk.join("package.json"))?;
        let version = package_version(&sdk.join("package.json"));
        Ok(WorkerLaunch {
            node,
            worker,
            sdk,
            version,
        })
    }
}

/// The language errors of this partition name (the first JS/TS language present).
fn partition_language(files: &[&SemanticFile<'_>]) -> Language {
    files
        .first()
        .map(|f| match f.language {
            Language::Tsx => Language::TypeScript,
            l => l,
        })
        .unwrap_or(Language::TypeScript)
}

/// The partition files served by the worker.
fn worker_files<'r, 'a>(request: &'r SemanticRequest<'a>) -> Vec<&'r SemanticFile<'a>> {
    request
        .files
        .iter()
        .filter(|f| matches!(f.language, Language::JavaScript | Language::TypeScript | Language::Tsx))
        .collect()
}

/// Worker input for `files` (the whole partition): sources (UTF-8, BOM kept), project
/// configuration, `node_modules` mappings and bundled Node types. Non-UTF-8 files go to
/// `invalid`.
fn worker_input<'a>(
    workspace: &Path,
    request: &SemanticRequest<'_>,
    files: &[&SemanticFile<'a>],
    invalid: &mut HashSet<&'a str>,
) -> WorkerInput<'a> {
    let setup = TsSetup::of(request.prepared);
    let mut inputs: Vec<WorkerFile<'a>> = Vec::with_capacity(files.len());
    for f in files {
        match std::str::from_utf8(f.source) {
            Ok(source) => inputs.push(WorkerFile { path: f.path, source }),
            Err(_) => {
                invalid.insert(f.path);
            }
        }
    }
    // Installed dependencies (`trace-env`): each `node_modules` next to a `package.json` is
    // served read-only to the worker's virtual file system at the same relative place in
    // the workspace, so imports of packages resolve to their declarations.
    let virtual_modules = |dir: &str| {
        if dir.is_empty() {
            workspace.join("node_modules")
        } else {
            workspace.join(dir).join("node_modules")
        }
    };
    let modules: Vec<WorkerModules> = setup
        .node_modules
        .iter()
        .map(|m| WorkerModules {
            virtual_dir: virtual_modules(&m.dir).display().to_string(),
            real_dir: m.path.display().to_string(),
        })
        .collect();
    let mut bundled_types = Vec::new();
    if setup.bundle_node_types {
        if let Some(tool) = request.tools.tool_dir("typescript") {
            let real = tool.join("node_modules");
            let mut packages = vec!["@types/node"];
            if setup.bundle_undici_types {
                packages.push("undici-types");
            }
            for package in packages {
                let dir: PathBuf = package.split('/').collect();
                if real.join(&dir).join("package.json").is_file() {
                    bundled_types.push(WorkerModules {
                        virtual_dir: workspace.join("node_modules").join(&dir).display().to_string(),
                        real_dir: real.join(&dir).display().to_string(),
                    });
                }
            }
        }
    }
    WorkerInput {
        workspace: workspace.display().to_string(),
        repo_root: request.repo.root.display().to_string(),
        files: inputs,
        configs: setup
            .configs
            .iter()
            .map(|(path, text)| WorkerConfig {
                path: path.clone(),
                text: text.clone(),
            })
            .collect(),
        modules,
        bundled_types,
        names: Vec::new(),
        query: None,
        references: None,
    }
}

/// A finished one-shot worker run.
struct WorkerRun<'r, 'a> {
    output: WorkerOutput,
    files: Vec<&'r SemanticFile<'a>>,
    invalid: HashSet<&'a str>,
    snapshot: Snapshot,
    version: Option<String>,
}

impl TypeScript {
    /// Snapshot the partition, run the worker once (analysis mode over the queried files, or
    /// references mode for `query`) and parse its output.
    fn launch<'r, 'a>(
        &self,
        request: &'r SemanticRequest<'a>,
        query: Option<&ReferenceQuery>,
    ) -> Result<WorkerRun<'r, 'a>, SemanticError> {
        let started = Instant::now();
        let deadline = started + request.tools.session_deadline;
        let files = worker_files(request);
        let language = partition_language(&files);
        let launch = Self::launch_inputs(request.tools, language)?;
        let copies: Vec<SnapshotFile<'_>> = files
            .iter()
            .map(|f| SnapshotFile {
                path: f.path,
                hash: f.hash,
                bytes: f.source,
            })
            .collect();
        let snapshot =
            Snapshot::create(&request.repo.workspaces_dir, self.id(), &request.repo.root, &copies)?;
        let mut invalid: HashSet<&'a str> = HashSet::new();
        let mut input = worker_input(&snapshot.dir, request, &files, &mut invalid);
        match query {
            Some(q) => {
                input.references = Some(WorkerQuery {
                    file: q.path.clone(),
                    start_byte: q.byte,
                });
            }
            None => {
                input.names = declared_names(&files);
                input.query = Some(
                    files
                        .iter()
                        .filter(|f| request.query.contains(f.path) && !invalid.contains(f.path))
                        .map(|f| f.path)
                        .collect(),
                );
            }
        }
        let state = snapshot.outside_dir();
        fs::create_dir_all(&state)?;
        let input_path = state.join("ts-worker.input.json");
        fs::write(&input_path, serde_json::to_vec(&input)?)?;
        drop(input);
        let limits = RunLimits {
            deadline,
            language,
            budget: request.tools.session_deadline,
        };
        let output = run_worker(&launch, &input_path, &snapshot, &state, &limits)?;
        Ok(WorkerRun {
            output,
            files,
            invalid,
            snapshot,
            version: launch.version,
        })
    }
}

/// Named (non-synthetic) declaration names of the partition: identifiers the worker reports
/// uses of.
fn declared_names<'a>(files: &[&SemanticFile<'a>]) -> Vec<&'a str> {
    let mut names: Vec<&'a str> = Vec::new();
    for f in files {
        let facts: &'a FileFacts = f.facts;
        for (i, d) in facts.declarations.iter().enumerate() {
            if !facts.is_synthetic(i as u32) && !d.name.starts_with('<') && !d.name.is_empty() {
                names.push(d.name.as_str());
            }
        }
    }
    names.sort_unstable();
    names.dedup();
    names
}

impl Backend for TypeScript {
    fn id(&self) -> &str {
        "typescript"
    }

    fn languages(&self) -> &[Language] {
        &[Language::JavaScript, Language::TypeScript, Language::Tsx]
    }

    fn fingerprint(&self, tools: &ToolEnv, prepared: &crate::languages::Prepared) -> String {
        let mut h = PartsHasher::new();
        h.text(trace_core::TRACE_VERSION)
            .text(&prepared.fingerprint)
            .text(self.id())
            .int(u64::from(crate::engine::ENGINE_VERSION));
        match Self::sdk(tools) {
            Some(sdk) => {
                hash_file_into(&mut h, &sdk.join("package.json"));
                // Native compiler packages (`@typescript/typescript-<platform>-<arch>`) and the
                // bundled Node types.
                if let Some(modules) = sdk.parent() {
                    let mut packages: Vec<PathBuf> = fs::read_dir(modules.join("@typescript"))
                        .map(|entries| {
                            entries
                                .filter_map(Result::ok)
                                .map(|e| e.path().join("package.json"))
                                .collect()
                        })
                        .unwrap_or_default();
                    packages.sort();
                    packages.push(modules.join("@types").join("node").join("package.json"));
                    packages.push(modules.join("undici-types").join("package.json"));
                    for package in packages {
                        h.text(&package.to_string_lossy());
                        hash_file_into(&mut h, &package);
                    }
                }
            }
            None => {
                h.text("no-sdk");
            }
        }
        hash_executable_into(&mut h, tools.runtime_exe("node").as_deref());
        h.text(&ts_worker_hash());
        h.finish().hex_prefix(32)
    }

    fn run(&self, request: &SemanticRequest<'_>) -> Result<BackendOutput, SemanticError> {
        let started = Instant::now();
        let run = self.launch(request, None)?;
        let decls = DeclTable::new(run.files.iter().map(|f| (f.path, f.source, f.facts)));
        let fingerprint = self.fingerprint(request.tools, request.prepared);
        let queried: Vec<&SemanticFile<'_>> = run
            .files
            .iter()
            .copied()
            .filter(|f| request.query.contains(f.path))
            .collect();
        let results = map_worker_output(&run.output, &queried, &decls, &run.invalid, &fingerprint);
        run.snapshot.verify_originals()?;
        Ok(BackendOutput {
            files: results,
            run: run_summary(
                self.id(),
                self.languages(),
                run.files.len(),
                queried.len(),
                1,
                started,
                run.version,
            ),
        })
    }

    fn open_session(
        &self,
        request: &SemanticRequest<'_>,
    ) -> Result<Option<Box<dyn BackendSession>>, SemanticError> {
        Ok(Some(Box::new(TsSession::open(self, request)?)))
    }

    /// One-shot references mode of the worker (checker symbol identity).
    fn references(
        &self,
        request: &SemanticRequest<'_>,
        query: &ReferenceQuery,
    ) -> Result<Option<LiveReferences>, SemanticError> {
        let known = request
            .files
            .iter()
            .any(|f| f.path == query.path && self.languages().contains(&f.language));
        if !known {
            return Ok(None);
        }
        let run = self.launch(request, Some(query))?;
        let found = map_worker_references(&run.output, &run.files, query, self.id());
        run.snapshot.verify_originals()?;
        Ok(Some(found))
    }
}

/// The worker's `{"ok":false,"error":...}` as a worker error (first line of the message).
fn worker_failure(value: &serde_json::Value) -> SemanticError {
    let error = value
        .get("error")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown error");
    let first = error.lines().next().unwrap_or(error);
    SemanticError::Worker(format!("the TypeScript worker failed: {first}"))
}

/// Minutes of a deadline for `ServerTimeout` (at least 1).
fn deadline_minutes(deadline: Duration) -> u32 {
    u32::try_from(deadline.as_secs().div_ceil(60))
        .unwrap_or(u32::MAX)
        .max(1)
}

/// Deadline and error attribution of a one-shot worker run.
struct RunLimits {
    deadline: Instant,
    language: Language,
    /// The whole budget (for the `ServerTimeout` minutes).
    budget: Duration,
}

/// Run the worker to completion within the deadline and parse its stdout. A failing exit is
/// `ServerCrashed`, a deadline `ServerTimeout` (details: the stderr log in the state dir).
fn run_worker(
    launch: &WorkerLaunch,
    input: &Path,
    snapshot: &Snapshot,
    state: &Path,
    limits: &RunLimits,
) -> Result<WorkerOutput, SemanticError> {
    let stdout_path = state.join("ts-worker.stdout.json");
    let stderr_path = state.join("ts-worker.stderr.log");
    let mut command = Command::new(&launch.node);
    command
        .arg(&launch.worker)
        .arg(input)
        .arg(&launch.sdk)
        .current_dir(&snapshot.dir)
        .env_clear()
        .envs(clean_env(&[], &[]))
        .stdin(Stdio::null())
        .stdout(Stdio::from(File::create(&stdout_path)?))
        .stderr(Stdio::from(File::create(&stderr_path)?));
    crate::procs::isolate(&mut command);
    let mut child = command.spawn().map_err(|source| SemanticError::Launch {
        program: launch.node.display().to_string(),
        source,
    })?;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            crate::procs::stop_leftovers(&mut child);
            break status;
        }
        if Instant::now() >= limits.deadline {
            crate::procs::stop_tree(&mut child, Duration::ZERO);
            return Err(SemanticError::Setup(SetupError::ServerTimeout {
                language: limits.language,
                minutes: deadline_minutes(limits.budget),
                log: stderr_path,
            }));
        }
        thread::sleep(Duration::from_millis(25));
    };
    if !status.success() {
        return Err(SemanticError::Setup(SetupError::ServerCrashed {
            language: limits.language,
            log: stderr_path,
        }));
    }
    let mut bytes = Vec::new();
    File::open(&stdout_path)?
        .take(MAX_WORKER_OUTPUT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_WORKER_OUTPUT {
        return Err(SemanticError::Worker("TypeScript worker output too large".into()));
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| SemanticError::Worker(format!("malformed TypeScript worker output: {e}")))
}

#[cfg(test)]
#[path = "../../../tests/unit/backends/typescript/mod.rs"]
mod tests;
