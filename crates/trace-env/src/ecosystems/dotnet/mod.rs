//! .NET (C#) toolchain + dependency detection.
//!
//! Everything here is read-only and never runs a program:
//!
//! * **Projects**: every `*.csproj` of the repository (XML, never evaluated by MSBuild), the
//!   solutions (`*.sln` line format, `*.slnx` XML) and the choice of what Roslyn opens: the
//!   solution covering most of the repository's C# projects ([`choose_solution`]), else every
//!   project. Required projects = the chosen solution's projects plus the projects they
//!   reference; every other project is a sub-project (set up on first use).
//! * **SDK**: `global.json` (`sdk.version`, `sdk.rollForward`, `sdk.allowPrerelease`,
//!   `sdk.paths`) nearest to the opened solution; dotnet roots from `sdk.paths` -> `--env`
//!   override -> `DOTNET_ROOT` -> `dotnet` on PATH (symlinks resolved) -> standard install
//!   locations; SDK versions are the directory names `<root>/sdk/<version>/dotnet.dll`, and
//!   the hostfxr roll-forward rules are applied to that list ([`select_sdk`]).
//! * **Workloads** (`-ios`, `-android`, `-maccatalyst`, `-macos`, `-tvos`, `-tizen` target
//!   frameworks, `UseMaui`): installed when `<root>/metadata/workloads/.../<band>/InstalledWorkloads/<id>` exists.
//! * **Restore** (dependencies): every required SDK-style project has
//!   `obj/project.assets.json` covering all its target frameworks, `obj/<file>.nuget.g.props`,
//!   and every package library's `.nupkg.metadata` (NuGet's own "installed" marker) in a
//!   package folder of the assets file; legacy `packages.config` projects need
//!   `<solution dir>/packages/<id>.<version>/`. Measured: an unrestored project still loads in
//!   Roslyn and answers WRONG (other overload, 3x references), so this check is mandatory.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use trace_core::formats::xml::{self, Element};

use crate::lookup;
use crate::os::{self, Arch, EnvVars, Os, Platform, Version, VersionReq};
use crate::relpath;
use crate::{
    hex, subdirs, DepsReport, DepsStatus, DetectContext, EcosystemId, LibraryKind, LibraryRoot, Origin,
    SubProject, Toolchain, ToolchainStatus,
};

mod project;
mod restore;
mod sdk;

pub use project::*;
pub use restore::*;
pub use sdk::*;

/// Upper bound of directory entries visited by one repository walk.
pub(crate) const WALK_LIMIT: usize = 200_000;
/// Deepest directory level a repository walk enters.
pub(crate) const WALK_DEPTH: usize = 24;

/// Relative (`/`-separated) paths of the files below `root` for which `want(name)` holds, in
/// sorted order. Directories for which `skip_dir(name)` holds and paths `allowed` rejects are
/// not entered; symbolic links are never followed. Bounded by [`WALK_LIMIT`] / [`WALK_DEPTH`].
pub(crate) fn walk_files(
    root: &Path,
    allowed: &dyn Fn(&Path) -> bool,
    skip_dir: &dyn Fn(&str) -> bool,
    want: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, String, usize)> = vec![(root.to_path_buf(), String::new(), 0)];
    let mut visited = 0usize;
    while let Some((dir, rel, depth)) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        let mut items: Vec<(String, PathBuf, bool, bool)> = Vec::new();
        for entry in rd.flatten() {
            visited += 1;
            if visited > WALK_LIMIT {
                break;
            }
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            items.push((name, entry.path(), kind.is_dir(), kind.is_file()));
        }
        items.sort();
        for (name, path, is_dir, is_file) in items.into_iter().rev() {
            let child_rel = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if is_dir {
                if depth + 1 < WALK_DEPTH && !skip_dir(&name) && allowed(&path) {
                    stack.push((path, child_rel, depth + 1));
                }
            } else if is_file && want(&name) {
                out.push(child_rel);
            }
        }
        if visited > WALK_LIMIT {
            break;
        }
    }
    out.sort();
    out
}

/// `path` inside `base` joined lexically (`..` and `.` resolved, `\` accepted); `None` when it
/// leaves the repository (above the root) or is absolute.
pub(crate) fn join_rel(base: &str, path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.as_bytes().get(1) == Some(&b':')
    {
        return None;
    }
    relpath::normalize(base, path)
}

/// The Dotnet ecosystem ([`crate::Ecosystem`]).
pub struct Dotnet;

impl crate::Ecosystem for Dotnet {
    fn id(&self) -> crate::EcosystemId {
        crate::EcosystemId::Dotnet
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
#[path = "../../../tests/unit/ecosystems/dotnet/mod.rs"]
mod tests;
