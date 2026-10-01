//! Per-language setup hooks (DESIGN §1.7; owner flow for this file, each language file
//! belongs to its language package).
//!
//! A backend's [`Server`] check the platform, the toolchain, the server (+ runtime),
//! the dependencies and the build approval before anything starts ([`Server::preflight`]
//! -> [`Prepared`] or a [`SetupError`]), write generated configuration and run approved build
//! steps into the workspace ([`Server::prepare_workspace`]), validate the loaded server
//! ([`Server::check_loaded`]) and tune how its answers are trusted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use trace_core::facts::FileFacts;
use trace_core::model::Diagnostic;
use trace_core::paths::RepoPaths;
use trace_core::repo_settings::RepoSettings;
use trace_core::setup_error::SetupError;
use trace_core::Language;
use trace_env::os::{EnvVars, Platform};
use trace_env::{DetectContext, EcosystemId};

use crate::backend::SemanticFile;
use crate::backends::fntype::FnTypeRoute;
use crate::registry::{BackendEntry, ToolchainSpec};
use crate::tools::ToolEnv;

pub mod bash; // script
pub mod php; // script
pub mod python; // script
pub mod typescript; // script

pub mod c_cpp; // native
pub mod go; // native
pub mod rust; // native

pub mod java; // jvm
pub(crate) mod jvm;
pub mod scala; // jvm

pub mod csharp; // dotnet

pub mod haskell; // haskell

pub mod r; // r

/// Everything a preflight may read. Nothing here is written.
pub struct SetupContext<'a> {
    pub repo: &'a RepoPaths,
    pub entry: &'a BackendEntry,
    /// Languages of this backend present (product).
    pub languages: &'a [Language],
    /// Their files, relative.
    pub files: &'a [(&'a str, Language)],
    /// Syntax facts by relative path.
    pub facts: &'a dyn Fn(&str) -> Option<&'a FileFacts>,
    pub settings: &'a RepoSettings,
    pub tools: &'a ToolEnv,
    pub platform: &'a Platform,
    pub vars: &'a EnvVars,
    /// Report mode (`trace status`): never run approved build steps, only static checks.
    pub report_only: bool,
}

impl SetupContext<'_> {
    /// The first language of this backend (the one errors name).
    pub fn language(&self) -> Language {
        self.languages
            .first()
            .or_else(|| self.entry.languages.first())
            .copied()
            .unwrap_or(Language::Python)
    }
}

/// How the backend's workspace is built (registry `workspace.mode`, may be narrowed by
/// preflight).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    /// Inventoried sources + listed configs, per run (Pyright, TS, PHP, Bash).
    #[default]
    Snapshot,
    /// Persistent full-tree copy of the repository (build-importing servers), synced
    /// incrementally.
    Mirror,
}

/// Result of a successful preflight.
#[derive(Clone, Default)]
pub struct Prepared {
    pub backend: String,
    pub languages: Vec<Language>,
    /// String placeholder values for registry strings (§1.8.3).
    pub vars: BTreeMap<String, String>,
    /// JSON placeholder values: a registry string that is exactly "{json:NAME}" becomes this.
    pub json_vars: BTreeMap<String, serde_json::Value>,
    /// Extra environment for the server (merged over the entry's env_set).
    pub env: BTreeMap<String, String>,
    pub workspace: WorkspaceMode,
    /// Files written into the workspace before launch (relative path, bytes).
    pub generated: Vec<(String, Vec<u8>)>,
    pub library_roots: Vec<trace_env::LibraryRoot>,
    pub toolchain: Option<trace_env::Toolchain>,
    /// The project runs code under this server (approval was required and given).
    pub runs_project_code: bool,
    /// Directories whose files are pending (sub-projects): relative dir -> reason.
    pub pending_dirs: BTreeMap<String, String>,
    /// Toolchain + dependency + settings fingerprint; part of the backend fingerprint.
    pub fingerprint: String,
    /// One-line notes for `trace status`.
    pub status: Vec<String>,
    /// Backend-private typed data (downcast by the backend that produced it).
    pub data: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("backend", &self.backend)
            .field("languages", &self.languages)
            .field("vars", &self.vars)
            .field("json_vars", &self.json_vars)
            .field("env", &self.env)
            .field("workspace", &self.workspace)
            .field("generated", &self.generated.iter().map(|(p, _)| p).collect::<Vec<_>>())
            .field("library_roots", &self.library_roots)
            .field("toolchain", &self.toolchain)
            .field("runs_project_code", &self.runs_project_code)
            .field("pending_dirs", &self.pending_dirs)
            .field("fingerprint", &self.fingerprint)
            .field("status", &self.status)
            .finish()
    }
}

/// Context after the workspace exists and before the server starts.
pub struct WorkspaceContext<'a> {
    pub prepared: &'a Prepared,
    /// Snapshot or mirror root.
    pub workspace: &'a Path,
    /// Stable per-backend state dir (never inside the workspace).
    pub outside: &'a Path,
    pub files: &'a [&'a SemanticFile<'a>],
    /// Backend log file for build-step output.
    pub log: &'a Path,
}

/// Context of [`Server::warm_up`]: one server process right after it opened its first
/// documents, before any query.
pub struct WarmUpContext<'a, 'f> {
    pub prepared: &'a Prepared,
    pub client: &'a mut crate::lsp::LspClient,
    /// The documents just opened (the process's first shard).
    pub files: &'a [&'a SemanticFile<'f>],
    pub snapshot: &'a crate::snapshot::Snapshot,
}

/// Context after readiness.
pub struct LoadedContext<'a> {
    pub prepared: &'a Prepared,
    /// window/logMessage + showMessage (type, text).
    pub log_messages: &'a [(u8, String)],
    /// Server notifications kept by the client.
    pub notifications: &'a [(String, serde_json::Value)],
    /// publishDiagnostics (uri, params) received before the process was warmed up (at most
    /// the first minutes; bounded).
    pub diagnostics: &'a [(String, serde_json::Value)],
    pub log: &'a Path,
}

/// How engine answers of this server are trusted / cleaned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnswerPolicy {
    /// Identical locations returned twice (Roslyn).
    pub dedupe_locations: bool,
    /// Metals: retry requests cancelled with -32800 while compiling (count).
    pub retry_cancelled: u8,
    /// jdtls: -32603 on definition into jars without sources -> unresolved, not a failure.
    pub internal_error_is_unresolved: bool,
    /// Roslyn: a file compiled by several projects is answered per project context
    /// (`_vs_projectContext`); answers are merged over every context of the file.
    pub project_contexts: bool,
    /// clangd: the server reports inactive preprocessor regions; calls inside them are
    /// `inactive_code`, never unresolved server answers.
    pub inactive_regions: bool,
}

/// Context of [`Server::settle_changes`]: one server process right after changed
/// documents were sent (`didChange` / `didOpen` of an update), before the update's queries.
pub struct SettleContext<'a> {
    pub prepared: &'a Prepared,
    pub client: &'a mut crate::lsp::LspClient,
    /// URIs of the documents just changed.
    pub changed: &'a [String],
}

pub struct InstallExtraContext<'a> {
    pub tools_dir: &'a Path,
    pub platform: &'a Platform,
    pub vars: &'a EnvVars,
    pub repo_root: Option<&'a Path>,
    pub log: &'a Path,
}

/// Server location outside the workspace, before trace-library names the symbol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalLocation {
    /// Absolute file path, or a virtual URI (jdt://..., csharp:/metadata/...).
    pub path: String,
    pub line: u32,
    pub column: u32,
    /// "python-stdlib", "anyio", "com.google.guava:guava"
    pub package: String,
    pub version: Option<String>,
    pub stdlib: bool,
    /// Source text on disk that trace-library may parse.
    pub readable: bool,
    /// When the server/URI already names it (R pkg::sym, jdt class).
    pub symbol: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallExtra {
    pub id: String,
    pub coordinates: Vec<String>,
    pub reason: String,
}

pub trait Server: Send + Sync {
    /// Checks in this order: platform, toolchain, server (+ runtime), dependencies (static),
    /// build approval. PLAN decision 15: collect EVERY independent failure through
    /// `setup::Collect` and return them combined; only a check that makes later ones
    /// meaningless stops early. Pure except for `os::toolchain_output`.
    fn preflight(&self, cx: &SetupContext<'_>) -> Result<Prepared, SetupError>;
    /// Generated files and approved build steps, after the workspace exists.
    fn prepare_workspace(&self, cx: &WorkspaceContext<'_>) -> Result<(), SetupError> {
        let _ = cx;
        Ok(())
    }
    /// Post-readiness validation: map load failures in logs/diagnostics to SetupErrors.
    /// The document text sent to the server for a file (`didOpen` / `didChange`). A server
    /// that misreads a construct the language defines may get an equivalent spelling here,
    /// found with the syntax tree; the result must have exactly the same length and line
    /// breaks so every position maps 1:1 (bash-language-server ignores the escaped source
    /// command `\. file` and gets ` . file`). Default: the file text unchanged.
    fn server_text<'s>(&self, language: Language, text: &'s str) -> std::borrow::Cow<'s, str> {
        let _ = language;
        std::borrow::Cow::Borrowed(text)
    }
    /// Once per server process, after its first documents are opened and before any query:
    /// make a server that loads project state lazily load it now and wait for it (HLS loads
    /// the cabal cradle only after `didOpen` and queues every request behind that load).
    /// A failed load is a setup error, never degraded answers.
    fn warm_up(&self, cx: &mut WarmUpContext<'_, '_>) -> Result<(), SetupError> {
        let _ = cx;
        Ok(())
    }
    fn check_loaded(&self, cx: &LoadedContext<'_>) -> Result<Vec<Diagnostic>, SetupError> {
        let _ = cx;
        Ok(Vec::new())
    }
    /// Map a server location outside the workspace to a library location.
    fn external_location(&self, uri: &str, prepared: &Prepared) -> Option<ExternalLocation> {
        let _ = (uri, prepared);
        None
    }
    fn answer_policy(&self) -> AnswerPolicy {
        AnswerPolicy::default()
    }
    /// How the function-type rule gets declared parameter types for this language (§1.10).
    fn fn_type_route(&self, language: Language) -> FnTypeRoute;
    /// Extra install work that depends on the repository or the user's toolchain.
    fn install_extras(&self, repo_root: Option<&Path>) -> Vec<InstallExtra> {
        let _ = repo_root;
        Vec::new()
    }
    /// Execute one extra (network allowed: this runs only under `trace status --install`).
    fn run_install_extra(
        &self,
        extra: &InstallExtra,
        cx: &InstallExtraContext<'_>,
    ) -> Result<(), SetupError> {
        let _ = (extra, cx);
        Ok(())
    }
    /// A request/diagnostic error for `path` that means "this file is not part of the build
    /// on this machine" -> reason.
    fn outside_build(&self, path: &str, error: &str) -> Option<String> {
        let _ = (path, error);
        None
    }
    /// Static per-file answer, before any request: `path` (repository-relative) is not part
    /// of the build on this machine (another platform / language version / build variant)
    /// -> reason. Its calls are unknown (`FileSemantics::outside_build`). Default: None.
    fn outside_build_file(&self, path: &str, prepared: &Prepared) -> Option<String> {
        let _ = (path, prepared);
        None
    }
    /// Whether `command` (a command word the server could not resolve) is an external program
    /// of this machine (found on the PATH / standard locations, never executed): the call is
    /// external, not unresolved. Default: false.
    fn external_program(&self, command: &str, prepared: &Prepared) -> bool {
        let _ = (command, prepared);
        false
    }
    /// After changed documents were sent to a running server and before the update's queries:
    /// wait until the server took the changes into account (bounded; a timeout is a setup
    /// error). Called for every process of a warm session right after the ordered
    /// `documentSymbol` confirmation of the changed files (`pool::apply_changes`); the
    /// client's readiness state (`LspClient::settle_after_changes`, `wait_progress*`) is the
    /// way to wait - kept notifications are bounded (`lsp` module docs). Default: nothing to
    /// wait for.
    fn settle_changes(&self, cx: &mut SettleContext<'_>) -> Result<(), SetupError> {
        let _ = cx;
        Ok(())
    }
    /// Server-specific per-file extras after the engine answered `file` (rust-analyzer
    /// `expandMacro`, DESIGN §1.15). `budget` is shared by the whole run (one unit per request).
    /// Returns the expansions and, when the budget ran out, a `bounded` diagnostic.
    fn file_expansions(
        &self,
        file: &SemanticFile<'_>,
        prepared: &Prepared,
        session: &mut dyn crate::backends::fntype::FnTypeSession,
        budget: &std::sync::atomic::AtomicU32,
    ) -> (Vec<trace_core::semantics::ExpandedMacro>, Option<Diagnostic>) {
        let _ = (file, prepared, session, budget);
        (Vec::new(), None)
    }
}

/// Backend ids without a language module (a `semantic.registry` override): the checks the
/// registry entry itself declares, in the shared order (server installed, server runtimes,
/// build approval for `requires_build.when == always`), every failure collected; then the
/// registry's `Prepared` (no toolchain / dependency detection). Table-only function-type route.
pub struct DefaultServer;

impl Server for DefaultServer {
    fn preflight(&self, cx: &SetupContext<'_>) -> Result<Prepared, SetupError> {
        let mut collect = crate::setup::Collect::default();
        collect.check(crate::setup::require_server(cx));
        collect.check(crate::setup::require_runtimes(cx));
        if let Some(spec) = cx
            .entry
            .requires_build
            .as_ref()
            .filter(|b| b.when == crate::registry::BuildWhen::Always)
        {
            collect.check(crate::setup::require_approval(cx, spec));
        }
        let mut prepared = default_prepared(cx);
        prepared.runs_project_code = cx.entry.requires_build.is_some() && cx.settings.allow_build;
        collect.finish(prepared)
    }
    fn fn_type_route(&self, _language: Language) -> FnTypeRoute {
        FnTypeRoute::TableOnly
    }
}

/// Today's preflight result: the backend, its languages and the registry workspace mode, plus
/// the install directory of the backend's server ([`crate::external::SERVER_DIR_VAR`]):
/// declarations bundled with the server (Intelephense's PHP stubs) are the language's
/// standard library.
pub fn default_prepared(cx: &SetupContext<'_>) -> Prepared {
    let mut vars = BTreeMap::new();
    if let Some(dir) = cx.entry.install.as_ref().and_then(|i| cx.tools.tool_dir(&i.id)) {
        vars.insert(crate::external::SERVER_DIR_VAR.to_string(), dir.to_string_lossy().into_owned());
    }
    Prepared {
        backend: cx.entry.id.clone(),
        languages: cx.languages.to_vec(),
        workspace: cx.entry.workspace.mode,
        vars,
        ..Prepared::default()
    }
}

/// The [`Server`] of a backend id; unknown ids get `DefaultServer`.
pub fn server_for(backend_id: &str) -> &'static dyn Server {
    match backend_id {
        "pyright" => &python::Hooks,
        "typescript" => &typescript::Hooks,
        "lsp:intelephense" => &php::Hooks,
        "lsp:bash-language-server" => &bash::Hooks,
        "lsp:rust-analyzer" => &rust::Hooks,
        "lsp:gopls" => &go::Hooks,
        "lsp:clangd" => &c_cpp::Hooks,
        "lsp:jdtls" => &java::Hooks,
        "lsp:metals" => &scala::Hooks,
        "lsp:roslyn" => &csharp::Hooks,
        "lsp:haskell-language-server" => &haskell::Hooks,
        "lsp:r-languageserver" => &r::Hooks,
        _ => &DefaultServer,
    }
}

/// Write one generated file into the workspace (relative, `/`-separated path; never
/// outside the workspace). An I/O failure is the language's `BuildFailed` with the log.
pub fn write_generated(
    cx: &WorkspaceContext<'_>,
    language: Language,
    rel: &str,
    bytes: &[u8],
) -> Result<(), SetupError> {
    let failed = |what: String| SetupError::BuildFailed {
        language,
        what,
        log: cx.log.to_path_buf(),
    };
    let rel_path = crate::registry::safe_relative(rel)
        .ok_or_else(|| failed(format!("invalid generated file name {rel}")))?;
    let path = cx.workspace.join(rel_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| failed(format!("writing {rel} failed: {e}")))?;
    }
    std::fs::write(&path, bytes).map_err(|e| failed(format!("writing {rel} failed: {e}")))
}

// ---------------------------------------------------------------------------------------------
// Helpers shared by the preflights
// ---------------------------------------------------------------------------------------------

/// The EXECUTE context of a preflight for `ecosystem` (`--env` override from the settings):
/// toolchain searches. Executables inside the repository or trace's own workspaces
/// (`cx.tools.forbidden_roots`) are never trusted, so nothing below them is found here.
pub(crate) fn detect_context<'a>(cx: &SetupContext<'a>, ecosystem: EcosystemId) -> DetectContext<'a> {
    DetectContext {
        root: cx.repo.root.as_path(),
        platform: cx.platform,
        vars: cx.vars,
        env_override: cx.settings.env_for(ecosystem.as_str()),
        forbidden: &cx.tools.forbidden_roots,
        files: cx.files,
    }
}

/// The READ context of a preflight for `ecosystem`: manifests, pin files, lockfiles and
/// installed dependencies are only read, inside the repository too; only the user's
/// protected roots (`readable`, from [`trace_core::paths::forbidden_roots`]) are off limits.
/// Toolchains are searched through [`detect_context`] (the execute context) only.
pub(crate) fn read_context<'a>(
    cx: &'a SetupContext<'_>,
    ecosystem: EcosystemId,
    readable: &'a [PathBuf],
) -> DetectContext<'a> {
    DetectContext {
        root: cx.repo.root.as_path(),
        platform: cx.platform,
        vars: cx.vars,
        env_override: cx.settings.env_for(ecosystem.as_str()),
        forbidden: readable,
        files: cx.files,
    }
}

/// The entry's toolchain spec, or the given default.
pub(crate) fn toolchain_spec(
    cx: &SetupContext<'_>,
    ecosystem: &str,
    needs: &str,
    install: &str,
) -> ToolchainSpec {
    cx.entry.toolchain.clone().unwrap_or(ToolchainSpec {
        ecosystem: ecosystem.into(),
        needs: needs.into(),
        install: install.into(),
        optional: false,
    })
}

/// First line of a message, bounded to 160 characters.
pub(crate) fn first_line(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let mut out: String = line.chars().take(160).collect();
    if line.chars().count() > 160 {
        out.push_str("...");
    }
    out
}

/// Time bound of one build step (setting `semantic.build_step_timeout_secs`).
pub(crate) fn build_step_timeout() -> Duration {
    Duration::from_secs(trace_core::config::current().semantic.build_step_timeout_secs)
}

/// One logged build step (a toolchain / build-tool process; project code only after approval).
pub(crate) struct Step<'a> {
    pub program: &'a Path,
    pub args: Vec<String>,
    pub cwd: &'a Path,
    /// The complete environment of the process.
    pub env: BTreeMap<String, String>,
    pub timeout: Duration,
    /// Discard stdout (large JSON such as `cargo metadata`); stderr is always logged.
    pub quiet_stdout: bool,
}

/// Outcome of a [`Step`].
pub(crate) struct StepOutcome {
    pub success: bool,
    pub timed_out: bool,
    /// What the step wrote to the log.
    pub output: String,
}

/// At most this much of what one step appended to its log is returned as its output (the
/// end of it: tools print their errors last).
pub(crate) const MAX_STEP_OUTPUT: u64 = 1024 * 1024;

/// Run `step` with its output appended to `log`, bounded by its timeout. The step's whole
/// process tree is stopped when the timeout passes, and whatever it left running is stopped
/// after it exited (`procs`); it never gets trace's own standard streams.
pub(crate) fn run_step(step: &Step<'_>, log: &Path) -> std::io::Result<StepOutcome> {
    use std::io::Write;
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
    let start = file.metadata()?.len();
    writeln!(file, "$ {} {}", step.program.display(), step.args.join(" "))?;
    let stdout = if step.quiet_stdout {
        Stdio::null()
    } else {
        Stdio::from(file.try_clone()?)
    };
    let mut child = crate::procs::command(step.program)
        .args(&step.args)
        .current_dir(step.cwd)
        .env_clear()
        .envs(&step.env)
        .stdout(stdout)
        .stderr(Stdio::from(file.try_clone()?))
        .spawn()?;
    let (success, timed_out) = match crate::procs::wait_bounded(&mut child, step.timeout)? {
        Some(status) => (status.success(), false),
        None => (false, true),
    };
    drop(file);
    Ok(StepOutcome {
        success,
        timed_out,
        output: appended_tail(log, start, MAX_STEP_OUTPUT),
    })
}

/// The last `max` bytes of what was appended to `log` after byte `start` (never the whole
/// append-only log: it grows across runs).
pub(crate) fn appended_tail(log: &Path, start: u64, max: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(log) else {
        return String::new();
    };
    let end = file.metadata().map(|m| m.len()).unwrap_or(start);
    let from = start.max(end.saturating_sub(max)).min(end);
    if file.seek(SeekFrom::Start(from)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    let _ = file.take(end - from).read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
#[path = "../../tests/unit/languages/mod.rs"]
mod tests;
