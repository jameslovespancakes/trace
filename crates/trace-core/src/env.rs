//! The only reader of process environment variables.
//!
//! Every other module and crate asks the typed accessors here; nothing else calls
//! `std::env::var*`. Public variables (documented in SPEC / README):
//!
//! | variable | accessor | meaning |
//! |---|---|---|
//! | `TRACE_OFFLINE` | [`offline`] | no automatic installs |
//! | `TRACE_CACHE_DIR` | [`cache_dir`] | cache home (absolute) |
//! | `TRACE_SEMANTIC_TOOLS` | [`semantic_tools`] | semantic tools directory |
//! | `TRACE_PROFILE` | [`profile`] | phase timings on stderr |
//! | `TRACE_NO_AUTO_INSTALL` | [`no_auto_install`] | automatic installs off |
//! | `TRACE_FORBIDDEN_ROOTS` | [`forbidden_roots`] | extra forbidden roots (PATH-style) |
//! | `TRACE_SEMANTIC_PROCESSES` | [`semantic_processes`] | analyzer pool size override |
//!
//! The operating system's variables (`HOME`, `USERPROFILE`, `LOCALAPPDATA`, `XDG_CACHE_HOME`,
//! `PATH`, `SYSTEMROOT`) have accessors too; child processes get an allow-listed copy built
//! from [`vars`]. Test-only variables live in [`test`]. Debug switches are settings
//! (`debug.*` in `assets/config/defaults.jsonc`), not variables.

use std::ffi::OsString;
use std::path::PathBuf;

pub const OFFLINE: &str = "TRACE_OFFLINE";
pub const CACHE_DIR: &str = "TRACE_CACHE_DIR";
pub const SEMANTIC_TOOLS: &str = "TRACE_SEMANTIC_TOOLS";
pub const PROFILE: &str = "TRACE_PROFILE";
pub const NO_AUTO_INSTALL: &str = "TRACE_NO_AUTO_INSTALL";
/// Additional forbidden roots, separated like `PATH` (`;` on Windows, `:` elsewhere).
pub const FORBIDDEN_ROOTS: &str = "TRACE_FORBIDDEN_ROOTS";
/// Override for the analyzer process pool size (`1..=32`).
pub const SEMANTIC_PROCESSES: &str = "TRACE_SEMANTIC_PROCESSES";

fn raw(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

/// Set to a non-empty value.
fn non_empty(name: &str) -> Option<OsString> {
    raw(name).filter(|v| !v.is_empty())
}

/// Set to a non-empty value other than `0`.
fn switch_on(name: &str) -> bool {
    non_empty(name).is_some_and(|v| v != "0")
}

/// `TRACE_OFFLINE` set to a non-empty value other than `0`.
pub fn offline() -> bool {
    switch_on(OFFLINE)
}

/// `TRACE_NO_AUTO_INSTALL` set to a non-empty value other than `0`.
pub fn no_auto_install() -> bool {
    switch_on(NO_AUTO_INSTALL)
}

/// `TRACE_PROFILE` set to a non-empty value other than `0` (phase timings on stderr).
pub fn profile() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| raw(PROFILE).is_some_and(|v| !v.to_string_lossy().trim().is_empty() && v != "0"))
}

/// `TRACE_CACHE_DIR` (non-empty; the caller checks that it is absolute).
pub fn cache_dir() -> Option<PathBuf> {
    non_empty(CACHE_DIR).map(PathBuf::from)
}

/// `TRACE_SEMANTIC_TOOLS` (non-empty).
pub fn semantic_tools() -> Option<PathBuf> {
    non_empty(SEMANTIC_TOOLS).map(PathBuf::from)
}

/// Absolute entries of `TRACE_FORBIDDEN_ROOTS`.
pub fn forbidden_roots() -> Vec<PathBuf> {
    raw(FORBIDDEN_ROOTS)
        .map(|v| std::env::split_paths(&v).filter(|p| p.is_absolute()).collect())
        .unwrap_or_default()
}

/// `TRACE_SEMANTIC_PROCESSES` as a positive number (the caller clamps it).
pub fn semantic_processes() -> Option<usize> {
    raw(SEMANTIC_PROCESSES)
        .and_then(|v| v.to_str().and_then(|v| v.trim().parse::<usize>().ok()))
        .filter(|&n| n > 0)
}

/// `XDG_CACHE_HOME` when absolute.
pub fn xdg_cache_home() -> Option<PathBuf> {
    raw("XDG_CACHE_HOME").map(PathBuf::from).filter(|p| p.is_absolute())
}

/// The user's home directory (platform lookup of the `dirs` crate: `HOME` / `USERPROFILE`).
pub fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

/// The per-user local data directory (`%LOCALAPPDATA%`, `~/.local/share`,
/// `~/Library/Application Support`).
pub fn data_local_dir() -> Option<PathBuf> {
    dirs::data_local_dir()
}

/// Every entry of `PATH`, in order (relative entries included; callers filter).
pub fn path_dirs() -> Vec<PathBuf> {
    raw("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default()
}

/// `PATH` as given.
pub fn path() -> Option<OsString> {
    raw("PATH")
}

/// The Windows directory: `SystemRoot`, `SYSTEMROOT` or `windir`.
pub fn system_root() -> Option<OsString> {
    raw("SystemRoot")
        .or_else(|| raw("SYSTEMROOT"))
        .or_else(|| raw("windir"))
}

/// One variable by name, for the fixed allow-lists of child-process environments (PATH,
/// SystemRoot, TEMP ...). Configuration never comes from here: use the typed accessors.
pub fn var(name: &str) -> Option<OsString> {
    raw(name)
}

/// Snapshot of every variable with a UTF-8 name (child-process environments are filtered from
/// it by an allow-list; `trace_env::os::EnvVars`).
pub fn vars() -> Vec<(String, OsString)> {
    std::env::vars_os()
        .filter_map(|(k, v)| k.into_string().ok().map(|k| (k, v)))
        .collect()
}

/// Test-only variables (read by test code only).
pub mod test {
    use std::ffi::OsString;
    use std::path::PathBuf;

    /// `TRACE_TEST_PYTHON`: the Python interpreter of opt-in tests.
    pub fn python() -> Option<OsString> {
        super::raw("TRACE_TEST_PYTHON")
    }

    /// `TRACE_BRIDGE_FIXTURES`: the bridge family fixtures (opt-in tests).
    pub fn bridge_fixtures() -> Option<PathBuf> {
        super::raw("TRACE_BRIDGE_FIXTURES").map(PathBuf::from)
    }

    /// `TRACE_TEST_KEEP`: keep test cache directories.
    pub fn keep() -> bool {
        super::raw("TRACE_TEST_KEEP").is_some()
    }

    /// `TRACE_TEST_LOCK_CHILD`: the lock file a child test process holds.
    pub const LOCK_CHILD: &str = "TRACE_TEST_LOCK_CHILD";

    /// [`LOCK_CHILD`].
    pub fn lock_child() -> Option<PathBuf> {
        super::raw(LOCK_CHILD).map(PathBuf::from)
    }

    /// `HOME` / `USERPROFILE` (first set).
    pub fn home() -> Option<OsString> {
        super::raw("HOME").or_else(|| super::raw("USERPROFILE"))
    }

    /// Any variable (tests that read a variable named by a fixture).
    pub fn var(name: &str) -> Option<OsString> {
        super::raw(name)
    }
}
