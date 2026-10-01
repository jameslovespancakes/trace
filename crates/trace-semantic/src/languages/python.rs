//! Python setup hooks (owner script).
//!
//! Preflight (every independent failure collected, PLAN decision 15):
//! 1. server: Pyright installed in trace's tools folder (default language: installed
//!    automatically before preflight unless automatic installs are off) and its Node runtime;
//! 2. environment (`trace_env::python`): `--env` override (not an environment ->
//!    `EnvNotFound`), then the project's environments in the documented order;
//! 3. dependencies: runtime dependencies the project declares but the environment lacks ->
//!    `DepsMissing` with the lockfile's hint (test/dev groups and sub-projects are status
//!    lines);
//! 4. no build approval: nothing of the project runs (no interpreter is configured).
//!
//! `Prepared.data` = [`crate::backends::pyright::PyrightSetup`]: the environment (`venvPath`/`venv`),
//! the Python version, extra search paths (project `extraPaths` or `src`, `--env`
//! `site-packages`, PEP 582 `__pypackages__`, the base interpreter's `site-packages` when the
//! venv includes system site packages) and the project's safe Pyright keys.

use std::path::Path;
use std::sync::Arc;

use serde_json::{Map, Value};
use trace_core::formats::{jsonc, toml_value};
use trace_core::setup_error::SetupError;
use trace_core::Language;
use trace_env::python::EnvKind;
use trace_env::{EcosystemId, ToolchainStatus};

use super::{default_prepared, read_context, Prepared, Server, SetupContext};
use crate::backends::fntype::FnTypeRoute;
use crate::backends::pyright::{PyrightSetup, SAFE_PROJECT_KEYS};
use crate::setup::{deps_error, require_runtimes, require_server, Collect};

pub struct Hooks;

impl Server for Hooks {
    fn preflight(&self, cx: &SetupContext<'_>) -> Result<Prepared, SetupError> {
        let language = Language::Python;
        let mut collect = Collect::default();
        collect.check(require_server(cx));
        collect.check(require_runtimes(cx));

        // Environments and manifests are only READ (in-project `.venv` included): only the
        // user's protected roots are off limits (`cx.tools.forbidden_roots`, the repository
        // itself, guards what may be EXECUTED; trace never runs a Python interpreter, so the
        // toolchain row is the environment's interpreter, read from the same context).
        let readable = trace_core::paths::forbidden_roots();
        let dcx = read_context(cx, EcosystemId::Python, &readable);
        let setup = trace_env::python::setup(&dcx);
        match &setup.env_not_found {
            Some(path) => collect.push(SetupError::EnvNotFound {
                language: Some(language),
                path: path.clone(),
            }),
            None => {
                if let Some(e) = deps_error(language, &setup.deps) {
                    collect.push(e);
                }
            }
        }

        let (project, project_extra) = project_config(&cx.repo.root);
        let pyright = pyright_setup(&setup, project, project_extra);
        let mut prepared = default_prepared(cx);
        prepared.library_roots = setup.deps.roots.clone();
        if let ToolchainStatus::Found(t) = trace_env::python::toolchain(&dcx) {
            prepared.toolchain = Some(t);
        }
        prepared.status = setup.deps.notes.clone();
        let mut h = blake3::Hasher::new();
        h.update(setup.deps.fingerprint.as_bytes());
        h.update(format!("{pyright:?}").as_bytes());
        prepared.fingerprint = h.finalize().to_hex()[..32].to_string();
        prepared.data = Some(Arc::new(pyright));
        collect.finish(prepared)
    }

    fn fn_type_route(&self, _language: Language) -> FnTypeRoute {
        FnTypeRoute::Declaration
    }
}

/// Pyright inputs from the environment search and the project's configuration.
pub fn pyright_setup(
    setup: &trace_env::python::PythonSetup,
    project: Map<String, Value>,
    project_extra: Option<Vec<String>>,
) -> PyrightSetup {
    let mut extra_paths = project_extra.unwrap_or_else(|| vec!["src".to_string()]);
    let mut venv = None;
    if let Some(env) = &setup.env {
        match env.kind {
            EnvKind::Venv | EnvKind::Conda => venv = Some(env.clone()),
            EnvKind::SitePackages | EnvKind::PyPackages => {
                extra_paths.extend(env.site_packages.iter().map(|p| p.display().to_string()));
            }
        }
        extra_paths.extend(env.system_site_packages.iter().map(|p| p.display().to_string()));
    }
    PyrightSetup {
        venv,
        python_version: setup.python_version.clone(),
        extra_paths,
        project,
    }
}

/// The project's safe Pyright keys and `extraPaths`: `pyrightconfig.json` (JSONC) wins over
/// `pyproject.toml` `[tool.pyright]`, as in Pyright itself.
pub fn project_config(root: &Path) -> (Map<String, Value>, Option<Vec<String>>) {
    let config = std::fs::read_to_string(root.join("pyrightconfig.json"))
        .ok()
        .and_then(|t| jsonc::parse(&t))
        .or_else(|| {
            std::fs::read_to_string(root.join("pyproject.toml"))
                .ok()
                .and_then(|t| toml_value(&t))
                .and_then(|v| v.get("tool").and_then(|t| t.get("pyright")).cloned())
        });
    let Some(Value::Object(obj)) = config else {
        return (Map::new(), None);
    };
    let mut safe = Map::new();
    for key in SAFE_PROJECT_KEYS {
        if let Some(v) = obj.get(*key) {
            safe.insert((*key).to_string(), v.clone());
        }
    }
    let extra = obj.get("extraPaths").and_then(Value::as_array).map(|a| {
        a.iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>()
    });
    (safe, extra)
}

#[cfg(test)]
#[path = "../../tests/unit/languages/python.rs"]
mod tests;
