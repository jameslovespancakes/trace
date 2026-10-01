//! Trusted tool discovery and controlled process environments
//! (port of codepath_v3/safety/permissions.py `executable` / `clean_env`).
//!
//! * Programs on PATH are found by `trace-env` only (`trace_env::lookup`: absolute entries,
//!   on Windows `.exe` / `.com` only).
//! * Registry entries ([`crate::registry::ExecutableSpec`], schema 2) resolve inside the
//!   tools directory: `<tools>/<tool>/<installed version>/<path>` per `MANIFEST.json`
//!   schema 2 ([`ToolEnv::tool_dir`]), runtime programs inside `<tools>/<runtime>/<version>`,
//!   toolchain binaries inside the prepared toolchain root; the same trusted-executable rules
//!   apply. Node-based servers resolve their script with [`ToolEnv::tool_file`] and run on
//!   [`ToolEnv::runtime_exe`]`("node")`.
//! * Server runtimes (Node, the JDK, the .NET runtime) are ALWAYS the trace-managed ones in the
//!   tools directory (PLAN decision 11), never a runtime found on the user's PATH; the user's
//!   toolchains serve the project itself and are found by `trace-env` / the preflights.
//! * The canonical executable must not live inside an inspected root or a snapshot
//!   workspace (`forbidden_roots`).
//! * Analyzer environments are built from an allow-list ([`clean_env`]): a variable that is
//!   not listed (a user's secrets) never reaches an analyzer.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use trace_core::config::{semantic_tools_dir, Settings};
use trace_core::inventory::{canonical_root, strip_verbatim};
use trace_core::paths::ensure_outside;

use crate::SemanticError;

/// Environment variables passed through to analyzer processes (case-insensitive names).
pub const BASE_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "SYSTEMROOT",
    // Windows known-folder lookups return nothing without it.
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "TEMP",
    "TMP",
    "OS",
    "LOCALAPPDATA",
    "APPDATA",
    "USERPROFILE",
    "HOME",
];

/// Everything backends need to launch tools safely.
#[derive(Clone, Debug)]
pub struct ToolEnv {
    /// Semantic tools directory (`None` when it does not exist: nothing installed).
    pub tools_dir: Option<PathBuf>,
    /// `<tools>/MANIFEST.json` schema 2 (empty when missing or of another schema).
    pub manifest: crate::install::manifest::Manifest,
    /// Materialized assets (`ts-worker/`, `lsp_guard.cjs`) under the cache home.
    pub assets: crate::assets::AssetPaths,
    /// Inspected roots and snapshot roots: executables here are never trusted.
    pub forbidden_roots: Vec<PathBuf>,
    pub request_timeout: Duration,
    pub session_deadline: Duration,
    pub max_in_flight: usize,
    /// Maximum analyzer processes per backend that supports sharding (Pyright); the pool
    /// uses at most this many, fewer when the memory budget or the work is smaller.
    pub processes: usize,
    /// Memory budget of one analyzer pool in MB (`memory.budget_mb`; 0 = unbounded). The
    /// pool picks its process count to respect it.
    pub pool_memory_mb: u64,
    /// Backend registry (`semantic.registry` override directory or the embedded
    /// `assets/backends/*.json`).
    pub registry: crate::registry::Registry,
}

impl ToolEnv {
    /// Discover tools for analyzing `target_root`, materializing assets under `home`.
    pub fn discover(
        cfg: &Settings,
        home: &Path,
        target_root: &Path,
    ) -> Result<ToolEnv, crate::SemanticError> {
        let target = canonical_root(target_root)?;
        ensure_outside(home, &[&target])?;
        // Snapshots live under `<home>/repos/<key>/workspaces`: nothing there is trusted.
        let forbidden = vec![target, canonical_or_self(&home.join("repos"))];
        // A missing tools directory means nothing is installed.
        let tools_dir = trusted_tools_dir(&semantic_tools_dir(cfg), &forbidden);
        let assets = crate::assets::materialize(home)?;
        let sc = &cfg.semantic;
        // `semantic.registry` (absolute; never inside the inspected root) overrides the
        // embedded registry; an invalid override is an error, never silently ignored.
        let registry = match &sc.registry {
            Some(path) => {
                if !path.is_absolute() {
                    return Err(SemanticError::Protocol(format!(
                        "semantic.registry must be an absolute path: {}",
                        path.display()
                    )));
                }
                let canonical = canonical_or_self(path);
                if forbidden.iter().any(|root| is_within(&canonical, root)) {
                    return Err(SemanticError::UntrustedExecutable(canonical));
                }
                crate::registry::Registry::load_dir(&canonical)?
            }
            None => crate::registry::Registry::builtin(),
        };
        let manifest = tools_dir
            .as_deref()
            .map(crate::install::manifest::Manifest::load)
            .unwrap_or_default();
        Ok(ToolEnv {
            tools_dir,
            manifest,
            assets,
            forbidden_roots: forbidden,
            request_timeout: Duration::from_secs(sc.request_timeout_secs.max(1)),
            session_deadline: Duration::from_secs(sc.session_deadline_secs.max(1)),
            max_in_flight: sc.max_in_flight.clamp(1, 64),
            processes: cfg.auto().server_processes,
            pool_memory_mb: cfg.memory.budget_mb,
            registry,
        })
    }

    /// Re-read the tools directory and MANIFEST.json after an install in this process
    /// (`install::auto_install` before preflight, `trace status --install`), so newly
    /// installed servers and runtimes resolve without rediscovering everything.
    pub fn reload_installed(&mut self, tools_dir: &Path) {
        self.tools_dir = trusted_tools_dir(tools_dir, &self.forbidden_roots);
        self.manifest = self
            .tools_dir
            .as_deref()
            .map(crate::install::manifest::Manifest::load)
            .unwrap_or_default();
    }

    /// `<tools>/<id>/<installed version>` per MANIFEST schema 2 (canonical, outside every
    /// forbidden root); `None` when not installed.
    pub fn tool_dir(&self, id: &str) -> Option<PathBuf> {
        let tools = self.tools_dir.as_ref()?;
        let dir = canonical_or_self(&self.manifest.tool_dir(tools, id)?);
        (!self.forbidden_roots.iter().any(|root| is_within(&dir, root))).then_some(dir)
    }

    /// A trusted executable file `<dir>/<rel>` (`.exe` appended on Windows when missing).
    fn trusted_file(&self, dir: &Path, rel: &str) -> Option<PathBuf> {
        let rel = crate::registry::safe_relative(rel)?;
        let path = dir.join(&rel);
        let candidates = if cfg!(windows) && path.extension().is_none() {
            vec![path.with_extension("exe"), path]
        } else {
            vec![path]
        };
        candidates.into_iter().find_map(|p| {
            if !p.is_file() {
                return None;
            }
            let canonical = canonical_or_self(&p);
            (!self.forbidden_roots.iter().any(|root| is_within(&canonical, root))).then_some(canonical)
        })
    }

    /// The program of a trace-managed runtime (`node`, `jdk`, `dotnet`): the first existing
    /// executable of its install record in `<tools>/<runtime>/<version>`. Never the user's
    /// runtime (PLAN decision 11): a missing runtime is `None` (the preflight reports it).
    pub fn runtime_exe(&self, id: &str) -> Option<PathBuf> {
        let dir = self.tool_dir(id)?;
        let spec = self.registry.runtime(id)?;
        let crate::registry::Recipe::Archive { executables, .. } = &spec.recipe else {
            return None;
        };
        executables.iter().find_map(|rel| self.trusted_file(&dir, rel))
    }

    /// Resolve a registry entry's executable: a tool (`<tools>/<tool>/<version>/<path>`), a
    /// runtime program (`<tools>/<runtime>/<version>/<program>`) or a toolchain binary (first
    /// existing of `paths` under `toolchain.root`). `NodeScript` entries resolve their script
    /// with [`ToolEnv::tool_file`] and run under node (None here).
    pub fn entry_executable(
        &self,
        spec: &crate::registry::ExecutableSpec,
        toolchain: Option<&trace_env::Toolchain>,
    ) -> Option<PathBuf> {
        use crate::registry::ExecutableSpec;
        match spec {
            ExecutableSpec::Tool { tool, path } => self.trusted_file(&self.tool_dir(tool)?, path),
            ExecutableSpec::Runtime { runtime, program } => {
                self.trusted_file(&self.tool_dir(runtime)?, program)
            }
            ExecutableSpec::Toolchain { paths, .. } => {
                let root = &toolchain?.root;
                paths.iter().find_map(|p| self.trusted_file(root, p))
            }
            ExecutableSpec::NodeScript { .. } => None,
        }
    }

    /// A file of an installed tool (`node_modules/x/cli.js` under `<tools>/<tool>/<version>`):
    /// relative path only, must exist, canonical form outside every forbidden root.
    pub fn tool_file(&self, tool: &str, rel: &str) -> Option<PathBuf> {
        let rel = crate::registry::safe_relative(rel)?;
        let path = self.tool_dir(tool)?.join(rel);
        if !path.is_file() {
            return None;
        }
        let canonical = canonical_or_self(&path);
        (!self.forbidden_roots.iter().any(|root| is_within(&canonical, root))).then_some(canonical)
    }

    /// Re-check a program or script path right before launch: absolute, an existing file,
    /// and outside every forbidden root.
    pub fn ensure_trusted(&self, path: &Path) -> Result<(), SemanticError> {
        if !path.is_absolute() || !path.is_file() {
            return Err(SemanticError::ExecutableUnavailable(path.display().to_string()));
        }
        let canonical = canonical_or_self(path);
        if self.forbidden_roots.iter().any(|root| is_within(&canonical, root)) {
            return Err(SemanticError::UntrustedExecutable(canonical));
        }
        Ok(())
    }

    /// `<tools>/<package>/<version>/node_modules/<package>` of an installed npm tool
    /// (pyright, typescript).
    pub fn package_dir(&self, package: &str) -> Option<PathBuf> {
        self.tool_dir(package).map(|t| t.join("node_modules").join(package))
    }
}

/// The tools directory when it exists and lies outside every forbidden root (canonical form);
/// a missing directory means nothing is installed.
fn trusted_tools_dir(dir: &Path, forbidden: &[PathBuf]) -> Option<PathBuf> {
    Some(dir)
        .filter(|p| p.is_dir())
        .map(canonical_or_self)
        .filter(|p| !forbidden.iter().any(|root| is_within(p, root)))
}

/// Controlled environment: allow-listed variables plus backend-specific additions; never a
/// variable that is not explicitly listed (user secrets stay with trace).
pub fn clean_env(extra_allow: &[&str], set: &[(&str, String)]) -> BTreeMap<String, String> {
    let allowed = |key: &str| {
        BASE_ENV_ALLOWLIST.iter().any(|a| a.eq_ignore_ascii_case(key))
            || extra_allow.iter().any(|a| a.eq_ignore_ascii_case(key))
    };
    let mut env = BTreeMap::new();
    for (key, value) in trace_core::env::vars() {
        let Some(value) = value.to_str() else {
            continue;
        };
        if allowed(&key) {
            env.insert(key.to_string(), value.to_string());
        }
    }
    for (key, value) in set {
        env.retain(|existing: &String, _| !existing.eq_ignore_ascii_case(key));
        env.insert((*key).to_string(), value.clone());
    }
    env
}

/// Canonical display form when the path exists, else the path itself.
pub(crate) fn canonical_or_self(path: &Path) -> PathBuf {
    fs::canonicalize(path)
        .map(strip_verbatim)
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Component-wise prefix test (case-insensitive on Windows).
pub(crate) fn is_within(path: &Path, root: &Path) -> bool {
    let mut components = path.components();
    for rc in root.components() {
        match components.next() {
            Some(pc) if component_eq(pc, rc) => {}
            _ => return false,
        }
    }
    true
}

pub(crate) fn component_eq(a: Component<'_>, b: Component<'_>) -> bool {
    if cfg!(windows) {
        let (a, b) = (a.as_os_str().to_string_lossy(), b.as_os_str().to_string_lossy());
        a.eq_ignore_ascii_case(&b) || a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

#[cfg(test)]
#[path = "../tests/unit/tools.rs"]
mod tests;
