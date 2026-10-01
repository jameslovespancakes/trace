//! Haskell (GHC + cabal / Stack) toolchain and dependency detection.
//!
//! Read-only; project code never runs. The only execution is `ghc --numeric-version`
//! (a toolchain binary) when no file names the version of a non-GHCup GHC.
//! * **Projects**: directories with `cabal.project` / `stack.yaml` (projects) and `*.cabal` /
//!   `package.yaml` (packages), found from the inventoried Haskell files' directories. The
//!   primary project is the repository root's, else the one holding most Haskell files; its
//!   packages (`packages:` of `cabal.project` / `stack.yaml`, default `.`) are served;
//!   every other project or package is an independent sub-project (pending).
//! * **Build tool**: the project's own `hie.yaml` cabal/stack cradle > `cabal.project*` or
//!   `dist-newstyle/` (cabal) > `stack.yaml` with `.stack-work/` and Stack installed (stack) >
//!   `*.cabal` (cabal) > `package.yaml` only (stack). A `stack.yaml` alone never selects Stack
//!   (its snapshot may pin a GHC no language server supports).
//! * **GHC**: the version the project asks for (`with-compiler:` of `cabal.project[.local]`,
//!   `compiler:` / the `.stack-work` install directory for Stack, `.tool-versions` / mise pins),
//!   found in GHCup (`<ghcup>/ghc/<v>`, `<ghcup>/bin/ghc-<v>`), on PATH, or in Stack's programs;
//!   without a request, `ghc` on PATH (version from the GHCup shim / symlink target), else
//!   GHCup's default. cabal and Stack from PATH / GHCup / standard locations.
//! * **Dependencies** (cabal): the closure of the local units in
//!   `dist-newstyle/cache/plan.json` (when it was planned for this GHC) must exist in the cabal
//!   store (`<store>/<compiler-id>[-<abi>]/<unit id>`); without a usable plan the Haskell setup
//!   plans the build itself (approved step) and checks the same closure. The Hackage package
//!   list must exist. Stack: the project's `.stack-work/install`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use trace_core::fingerprint::PartsHasher;
use trace_core::Language;

use crate::lookup::{self, Lookup};
use crate::os::{self, EnvVars, Os, Platform, Version};
use crate::relpath;
use crate::Where;
use crate::{
    DepsReport, DepsStatus, DetectContext, EcosystemId, LibraryKind, LibraryRoot, Origin, SubProject,
    Toolchain, ToolchainStatus,
};

mod deps;
mod projects;
mod toolchain;

pub use deps::*;
use projects::*;
pub use toolchain::*;

/// Dependency hint for cabal projects.
pub const DEPS_HINT_CABAL: &str = "cabal build --only-dependencies --enable-tests all";
/// Dependency hint for Stack projects.
pub const DEPS_HINT_STACK: &str = "stack build --only-dependencies --test --no-run-tests";

const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// How the project is built (and which hie-bios cradle HLS uses).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildTool {
    Cabal,
    Stack,
    /// Loose files without a cabal/Stack project (GHC directly).
    Direct,
}

impl BuildTool {
    pub const fn as_str(self) -> &'static str {
        match self {
            BuildTool::Cabal => "cabal",
            BuildTool::Stack => "stack",
            BuildTool::Direct => "direct",
        }
    }
}

/// The served Haskell project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HaskellProject {
    /// Relative directory (`""` = repository root).
    pub dir: String,
    pub tool: BuildTool,
    /// The project's own `hie.yaml` uses a cabal / stack cradle (kept as it is).
    pub own_cradle: bool,
    /// Only `package.yaml` (no `.cabal` file): needs Stack (hpack).
    pub package_yaml_only: bool,
    /// Served package directories (relative to the repository).
    pub packages: Vec<String>,
}

/// GHCup's directories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ghcup {
    /// `<base>/ghc/<v>`, `<base>/hls/<v>`.
    pub base: PathBuf,
    /// Shims / symlinks (`ghc`, `cabal`, `haskell-language-server-<ghc>`).
    pub bin: PathBuf,
}

/// Everything detected for the Haskell setup.
#[derive(Clone, Debug, PartialEq)]
pub struct HaskellSetup {
    pub project: Option<HaskellProject>,
    pub pending: Vec<SubProject>,
    pub ghcup: Option<Ghcup>,
    /// The GHC version the project asks for, and the file that asks.
    pub wanted: Option<(Version, String)>,
    pub ghc: Option<PathBuf>,
    pub ghc_version: Option<Version>,
    pub cabal: Option<PathBuf>,
    pub stack: Option<PathBuf>,
    pub cabal_dir: Option<PathBuf>,
    /// cabal uses the XDG layout (config, cache and store in separate XDG directories).
    pub cabal_xdg: bool,
    pub store_dir: Option<PathBuf>,
    /// cabal's `remote-repo-cache` (package lists).
    pub package_cache: Option<PathBuf>,
    /// Windows: GHCup's MSYS2 tool directories (configure scripts of dependencies).
    pub msys_dirs: Vec<PathBuf>,
    pub searched: Vec<String>,
}

/// The Haskell ecosystem ([`crate::Ecosystem`]).
pub struct Haskell;

impl crate::Ecosystem for Haskell {
    fn id(&self) -> crate::EcosystemId {
        crate::EcosystemId::Haskell
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
#[path = "../../../tests/unit/ecosystems/haskell/mod.rs"]
mod tests;
