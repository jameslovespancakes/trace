//! Pyright backend (port of codepath_v3/backends/python/pyright.py).
//!
//! Launch (per pool process `n`): `<trace's node runtime> --require <assets>/lsp_guard.cjs
//!          <tools>/pyright/<v>/node_modules/pyright/langserver.index.js --stdio`, cwd = snapshot,
//! env = clean_env + `PYRIGHT_TMPDIR=<snapshot>/.tmp/p<n>`, `CODEPATH_LSP_WRITE_ROOT=<snapshot>`,
//! stderr to `<snapshot>/lsp-<n>.stderr.log`. Node is always the trace-managed runtime
//! (`ToolEnv::runtime_exe("node")`), never the user's.
//!
//! Snapshot: all Python files (`.py`, `.pyi`) of the partition, plus a controlled
//! `pyrightconfig.json` built from the preflight ([`PyrightSetup`], `languages/python.rs`):
//! `{"include": [<files>], "exclude": [".tmp"], "pythonVersion": <env | .python-version |
//!   requires-python | "3.11">, "typeCheckingMode": "off", "useLibraryCodeForTypes": true,
//!   "extraPaths": <project extraPaths | ["src"]> + <site-packages given with --env, PEP 582
//!   __pypackages__, the base interpreter's site-packages when include-system-site-packages>,
//!   "venvPath"/"venv": <the virtual / conda environment>}` plus the project's safe keys
//! (`stubPath`, `executionEnvironments`, `defineConstant`, `pythonPlatform`) from its
//! `pyrightconfig.json` or `[tool.pyright]`.
//! Settings (`workspace/configuration`, `didChangeConfiguration`):
//! `{"python": {"analysis": {"autoImportCompletions": false, "autoSearchPaths": false,
//!   "diagnosticMode": "openFilesOnly", "typeCheckingMode": "off",
//!   "useLibraryCodeForTypes": true, "extraPaths": [<the same paths, absolute>]}}}`.
//! No interpreter is configured, so Pyright never runs Python (it reads `site-packages`
//! from the directory); the guard refuses child processes anyway.
//!
//! Process pool (`pool.rs`): the queried files are sharded (grouped by directory) over up to
//! [`crate::tools::ToolEnv::processes`] Pyright processes (default `min(cores / 2, 4)`, at
//! least 10 files per process, and only as many as the estimated pool memory allows under
//! `memory.budget_mb`: [`crate::pool::pool_size`]) that share the snapshot; each process
//! opens only its shard (other modules are read from the snapshot on demand, with identical
//! contents) and results are merged deterministically (disjoint per-file results, stub rule
//! applied once). When even one process is estimated above the budget, Node runs with
//! `--max-old-space-size=<budget>` ([`crate::pool::heap_cap_mb`]).
//! [`Backend::open_session`] keeps the pool alive for `index --watch`.
//!
//! Algorithm per shard (`engine`):
//! 1. Declaration positions come from syntax (`name_span`); `documentSymbol` (kinds
//!    5/6/7/9/12) is only requested for files with syntax errors, to report declarations the
//!    syntax tree lost (`unmapped_declaration`).
//! 2. Constructor bridge: class -> its own `__init__` (edge `constructor`,
//!    resolution `constructor_declaration`).
//! 3. For each named callable: `prepareCallHierarchy` at its name; exactly one item that
//!    maps back to the same declaration is required (else diagnostic `prepare_not_unique`);
//!    `callHierarchy/outgoingCalls`; each `fromRanges` start is attributed to its executing
//!    owner (syntax `CallSite.owner` of the smallest call whose callee span contains the
//!    point): the declaration itself or a nested declaration (nested function, synthetic
//!    `<lambda>` / `<genexpr>` scope); points in lazy scopes without a declaration become a
//!    value reference at the callee when the target is unique (`lazy_scope_call`); others
//!    count as `different_execution_scope`. Group by (owner, point): exactly one mapped target
//!    -> edge; otherwise `external_or_ambiguous` unresolved with candidate locations
//!    (resolved later by [`crate::stubs`]).
//!    Edge kind: point inside a syntax call callee -> `calls`, else target kind 7 ->
//!    `property_get`, else `references`; if the target's execution model is not ordinary and
//!    the point is a call: activation `awaits`/`iterates` from syntax, else
//!    `creates_coroutine` / `creates_generator`.
//! 4. Callbacks: for each syntax `CallbackArg` owned by a callable whose name equals some
//!    indexed function name: `textDocument/definition` at the argument identifier; keep targets
//!    that are functions with that exact name; collapse stub pairs; exactly one -> edge
//!    `passes_callback` (resolution `definition`).
//! 5. Blind sites: every syntax call owned by a declaration with no outgoing-call point
//!    inside its callee span -> unresolved `no_semantic_target`.
//! 6. Value references (all queried files, any owner): syntax `Reference`s whose name is an
//!    indexed function/class name -> `definition`; targets with the same name; collapse stub
//!    pairs; exactly one -> `SemValueRef`.
//! 7. Python stub rule ([`crate::stubs`]) over the merged results.
//!
//! Requests of steps 1, 3 (prepare), 4 and 6 form one pipelined batch; outgoing calls a
//! second. Finally the originals of the analyzed files are re-verified.
//!
//! Since engine version 4 (SPEC section 8.5 "Complete references"): module-level and
//! class-body calls (owner `<module>`) and calls in lambdas outside any named function get
//! `definition` at their member identifier in batch A (edges owned by `<module>` / the
//! lambda); every non-local syntax use of a declared name becomes a `references` /
//! `writes` / `imports` / `reexports` edge owned by its executing owner (step 6 keeps the
//! value reference for reads); calls of declarations whose prepare did not map back get
//! `definition` in a third batch. Live find-references ([`Backend::references`]) sends one
//! `textDocument/references` through a one-process session.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value};
use trace_core::fingerprint::PartsHasher;
use trace_core::model::Provider;
use trace_core::Language;

use trace_core::setup_error::SetupError;

use crate::assets::{asset_hash, LSP_GUARD};
use crate::backend::{
    hash_executable_into, hash_file_into, package_version, Backend, BackendOutput, SemanticFile,
    SemanticRequest,
};
use crate::engine::ENGINE_VERSION;
use crate::lsp::{ClientOptions, ServerCommand};
use crate::pool::{self, Launcher, LspSession, Profile};
use crate::session::BackendSession;
use crate::snapshot::Snapshot;
use crate::tools::{clean_env, ToolEnv};
use crate::SemanticError;

/// Documented pyright `documentSymbol` kinds that become declarations (class, method,
/// property, constructor, function).
pub const SYMBOL_KINDS: [u64; 5] = [5, 6, 7, 9, 12];

/// Python version assumed when no file names one (no interpreter is probed).
pub const PYTHON_VERSION: &str = "3.11";

/// Project configuration keys that only steer import resolution and evaluation; they are
/// copied from the project's `pyrightconfig.json` / `[tool.pyright]` (every other key, e.g.
/// `venvPath`, `pythonPath`, `include`, is trace's).
pub const SAFE_PROJECT_KEYS: &[&str] =
    &["stubPath", "executionEnvironments", "defineConstant", "pythonPlatform"];

/// Workspace name Pyright sees for an analysed Python file (rule: an extensionless Python
/// file is analysed as a module). Pyright's module machinery (enumeration, module names,
/// package detection) keys Python sources by the `.py` / `.pyi` extension; a Python file
/// recognised by its shebang (`bin/tool`, `benchsuite/benchsuite`) is handled as a loose
/// document whose call-hierarchy answers varied between identical runs. In the workspace it
/// is written as `<path>.py` (same bytes, so every position is 1:1); answers are mapped back
/// to the original path by the workspace. `None`: the file keeps its name.
pub fn server_name(rel: &str) -> Option<String> {
    let name = rel.rsplit('/').next().filter(|n| !n.is_empty())?;
    match trace_core::languages::from_path(Path::new(name)) {
        Some(Language::Python) => None,
        _ => Some(format!("{rel}.py")),
    }
}

/// The relative path a file has inside the workspace the server sees (its
/// [`server_name`] when the workspace applies it, else `rel`).
fn workspace_rel(snapshot: &Snapshot, rel: &str) -> String {
    crate::mapping::relative_to(&snapshot.dir, &snapshot.path_of(rel)).unwrap_or_else(|| rel.to_string())
}

/// The preflight's Pyright inputs (`Prepared.data` of the `pyright` backend).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PyrightSetup {
    /// A virtual or conda environment (`venvPath` / `venv`).
    pub venv: Option<trace_env::PythonEnv>,
    /// `major.minor`.
    pub python_version: Option<String>,
    /// Relative (to the project root) or absolute search paths, in order.
    pub extra_paths: Vec<String>,
    /// The project's safe keys ([`SAFE_PROJECT_KEYS`]).
    pub project: serde_json::Map<String, Value>,
}

impl PyrightSetup {
    /// The setup the preflight stored in `prepared`, or the default (no environment).
    pub fn of(prepared: &crate::languages::Prepared) -> PyrightSetup {
        prepared
            .data
            .as_ref()
            .and_then(|d| d.downcast_ref::<PyrightSetup>())
            .cloned()
            .unwrap_or_default()
    }

    fn extra_paths_or_default(&self) -> Vec<String> {
        if self.extra_paths.is_empty() {
            vec!["src".to_string()]
        } else {
            self.extra_paths.clone()
        }
    }
}

pub struct Pyright;

impl Pyright {
    fn package(tools: &ToolEnv) -> Option<PathBuf> {
        tools.package_dir("pyright")
    }

    fn server_script(tools: &ToolEnv) -> Option<PathBuf> {
        Self::package(tools).map(|p| p.join("langserver.index.js"))
    }

    /// Trusted launch inputs and the session profile.
    fn parts(
        &self,
        tools: &ToolEnv,
        prepared: &crate::languages::Prepared,
    ) -> Result<(Profile, Box<dyn Launcher>), SemanticError> {
        let missing = || {
            SemanticError::Setup(SetupError::ServerMissing {
                language: Language::Python,
            })
        };
        let node = tools.runtime_exe("node").ok_or_else(missing)?;
        let server = Self::server_script(tools)
            .filter(|p| p.is_file())
            .ok_or_else(missing)?;
        let guard = tools.assets.lsp_guard.clone();
        for path in [&node, &server, &guard] {
            tools.ensure_trusted(path)?;
        }
        let profile = Profile {
            id: self.id().to_string(),
            languages: self.languages().to_vec(),
            provider: Provider::Pyright,
            fingerprint: self.fingerprint(tools, prepared),
            tool_version: Self::package(tools).and_then(|p| package_version(&p.join("package.json"))),
            python: true,
            max_processes: tools.processes.max(1),
            config_names: &[],
            keep_documents_open: false,
            shard: crate::registry::ShardMode::Files,
            workspace: prepared.workspace,
            rules: crate::mirror::MirrorRules::default(),
            server_name: Some(server_name),
        };
        let setup = PyrightSetup::of(prepared);
        Ok((
            profile,
            Box::new(PyrightLauncher {
                node,
                server,
                guard,
                setup,
            }),
        ))
    }
}

/// The controlled `pyrightconfig.json` written into the snapshot (see the module docs):
/// `venvPath` / `venv` let Pyright read the environment's `site-packages` (nothing is
/// executed: Pyright takes the search paths from the directory, the guard refuses any
/// interpreter probe), `pythonVersion` follows the preflight.
pub fn pyright_config(include: &[&str], setup: &PyrightSetup) -> Value {
    let mut config = json!({
        "include": include,
        "exclude": [".tmp"],
        "pythonVersion": setup.python_version.as_deref().unwrap_or(PYTHON_VERSION),
        "typeCheckingMode": "off",
        "useLibraryCodeForTypes": true,
        "extraPaths": setup.extra_paths_or_default()
    });
    for key in SAFE_PROJECT_KEYS {
        if let Some(v) = setup.project.get(*key) {
            config[*key] = v.clone();
        }
    }
    if let Some(v) = &setup.venv {
        if let (Some(parent), Some(name)) = (v.root.parent(), v.root.file_name()) {
            config["venvPath"] = json!(parent.display().to_string());
            config["venv"] = json!(name.to_string_lossy());
        }
    }
    config
}

/// Settings answered to `workspace/configuration` and sent with `didChangeConfiguration`
/// (the config file wins; the extra paths are the same, made absolute).
pub fn settings(snapshot: &Path, setup: &PyrightSetup) -> Value {
    let extra: Vec<String> = setup
        .extra_paths_or_default()
        .iter()
        .map(|p| {
            let path = Path::new(p);
            if path.is_absolute() {
                p.clone()
            } else {
                snapshot.join(path).display().to_string()
            }
        })
        .collect();
    json!({
        "python": {
            "analysis": {
                "autoImportCompletions": false,
                "autoSearchPaths": false,
                "diagnosticMode": "openFilesOnly",
                "typeCheckingMode": "off",
                "useLibraryCodeForTypes": true,
                "extraPaths": extra
            }
        }
    })
}

/// Launches guarded Pyright processes over a shared snapshot.
struct PyrightLauncher {
    node: PathBuf,
    server: PathBuf,
    guard: PathBuf,
    /// The preflight's environment, version and project configuration.
    setup: PyrightSetup,
}

impl Launcher for PyrightLauncher {
    fn prepare(&self, snapshot: &Snapshot, files: &[&SemanticFile<'_>]) -> Result<(), SemanticError> {
        // Every analysed file under the name Pyright sees ([`server_name`]).
        let names: Vec<String> = files.iter().map(|f| workspace_rel(snapshot, f.path)).collect();
        let include: Vec<&str> = names.iter().map(String::as_str).collect();
        snapshot
            .write_aux("pyrightconfig.json", &serde_json::to_vec(&pyright_config(&include, &self.setup))?)?;
        fs::create_dir_all(snapshot.dir.join(".tmp"))?;
        Ok(())
    }

    fn command(
        &self,
        snapshot: &Snapshot,
        index: usize,
        tools: &ToolEnv,
        deadline: Instant,
        heap_cap_mb: Option<u64>,
    ) -> Result<(ServerCommand, ClientOptions), SemanticError> {
        let tmp = snapshot.dir.join(".tmp").join(format!("p{index}"));
        fs::create_dir_all(&tmp)?;
        let env = clean_env(
            &[],
            &[
                ("PYRIGHT_TMPDIR", tmp.display().to_string()),
                ("CODEPATH_LSP_WRITE_ROOT", snapshot.dir.display().to_string()),
            ],
        );
        let command = ServerCommand {
            program: self.node.clone(),
            args: node_args(heap_cap_mb, &self.guard, &self.server),
            env: env.into_iter().collect(),
            cwd: snapshot.dir.clone(),
        };
        // Pyright's requests block until their answer is complete: readiness `None`.
        let mut options =
            ClientOptions::new(Language::Python, tools.request_timeout, deadline, tools.max_in_flight);
        options.settings = settings(&snapshot.dir, &self.setup);
        options.stderr_log = Some(snapshot.dir.join(format!("lsp-{index}.stderr.log")));
        Ok((command, options))
    }
}

/// Node arguments of one Pyright process: the optional heap cap (`--max-old-space-size`,
/// only when the pool's memory model asks for it), the guard, the server script.
fn node_args(heap_cap_mb: Option<u64>, guard: &Path, server: &Path) -> Vec<String> {
    let mut args = Vec::with_capacity(5);
    if let Some(mb) = heap_cap_mb {
        args.push(format!("--max-old-space-size={mb}"));
    }
    args.extend([
        "--require".to_string(),
        guard.display().to_string(),
        server.display().to_string(),
        "--stdio".to_string(),
    ]);
    args
}

impl Backend for Pyright {
    fn id(&self) -> &str {
        "pyright"
    }

    fn languages(&self) -> &[Language] {
        &[Language::Python]
    }

    fn fingerprint(&self, tools: &ToolEnv, prepared: &crate::languages::Prepared) -> String {
        let mut h = PartsHasher::new();
        h.text(trace_core::TRACE_VERSION)
            .text(&prepared.fingerprint)
            .text(self.id())
            .int(u64::from(ENGINE_VERSION));
        match Self::package(tools) {
            Some(package) => hash_file_into(&mut h, &package.join("package.json")),
            None => {
                h.text("no-package");
            }
        }
        hash_executable_into(&mut h, tools.runtime_exe("node").as_deref());
        let setup = PyrightSetup::of(prepared);
        h.text(&asset_hash(LSP_GUARD))
            .text(&pyright_config(&[], &setup).to_string())
            .text(&settings(Path::new("<snapshot>"), &setup).to_string());
        h.finish().hex_prefix(32)
    }

    fn run(&self, request: &SemanticRequest<'_>) -> Result<BackendOutput, SemanticError> {
        let (profile, launcher) = self.parts(request.tools, request.prepared)?;
        pool::run_once(profile, launcher, request)
    }

    fn open_session(
        &self,
        request: &SemanticRequest<'_>,
    ) -> Result<Option<Box<dyn BackendSession>>, SemanticError> {
        let (profile, launcher) = self.parts(request.tools, request.prepared)?;
        Ok(Some(Box::new(LspSession::open(profile, launcher, request)?)))
    }

    /// One-shot live find-references: one Pyright process over a fresh snapshot.
    fn references(
        &self,
        request: &SemanticRequest<'_>,
        query: &crate::references::ReferenceQuery,
    ) -> Result<Option<crate::references::LiveReferences>, SemanticError> {
        let (profile, launcher) = self.parts(request.tools, request.prepared)?;
        pool::references_once(profile, launcher, request, query)
    }
}

#[cfg(test)]
#[path = "../../tests/unit/backends/pyright.rs"]
mod tests;
