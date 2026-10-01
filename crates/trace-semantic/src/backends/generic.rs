//! Generic LSP backend built from a registry entry (`assets/backends/*.json`, schema 2,
//! DESIGN §1.8). Every `kind: "lsp"` entry becomes one [`GenericLsp`]; languages, launch
//! arguments, environment, LSP settings, readiness, server-request policy, workspace mode
//! and sharding are registry data, not code. Per-language behaviour (preflight, generated
//! files, approved build steps, load validation, answer policy) comes from the backend's
//! [`crate::languages::Server`].
//!
//! Launch (never through `.cmd` shims):
//! * `tool`: `<tools>/<tool>/<version>/<path>` per MANIFEST ([`ToolEnv::entry_executable`]);
//! * `runtime`: a trace-managed runtime program (`java`, `dotnet`) with the tool's files as
//!   arguments (`{tool:<id>}` placeholders);
//! * `toolchain`: the first existing of `paths` in the preflight's detected toolchain
//!   (`Prepared.toolchain`: HLS, R);
//! * `node_script`: the trace-managed Node runtime (`ToolEnv::runtime_exe("node")`, never the
//!   user's node) runs `--require <lsp_guard.cjs> <tools>/<tool>/<version>/<script>` with
//!   `CODEPATH_LSP_WRITE_ROOT=<workspace>`.
//!
//! A missing executable or tools-folder path the entry names (jdtls plugins) is
//! `SetupError::ServerMissing` (the preflight normally reports it first).
//!
//! Placeholders (§1.8.3) are expanded in arguments, environment values, initialization
//! options, settings, `after_initialized` params and `request` readiness params:
//! `{snapshot}` (workspace root: snapshot or mirror), `{outside}` (stable per-backend state
//! dir), `{tmp}` (`{outside}/tmp`), `{repo_cache}`, `{tools}`, `{tool:<id>}`,
//! `{runtime:<id>}`, `{os}` (`windows|linux|macos`), `{arch}` (`x86_64|aarch64`), every
//! `Prepared.vars` name (`{toolchain}`, `{toolchain:<key>}`, `{jdk}`, `{cdb_dir}`, ...) and
//! `{json:<name>}` (a string that is exactly this becomes the JSON value
//! `Prepared.json_vars[name]`). An unknown placeholder is `SemanticError::Protocol` (a
//! registry bug; the registry test expands every entry). An argument whose last path
//! segment contains `*` is expanded to the lexicographically last matching file
//! (`org.eclipse.equinox.launcher_*.jar`).
//!
//! Environment: `clean_env(env_allow, env_set + Prepared.env)` (the preflight's values win):
//! never the API key, PATH only as the entry or the preflight sets it. Scratch directories
//! named by `{snapshot}/...`, `{outside}/...` or `{tmp}` values exist before the server
//! starts.
//!
//! [`Launcher::prepare`] (once per workspace open): writes `Prepared.generated` into the
//! workspace, then `Server::prepare_workspace` (generated configs, approved build
//! steps; log `{outside}/prepare.log`). After readiness, [`Launcher::loaded`] runs
//! `Server::check_loaded` over the kept log messages, notifications and diagnostics
//! (a load failure is the hook's `SetupError`).
//!
//! Algorithm: [`crate::engine`] (shared with Pyright); sessions, workspaces, request
//! sharding and warm updates: [`crate::pool`].
//!
//! Fingerprint: blake3 over the trace version, the entry id, `ENGINE_VERSION`, the entry's
//! JSON, the preflight fingerprint, and the executable (and node + script + guard) stamps.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;
use trace_core::fingerprint::PartsHasher;
use trace_core::model::{Diagnostic, Provider};
use trace_core::{Language, SetupError};

use crate::backend::{hash_executable_into, Backend, BackendOutput, SemanticFile, SemanticRequest};
use crate::engine::ENGINE_VERSION;
use crate::languages::{server_for, LoadedContext, Prepared, WorkspaceContext};
use crate::lsp::{ClientOptions, LspClient, ServerCommand};
use crate::mirror::MirrorRules;
use crate::pool::{self, Launcher, LspSession, Profile};
use crate::references::{LiveReferences, ReferenceQuery};
use crate::registry::{BackendEntry, ExecutableSpec, ReadySpec};
use crate::session::BackendSession;
use crate::snapshot::Snapshot;
use crate::tools::{clean_env, ToolEnv};
use crate::SemanticError;

/// Configuration globs as the `'static` slices `Profile` / `Backend::snapshot_configs`
/// use. Interned: equal lists share one allocation, so rebuilding backends does not grow.
fn intern(list: &[String]) -> &'static [&'static str] {
    static CACHE: OnceLock<Mutex<HashMap<Vec<String>, &'static [&'static str]>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = match cache.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(found) = guard.get(list) {
        return found;
    }
    let leaked: Vec<&'static str> = list.iter().map(|s| &*Box::leak(s.clone().into_boxed_str())).collect();
    let slice: &'static [&'static str] = Box::leak(leaked.into_boxed_slice());
    guard.insert(list.to_vec(), slice);
    slice
}

/// A generic LSP backend instance (one registry entry).
pub struct GenericLsp {
    pub entry: BackendEntry,
    configs: &'static [&'static str],
}

impl GenericLsp {
    pub fn new(entry: BackendEntry) -> Self {
        let configs = intern(&entry.workspace.configs);
        GenericLsp { entry, configs }
    }

    /// Server name (`gopls` for `lsp:gopls`).
    pub fn name(&self) -> &str {
        self.entry.id.strip_prefix("lsp:").unwrap_or(&self.entry.id)
    }

    /// The language setup errors name: the first planned language, else the entry's first.
    fn language(&self, prepared: &Prepared) -> Language {
        prepared
            .languages
            .first()
            .or_else(|| self.entry.languages.first())
            .copied()
            .unwrap_or(Language::Python)
    }
}

/// What to launch for an entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Launch {
    /// A trusted native executable.
    Exe(PathBuf),
    /// `<trace-managed node> --require <guard> <script>`.
    Node {
        node: PathBuf,
        guard: PathBuf,
        script: PathBuf,
    },
}

impl Launch {
    fn program(&self) -> &Path {
        match self {
            Launch::Exe(p) => p,
            Launch::Node { node, .. } => node,
        }
    }
}

/// Every string of an entry that may carry placeholders.
fn entry_strings(entry: &BackendEntry) -> Vec<String> {
    fn strings(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(s) => out.push(s.clone()),
            Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
            Value::Object(map) => map.values().for_each(|v| strings(v, out)),
            _ => {}
        }
    }
    let mut all: Vec<String> = entry.args.clone();
    all.extend(entry.env_set.values().cloned());
    strings(&entry.initialization_options, &mut all);
    strings(&entry.settings, &mut all);
    for n in &entry.after_initialized {
        strings(&n.params, &mut all);
    }
    if let ReadySpec::Request { params, .. } = &entry.ready {
        strings(params, &mut all);
    }
    all
}

/// Every tools-folder path an entry names (the part of the string from `{tool:<id>}`,
/// `{runtime:<id>}` or `{tools}` on).
fn tool_references(entry: &BackendEntry) -> Vec<String> {
    let mut out: Vec<String> = entry_strings(entry)
        .iter()
        .filter_map(|s| {
            ["{tool:", "{runtime:", "{tools}"]
                .iter()
                .filter_map(|m| s.find(m))
                .min()
                .map(|i| s[i..].to_string())
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Placeholder values of one launch (§1.8.3, module docs).
#[derive(Clone, Debug)]
pub(crate) struct Placeholders {
    /// Name (without braces) -> value.
    pub values: BTreeMap<String, String>,
    /// `{json:<name>}` whole-value substitutions.
    pub json: BTreeMap<String, Value>,
    /// Language named when a `{tool:..}` / `{runtime:..}` is not installed.
    pub language: Language,
}

impl Placeholders {
    /// The values for `entry` launched over a workspace (tool directories from `tools`,
    /// preflight variables from `prepared`).
    pub(crate) fn new(
        entry: &BackendEntry,
        workspace: &Path,
        outside: &Path,
        repo_cache: &Path,
        tools: &ToolEnv,
        prepared: &Prepared,
    ) -> Placeholders {
        let mut values = BTreeMap::new();
        let text = |p: &Path| p.display().to_string();
        values.insert("snapshot".to_string(), text(workspace));
        // The workspace root as a file URI without the trailing slash (jvm #1, dotnet #1:
        // absolute project / solution URIs inside the mirror).
        if let Ok(uri) = url::Url::from_directory_path(workspace) {
            values.insert("snapshot_uri".to_string(), uri.as_str().trim_end_matches('/').to_string());
        }
        values.insert("outside".to_string(), text(outside));
        values.insert("tmp".to_string(), text(&outside.join("tmp")));
        values.insert("repo_cache".to_string(), text(repo_cache));
        if let Some(t) = &tools.tools_dir {
            values.insert("tools".to_string(), text(t));
        }
        let (os, arch) = os_arch();
        values.insert("os".to_string(), os);
        values.insert("arch".to_string(), arch);
        if let Some(heap) = entry.resources().heap_mb {
            values.insert("heap_mb".to_string(), heap.to_string());
        }
        for s in entry_strings(entry) {
            for name in crate::registry::placeholders(&s) {
                if let Some((kind, id)) = name.split_once(':') {
                    if matches!(kind, "tool" | "runtime") {
                        if let Some(dir) = tools.tool_dir(id) {
                            values.insert(name.to_string(), text(&dir));
                        }
                    }
                }
            }
        }
        for (k, v) in &prepared.vars {
            values.insert(k.clone(), v.clone());
        }
        Placeholders {
            values,
            json: prepared.json_vars.clone(),
            language: prepared
                .languages
                .first()
                .or_else(|| entry.languages.first())
                .copied()
                .unwrap_or(Language::Python),
        }
    }

    /// Substitute every `{name}` of `text`; an unknown name is an error.
    pub(crate) fn expand(&self, text: &str) -> Result<String, SemanticError> {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find('{') {
            out.push_str(&rest[..start]);
            let after = &rest[start + 1..];
            let Some(end) = after.find('}') else {
                out.push_str(&rest[start..]);
                rest = "";
                break;
            };
            let name = &after[..end];
            match self.values.get(name) {
                Some(v) => out.push_str(v),
                None => return Err(self.unknown(name, text)),
            }
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }

    fn unknown(&self, name: &str, text: &str) -> SemanticError {
        match name.split_once(':') {
            Some(("tool" | "runtime", _)) => SemanticError::Setup(SetupError::ServerMissing {
                language: self.language,
            }),
            _ => SemanticError::Protocol(format!("unknown placeholder {{{name}}} in {text:?}")),
        }
    }

    /// Substitute plain placeholders in every string of a JSON value.
    fn expand_strings(&self, value: &Value) -> Result<Value, SemanticError> {
        Ok(match value {
            Value::String(s) => Value::String(self.expand(s)?),
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|v| self.expand_strings(v))
                    .collect::<Result<_, _>>()?,
            ),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| Ok((k.clone(), self.expand_strings(v)?)))
                    .collect::<Result<_, SemanticError>>()?,
            ),
            other => other.clone(),
        })
    }

    /// Whether `value` is exactly `{json:NAME}` for a variable the preflight did not set (absent
    /// or JSON `null`): the setting holding it is omitted (servers read an absent setting as
    /// their default; a `null` is read as a value - Metals logs `mill-script null`).
    fn absent_json(&self, value: &Value) -> bool {
        match value {
            Value::String(s) => s
                .strip_prefix("{json:")
                .and_then(|r| r.strip_suffix('}'))
                .is_some_and(|name| self.json.get(name).is_none_or(Value::is_null)),
            _ => false,
        }
    }

    /// Substitute in every string of a JSON value; a string that is exactly `{json:NAME}`
    /// becomes that JSON value (its strings expanded once more). Inside objects and arrays a
    /// `{json:NAME}` whose variable is absent or `null` is omitted with its key
    /// ([`Placeholders::absent_json`]); as the whole value an absent one is an error (a
    /// registry bug).
    pub(crate) fn expand_json(&self, value: &Value) -> Result<Value, SemanticError> {
        Ok(match value {
            Value::String(s) => {
                if let Some(name) = s.strip_prefix("{json:").and_then(|r| r.strip_suffix('}')) {
                    let value = self
                        .json
                        .get(name)
                        .ok_or_else(|| self.unknown(&format!("json:{name}"), s))?;
                    // Plain placeholders inside the substituted value (one pass, no nested
                    // `{json:..}`): a preflight cannot know the workspace path.
                    return self.expand_strings(value);
                }
                Value::String(self.expand(s)?)
            }
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .filter(|v| !self.absent_json(v))
                    .map(|v| self.expand_json(v))
                    .collect::<Result<_, _>>()?,
            ),
            Value::Object(map) => Value::Object(
                map.iter()
                    .filter(|(_, v)| !self.absent_json(v))
                    .map(|(k, v)| Ok((k.clone(), self.expand_json(v)?)))
                    .collect::<Result<_, SemanticError>>()?,
            ),
            other => other.clone(),
        })
    }
}

/// `{os}` / `{arch}` of this machine.
fn os_arch() -> (String, String) {
    let platform = trace_env::os::Platform::current();
    let key = platform.key();
    let (os, arch) = key.split_once('-').unwrap_or((key.as_str(), ""));
    (os.to_string(), arch.trim_end_matches("-musl").to_string())
}

/// The first tools-folder path of the entry that does not exist (jdtls plugins): the server
/// is then not installed.
fn missing_tool_path(entry: &BackendEntry, tools: &ToolEnv) -> Option<String> {
    let refs = tool_references(entry);
    if refs.is_empty() {
        return None;
    }
    let ph =
        Placeholders::new(entry, Path::new(""), Path::new(""), Path::new(""), tools, &Prepared::default());
    refs.into_iter().find(|r| match ph.expand(r) {
        Ok(path) => {
            let path = expand_glob(&path);
            path.contains('*') || !Path::new(&path).exists()
        }
        Err(_) => true,
    })
}

/// Resolve the launch of `entry` (trusted executables only); a missing piece is
/// `ServerMissing` for `language`. A preflight that found the server itself (a system
/// clangd where no release exists, `haskell-language-server-<ghc>` for the project's GHC)
/// names it in `Prepared.vars["server_executable"]` (an absolute existing file): that
/// program is launched instead of the entry's executable (the caller checks trust).
pub(crate) fn resolve_launch(
    entry: &BackendEntry,
    tools: &ToolEnv,
    prepared: &Prepared,
    language: Language,
) -> Result<Launch, SemanticError> {
    let missing = || SemanticError::Setup(SetupError::ServerMissing { language });
    if let Some(exe) = prepared
        .vars
        .get("server_executable")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_file())
    {
        return Ok(Launch::Exe(exe));
    }
    let toolchain = prepared.toolchain.as_ref();
    if missing_tool_path(entry, tools).is_some() {
        return Err(missing());
    }
    match &entry.executable {
        ExecutableSpec::NodeScript { tool, script } => {
            let node = tools.runtime_exe("node").ok_or_else(missing)?;
            let script_path = tools.tool_file(tool, script).ok_or_else(missing)?;
            if !tools.assets.lsp_guard.is_file() {
                return Err(SemanticError::ExecutableUnavailable("LSP guard asset missing".into()));
            }
            Ok(Launch::Node {
                node,
                guard: tools.assets.lsp_guard.clone(),
                script: script_path,
            })
        }
        spec => tools
            .entry_executable(spec, toolchain)
            .map(Launch::Exe)
            .ok_or_else(missing),
    }
}

impl Backend for GenericLsp {
    fn id(&self) -> &str {
        &self.entry.id
    }

    fn languages(&self) -> &[Language] {
        &self.entry.languages
    }

    fn fingerprint(&self, tools: &ToolEnv, prepared: &Prepared) -> String {
        let mut h = PartsHasher::new();
        h.text(trace_core::TRACE_VERSION)
            .text(&self.entry.id)
            .int(u64::from(ENGINE_VERSION));
        // Launch and protocol: the entry without its `safety` prose (resource numbers are
        // settings, not entry fields). serde_json maps are sorted (no `preserve_order`): the
        // JSON text is deterministic.
        let mut entry = serde_json::to_value(&self.entry).unwrap_or_default();
        if let Some(fields) = entry.as_object_mut() {
            fields.remove("safety");
        }
        h.text(&entry.to_string());
        h.text(&prepared.fingerprint);
        match resolve_launch(&self.entry, tools, prepared, self.language(prepared)) {
            Ok(Launch::Exe(exe)) => hash_executable_into(&mut h, Some(&exe)),
            Ok(Launch::Node { node, script, .. }) => {
                hash_executable_into(&mut h, Some(&node));
                hash_executable_into(&mut h, Some(&script));
                h.text(&crate::assets::asset_hash(crate::assets::LSP_GUARD));
            }
            Err(_) => hash_executable_into(&mut h, None),
        }
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

    fn snapshot_configs(&self) -> &[&str] {
        self.configs
    }

    fn references(
        &self,
        request: &SemanticRequest<'_>,
        query: &ReferenceQuery,
    ) -> Result<Option<LiveReferences>, SemanticError> {
        let (profile, launcher) = self.parts(request.tools, request.prepared)?;
        pool::references_once(profile, launcher, request, query)
    }
}

impl GenericLsp {
    /// Trusted launch inputs and the session profile.
    fn parts(
        &self,
        tools: &ToolEnv,
        prepared: &Prepared,
    ) -> Result<(Profile, Box<dyn Launcher>), SemanticError> {
        let language = self.language(prepared);
        let launch = resolve_launch(&self.entry, tools, prepared, language)?;
        match &launch {
            Launch::Exe(exe) => tools.ensure_trusted(exe)?,
            Launch::Node { node, guard, script } => {
                for path in [node, guard, script] {
                    tools.ensure_trusted(path)?;
                }
            }
        }
        let tool_version = self.entry.install.as_ref().and_then(|i| {
            let dir = tools.tool_dir(&i.id)?;
            let program = match &launch {
                Launch::Exe(p) => p.clone(),
                Launch::Node { script, .. } => script.clone(),
            };
            crate::tools::is_within(&crate::tools::canonical_or_self(&program), &dir)
                .then(|| i.version.clone())
        });
        let profile = Profile {
            id: self.entry.id.clone(),
            languages: self.entry.languages.clone(),
            provider: Provider::Lsp(self.name().to_string()),
            fingerprint: self.fingerprint(tools, prepared),
            tool_version,
            python: false,
            max_processes: (self.entry.resources().processes.unwrap_or(0) as usize).max(1),
            config_names: self.configs,
            keep_documents_open: true,
            shard: self.entry.shard,
            workspace: prepared.workspace,
            rules: MirrorRules {
                include_ignored: self.entry.workspace.include_ignored.clone(),
                exclude: self.entry.workspace.exclude.clone(),
            },
            server_name: None,
        };
        let launcher = GenericLauncher {
            entry: self.entry.clone(),
            launch,
            prepared: prepared.clone(),
            language,
        };
        Ok((profile, Box::new(launcher)))
    }
}

/// Expand a `*` in the last path segment of `arg` to the lexicographically last existing
/// match (structural directory listing; no pattern language beyond one `*`).
fn expand_glob(arg: &str) -> String {
    let Some(star) = arg.find('*') else {
        return arg.to_string();
    };
    let cut = arg[..star].rfind(['/', '\\']).map_or(0, |i| i + 1);
    if arg[star..].contains(['/', '\\']) || cut == 0 {
        return arg.to_string();
    }
    let dir = &arg[..cut];
    let pattern = &arg[cut..];
    let Some((prefix, suffix)) = pattern.split_once('*') else {
        return arg.to_string();
    };
    if suffix.contains('*') {
        return arg.to_string();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return arg.to_string();
    };
    let mut matches: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|name| {
            name.len() >= prefix.len() + suffix.len() && name.starts_with(prefix) && name.ends_with(suffix)
        })
        .collect();
    matches.sort();
    match matches.pop() {
        Some(name) => format!("{dir}{name}"),
        None => arg.to_string(),
    }
}

/// Program, arguments and forced environment variables of a launch.
pub(crate) type CommandLine = (PathBuf, Vec<String>, Vec<(String, String)>);

/// Program, arguments and forced environment of a launch (pure; unit-tested). The
/// preflight's `Prepared.env` is merged over the entry's `env_set`.
pub(crate) fn command_line(
    entry: &BackendEntry,
    launch: &Launch,
    workspace: &Path,
    placeholders: &Placeholders,
    prepared_env: &BTreeMap<String, String>,
) -> Result<CommandLine, SemanticError> {
    let mut args: Vec<String> = Vec::new();
    let mut env: Vec<(String, String)> = Vec::new();
    for (k, v) in &entry.env_set {
        if !prepared_env.keys().any(|p| p.eq_ignore_ascii_case(k)) {
            env.push((k.clone(), placeholders.expand(v)?));
        }
    }
    for (k, v) in prepared_env {
        env.push((k.clone(), placeholders.expand(v)?));
    }
    if let Launch::Node { guard, script, .. } = launch {
        args.push("--require".into());
        args.push(guard.display().to_string());
        args.push(script.display().to_string());
        env.retain(|(k, _)| !k.eq_ignore_ascii_case("CODEPATH_LSP_WRITE_ROOT"));
        env.push(("CODEPATH_LSP_WRITE_ROOT".into(), workspace.display().to_string()));
    }
    for arg in &entry.args {
        args.push(expand_glob(&placeholders.expand(arg)?));
    }
    Ok((launch.program().to_path_buf(), args, env))
}

/// Requests pipelined to one server process: the configured window, lowered to the
/// backend's `max_in_flight` (setting `semantic.per_backend.<id>`) when it sets one; at
/// least 1. For servers that do the work of concurrent requests one after the other, where a
/// deeper pipeline only turns waiting time into request timeouts.
fn in_flight_limit(configured: usize, own: Option<u32>) -> usize {
    match own {
        None | Some(0) => configured,
        Some(own) => configured.min(own as usize),
    }
    .max(1)
}

/// Launches an entry's server over the workspace.
struct GenericLauncher {
    entry: BackendEntry,
    launch: Launch,
    prepared: Prepared,
    language: Language,
}

impl GenericLauncher {
    fn placeholders(&self, snapshot: &Snapshot, tools: &ToolEnv) -> Placeholders {
        Placeholders::new(
            &self.entry,
            &snapshot.dir,
            &snapshot.outside_dir(),
            snapshot.repo_cache(),
            tools,
            &self.prepared,
        )
    }
}

impl Launcher for GenericLauncher {
    fn prepare(&self, snapshot: &Snapshot, files: &[&SemanticFile<'_>]) -> Result<(), SemanticError> {
        for (name, bytes) in &self.prepared.generated {
            snapshot.write_aux(name, bytes)?;
        }
        let outside = snapshot.outside_dir();
        std::fs::create_dir_all(outside.join("tmp"))?;
        // Scratch directories named by environment values (caches, GOPATH, TEMP) exist
        // before the server starts; they are inside the workspace or the state dir.
        for value in self.entry.env_set.values().chain(self.prepared.env.values()) {
            for (prefix, root) in [("{snapshot}", &snapshot.dir), ("{outside}", &outside)] {
                let Some(rest) = value.strip_prefix(prefix) else { continue };
                if let Some(rel) = crate::registry::safe_relative(rest.trim_start_matches(['/', '\\'])) {
                    // A value naming a FILE a hook wrote on an earlier start (R's empty
                    // `Rprofile`) already exists and stays a file.
                    let target = root.join(rel);
                    if !target.is_file() {
                        std::fs::create_dir_all(target)?;
                    }
                }
            }
        }
        let log = outside.join("prepare.log");
        server_for(&self.entry.id).prepare_workspace(&WorkspaceContext {
            prepared: &self.prepared,
            workspace: &snapshot.dir,
            outside: &outside,
            files,
            log: &log,
        })?;
        Ok(())
    }

    fn command(
        &self,
        snapshot: &Snapshot,
        index: usize,
        tools: &ToolEnv,
        deadline: Instant,
        _heap_cap_mb: Option<u64>,
    ) -> Result<(ServerCommand, ClientOptions), SemanticError> {
        let placeholders = self.placeholders(snapshot, tools);
        let (program, args, env_set) =
            command_line(&self.entry, &self.launch, &snapshot.dir, &placeholders, &self.prepared.env)?;
        let allow: Vec<&str> = self.entry.env_allow.iter().map(String::as_str).collect();
        let set: Vec<(&str, String)> = env_set.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        let command = ServerCommand {
            program,
            args,
            env: clean_env(&allow, &set).into_iter().collect(),
            cwd: snapshot.dir.clone(),
        };
        Ok((command, self.client_options(snapshot, index, tools, deadline, &placeholders)?))
    }

    fn warm_up(
        &self,
        client: &mut LspClient,
        files: &[&SemanticFile<'_>],
        snapshot: &Snapshot,
    ) -> Result<(), SemanticError> {
        server_for(&self.entry.id).warm_up(&mut crate::languages::WarmUpContext {
            prepared: &self.prepared,
            client,
            files,
            snapshot,
        })?;
        Ok(())
    }

    fn loaded(&self, client: &LspClient) -> Result<Vec<Diagnostic>, SemanticError> {
        let diagnostics = server_for(&self.entry.id).check_loaded(&LoadedContext {
            prepared: &self.prepared,
            log_messages: client.log_messages(),
            notifications: client.notifications(),
            diagnostics: client.diagnostics(),
            log: client.log_path(),
        })?;
        Ok(diagnostics)
    }
}

impl GenericLauncher {
    /// The client options of pool process `index` (readiness, policies, expanded settings).
    fn client_options(
        &self,
        snapshot: &Snapshot,
        index: usize,
        tools: &ToolEnv,
        deadline: Instant,
        placeholders: &Placeholders,
    ) -> Result<ClientOptions, SemanticError> {
        let resources = self.entry.resources();
        let mut options = ClientOptions::new(
            self.language,
            tools.request_timeout,
            deadline,
            in_flight_limit(tools.max_in_flight, resources.max_in_flight),
        );
        options.initialization_options = placeholders.expand_json(&self.entry.initialization_options)?;
        options.settings = match placeholders.expand_json(&self.entry.settings)? {
            Value::Null => Value::Object(Default::default()),
            v => v,
        };
        options.stderr_log = Some(snapshot.outside_dir().join(format!("lsp-{index}.stderr.log")));
        options.ready = match &self.entry.ready {
            ReadySpec::Request { method, params } => ReadySpec::Request {
                method: method.clone(),
                params: placeholders.expand_json(params)?,
            },
            other => other.clone(),
        };
        options.ready_timeout = Duration::from_secs(self.entry.ready_timeout_secs().max(1));
        options.progress_grace = Duration::from_millis(resources.ready_grace_ms.unwrap_or(0));
        options.progress_settle = Duration::from_millis(resources.ready_settle_ms.unwrap_or(0));
        options.server_requests = self.entry.server_requests.clone();
        // A notification whose expanded params are `null` is not sent (the preflight chose
        // another one of the list, e.g. Roslyn `project/open` instead of `solution/open`).
        options.after_initialized = self
            .entry
            .after_initialized
            .iter()
            .map(|n| Ok((n.method.clone(), placeholders.expand_json(&n.params)?)))
            .filter(|r: &Result<(String, Value), SemanticError>| !matches!(r, Ok((_, Value::Null))))
            .collect::<Result<_, SemanticError>>()?;
        options.answer_policy = server_for(&self.entry.id).answer_policy();
        options.symbol_poll_query = self.prepared.vars.get("symbol_poll_query").cloned();
        Ok(options)
    }
}

#[cfg(test)]
#[path = "../../tests/unit/backends/generic.rs"]
mod tests;
