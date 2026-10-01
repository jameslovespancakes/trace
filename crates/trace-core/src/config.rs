//! Central settings ([`Settings`]).
//!
//! Every tunable of trace has exactly one default, in `assets/config/defaults.jsonc`
//! (embedded; JSON with comments). The user's `<home>/config.json` overrides any of its keys
//! (same layout, plain JSON; [`Settings::load`]); a key that names no setting is an error that
//! names it and the closest setting ([`unknown_setting`]). Nothing is ever read from an
//! inspected repository. Settings are loaded once per command
//! ([`Settings::load`], then [`install`]) and passed down to every layer that has a parameter;
//! engine code without a settings parameter reads [`current`]. `"auto"` values are computed
//! in one function ([`Settings::auto`]).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::error::{CoreError, Result};
use crate::inventory::InventoryOptions;
use crate::paths::normalize_lexically;

/// The embedded defaults: every tunable with its comment.
const DEFAULTS: &str = include_str!("../../../assets/config/defaults.jsonc");

/// All settings (`assets/config/defaults.jsonc` sections).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub memory: MemorySettings,
    pub workers: WorkerSettings,
    pub semantic: SemanticSettings,
    pub inventory: InventoryOptions,
    pub syntax: SyntaxSettings,
    pub flow: FlowSettings,
    pub derive: DeriveSettings,
    pub bridges: BridgeSettings,
    pub watch: WatchSettings,
    pub analysis: AnalysisSettings,
    pub cache: CacheSettings,
    pub debug: DebugSettings,
    /// Absolute directories trace must never inspect (in addition to
    /// `TRACE_FORBIDDEN_ROOTS`); see `paths::forbidden_roots`.
    pub forbidden_roots: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemorySettings {
    /// Memory budget of one analyzer pool in MB (0 = unbounded). Pyright picks its process
    /// count K as the largest whose estimated pool memory (`trace_semantic::estimate_mb`) fits
    /// the budget, never below 1, and adds a Node heap cap only when even one process is
    /// estimated above it (SPEC §4.3, §8.8).
    pub budget_mb: u64,
    /// Estimated fixed memory of one Pyright process in MB.
    pub server_base_mb: u64,
    /// Growth of a pool per extra process in percent of one process's estimate.
    pub server_extra_percent: u64,
    /// Estimated memory of one request-sharded process in MB.
    pub request_shard_mb: u64,
    /// Library source kept parsed while library derivations run (MB of source).
    pub library_parse_mb: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkerSettings {
    /// Most analyzer processes of a sharded pool (resolved by [`Settings::auto`]).
    pub server_processes: Count,
    /// Upper bound of an automatic `server_processes`.
    pub server_processes_max: usize,
    /// Most processes of a request-sharded pool (resolved by [`Settings::auto`]).
    pub max_request_shards: usize,
    /// Estimated requests worth one more request-sharded process.
    pub requests_per_shard: u64,
    /// Fewest queried files worth a process of their own.
    pub min_files_per_process: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticSettings {
    /// Semantic tools directory (absolute; language servers and their runtimes installed by
    /// trace, [`semantic_tools_dir`]).
    pub tools_dir: Option<PathBuf>,
    /// Backend registry override (absolute path to a directory of JSON files in the schema
    /// of `trace/assets/backends/*.json`, schema 2); `None` = the registry embedded in the
    /// binary.
    pub registry: Option<PathBuf>,
    /// Install the language servers of default languages automatically on first need (PLAN
    /// decision 10). `TRACE_NO_AUTO_INSTALL=1` and `TRACE_OFFLINE=1` turn it off
    /// ([`auto_install_enabled`]).
    pub auto_install: bool,
    pub request_timeout_secs: u64,
    pub session_deadline_secs: u64,
    /// Maximum in-flight LSP requests per session.
    pub max_in_flight: usize,
    /// Readiness bound of a backend whose registry entry names none.
    pub ready_timeout_secs: u64,
    /// Bound on `typeHierarchy/subtypes` requests per shard.
    pub max_subtype_requests: usize,
    /// Bound on `textDocument/implementation` requests at library calls per shard.
    pub max_library_dispatch_requests: usize,
    /// Bound of one approved toolchain / build step of a preflight.
    pub build_step_timeout_secs: u64,
    /// Bound of one install command.
    pub install_timeout_secs: u64,
    /// Parallel package downloads of one npm install.
    pub parallel_downloads: usize,
    /// Largest accepted download (MB).
    pub max_download_mb: u64,
    /// Largest unpacked size of one archive (MB).
    pub max_unpacked_mb: u64,
    /// Largest repository mirrored into a server workspace (MB).
    pub max_mirror_mb: u64,
    /// Resources per backend id.
    pub per_backend: BTreeMap<String, BackendResources>,
}

/// Resource numbers of one backend (`semantic.per_backend.<id>`; unset keys fall back to the
/// semantic settings).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BackendResources {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_in_flight: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_timeout_secs: Option<u64>,
    /// JVM heap of the server (`-Xmx`) in MB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heap_mb: Option<u64>,
    /// `$/progress` readiness: wait for a first begin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_grace_ms: Option<u64>,
    /// `$/progress` readiness: quiet period after the last end.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_settle_ms: Option<u64>,
}

/// Syntax extraction (`syntax`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyntaxSettings {
    /// Per-file parse deadline in milliseconds.
    pub parse_timeout_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FlowSettings {
    /// Safety bound on rounds per value-flow solver phase.
    pub max_iterations: usize,
    /// Evaluation budget of one solve: `eval_budget_base + eval_budget_per_constraint x
    /// constraints`, unless `eval_budget` is set.
    pub eval_budget_base: u64,
    pub eval_budget_per_constraint: u64,
    pub eval_budget: Option<u64>,
    /// Receiver contexts analysed per method before new receivers are widened to their class.
    pub max_contexts: usize,
    /// Values stored per slot (and allocations per (class, attribute)) before widening.
    pub max_slot_values: usize,
    /// Rounds of the consumed-parameter fixpoint (forwarding chains).
    pub max_consume_rounds: usize,
    /// Objects one member lookup visits through delegates (mixins, prototypes).
    pub max_delegate_visits: usize,
    /// Saturated slots named in the `flow_bound` diagnostic.
    pub bound_examples: usize,
    /// Levels of site composition by receiver (a composed site's parent chain).
    pub max_compose_depth: u8,
    /// Depth of receiver-type resolution (aliases, fields, returns).
    pub max_type_depth: u32,
}

/// Library derivation (`derive`): bounds of reading installed library source. Summaries
/// derived with other values than the defaults are cached under their own key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeriveSettings {
    /// Library files loaded for one derivation (the target file, its namespace group and
    /// imports).
    pub max_units: usize,
    /// Fixed-point rounds per derivation phase.
    pub max_rounds: usize,
    /// Import hops followed from the target file.
    pub import_depth: usize,
    /// Library files larger than this are not read (bytes).
    pub max_file_bytes: u64,
    /// Expression evaluation depth.
    pub max_eval_depth: usize,
    /// Slots of one attribute name compared for class relationships per round.
    pub max_related_slots: usize,
    /// Files of one package read for its declared types (typed dispatch).
    pub max_package_files: usize,
    /// Size bound of the per-machine summary cache (bytes; pruned once per process).
    pub cache_max_bytes: u64,
}

/// Cross-language bridges (`bridges`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BridgeSettings {
    /// Detect bridges while indexing (library callers can still skip traversing them with
    /// `OpenOptions::no_bridges`).
    pub enabled: bool,
    /// Detect HTTP route <-> client bridges (framework tables, literal URL templates).
    pub http: bool,
    /// Detect weak (possible-only) boundaries: subprocess, FFI lookups, message names.
    pub weak: bool,
    /// Hand-written bridge manifests (codepath_next format; absolute paths outside every
    /// inspected root). Entries are labelled `user_contract`.
    pub manifests: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WatchSettings {
    /// Quiet period after the last relevant event before the changes are delivered.
    pub debounce_ms: u64,
    /// Upper bound on how long a continuous stream of events can postpone delivery.
    pub max_delay_ms: u64,
    /// Polling interval (fallback mode).
    pub poll_ms: u64,
    /// How long the watch loop waits for changes before it looks at the root and idle work again.
    pub tick_ms: u64,
    /// Stale dependents resolved per batch.
    pub stale_batch: usize,
    /// A failed update is not retried before this long (new changes retry at once).
    pub retry_after_ms: u64,
    /// Retry this soon when another process holds the build lock.
    pub retry_locked_ms: u64,
}

/// Queries and the workspace (`analysis`): waiting for other processes, status thresholds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnalysisSettings {
    /// Longest wait for another process updating the index, then the `locked` error (seconds).
    pub wait_limit_secs: u64,
    /// Polling interval while waiting for another process (ms).
    pub wait_poll_ms: u64,
    /// How long a command waits for a running `trace index --watch` to publish the update of
    /// changed files before it updates in-process itself (ms).
    pub watch_grace_ms: u64,
    /// How long a command waits for a running `trace index --watch` to publish its next batch
    /// of stale dependents before resolving them in-process (seconds).
    pub stale_grace_secs: u64,
    /// `trace status` warns when a semantic language resolves less than this share of its
    /// in-repository calls.
    pub resolution_warn: f64,
}

/// Compaction of the index delta journal (`index.bin.d<n>`, [`crate::cache`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CacheSettings {
    /// An update rewrites the base once the carried file blocks exceed `base size /
    /// compact_ratio`; an idle process compacts once the whole journal does.
    pub compact_ratio: u64,
    /// An idle process compacts once the journal holds this many segments.
    pub idle_compact_segments: usize,
}

/// Diagnosis switches (stderr output and A/B verification; results never change).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DebugSettings {
    /// Print by-name injection bindings and flow candidates.
    pub fixtures: bool,
    /// Print the value-flow items evaluated per round.
    pub flow_items: bool,
    /// Syntax answers of SPEC section 8.8 (false: ask the server for everything).
    pub syntax_answers: bool,
    /// Backend ids (or `all`) whose calls are resolved with `definition` instead of call
    /// hierarchy.
    pub calls_by_definition: Vec<String>,
}

/// A count that may be `"auto"` ([`Settings::auto`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Count {
    Auto,
    Fixed(usize),
}

impl Serialize for Count {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Count::Auto => s.serialize_str("auto"),
            Count::Fixed(n) => s.serialize_u64(*n as u64),
        }
    }
}

impl<'de> Deserialize<'de> for Count {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Count, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Number(usize),
            Text(String),
        }
        match Raw::deserialize(d)? {
            Raw::Number(n) => Ok(Count::Fixed(n)),
            Raw::Text(t) if t == "auto" => Ok(Count::Auto),
            Raw::Text(t) => {
                Err(serde::de::Error::custom(format!("expected a number or \"auto\", found \"{t}\"")))
            }
        }
    }
}

/// Values of the `"auto"` settings on this machine ([`Settings::auto`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Auto {
    /// Logical cores (1 when unknown).
    pub cores: usize,
    /// Most analyzer processes of a sharded pool (`workers.server_processes`).
    pub server_processes: usize,
    /// Most processes of a request-sharded pool (`workers.max_request_shards`).
    pub request_shards: usize,
}

/// The embedded defaults as a JSON value.
fn defaults_value() -> &'static Value {
    static VALUE: OnceLock<Value> = OnceLock::new();
    VALUE.get_or_init(|| {
        // Proven invariant: the embedded file is valid JSONC (test `rule_defaults_parse`).
        crate::formats::jsonc::parse(DEFAULTS).expect("assets/config/defaults.jsonc is valid JSONC")
    })
}

/// The default settings.
pub fn defaults() -> &'static Settings {
    static SETTINGS: OnceLock<Settings> = OnceLock::new();
    SETTINGS.get_or_init(|| {
        // Proven invariant: the embedded defaults are complete and valid (test
        // `rule_defaults_parse`).
        serde_json::from_value(defaults_value().clone())
            .expect("assets/config/defaults.jsonc matches Settings")
    })
}

static CURRENT: OnceLock<Settings> = OnceLock::new();

/// Make `settings` the settings of this process (the first call wins: settings are loaded
/// once per command).
pub fn install(settings: &Settings) {
    let _ = CURRENT.set(settings.clone());
}

/// The settings of this process ([`install`]), or the defaults before any were installed.
pub fn current() -> &'static Settings {
    CURRENT.get().unwrap_or_else(defaults)
}

macro_rules! default_from_settings {
    ($($ty:ty => $field:ident),*) => {$(
        impl Default for $ty {
            fn default() -> Self {
                defaults().$field.clone()
            }
        }
    )*};
}

default_from_settings!(
    MemorySettings => memory,
    WorkerSettings => workers,
    SemanticSettings => semantic,
    InventoryOptions => inventory,
    SyntaxSettings => syntax,
    FlowSettings => flow,
    DeriveSettings => derive,
    BridgeSettings => bridges,
    WatchSettings => watch,
    AnalysisSettings => analysis,
    CacheSettings => cache,
    DebugSettings => debug
);

impl Default for Settings {
    fn default() -> Self {
        defaults().clone()
    }
}

/// Merge `over` into `base`: objects key by key, anything else replaces.
fn merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (slot, v) => *slot = v,
    }
}

/// The settings section whose keys are backend ids chosen by the user, not setting names;
/// each entry uses the [`BackendResources`] keys.
const PER_BACKEND: &str = "semantic.per_backend";

/// The keys of one `semantic.per_backend.<id>` entry (every field of [`BackendResources`]).
fn backend_resource_keys() -> Vec<String> {
    let every = BackendResources {
        processes: Some(0),
        max_in_flight: Some(0),
        ready_timeout_secs: Some(0),
        heap_mb: Some(0),
        ready_grace_ms: Some(0),
        ready_settle_ms: Some(0),
    };
    match serde_json::to_value(every) {
        Ok(Value::Object(map)) => map.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// The first key of the user's document that names no setting, as (dotted key, the closest
/// setting when one is close): every object level uses the keys of the defaults, except the
/// backend ids of `semantic.per_backend`, whose entries use the [`BackendResources`] keys.
/// Wrong value types are left to deserialization.
pub(crate) fn unknown_setting(user: &Value) -> Option<(String, Option<String>)> {
    fn walk(user: &Value, known: &Value, prefix: &str) -> Option<(String, Option<String>)> {
        let (Value::Object(map), Value::Object(known)) = (user, known) else {
            return None;
        };
        let dotted = |key: &str| {
            if prefix.is_empty() {
                key.to_string()
            } else {
                format!("{prefix}.{key}")
            }
        };
        for (key, value) in map {
            if prefix == PER_BACKEND {
                let fields = backend_resource_keys();
                let entry = dotted(key);
                for field in value.as_object().into_iter().flat_map(|m| m.keys()) {
                    if !fields.contains(field) {
                        let near = closest(field, &fields).map(|f| format!("{entry}.{f}"));
                        return Some((format!("{entry}.{field}"), near));
                    }
                }
                continue;
            }
            match known.get(key) {
                Some(default) => {
                    if let Some(found) = walk(value, default, &dotted(key)) {
                        return Some(found);
                    }
                }
                None => {
                    let names: Vec<String> = known.keys().cloned().collect();
                    return Some((dotted(key), closest(key, &names).map(|k| dotted(&k))));
                }
            }
        }
        None
    }
    walk(user, defaults_value(), "")
}

/// The name in `names` closest to `key` (edit distance, or one a prefix of the other), when
/// it is close enough to be a likely misspelling.
fn closest(key: &str, names: &[String]) -> Option<String> {
    names
        .iter()
        .map(|n| (edit_distance(key, n), n))
        .filter(|(d, n)| *d <= (key.len() / 3).max(2) || n.starts_with(key) || key.starts_with(n.as_str()))
        .min_by_key(|(d, _)| *d)
        .map(|(_, n)| n.clone())
}

/// Levenshtein distance over characters.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1).min(row[j] + 1).min(diagonal + usize::from(ca != *cb));
            diagonal = above;
        }
    }
    row[b.len()]
}

/// Dotted leaf keys of a JSON value (`memory.budget_mb` -> value).
fn leaves(prefix: &str, v: &Value, out: &mut BTreeMap<String, Value>) {
    match v {
        Value::Object(map) if !map.is_empty() => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                leaves(&key, v, out);
            }
        }
        other => {
            out.insert(prefix.to_string(), other.clone());
        }
    }
}

impl Settings {
    pub fn path(home: &Path) -> PathBuf {
        home.join("config.json")
    }

    /// The defaults overridden by `<home>/config.json` (a missing file yields the defaults).
    /// Validates all limits.
    pub fn load(home: &Path) -> Result<Settings> {
        let path = Self::path(home);
        let settings = match fs::read(&path) {
            Ok(bytes) => {
                let user: Value = serde_json::from_slice(&bytes)
                    .map_err(|e| CoreError::Config(format!("{}: {e}", path.display())))?;
                Settings::from_user(user)
                    .map_err(|e| CoreError::Config(format!("{}: {e}", path.display())))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(e) => return Err(CoreError::io(&path, e)),
        };
        settings
            .validate()
            .map_err(|msg| CoreError::Config(format!("{}: {msg}", path.display())))?;
        Ok(settings)
    }

    /// The defaults overridden by the user's JSON document (a key that names no setting is
    /// an error; limits are checked by [`Settings::validate`]).
    pub fn from_user(user: Value) -> std::result::Result<Settings, String> {
        if let Some((key, near)) = unknown_setting(&user) {
            return Err(match near {
                Some(near) => format!("unknown setting `{key}`; did you mean `{near}`?"),
                None => format!("unknown setting `{key}`"),
            });
        }
        let mut merged = defaults_value().clone();
        merge(&mut merged, user);
        serde_json::from_value(merged).map_err(|e| e.to_string())
    }

    /// Settings that differ from the defaults, as dotted keys with their JSON values (for
    /// `trace status`; their origin is `<home>/config.json`).
    pub fn non_default(&self) -> Vec<(String, String)> {
        let mut base = BTreeMap::new();
        leaves("", defaults_value(), &mut base);
        let mut mine = BTreeMap::new();
        leaves("", &serde_json::to_value(self).unwrap_or(Value::Null), &mut mine);
        mine.into_iter()
            .filter(|(k, v)| base.get(k) != Some(v))
            .map(|(k, v)| (k, v.to_string()))
            .collect()
    }

    /// The only function that computes `"auto"` values (from the machine's cores;
    /// `TRACE_SEMANTIC_PROCESSES` overrides the server processes, clamped to `1..=32`).
    pub fn auto(&self) -> Auto {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        let w = &self.workers;
        let server_processes = crate::env::semantic_processes()
            .map(|n| n.min(32))
            .unwrap_or_else(|| match w.server_processes {
                Count::Fixed(n) => n.clamp(1, 32),
                Count::Auto => (cores / 2).clamp(1, w.server_processes_max.max(1)),
            });
        Auto {
            cores,
            server_processes,
            request_shards: w.max_request_shards.min((cores / 2).max(1)),
        }
    }

    /// Check every documented constraint; the message names the offending field.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.watch.tick_ms == 0 || self.watch.stale_batch == 0 || self.analysis.wait_poll_ms == 0 {
            return Err("watch.tick_ms, watch.stale_batch and analysis.wait_poll_ms must be positive".into());
        }
        if self.cache.compact_ratio == 0 || self.cache.idle_compact_segments == 0 {
            return Err("cache.compact_ratio and cache.idle_compact_segments must be positive".into());
        }
        let s = &self.semantic;
        if s.request_timeout_secs == 0 || s.session_deadline_secs == 0 || s.max_in_flight == 0 {
            return Err(
                "semantic.request_timeout_secs, session_deadline_secs and max_in_flight must be positive"
                    .into(),
            );
        }
        if s.tools_dir.as_deref().is_some_and(|p| !p.is_absolute()) {
            return Err("semantic.tools_dir must be an absolute path".into());
        }
        if s.registry.as_deref().is_some_and(|p| !p.is_absolute()) {
            return Err("semantic.registry must be an absolute path".into());
        }
        // `memory.budget_mb`: every value is valid (0 = unbounded; a budget below one
        // process's estimate still runs one process, with a Node heap cap).
        let w = &self.workers;
        if w.server_processes_max == 0
            || w.max_request_shards == 0
            || w.requests_per_shard == 0
            || w.min_files_per_process == 0
            || self.memory.request_shard_mb == 0
        {
            return Err("workers limits and memory.request_shard_mb must be positive".into());
        }
        if self.flow.max_iterations == 0 || self.flow.eval_budget == Some(0) {
            return Err("flow.max_iterations and flow.eval_budget must be positive".into());
        }
        let f = &self.flow;
        if f.max_contexts == 0 || f.max_slot_values == 0 || f.max_delegate_visits == 0 {
            return Err("flow.max_contexts, max_slot_values and max_delegate_visits must be positive".into());
        }
        let d = &self.derive;
        if d.max_units == 0 || d.max_rounds == 0 || d.max_file_bytes == 0 || d.max_eval_depth == 0 {
            return Err(
                "derive.max_units, max_rounds, max_file_bytes and max_eval_depth must be positive".into()
            );
        }
        if self.watch.poll_ms == 0 {
            return Err("watch.poll_ms must be positive".into());
        }
        if self.bridges.manifests.iter().any(|p| !p.is_absolute()) {
            return Err("bridges.manifests must be absolute paths".into());
        }
        let i = &self.inventory;
        if i.max_files == 0 || i.max_file_bytes == 0 || i.max_total_bytes == 0 {
            return Err("inventory limits must be positive".into());
        }
        Ok(())
    }
}

/// Absolute, lexically normalized form (relative paths are resolved against the current
/// directory; callers must still check the result is outside inspected roots).
fn absolute(p: PathBuf) -> PathBuf {
    let abs = if p.is_absolute() {
        p
    } else {
        std::path::absolute(&p).unwrap_or(p)
    };
    normalize_lexically(&abs)
}

/// Semantic tools directory (never `None`): `TRACE_SEMANTIC_TOOLS` > config
/// `semantic.tools_dir` > per-user default `<data local dir>/trace/tools` (Windows `%LOCALAPPDATA%\trace\tools`,
/// Linux `~/.local/share/trace/tools`, macOS `~/Library/Application Support/trace/tools`).
/// A missing directory means "nothing installed".
pub fn semantic_tools_dir(settings: &Settings) -> PathBuf {
    crate::env::semantic_tools()
        .or_else(|| settings.semantic.tools_dir.clone())
        .map(absolute)
        .unwrap_or_else(|| {
            crate::env::data_local_dir()
                .map(|d| d.join("trace").join("tools"))
                .unwrap_or_else(|| absolute(PathBuf::from("trace-tools")))
        })
}

/// Whether automatic installs of default languages may run: `semantic.auto_install`, not
/// `TRACE_OFFLINE=1`, and `TRACE_NO_AUTO_INSTALL` not set to a non-empty value other than `0`.
pub fn auto_install_enabled(settings: &Settings, offline: bool) -> bool {
    settings.semantic.auto_install && !offline && !crate::env::no_auto_install()
}

#[cfg(test)]
#[path = "../tests/unit/config.rs"]
mod tests;
