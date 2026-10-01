//! The environment contract every ecosystem implements ([`Ecosystem`]) and what it returns
//! ([`Environment`]): the toolchain and the installed dependencies of a repository, found
//! statically, with where each came from ([`Where`], one lookup order for every ecosystem,
//! [`ORDER`]).
//!
//! * One unit struct per ecosystem module implements [`Ecosystem`] (`python::Python`,
//!   `node::Node`, ...); [`EcosystemId::ecosystem`] is the registry. Adding an ecosystem =
//!   one module with its `impl Ecosystem` + one [`EcosystemId`] variant.
//! * Detection results are cached per repository ([`DetectionCache`]) and re-checked when the
//!   caller's input signal (manifests, lockfiles, pins: the inventory's configuration files)
//!   or the `--env` path changes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::Serialize;

use crate::{DepsReport, DetectContext, EcosystemId, Origin, Toolchain};

/// The lookup order of every toolchain, dependency and runtime search:
///
/// 1. the paths remembered from `trace index --env`;
/// 2. project-local environments (`.venv`, `node_modules`, `target`, `vendor`, `.gradle` ...);
/// 3. configuration overrides;
/// 4. `PATH`;
/// 5. the standard install locations of the OS (and the user's shared package caches);
/// 6. trace's own installed tools (servers and their runtimes only).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Where {
    Remembered,
    Project,
    Config,
    Path,
    Standard,
    Tools,
}

/// [`Where`] in lookup order.
pub const ORDER: [Where; 6] = [
    Where::Remembered,
    Where::Project,
    Where::Config,
    Where::Path,
    Where::Standard,
    Where::Tools,
];

impl Origin {
    /// The lookup step that found it.
    pub fn step(self) -> Where {
        match self {
            Origin::Override => Where::Remembered,
            Origin::Pin | Origin::Project => Where::Project,
            Origin::Path => Where::Path,
            Origin::StandardLocation | Origin::UserCache => Where::Standard,
            Origin::Tools => Where::Tools,
        }
    }
}

/// The result of one search: found, missing (with what was searched), found but too old, or
/// not needed by this project.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Found<T> {
    Found(T),
    Missing {
        searched: Vec<String>,
    },
    TooOld {
        found: T,
        needed: crate::os::VersionReq,
        source: String,
    },
    NotNeeded,
}

impl<T> Found<T> {
    /// The found value (`Found` only).
    pub fn value(&self) -> Option<&T> {
        match self {
            Found::Found(v) => Some(v),
            _ => None,
        }
    }
}

/// The installed dependencies of a project (status, library roots, fingerprint, hint).
pub type Dependencies = DepsReport;

/// Whether the project needs an approved build before its server can answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildState {
    /// Not decided by the ecosystem's detection (the language's preflight decides).
    NotChecked,
    NotNeeded,
    /// A build import runs project code: `trace index --allow-build` is required.
    NeedsApproval,
    Approved,
}

/// Toolchain + dependencies of one ecosystem in one repository.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Environment {
    pub ecosystem: EcosystemId,
    pub toolchain: Found<Toolchain>,
    pub dependencies: Dependencies,
    pub build: BuildState,
    /// Where the toolchain was found (`None` when none was found).
    pub source: Option<Where>,
}

/// One toolchain/dependency ecosystem. Implementations only read directory entries and small
/// structured files (never run project code; `os::toolchain_output` on a toolchain binary is
/// the only execution).
pub trait Ecosystem: Sync {
    fn id(&self) -> EcosystemId;

    /// Whether `path` (`trace index --env <path>`) is an environment of this ecosystem.
    fn accepts_env_path(&self, path: &Path) -> bool;

    /// The toolchain, searched in [`ORDER`] with the EXECUTE context (nothing inside the
    /// repository or trace's workspaces is trusted).
    fn toolchain(&self, cx: &DetectContext<'_>) -> Found<Toolchain>;

    /// The declared dependencies against the installed ones, with the READ context
    /// (manifests, lockfiles and installed packages are only read, inside the repository too).
    fn deps(&self, read: &DetectContext<'_>, toolchain: Option<&Toolchain>) -> Dependencies;

    /// Toolchain and dependencies. `readable` replaces the forbidden roots of `cx` for the
    /// read-only part (the user's protected roots, `trace_core::paths::forbidden_roots`).
    fn detect(&self, cx: &DetectContext<'_>, readable: &[PathBuf]) -> Environment {
        let toolchain = self.toolchain(cx);
        let read = DetectContext {
            forbidden: readable,
            ..*cx
        };
        let dependencies = self.deps(&read, toolchain.value());
        Environment {
            ecosystem: self.id(),
            source: toolchain.value().map(|t| t.origin.step()),
            toolchain,
            dependencies,
            build: BuildState::NotChecked,
        }
    }
}

/// Key of a cached detection.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CacheKey {
    ecosystem: EcosystemId,
    root: PathBuf,
    env_override: Option<PathBuf>,
    /// Size and modification time of the `--env` path (a changed environment re-detects).
    env_stamp: Option<(u64, u128)>,
    /// The caller's content signal of the inputs (manifests, lockfiles, pin files).
    inputs: String,
}

fn stamp(path: &Path) -> Option<(u64, u128)> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let nanos = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    Some((meta.len(), nanos))
}

/// Detections of this process, per repository and ecosystem (see the module docs).
#[derive(Default)]
pub struct DetectionCache {
    entries: Mutex<BTreeMap<CacheKey, Environment>>,
}

impl DetectionCache {
    /// The cache of this process.
    pub fn global() -> &'static DetectionCache {
        static CACHE: OnceLock<DetectionCache> = OnceLock::new();
        CACHE.get_or_init(DetectionCache::default)
    }

    /// [`Ecosystem::detect`], reused while `inputs` (the caller's content signal of the
    /// repository's manifests, lockfiles and pins) and the `--env` path are unchanged.
    pub fn detect(
        &self,
        ecosystem: &dyn Ecosystem,
        cx: &DetectContext<'_>,
        readable: &[PathBuf],
        inputs: &str,
    ) -> Environment {
        let key = CacheKey {
            ecosystem: ecosystem.id(),
            root: cx.root.to_path_buf(),
            env_override: cx.env_override.map(Path::to_path_buf),
            env_stamp: cx.env_override.and_then(stamp),
            inputs: inputs.to_string(),
        };
        if let Some(found) = self.lock().get(&key) {
            return found.clone();
        }
        let env = ecosystem.detect(cx, readable);
        self.lock().insert(key, env.clone());
        env
    }

    /// Forget every detection of `root` (after an install or a `--env` change).
    pub fn forget(&self, root: &Path) {
        self.lock().retain(|k, _| k.root != root);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<CacheKey, Environment>> {
        // A panic while holding the lock leaves a consistent map (inserts are atomic).
        self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
#[path = "../tests/unit/environment.rs"]
mod tests;
