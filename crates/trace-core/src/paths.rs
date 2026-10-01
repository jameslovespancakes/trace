//! Cache location resolution. Every cache/snapshot/output path is outside inspected roots.
//!
//! Cache home, first match wins:
//! 1. `TRACE_CACHE_DIR` (must be absolute; tests use `<temp>/trace-tests/*`);
//! 2. Windows: `%LOCALAPPDATA%\trace`;
//! 3. otherwise `$XDG_CACHE_HOME/trace` (absolute only) or `~/.cache/trace`.
//!
//! Layout (see SPEC §4):
//! ```text
//! <home>/config.json            optional global config
//! <home>/assets/<hash>/         materialized ts-worker/*.mjs, lsp_guard.cjs
//! <home>/repos/<key>/meta.json
//! <home>/repos/<key>/index.bin
//! <home>/repos/<key>/index.lock
//! <home>/repos/<key>/similar.bin
//! <home>/repos/<key>/workspaces/<backend>-<uuid>/
//! ```
//!
//! Forbidden roots ([`FORBIDDEN_ROOTS`], plus `TRACE_FORBIDDEN_ROOTS`) are never inspected:
//! they are compared lexically (never stat'ed or read) against inspected roots, and the
//! inventory walk never descends into them.

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::error::{CoreError, Result};
use crate::fingerprint::Hash32;
use crate::inventory::{canonical_root, strip_verbatim};

/// Built-in forbidden roots (none). Machine-specific directories that must never be
/// inspected go into `<home>/config.json` `forbidden_roots` or `TRACE_FORBIDDEN_ROOTS`.
pub(crate) const FORBIDDEN_ROOTS: &[&str] = &[];

/// Resolve the cache home directory (not created).
pub fn cache_home() -> Result<PathBuf> {
    if let Some(dir) = crate::env::cache_dir() {
        if !dir.is_absolute() {
            return Err(CoreError::Config(format!("{} must be an absolute path", crate::env::CACHE_DIR)));
        }
        // Verbatim (`\\?\C:\...`) spellings are reduced to the ordinary form: analyzers such as
        // node cannot load modules from verbatim paths.
        return Ok(normalize_lexically(&strip_verbatim(dir)));
    }
    let base = if cfg!(windows) {
        crate::env::data_local_dir()
    } else {
        crate::env::xdg_cache_home().or_else(|| crate::env::home_dir().map(|h| h.join(".cache")))
    };
    base.map(|b| b.join("trace")).ok_or_else(|| {
        CoreError::Config("trace could not find a folder for its cache; set TRACE_CACHE_DIR to one".into())
    })
}

/// Remove `.` and resolve `..` components without touching the file system.
pub(crate) fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            // `..` above an absolute root stays at the root; leading `..` of relative paths
            // are kept.
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Lexical comparison key: `/`-separated, verbatim prefix removed, no trailing separator,
/// ASCII-lowercased on Windows (case-insensitive file system).
pub fn path_key(path: &Path) -> String {
    let path = strip_verbatim(normalize_lexically(path));
    let mut key = path.to_string_lossy().replace('\\', "/");
    while key.len() > 1 && key.ends_with('/') && !key.ends_with(":/") {
        key.pop();
    }
    if cfg!(windows) {
        key.make_ascii_lowercase();
    }
    key
}

/// Lexical containment: `path` equals `root` or lies below it (component boundary).
pub fn is_within(path: &Path, root: &Path) -> bool {
    key_within(&path_key(path), &path_key(root))
}

fn key_within(path: &str, root: &str) -> bool {
    match path.strip_prefix(root) {
        Some("") => true,
        Some(rest) => rest.starts_with('/') || root.ends_with('/'),
        None => false,
    }
}

/// Built-in forbidden roots, `<home>/config.json` `forbidden_roots` and
/// `TRACE_FORBIDDEN_ROOTS` entries (absolute only). A config file that cannot be read adds
/// nothing here; commands that load the configuration report its errors.
pub fn forbidden_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = FORBIDDEN_ROOTS.iter().map(PathBuf::from).collect();
    if let Ok(home) = cache_home() {
        if let Ok(cfg) = crate::config::Settings::load(&home) {
            roots.extend(
                cfg.forbidden_roots
                    .iter()
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute()),
            );
        }
    }
    roots.extend(crate::env::forbidden_roots());
    roots
}

/// Fail if `path` is (inside) a forbidden root. Purely lexical: forbidden roots are never
/// touched. `path` should already be canonical (see [`canonical_root`]).
pub(crate) fn ensure_not_forbidden(path: &Path) -> Result<()> {
    match forbidden_roots().iter().find(|f| is_within(path, f)) {
        Some(_) => Err(CoreError::Excluded(path.to_path_buf())),
        None => Ok(()),
    }
}

/// Forbidden roots strictly below `root` (the inventory walk must skip them), as keys.
pub(crate) fn forbidden_keys_below(root: &Path) -> Vec<String> {
    let root_key = path_key(root);
    forbidden_roots()
        .iter()
        .map(|f| path_key(f))
        .filter(|k| *k != root_key && key_within(k, &root_key))
        .collect()
}

/// Fail if `path` is inside any of `roots` (compared on canonical display forms when possible,
/// case-insensitively on Windows). `path` and roots need not exist yet.
pub fn ensure_outside(path: &Path, roots: &[&Path]) -> Result<()> {
    let probe = path_key(&nearest_existing_canonical(path));
    for root in roots {
        let root = path_key(&nearest_existing_canonical(root));
        if key_within(&probe, &root) {
            return Err(CoreError::InsideInspectedRoot(path.to_path_buf()));
        }
    }
    Ok(())
}

/// Canonicalize the deepest existing ancestor and re-append the rest (path may not exist yet).
fn nearest_existing_canonical(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut existing = normalize_lexically(&absolute);
    let mut rest = Vec::new();
    while fs::symlink_metadata(&existing).is_err() {
        match (existing.file_name().map(|n| n.to_os_string()), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            _ => return absolute,
        }
    }
    let mut out = fs::canonicalize(&existing).map(strip_verbatim).unwrap_or(existing);
    for part in rest.into_iter().rev() {
        out.push(part);
    }
    out
}

/// All per-repository cache paths.
#[derive(Clone, Debug)]
pub struct RepoPaths {
    /// Canonical inspected root (display form).
    pub root: PathBuf,
    pub home: PathBuf,
    pub key: String,
    pub repo_dir: PathBuf,
    pub meta_file: PathBuf,
    pub index_file: PathBuf,
    pub lock_file: PathBuf,
    pub similar_file: PathBuf,
    pub workspaces_dir: PathBuf,
    /// Per-repository settings (`--allow-build`, `--env`): `repo_dir/settings.json`.
    pub settings_file: PathBuf,
}

impl RepoPaths {
    /// Resolve paths for `root` under the cache home. Errors if the home is inside `root`.
    pub fn resolve(root: &Path) -> Result<RepoPaths> {
        let home = cache_home()?;
        Self::resolve_in(root, &home)
    }

    /// Resolve under an explicit home (must be absolute and outside `root`).
    pub fn resolve_in(root: &Path, home: &Path) -> Result<RepoPaths> {
        let root = canonical_root(root)?;
        ensure_not_forbidden(&root)?;
        if !home.is_absolute() {
            return Err(CoreError::Config(format!("cache home must be absolute: {}", home.display())));
        }
        let home = normalize_lexically(home);
        ensure_outside(&home, &[&root])?;
        let key = repo_key(&root);
        let repo_dir = home.join("repos").join(&key);
        Ok(RepoPaths {
            meta_file: repo_dir.join("meta.json"),
            index_file: repo_dir.join("index.bin"),
            lock_file: repo_dir.join("index.lock"),
            similar_file: repo_dir.join("similar.bin"),
            workspaces_dir: repo_dir.join("workspaces"),
            settings_file: repo_dir.join("settings.json"),
            repo_dir,
            key,
            home,
            root,
        })
    }

    /// Root in display form as stored in [`crate::IndexHeader::root`].
    pub fn root_display(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }

    /// OS lock held by a running `trace index --watch` for its whole life:
    /// other commands wait for that watcher to publish its updates instead of updating
    /// themselves.
    pub fn watch_lock_file(&self) -> PathBuf {
        self.repo_dir.join("watch.lock")
    }

    /// Whether a `trace index --watch` process watches this repository now.
    pub fn watching(&self) -> bool {
        crate::cache::CacheLock::is_held(&self.watch_lock_file())
    }

    /// Create the repository cache directory (outside the root, re-checked).
    pub fn ensure_repo_dir(&self) -> Result<()> {
        ensure_outside(&self.repo_dir, &[&self.root])?;
        fs::create_dir_all(&self.repo_dir).map_err(|e| CoreError::io(&self.repo_dir, e))
    }
}

/// First 16 hex chars of blake3 over the canonical root path string (lower-cased on Windows).
pub(crate) fn repo_key(root: &Path) -> String {
    let text = root.to_string_lossy();
    let text = if cfg!(windows) {
        text.to_lowercase()
    } else {
        text.into_owned()
    };
    Hash32::of(text.as_bytes()).hex_prefix(16)
}

#[cfg(test)]
#[path = "../tests/unit/paths.rs"]
mod tests;
