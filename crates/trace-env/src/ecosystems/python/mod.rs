//! Python: the environment that holds the project's dependencies, the Python version and the
//! declared-vs-installed dependency check. Nothing is executed: the
//! interpreter is never run (Pyright reads `site-packages` directly and takes the standard
//! library from its bundled typeshed), manifests are read with structured readers (TOML,
//! INI, YAML, tree-sitter for a literal `setup.py`), and PEP 508 requirements and markers are
//! parsed by a small structured parser ([`pep508`]).
//!
//! Environment order (first hit wins):
//! 1. `--env <path>` (settings override): a virtual environment, a conda environment or a
//!    `site-packages` directory; anything else is `EnvNotFound`.
//! 2. `UV_PROJECT_ENVIRONMENT` (relative to the project root when relative).
//! 3. In-project virtual environments: `.venv`, `venv`, `env`, `.virtualenv` at the root or
//!    one directory below (uv, PDM, Poetry `in-project`, Pipenv `PIPENV_VENV_IN_PROJECT`).
//! 4. PEP 582 `__pypackages__/<X.Y>/lib` (PDM).
//! 5. Poetry's shared environment `<virtualenvs.path>/<name>-<hash>-py<X.Y>`.
//! 6. Pipenv's `<WORKON_HOME | ~/.virtualenvs>/<dir>-*` whose `.project` names this project.
//! 7. Conda: `environment.yml` `prefix:` / `name:` under the known conda bases.
//! 8. pyenv-virtualenv: `.python-version` naming an environment under `<pyenv>/versions`.
//! 9. The activated shell: `VIRTUAL_ENV`, `CONDA_PREFIX`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use trace_core::formats::{toml_value, yaml};
use trace_core::Language;

use crate::lookup::Lookup;
use crate::os::{self, EnvVars, Os, Platform, Version};
use crate::Where;
use crate::{
    entries, hex, subdirs, DepsReport, DepsStatus, DetectContext, EcosystemId, LibraryKind, LibraryRoot,
    Origin, SubProject, Toolchain, ToolchainStatus, SKIP_DIRS,
};

mod check;
mod envs;
mod manifests;
pub mod pep508;

pub use check::*;
use envs::*;
use manifests::*;

/// Virtual-environment directory names, in preference order.
pub(crate) const VENV_NAMES: &[&str] = &[".venv", "venv", "env", ".virtualenv"];

/// Maximum directory depth searched for nested Python projects (sub-projects).
const SUBPROJECT_DEPTH: usize = 4;

/// What kind of directory an environment is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvKind {
    /// A virtual environment (`pyvenv.cfg`).
    Venv,
    /// A conda environment (`conda-meta/`, no `pyvenv.cfg`).
    Conda,
    /// A bare `site-packages` directory given with `--env`.
    SitePackages,
    /// PEP 582 `__pypackages__/<X.Y>/lib`.
    PyPackages,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PythonEnv {
    /// The environment directory (contains `pyvenv.cfg` / `conda-meta`, or is the
    /// `site-packages` / `__pypackages__` library directory itself).
    pub root: PathBuf,
    pub site_packages: Vec<PathBuf>,
    /// `major.minor`, when a file says it.
    pub version: Option<String>,
    pub origin: Origin,
    pub kind: EnvKind,
    /// `include-system-site-packages = true`: the base interpreter's `site-packages`.
    pub system_site_packages: Vec<PathBuf>,
    /// The base interpreter's standard library directory (readable source), when on disk.
    pub stdlib: Option<PathBuf>,
}

impl PythonEnv {
    /// Every library directory Pyright reads (own + system site-packages).
    pub(crate) fn library_dirs(&self) -> Vec<PathBuf> {
        let mut out = self.site_packages.clone();
        out.extend(self.system_site_packages.iter().cloned());
        out
    }

    /// The interpreter inside the environment, when present (never run by trace).
    pub fn interpreter(&self) -> Option<PathBuf> {
        [
            self.root.join("Scripts").join("python.exe"),
            self.root.join("python.exe"),
            self.root.join("bin").join("python3"),
            self.root.join("bin").join("python"),
        ]
        .into_iter()
        .find(|p| p.is_file())
    }
}

/// Everything the Python preflight needs, computed once.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PythonSetup {
    pub env: Option<PythonEnv>,
    /// `--env` pointed to a directory that is no Python environment.
    pub env_not_found: Option<PathBuf>,
    /// `major.minor` for the server: environment > `.python-version` > `requires-python`.
    pub python_version: Option<String>,
    pub deps: DepsReport,
}

/// The interpreter, found without running it: the environment's, else `python3`/`python`
/// on PATH (never the Windows Store stub).
pub fn toolchain(cx: &DetectContext<'_>) -> ToolchainStatus {
    let setup_env = find_environment(cx).ok().flatten();
    if let Some(env) = setup_env {
        let mut executables = BTreeMap::new();
        if let Some(exe) = env.interpreter() {
            executables.insert("python".to_string(), exe);
        }
        let mut facts = BTreeMap::new();
        facts.insert("environment".to_string(), env.root.display().to_string());
        facts.insert(
            "kind".to_string(),
            serde_json::to_value(env.kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default(),
        );
        return ToolchainStatus::Found(Toolchain {
            id: "python",
            root: env.root.clone(),
            version: env.version.as_deref().and_then(Version::parse),
            executables,
            origin: env.origin,
            facts,
        });
    }
    let path = os::path_dirs(cx.vars).into_iter().filter(|d| !is_store_stub_dir(d));
    match Lookup::new(cx.platform, &[])
        .with(Where::Path, path)
        .find(&["python3", "python"])
    {
        Some((exe, _)) => {
            let root = exe.parent().map(Path::to_path_buf).unwrap_or_default();
            let mut executables = BTreeMap::new();
            executables.insert("python".to_string(), exe);
            ToolchainStatus::Found(Toolchain {
                id: "python",
                version: version_from_dir_name(&root),
                root,
                executables,
                origin: Origin::Path,
                facts: BTreeMap::new(),
            })
        }
        None => ToolchainStatus::Missing {
            searched: vec!["the project's virtual environments".into(), "PATH (python3, python)".into()],
        },
    }
}

/// Declared dependencies against the environment (see [`setup`]).
pub fn deps(cx: &DetectContext<'_>, _toolchain: Option<&Toolchain>) -> DepsReport {
    setup(cx).deps
}

/// A virtual environment, a conda environment or a `site-packages` directory.
pub(crate) fn accepts_env_path(path: &Path) -> bool {
    env_at(path, Origin::Override).is_some()
}

/// Environment, Python version and dependency report of the repository.
pub fn setup(cx: &DetectContext<'_>) -> PythonSetup {
    let (env, env_not_found) = match find_environment(cx) {
        Ok(env) => (env, None),
        Err(path) => (None, Some(path)),
    };
    let manifests = Manifests::read(cx.root);
    let python_version = env
        .as_ref()
        .and_then(|e| e.version.clone())
        .or_else(|| pinned_version(cx.root))
        .or_else(|| manifests.requires_python.clone());
    let deps = check_deps(cx, env.as_ref(), python_version.as_deref(), &manifests);
    PythonSetup {
        env,
        env_not_found,
        python_version,
        deps,
    }
}

/// The Python ecosystem ([`crate::Ecosystem`]).
pub struct Python;

impl crate::Ecosystem for Python {
    fn id(&self) -> crate::EcosystemId {
        crate::EcosystemId::Python
    }

    fn accepts_env_path(&self, path: &Path) -> bool {
        accepts_env_path(path)
    }

    fn toolchain(&self, cx: &DetectContext<'_>) -> ToolchainStatus {
        toolchain(cx)
    }

    fn deps(&self, read: &DetectContext<'_>, toolchain: Option<&Toolchain>) -> DepsReport {
        deps(read, toolchain)
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/ecosystems/python/mod.rs"]
mod tests;
