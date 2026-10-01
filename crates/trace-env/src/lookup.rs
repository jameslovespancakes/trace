//! Program lookup: the only code that searches `PATH`, project-local environments, standard
//! install locations and trace's tools folder for a program, always in [`ORDER`].
//!
//! * [`Lookup`] collects the directories of each step and finds the first program
//!   ([`Lookup::find`]); callers add only the steps that apply (a server runtime: only
//!   [`Where::Tools`]; a toolchain: every step).
//! * [`path_dirs`] / [`compose_path`] read and build `PATH` values for child processes.
//! * [`mise_installs`] / [`asdf_installs`]: the install folders of the version managers mise
//!   and asdf, standard locations of the toolchains they install (their `PATH` entries are
//!   shims, which name no toolchain folder).

use std::path::{Path, PathBuf};

use crate::environment::{Where, ORDER};
use crate::os::{self, EnvVars, Os, Platform};

/// Directories to search, per lookup step.
pub struct Lookup<'a> {
    platform: &'a Platform,
    /// Programs inside these are never returned.
    forbidden: &'a [PathBuf],
    steps: Vec<(Where, PathBuf)>,
}

impl<'a> Lookup<'a> {
    pub fn new(platform: &'a Platform, forbidden: &'a [PathBuf]) -> Lookup<'a> {
        Lookup {
            platform,
            forbidden,
            steps: Vec::new(),
        }
    }

    /// Add `dirs` to step `step` (kept in [`ORDER`] whatever the call order).
    pub fn with(mut self, step: Where, dirs: impl IntoIterator<Item = PathBuf>) -> Lookup<'a> {
        self.steps.extend(dirs.into_iter().map(|d| (step, d)));
        self.steps.sort_by_key(|(w, _)| ORDER.iter().position(|o| o == w));
        self
    }

    /// Add the absolute entries of the `PATH` of `vars` ([`Where::Path`]).
    pub fn with_path(self, vars: &EnvVars) -> Lookup<'a> {
        self.with(Where::Path, os::path_dirs(vars))
    }

    /// The directories in lookup order.
    pub fn dirs(&self) -> impl Iterator<Item = (Where, &Path)> {
        self.steps.iter().map(|(w, d)| (*w, d.as_path()))
    }

    /// The first `<dir>/<name>` (directories in lookup order, then `names` in order) that is
    /// an executable file outside the forbidden roots, with the step that found it.
    pub fn find(&self, names: &[&str]) -> Option<(PathBuf, Where)> {
        self.steps.iter().find_map(|(step, dir)| {
            os::find_executable(names, std::slice::from_ref(dir), self.platform)
                .filter(|exe| !self.forbidden.iter().any(|f| crate::within(exe, f)))
                .map(|exe| (exe, *step))
        })
    }
}

/// A program on the `PATH` of `vars` (first match of `names`).
pub fn on_path(names: &[&str], vars: &EnvVars, platform: &Platform) -> Option<PathBuf> {
    Lookup::new(platform, &[])
        .with_path(vars)
        .find(names)
        .map(|(exe, _)| exe)
}

/// mise's data folder: `MISE_DATA_DIR`, else `%LOCALAPPDATA%\mise` (Windows) or
/// `$XDG_DATA_HOME/mise`, else `~/.local/share/mise`.
fn mise_dir(vars: &EnvVars, p: &Platform) -> Option<PathBuf> {
    vars.path("MISE_DATA_DIR").or_else(|| match p.os {
        Os::Windows => vars.path("LOCALAPPDATA").map(|d| d.join("mise")),
        _ => vars
            .path("XDG_DATA_HOME")
            .map(|d| d.join("mise"))
            .or_else(|| os::home_dir(vars, p).map(|h| h.join(".local").join("share").join("mise"))),
    })
}

/// asdf's data folder: `ASDF_DATA_DIR`, else `~/.asdf`.
fn asdf_dir(vars: &EnvVars, p: &Platform) -> Option<PathBuf> {
    vars.path("ASDF_DATA_DIR")
        .or_else(|| os::home_dir(vars, p).map(|h| h.join(".asdf")))
}

/// `<mise>/installs/<tool>`: the folder holding one folder per installed version.
pub fn mise_tool_dir(vars: &EnvVars, p: &Platform, tool: &str) -> Option<PathBuf> {
    mise_dir(vars, p).map(|d| d.join("installs").join(tool))
}

/// `<asdf>/installs/<plugin>`: the folder holding one folder per installed version.
pub fn asdf_tool_dir(vars: &EnvVars, p: &Platform, plugin: &str) -> Option<PathBuf> {
    asdf_dir(vars, p).map(|d| d.join("installs").join(plugin))
}

/// The versions of `tool` mise installed, newest first.
pub fn mise_installs(vars: &EnvVars, p: &Platform, tool: &str) -> Vec<PathBuf> {
    versions_in(mise_tool_dir(vars, p, tool))
}

/// The versions of asdf plugin `plugin` installed, newest first.
pub fn asdf_installs(vars: &EnvVars, p: &Platform, plugin: &str) -> Vec<PathBuf> {
    versions_in(asdf_tool_dir(vars, p, plugin))
}

fn versions_in(dir: Option<PathBuf>) -> Vec<PathBuf> {
    dir.map(|d| {
        os::versioned_children(&d, "")
            .into_iter()
            .map(|(_, dir)| dir)
            .collect()
    })
    .unwrap_or_default()
}

/// Absolute directories of this process's `PATH`, in order, without duplicates.
pub fn path_dirs() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in trace_core::env::path_dirs() {
        if dir.is_absolute() && !out.contains(&dir) {
            out.push(dir);
        }
    }
    out
}

/// A `PATH` value: `first` directories, then the `PATH` entries of `vars` (deduplicated).
pub fn compose_path(first: &[PathBuf], vars: &EnvVars, platform: &Platform) -> String {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for d in first.iter().cloned().chain(os::path_dirs(vars)) {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    let sep = platform.path_list_sep().to_string();
    dirs.iter()
        .map(|d| d.display().to_string())
        .collect::<Vec<_>>()
        .join(&sep)
}

#[cfg(test)]
#[path = "../tests/unit/lookup.rs"]
mod tests;
