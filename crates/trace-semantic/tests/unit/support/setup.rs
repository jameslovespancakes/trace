//! Preflight contexts, tool environments and setup assertions.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use trace_core::facts::FileFacts;
use trace_core::paths::RepoPaths;
use trace_core::repo_settings::RepoSettings;
use trace_core::{Language, SetupError};
use trace_env::os::{EnvVars, Platform};

use crate::languages::{Prepared, SetupContext};
use crate::registry::{BackendEntry, Registry};
use crate::tools::ToolEnv;

/// Every error of a (possibly combined) setup error.
pub(crate) fn items(e: &SetupError) -> Vec<SetupError> {
    match e {
        SetupError::Several { items } => items.clone(),
        other => vec![other.clone()],
    }
}

pub(crate) fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// Run `f` with a SetupContext over `root` (empty tools folder, no JDK on PATH).
pub(crate) fn with_context<R>(
    backend: &str,
    root: &Path,
    files: &[(&str, Language)],
    allow_build: bool,
    f: impl FnOnce(&SetupContext<'_>) -> R,
) -> R {
    let home = tempfile::tempdir().unwrap();
    let repo = RepoPaths::resolve_in(root, home.path()).unwrap();
    let registry = Registry::builtin();
    let entry: BackendEntry = registry
        .entry(backend)
        .cloned()
        .unwrap_or_else(|| panic!("{backend}"));
    let tools = crate::test_support::setup::tool_env(None);
    let settings = RepoSettings {
        allow_build,
        ..RepoSettings::default()
    };
    let platform = Platform::current();
    let user_home = home.path().join("user");
    std::fs::create_dir_all(&user_home).unwrap();
    let user = user_home.display().to_string();
    let vars = EnvVars::from_pairs(&[("HOME", user.as_str()), ("USERPROFILE", user.as_str())]);
    let facts = |_: &str| -> Option<&FileFacts> { None };
    let cx = SetupContext {
        repo: &repo,
        entry: &entry,
        languages: &entry.languages,
        files,
        facts: &facts,
        settings: &settings,
        tools: &tools,
        platform: &platform,
        vars: &vars,
        report_only: false,
    };
    f(&cx)
}

/// Every `{json:X}` and preflight `{X}` of an entry has a value in `prepared`.
pub(crate) fn assert_placeholders_filled(entry: &BackendEntry, prepared: &Prepared) {
    fn strings(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) => out.push(s.clone()),
            Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
            Value::Object(m) => m.values().for_each(|x| strings(x, out)),
            _ => {}
        }
    }
    let mut all: Vec<String> = entry.args.clone();
    all.extend(entry.env_set.values().cloned());
    strings(&entry.initialization_options, &mut all);
    strings(&entry.settings, &mut all);
    for s in &all {
        for name in crate::registry::placeholders(s) {
            if let Some(j) = name.strip_prefix("json:") {
                assert!(prepared.json_vars.contains_key(j), "{}: {{{name}}}", entry.id);
            } else if crate::registry::PREPARED_VARS.contains(&name) {
                assert!(prepared.vars.contains_key(name), "{}: {{{name}}}", entry.id);
            }
        }
    }
}

/// A minimal environment for unit tests (no PATH, optional tools directory).
pub(crate) fn tool_env(tools_dir: Option<PathBuf>) -> ToolEnv {
    ToolEnv {
        tools_dir,
        manifest: Default::default(),
        assets: crate::assets::AssetPaths {
            dir: PathBuf::new(),
            ts_worker: PathBuf::new(),
            lsp_guard: PathBuf::new(),
        },
        forbidden_roots: Vec::new(),
        request_timeout: Duration::from_secs(1),
        session_deadline: Duration::from_secs(1),
        max_in_flight: 1,
        processes: 1,
        pool_memory_mb: 0,
        registry: crate::registry::Registry::builtin(),
    }
}

/// The builtin registry, every embedded file parsed strictly.
pub(crate) fn builtin_strict() -> Result<Registry, String> {
    let mut registry = Registry {
        backends: Vec::new(),
        runtimes: Vec::new(),
    };
    for (stem, text) in crate::registry::BUILTIN_FILES {
        let file: crate::registry::RegistryFile =
            serde_json::from_str(text).map_err(|e| format!("{stem}.json: {e}"))?;
        if file.schema != crate::registry::REGISTRY_VERSION {
            return Err(format!(
                "{stem}.json: schema {} (expected {})",
                file.schema,
                crate::registry::REGISTRY_VERSION
            ));
        }
        registry.backends.extend(file.backends);
        registry.runtimes.extend(file.runtimes);
    }
    registry.validate()?;
    Ok(registry)
}
