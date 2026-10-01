//! trace-env: the toolchains and locally installed dependencies of an inspected repository,
//! found statically.
//!
//! Language servers resolve calls into libraries only when they can read the libraries.
//! Measured on Python, TypeScript and Rust projects, making the installed dependencies
//! visible removes 60-99% of unresolved calls with identical answers for the project's own
//! code. This crate finds toolchains and dependencies **without executing project code**:
//! no interpreter, package manager, `cargo` or `go` command runs on the project, and no
//! project file is written. It only reads directory entries and small structured metadata
//! files (`pyvenv.cfg`, `Cargo.lock`, lockfile hashes). `.env` files are never opened. The
//! only execution allowed is [`os::toolchain_output`] on a toolchain binary for its version.
//!
//! One module per ecosystem under [`ecosystems`] (re-exported here: `trace_env::python`, ...),
//! each implementing [`Ecosystem`] (`toolchain`, `deps`, `accepts_env_path`,
//! [`Ecosystem::detect`] -> [`Environment`]); [`EcosystemId`] names them. Shared helpers:
//! [`os`] (platform, environment snapshot, versions), `relpath` (repository-relative paths,
//! bounded manifest reads), `syn` (owned syntax trees of build files).
//! Every lookup follows one order ([`ORDER`]): remembered `--env` paths -> project-local
//! environments -> configuration overrides -> PATH -> standard install locations -> trace's own
//! tools ([`lookup::Lookup`]). This crate is the only code that searches PATH and install
//! locations ([`lookup`]); detections are cached per repository ([`DetectionCache`]).
//!
//! Each language's preflight (`trace_semantic` `languages/<lang>.rs`) calls its ecosystem module
//! and turns the result into `Prepared` (library roots, fingerprint, status lines) or a
//! setup error: a missing toolchain or missing declared dependencies stop the index (no
//! fallback). `trace index --env <path>` is classified by [`classify_env_path`] and stored per
//! ecosystem in the repository settings ([`DetectContext::env_override`]).
//!
//! Every dependency report has a fingerprint (paths plus a cheap content signal such as a
//! directory listing or a lockfile hash): installing or upgrading packages changes it, so
//! cached semantic results computed without those packages are recomputed.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use trace_core::Language;

pub mod ecosystems;
pub mod environment;
pub mod lookup;
pub mod os;
mod relpath;
mod syn;

pub use ecosystems::{cfamily, dotnet, go, haskell, jvm, node, php, python, r, rust};
pub use ecosystems::{classify_env_path, EcosystemId};
pub use environment::{
    BuildState, Dependencies, DetectionCache, Ecosystem, Environment, Found, Where, ORDER,
};
pub use node::NodeModules;
pub use python::PythonEnv;
pub use rust::{cargo_lock_packages, LockPackage};

/// Directory names never entered while looking for environments (and never environments
/// themselves except as noted).
pub(crate) const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    "out",
    ".next",
    ".turbo",
    "vendor",
    "__pycache__",
    ".tox",
    ".mypy_cache",
];

// ---------------------------------------------------------------------------------------------
// Ecosystem contracts
// ---------------------------------------------------------------------------------------------

/// Inputs of every detection function (read only).
#[derive(Clone, Copy)]
pub struct DetectContext<'a> {
    /// Canonical repository root (read only).
    pub root: &'a Path,
    pub platform: &'a os::Platform,
    pub vars: &'a os::EnvVars,
    /// `RepoSettings::env_for(this ecosystem)`.
    pub env_override: Option<&'a Path>,
    /// Never read below these.
    pub forbidden: &'a [PathBuf],
    /// Inventoried code files (relative).
    pub files: &'a [(&'a str, Language)],
}

impl DetectContext<'_> {
    /// Whether `path` may be read (not inside a forbidden root).
    pub fn allowed(&self, path: &Path) -> bool {
        !self.forbidden.iter().any(|f| within(path, f))
    }
}

/// How a toolchain or environment was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Selected by a project pin file (`.python-version`, `rust-toolchain.toml`, ...).
    Pin,
    /// Chosen explicitly (`--env`, settings, `semantic.python_environment`).
    Override,
    /// Found on PATH.
    Path,
    /// A standard install location of the OS.
    StandardLocation,
    /// Found next to the project files.
    Project,
    /// The user's shared package cache (Cargo home, Go module cache).
    UserCache,
    /// trace's own tools folder.
    Tools,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Toolchain {
    /// "python", "node", "go", "jdk", "maven", "gradle", "dotnet-sdk", ...
    pub id: &'static str,
    pub root: PathBuf,
    pub version: Option<os::Version>,
    /// "go" -> .../bin/go.exe
    pub executables: BTreeMap<String, PathBuf>,
    pub origin: Origin,
    /// Module-specific facts ("GOMODCACHE" -> ..., "abi" -> "3.4.0").
    pub facts: BTreeMap<String, String>,
}

/// The toolchain search result of an ecosystem.
pub type ToolchainStatus = Found<Toolchain>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LibraryKind {
    Stdlib,
    Dependency,
}

/// A directory holding library code the servers read and trace-library derives from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LibraryRoot {
    pub path: PathBuf,
    pub kind: LibraryKind,
    pub ecosystem: EcosystemId,
    /// How package name + version are read from a path below `path`:
    /// "site_packages" | "node_modules" | "go_modcache" | "cargo_registry" | "rust_src" |
    /// "r_library" | "maven_repo" | "gradle_cache" | "coursier_cache" | "nuget_packages" |
    /// "cabal_store" | "php_vendor" | "toolchain_stdlib"
    pub layout: &'static str,
    /// Stdlib version when kind == Stdlib.
    pub version: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DepsStatus {
    Installed,
    Missing,
    NoneDeclared,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SubProject {
    pub dir: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DepsReport {
    pub status: DepsStatus,
    /// Package names (for status and the hint).
    pub missing: Vec<String>,
    /// "pip install -r requirements.txt", "go mod download", ...
    pub hint: String,
    pub roots: Vec<LibraryRoot>,
    /// Cheap content signal; changes invalidate semantic caches.
    pub fingerprint: String,
    /// Nested projects that are not the root or a declared workspace member: analysed on
    /// first use; their missing dependencies are status lines, not index errors.
    pub subprojects: Vec<SubProject>,
    pub notes: Vec<String>,
}

impl DepsReport {
    /// Nothing declared, nothing found (the stub answer of modules not written yet).
    pub fn none_declared() -> DepsReport {
        DepsReport {
            status: DepsStatus::NoneDeclared,
            missing: Vec::new(),
            hint: String::new(),
            roots: Vec::new(),
            fingerprint: String::new(),
            subprojects: Vec::new(),
            notes: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

pub(crate) fn hex(h: blake3::Hasher) -> String {
    h.finalize().to_hex()[..32].to_string()
}

/// `path` equals or is inside `root` (lexical, case-insensitive on Windows).
pub(crate) fn within(path: &Path, root: &Path) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('\\', "/");
        let s = s.trim_end_matches('/').to_string();
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s
        }
    };
    let (p, r) = (norm(path), norm(root));
    p == r || p.starts_with(&format!("{r}/"))
}

/// Sorted directory entries `(name, path)` (errors -> empty).
pub(crate) fn entries(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter_map(|e| e.file_name().into_string().ok().map(|n| (n, e.path())))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Sorted sub-directories `(name, path)`.
pub(crate) fn subdirs(dir: &Path) -> Vec<(String, PathBuf)> {
    entries(dir).into_iter().filter(|(_, p)| p.is_dir()).collect()
}

/// Test helpers (`tests/unit/support`).
#[cfg(test)]
#[path = "../tests/unit/support/mod.rs"]
pub(crate) mod test_support;
