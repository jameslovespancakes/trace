//! Data-driven backend registry, schema 2 (DESIGN §1.8; owner install).
//!
//! One embedded asset file per language package (`assets/backends/<stem>.json`, see
//! [`BUILTIN_FILES`]) plus `_runtimes.json` (the trace-managed runtimes servers run on: Node,
//! Temurin JDK 21, the .NET 10 runtime). Each entry lists: languages, the server, how its executable is found
//! ([`ExecutableSpec`]: a tool of the tools directory, a Node script, a runtime program or a
//! toolchain binary), launch arguments with placeholders (§1.8.3), environment, LSP
//! initialization options / settings, the workspace mode, readiness, the build approval it
//! needs, its toolchain / dependency ecosystems, runtimes and the pinned install recipe.
//!
//! `config.json` `semantic.registry` may point to a directory of files in this schema
//! (absolute, never inside an inspected root). Every language is served by at most one entry.
//!
//! The registry changes *data* only: the dedicated backends (`pyright`, `typescript_worker`)
//! keep their Rust implementations; every `lsp` entry (rust-analyzer included) runs through
//! `generic::GenericLsp` built from its entry.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use trace_core::Language;

use crate::languages::WorkspaceMode;

/// Current schema version of the registry files.
pub(crate) const REGISTRY_VERSION: u32 = 2;

/// (file stem, contents) in priority order; [`Registry::builtin`] concatenates them.
pub const BUILTIN_FILES: &[(&str, &str)] = &[
    ("python", include_str!("../../../assets/backends/python.json")),
    // javascript, typescript, tsx
    ("typescript", include_str!("../../../assets/backends/typescript.json")),
    ("rust", include_str!("../../../assets/backends/rust.json")),
    ("go", include_str!("../../../assets/backends/go.json")),
    ("java", include_str!("../../../assets/backends/java.json")),
    ("c-cpp", include_str!("../../../assets/backends/c-cpp.json")),
    ("php", include_str!("../../../assets/backends/php.json")),
    ("bash", include_str!("../../../assets/backends/bash.json")),
    ("csharp", include_str!("../../../assets/backends/csharp.json")),
    ("scala", include_str!("../../../assets/backends/scala.json")),
    ("haskell", include_str!("../../../assets/backends/haskell.json")),
    ("r", include_str!("../../../assets/backends/r.json")),
    // node, jdk, dotnet
    ("_runtimes", include_str!("../../../assets/backends/_runtimes.json")),
];

/// Fixed placeholders (§1.8.3); `{tool:<id>}`, `{runtime:<id>}`, `{json:<name>}` and
/// `{toolchain:<key>}` are prefixes; [`PREPARED_VARS`] are filled by preflights.
pub(crate) const FIXED_PLACEHOLDERS: &[&str] = &[
    "snapshot",
    "snapshot_uri",
    "outside",
    "repo_cache",
    "tools",
    "os",
    "arch",
    "tmp",
    "heap_mb",
];

/// Placeholder prefixes (`{tool:jdtls}`).
pub(crate) const PLACEHOLDER_PREFIXES: &[&str] = &["tool", "runtime", "json", "toolchain"];

/// Names preflights put into `Prepared.vars` (language packages extend this list).
pub(crate) const PREPARED_VARS: &[&str] = &[
    "toolchain",
    "jdk",
    "cdb_dir",
    "query_driver",
    "jdtls_config",
    "jdtls_os",
    "solution_uri",
];

/// One registry file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct RegistryFile {
    pub schema: u32,
    #[serde(default)]
    pub backends: Vec<BackendEntry>,
    #[serde(default)]
    pub runtimes: Vec<InstallSpec>,
}

/// The whole registry (every file concatenated in priority order).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Registry {
    pub backends: Vec<BackendEntry>,
    pub runtimes: Vec<InstallSpec>,
}

/// Implementation used for an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// `pyright::Pyright` (node + lsp_guard, process pool).
    Pyright,
    /// `typescript::TypeScript` (embedded worker over the TypeScript 7 API).
    TypescriptWorker,
    /// `generic::GenericLsp` built from this entry.
    Lsp,
}

/// One backend.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BackendEntry {
    /// "pyright" | "typescript" | "lsp:<server>"
    pub id: String,
    pub kind: BackendKind,
    pub languages: Vec<Language>,
    #[serde(default)]
    pub language_ids: Vec<String>,
    pub server: ServerInfo,
    pub executable: ExecutableSpec,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env_allow: Vec<String>,
    #[serde(default)]
    pub env_set: BTreeMap<String, String>,
    #[serde(default)]
    pub initialization_options: Value,
    #[serde(default)]
    pub settings: Value,
    #[serde(default)]
    pub workspace: WorkspaceSpec,
    #[serde(default)]
    pub ready: ReadySpec,
    #[serde(default)]
    pub server_requests: ServerRequestPolicy,
    /// Notifications sent right after `initialized` (placeholders expanded).
    #[serde(default)]
    pub after_initialized: Vec<NotificationSpec>,
    #[serde(default)]
    pub shard: ShardMode,
    #[serde(default)]
    pub requires_build: Option<BuildSpec>,
    #[serde(default)]
    pub toolchain: Option<ToolchainSpec>,
    #[serde(default)]
    pub deps: Option<DepsSpec>,
    /// Runtime ids from `_runtimes.json` the server runs on ("node", "jdk", "dotnet").
    #[serde(default)]
    pub runtime: Vec<String>,
    #[serde(default)]
    pub install: Option<InstallSpec>,
    #[serde(default)]
    pub safety: String,
}

impl BackendEntry {
    /// The resource numbers of this backend: setting `semantic.per_backend.<id>` (processes,
    /// requests in flight, readiness bounds, heap). Tunables live in the settings only, so the
    /// backend files (and the tool fingerprint hashing them) hold launch and protocol.
    pub fn resources(&self) -> trace_core::config::BackendResources {
        trace_core::config::current()
            .semantic
            .per_backend
            .get(&self.id)
            .cloned()
            .unwrap_or_default()
    }

    /// Bound of `initialize` + the readiness wait: the backend's `ready_timeout_secs`, else
    /// setting `semantic.ready_timeout_secs`.
    pub fn ready_timeout_secs(&self) -> u64 {
        self.resources()
            .ready_timeout_secs
            .unwrap_or(trace_core::config::current().semantic.ready_timeout_secs)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub license: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum ExecutableSpec {
    /// `<tools>/<tool>/<installed version>/<path>` (per MANIFEST); `.exe` appended on
    /// Windows when missing.
    Tool { tool: String, path: String },
    /// `<runtime node> --require <lsp_guard> <tools>/<tool>/<version>/<script>`
    NodeScript { tool: String, script: String },
    /// Inside the detected toolchain root: first existing of `paths`.
    Toolchain { ecosystem: String, paths: Vec<String> },
    /// A runtime executable ("java", "dotnet") with the tool's files as arguments.
    Runtime { runtime: String, program: String },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceSpec {
    pub mode: WorkspaceMode,
    /// Snapshot: config files copied (relative globs / basenames): "go.mod", "**/pom.xml".
    pub configs: Vec<String>,
    /// Mirror: git-ignored paths that are still copied (restore outputs).
    pub include_ignored: Vec<String>,
    /// Mirror: never copied.
    pub exclude: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReadySpec {
    /// Requests block until answers are complete (Pyright, TS worker).
    #[default]
    None,
    /// $/progress: wait up to the backend's `ready_grace_ms` for a first begin, then until
    /// all ended and quiet `ready_settle_ms` (setting `semantic.per_backend.<id>`).
    Progress,
    /// rust-analyzer experimental/serverStatus quiescent.
    Quiescent,
    /// jdtls language/status ServiceReady, then all progress ended + 2 s quiet.
    LanguageStatus,
    /// A server notification.
    Notification { method: String },
    /// A client request whose answer means ready.
    Request { method: String, params: Value },
    /// A window/logMessage containing the text, then progress ended.
    Log { contains: String },
    /// R languageserver: poll workspace/symbol until the count is non-zero and stable twice.
    SymbolPoll,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationMissing {
    /// Answer `{}` (today's behaviour).
    #[default]
    Object,
    /// Answer `null` (Roslyn crashes on `{}`).
    Null,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerRequestPolicy {
    /// window/showMessageRequest: action title -> answer it (true); all others null.
    pub message_actions: BTreeMap<String, bool>,
    /// workspace/configuration for an unknown section.
    pub configuration_missing: ConfigurationMissing,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShardMode {
    #[default]
    Files,
    Requests,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NotificationSpec {
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildWhen {
    Always,
    /// Only when the preflight finds a build file.
    BuildFiles,
    DecidedByHooks,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BuildSpec {
    /// "Gradle", "Maven", "Cargo", "CMake", "MSBuild", "sbt", "cabal", ...
    pub tool: String,
    /// "this project's build scripts", ...
    pub runs: String,
    pub when: BuildWhen,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolchainSpec {
    /// `trace_env::EcosystemId::as_str()`.
    pub ecosystem: String,
    /// "a JDK 17 or newer" | "Go" | "the .NET SDK"
    pub needs: String,
    /// "Install one from https://adoptium.net"
    pub install: String,
    /// PHP: the server works without; only hints.
    #[serde(default)]
    pub optional: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DepsSpec {
    pub ecosystem: String,
}

// ---------------------------------------------------------------------------------------------
// Install recipes (§1.8.4)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstallSpec {
    /// Tools dir name: "pyright", "jdtls", "node", "jdk", ...
    pub id: String,
    pub version: String,
    pub license: String,
    /// "Java language server", "Node.js runtime"
    pub display: String,
    /// Progress-line name: "Pyright", "Temurin JDK", "Intelephense".
    pub product: String,
    /// PLAN decision 11: install only after the user accepts this licence.
    #[serde(default)]
    pub licence_gate: Option<LicenceGate>,
    #[serde(flatten)]
    pub recipe: Recipe,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LicenceGate {
    pub url: String,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "recipe", rename_all = "snake_case")]
pub enum Recipe {
    /// Per-platform archive (zip / tar.gz / tar.xz / single .gz / raw binary / nupkg).
    Archive {
        artifacts: Vec<Artifact>,
        executables: Vec<String>,
    },
    /// npm packages from an embedded lock (no npm, no scripts; SRI sha512).
    Npm { packages: Vec<NpmPackage> },
    /// `go install <module>@<version>` with the user's Go into the tools dir.
    GoInstall {
        module: String,
        versions: Vec<GoInstallVersion>,
    },
    /// R package closure as BINARY packages from a dated Posit Package Manager snapshot.
    RPackage {
        package: String,
        snapshot_date: String,
        files: Vec<RBinary>,
    },
    /// HLS for the project's GHC (ghcup).
    Ghcup {
        hls_version: String,
        supported_ghc: Vec<String>,
    },
    /// Pinned coursier launcher + `cs fetch` of pinned coordinates.
    Coursier {
        launcher: Vec<Artifact>,
        fetch: Vec<String>,
        lock: Vec<PinnedFile>,
        main_class: String,
    },
    /// Nothing to download: the server ships with the toolchain.
    FromToolchain { ecosystem: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    /// `Platform::key()` or "any".
    pub platform: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub strip: u32,
    #[serde(default)]
    pub strip_prefix: Option<String>,
    #[serde(default)]
    pub subdir: Option<String>,
    /// Linux builds linked against glibc: the lowest glibc they run on ("2.28" for Node 24).
    #[serde(default)]
    pub min_glibc: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NpmPackage {
    /// node_modules/x or node_modules/@s/x
    pub path: String,
    pub version: String,
    pub url: String,
    pub integrity: String,
    #[serde(default)]
    pub os: Vec<String>,
    #[serde(default)]
    pub cpu: Vec<String>,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GoInstallVersion {
    pub min_go: String,
    pub version: String,
    pub h1: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PinnedFile {
    pub name: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
}

/// One Posit binary package file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RBinary {
    pub platform: String,
    #[serde(default)]
    pub distro: Option<String>,
    pub r_minor: String,
    pub package: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
}

// ---------------------------------------------------------------------------------------------
// Loading and validation
// ---------------------------------------------------------------------------------------------

impl Registry {
    /// The embedded registry. The `builtin_registry_is_valid` test proves every embedded file
    /// parses and validates; an invalid embedded file is a build defect, reported by that
    /// test, so this never panics at runtime (an unparsable file contributes nothing).
    pub fn builtin() -> Registry {
        let mut registry = Registry {
            backends: Vec::new(),
            runtimes: Vec::new(),
        };
        for (_, text) in BUILTIN_FILES {
            if let Ok(file) = serde_json::from_str::<RegistryFile>(text) {
                registry.backends.extend(file.backends);
                registry.runtimes.extend(file.runtimes);
            }
        }
        registry
    }

    /// `semantic.registry` override: a directory of `*.json` files in this schema (file-name
    /// order), validated.
    pub(crate) fn load_dir(dir: &Path) -> Result<Registry, crate::SemanticError> {
        let protocol = |m: String| crate::SemanticError::Protocol(m);
        let mut names: Vec<PathBuf> = std::fs::read_dir(dir)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        names.sort();
        let mut registry = Registry {
            backends: Vec::new(),
            runtimes: Vec::new(),
        };
        for path in names {
            let bytes = std::fs::read(&path)?;
            let file: RegistryFile =
                serde_json::from_slice(&bytes).map_err(|e| protocol(format!("{}: {e}", path.display())))?;
            if file.schema != REGISTRY_VERSION {
                return Err(protocol(format!(
                    "{}: schema {} (expected {REGISTRY_VERSION})",
                    path.display(),
                    file.schema
                )));
            }
            registry.backends.extend(file.backends);
            registry.runtimes.extend(file.runtimes);
        }
        registry
            .validate()
            .map_err(|e| protocol(format!("{}: {e}", dir.display())))?;
        Ok(registry)
    }

    /// Structural checks: unique ids, every language served by at most one entry, `lsp:`
    /// prefix and aligned language ids for `lsp` entries, known placeholders, safe relative
    /// tool paths, pinned https install artifacts (64-hex sha256 or an npm sha512 integrity)
    /// for known platform keys, complete recipes, https licence gates, runtimes known (archive
    /// records without a gate), and a build approval for mirror workspaces (unless `safety`
    /// justifies none).
    pub fn validate(&self) -> Result<(), String> {
        let mut ids = BTreeSet::new();
        let mut served: HashMap<Language, &str> = HashMap::new();
        let runtime_ids: BTreeSet<&str> = self.runtimes.iter().map(|r| r.id.as_str()).collect();
        for r in &self.runtimes {
            validate_install(&r.id, r)?;
            // Runtimes install without a question (default languages run on them).
            if !matches!(r.recipe, Recipe::Archive { .. }) || r.licence_gate.is_some() {
                return Err(format!("runtime {}: must be an archive without a licence gate", r.id));
            }
        }
        if runtime_ids.len() != self.runtimes.len() {
            return Err("duplicate runtime id".to_string());
        }
        for b in &self.backends {
            if !ids.insert(b.id.as_str()) {
                return Err(format!("duplicate backend id {}", b.id));
            }
            if b.languages.is_empty() {
                return Err(format!("{}: no languages", b.id));
            }
            for l in &b.languages {
                if let Some(other) = served.insert(*l, b.id.as_str()) {
                    return Err(format!("{l} is served by both {other} and {}", b.id));
                }
            }
            if b.kind == BackendKind::Lsp {
                if !b.id.starts_with("lsp:") {
                    return Err(format!("{}: lsp entries are named lsp:<server>", b.id));
                }
                if b.language_ids.len() != b.languages.len() {
                    return Err(format!("{}: language_ids must align with languages", b.id));
                }
            }
            match &b.executable {
                ExecutableSpec::Tool { tool, path } => check_rel(&b.id, tool, path)?,
                ExecutableSpec::NodeScript { tool, script } => check_rel(&b.id, tool, script)?,
                ExecutableSpec::Runtime { runtime, program } => {
                    check_rel(&b.id, runtime, program)?;
                    if !runtime_ids.contains(runtime.as_str()) {
                        return Err(format!("{}: unknown runtime {runtime}", b.id));
                    }
                }
                ExecutableSpec::Toolchain { ecosystem, paths } => {
                    if trace_env::EcosystemId::parse(ecosystem).is_none() {
                        return Err(format!("{}: unknown ecosystem {ecosystem}", b.id));
                    }
                    for p in paths {
                        check_rel(&b.id, ecosystem, p)?;
                    }
                }
            }
            for r in &b.runtime {
                if !runtime_ids.contains(r.as_str()) {
                    return Err(format!("{}: unknown runtime {r}", b.id));
                }
            }
            for spec in [
                b.toolchain.as_ref().map(|t| &t.ecosystem),
                b.deps.as_ref().map(|d| &d.ecosystem),
            ]
            .into_iter()
            .flatten()
            {
                if trace_env::EcosystemId::parse(spec).is_none() {
                    return Err(format!("{}: unknown ecosystem {spec}", b.id));
                }
            }
            for text in entry_strings(b) {
                check_placeholders(&b.id, &text)?;
            }
            if let Some(install) = &b.install {
                validate_install(&b.id, install)?;
            }
            if b.workspace.mode == WorkspaceMode::Mirror
                && b.requires_build.is_none()
                && !b.safety.contains("requires_build: null")
            {
                return Err(format!(
                    "{}: mirror workspaces need requires_build (or `requires_build: null` justified in safety)",
                    b.id
                ));
            }
        }
        Ok(())
    }

    /// Entry by id.
    pub fn entry(&self, id: &str) -> Option<&BackendEntry> {
        self.backends.iter().find(|b| b.id == id)
    }

    /// The entry serving `language` (at most one, validated).
    pub fn entry_for(&self, language: Language) -> Option<&BackendEntry> {
        self.backends.iter().find(|b| b.languages.contains(&language))
    }

    /// Runtime install record by id ("node", "jdk", "dotnet").
    pub fn runtime(&self, id: &str) -> Option<&InstallSpec> {
        self.runtimes.iter().find(|r| r.id == id)
    }
}

fn check_rel(id: &str, what: &str, rel: &str) -> Result<(), String> {
    if what.is_empty() || safe_relative(rel).is_none() {
        return Err(format!("{id}: path {rel:?} must be relative to {what:?}"));
    }
    Ok(())
}

/// Every string of an entry that may carry placeholders.
fn entry_strings(b: &BackendEntry) -> Vec<String> {
    fn strings(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(s) => out.push(s.clone()),
            Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
            Value::Object(map) => map.values().for_each(|v| strings(v, out)),
            _ => {}
        }
    }
    let mut all: Vec<String> = b.args.clone();
    all.extend(b.env_set.values().cloned());
    strings(&b.initialization_options, &mut all);
    strings(&b.settings, &mut all);
    for n in &b.after_initialized {
        strings(&n.params, &mut all);
    }
    all
}

/// `{name}` tokens of `text` (outermost braces only).
pub fn placeholders(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else { break };
        out.push(&after[..end]);
        rest = &after[end + 1..];
    }
    out
}

fn check_placeholders(id: &str, text: &str) -> Result<(), String> {
    for name in placeholders(text) {
        let known = FIXED_PLACEHOLDERS.contains(&name)
            || PREPARED_VARS.contains(&name)
            || name
                .split_once(':')
                .is_some_and(|(prefix, rest)| PLACEHOLDER_PREFIXES.contains(&prefix) && !rest.is_empty());
        if !known {
            return Err(format!("{id}: unknown placeholder {{{name}}} in {text:?}"));
        }
    }
    Ok(())
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// An artifact platform: "any" or a [`trace_env::os::Platform::key`] shape
/// (`windows|linux|macos` - arch, optional `-musl` on Linux).
pub(crate) fn valid_platform_key(key: &str) -> bool {
    if key == "any" {
        return true;
    }
    let mut parts = key.split('-');
    let (Some(os), Some(arch)) = (parts.next(), parts.next()) else {
        return false;
    };
    let musl = match parts.next() {
        None => false,
        Some("musl") => true,
        Some(_) => return false,
    };
    parts.next().is_none()
        && ["windows", "linux", "macos"].contains(&os)
        && !arch.is_empty()
        && arch
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && (!musl || os == "linux")
}

/// A registry path prefix (`strip_prefix`, `subdir`): relative, may end with `/`.
fn safe_prefix(prefix: &str) -> bool {
    safe_relative(prefix.trim_end_matches('/')).is_some()
}

fn validate_install(owner: &str, spec: &InstallSpec) -> Result<(), String> {
    if spec.id.is_empty() || safe_relative(&spec.id).is_none() || spec.id.contains('/') {
        return Err(format!("{owner}: invalid install id {:?}", spec.id));
    }
    if spec.version.is_empty()
        || spec.license.is_empty()
        || spec.display.is_empty()
        || spec.product.is_empty()
    {
        return Err(format!("{owner}: install records need version, license, display and product"));
    }
    if let Some(gate) = &spec.licence_gate {
        if !gate.url.starts_with("https://") || gate.summary.trim().is_empty() {
            return Err(format!("{owner}: a licence gate needs an https url and a one-line summary"));
        }
    }
    let https = |url: &str| -> Result<(), String> {
        if url.starts_with("https://") {
            Ok(())
        } else {
            Err(format!("{owner}: {url} is not https"))
        }
    };
    let artifacts = |list: &[Artifact]| -> Result<(), String> {
        for a in list {
            if !is_sha256(&a.sha256) {
                return Err(format!("{owner}: artifact {} has no sha256", a.url));
            }
            https(&a.url)?;
            if !valid_platform_key(&a.platform) {
                return Err(format!("{owner}: unknown artifact platform {:?}", a.platform));
            }
            for prefix in [&a.strip_prefix, &a.subdir].into_iter().flatten() {
                if !safe_prefix(prefix) {
                    return Err(format!("{owner}: unsafe archive prefix {prefix:?}"));
                }
            }
            if let Some(g) = &a.min_glibc {
                if !a.platform.starts_with("linux-") || trace_env::os::Version::parse(g).is_none() {
                    return Err(format!("{owner}: min_glibc {g:?} only on linux artifacts"));
                }
            }
        }
        Ok(())
    };
    // `coordinates`: the names are Maven coordinates (`group:artifact`, the Coursier lock;
    // the file lives at its URL's cache path), otherwise plain file names.
    let pinned = |list: &[PinnedFile], coordinates: bool| -> Result<(), String> {
        for f in list {
            if !is_sha256(&f.sha256) {
                return Err(format!("{owner}: {} has no sha256", f.url));
            }
            https(&f.url)?;
            let no_path = !f.name.contains(['/', '\\']) && !f.name.contains("..");
            let ok = if coordinates {
                // `group:artifact` (never a path).
                no_path && f.name.split(':').count() == 2 && f.name.split(':').all(|p| !p.trim().is_empty())
            } else {
                no_path && safe_relative(&f.name).is_some()
            };
            if !ok {
                return Err(format!("{owner}: pinned file name {:?} is not a plain name", f.name));
            }
        }
        Ok(())
    };
    match &spec.recipe {
        Recipe::Archive {
            artifacts: a,
            executables,
        } => {
            artifacts(a)?;
            for e in executables {
                check_rel(owner, &spec.id, e)?;
            }
        }
        Recipe::Npm { packages } => {
            if packages.is_empty() {
                return Err(format!("{owner}: npm recipe without packages"));
            }
            for p in packages {
                if !p.integrity.split_whitespace().any(|i| i.starts_with("sha512-")) {
                    return Err(format!("{owner}: npm package {} needs a sha512 integrity", p.path));
                }
                https(&p.url)?;
                if safe_relative(&p.path).is_none() || !p.path.starts_with("node_modules/") {
                    return Err(format!("{owner}: npm path {:?} must be node_modules/<name>", p.path));
                }
            }
        }
        Recipe::GoInstall { module, versions } => {
            if module.is_empty() || versions.is_empty() {
                return Err(format!("{owner}: go_install needs a module and versions"));
            }
            for v in versions {
                if trace_env::os::Version::parse(&v.min_go).is_none() || v.version.is_empty() {
                    return Err(format!("{owner}: go_install version {:?} needs min_go", v.version));
                }
            }
        }
        Recipe::Coursier {
            launcher,
            lock,
            main_class,
            ..
        } => {
            artifacts(launcher)?;
            pinned(lock, true)?;
            if main_class.is_empty() {
                return Err(format!("{owner}: coursier recipe needs main_class"));
            }
        }
        Recipe::RPackage { package, files, .. } => {
            if package.is_empty() || files.is_empty() {
                return Err(format!("{owner}: r_package needs the package and its files"));
            }
            for f in files {
                if !is_sha256(&f.sha256) {
                    return Err(format!("{owner}: {} has no sha256", f.url));
                }
                https(&f.url)?;
                if !valid_platform_key(&f.platform) || f.platform == "any" {
                    return Err(format!("{owner}: R binary platform {:?}", f.platform));
                }
                if f.distro.is_some() != f.platform.starts_with("linux-") {
                    return Err(format!("{owner}: R binaries name a distro exactly on Linux ({})", f.url));
                }
                if trace_env::os::Version::parse(&f.r_minor).is_none() {
                    return Err(format!("{owner}: R minor {:?}", f.r_minor));
                }
            }
        }
        Recipe::Ghcup {
            hls_version,
            supported_ghc,
        } => {
            if hls_version.is_empty() || supported_ghc.is_empty() {
                return Err(format!("{owner}: ghcup recipe needs hls_version and supported_ghc"));
            }
        }
        Recipe::FromToolchain { ecosystem } => {
            if trace_env::EcosystemId::parse(ecosystem).is_none() {
                return Err(format!("{owner}: unknown ecosystem {ecosystem}"));
            }
        }
    }
    Ok(())
}

/// A registry-relative path (`gopls/bin`, `node_modules/x/cli.js`): `/`-separated, no
/// empty, `.` or `..` segments, never absolute or drive-qualified.
pub(crate) fn safe_relative(rel: &str) -> Option<PathBuf> {
    if rel.is_empty() || rel.starts_with('/') || rel.starts_with('\\') || rel.contains(':') {
        return None;
    }
    let mut out = PathBuf::new();
    for part in rel.split(['/', '\\']) {
        if part.is_empty() || part == "." || part == ".." {
            return None;
        }
        out.push(part);
    }
    Some(out)
}

#[cfg(test)]
#[path = "../tests/unit/registry.rs"]
mod tests;
