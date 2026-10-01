//! Read-only source inventory.
//!
//! Uses the `ignore` crate: respects `.gitignore`/`.ignore`/`.git/info/exclude` even without
//! git (the files are read, git is never run), skips hidden entries (so `.env*`, `.git`,
//! `.venv`, `.cargo` never appear), excluded build/vendor directories, forbidden roots,
//! sensitive file names and symlinks/junctions. Never runs git, hooks or any target code.
//!
//! Files that would otherwise be indexed but are refused are listed in
//! [`Inventory::omitted`] with a reason (`symlink`, `sensitive`, `file_size_limit`,
//! `unreadable`, `non_utf8_path`, `unsupported_path`, `forbidden_root`, `excluded`); they
//! are never silently dropped. Hidden entries and excluded directories are skipped without
//! listing (their contents are never enumerated).
//!
//! User exclusions ([`InventoryOptions::exclude`], config `inventory.exclude`): gitignore-style
//! globs relative to the root (`examples/`, `benchmarks/**`, `*.gen.py`). A matching file is
//! listed as omitted with reason `excluded`; a matching directory is not entered. Nothing
//! excluded is analysed, set up or required, and `trace status` prints the globs, so nothing
//! is ever skipped silently.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use ignore::WalkBuilder;
use rayon::prelude::*;

use crate::error::{CoreError, Result};
use crate::fingerprint::{Hash32, PartsHasher};
use crate::languages::Language;
use crate::model::OmittedFile;
use crate::paths::{ensure_not_forbidden, forbidden_keys_below, path_key};

/// Directory names never descended into (in addition to hidden directories).
pub(crate) const EXCLUDED_DIRS: &[&str] = &[
    "node_modules",
    "vendor",
    "venv",
    "__pycache__",
    "dist",
    "build",
    "target",
    "creds",
    "credentials",
    "site-packages",
    "bower_components",
];

/// File names never read (case-insensitive), in addition to hidden files like `.env`.
pub(crate) const SENSITIVE_NAMES: &[&str] = &[
    "auth.json",
    "credentials.json",
    "secrets.json",
    "secrets.yaml",
    "secrets.yml",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
];

/// Sensitive extensions (keys/certificates), case-insensitive.
pub(crate) const SENSITIVE_EXTENSIONS: &[&str] = &["pem", "key", "p12", "pfx", "jks", "keystore"];

/// Configuration files fingerprinted (never executed) and copied into semantic snapshots.
pub(crate) const CONFIG_NAMES: &[&str] = &[
    "pyproject.toml",
    "pyrightconfig.json",
    "requirements.txt",
    "requirements.lock",
    "uv.lock",
    "poetry.lock",
    "package.json",
    "package-lock.json",
    "tsconfig.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "go.mod",
    "go.sum",
    "Cargo.toml",
    "Cargo.lock",
    "compile_commands.json",
    "global.json",
    // Language-server workspace configs (registry `workspace.configs`).
    "composer.json",
    "jsconfig.json",
    // Test-runner / build configuration read by `trace_syntax::testing::TestConfig`.
    "pytest.ini",
    "tox.ini",
    "setup.cfg",
    "requirements-dev.txt",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "DESCRIPTION",
    "jest.config.json",
    ".mocharc.json",
    ".mocharc.yml",
];

/// Configuration file-name patterns (one `*`; never a source-file extension, so editing a
/// source never counts as a configuration change): `tsconfig.<name>.json`, MSBuild
/// projects.
pub(crate) const CONFIG_PATTERNS: &[&str] = &["tsconfig.*.json", "*.csproj"];

/// Whether a file name is a configuration file ([`CONFIG_NAMES`], [`CONFIG_PATTERNS`]).
pub fn is_config_name(name: &str) -> bool {
    CONFIG_NAMES.contains(&name)
        || CONFIG_PATTERNS.iter().any(|p| {
            p.split_once('*').is_some_and(|(prefix, suffix)| {
                name.len() > prefix.len() + suffix.len() && name.starts_with(prefix) && name.ends_with(suffix)
            })
        })
}

/// Inventory limits and user exclusions.
/// Defaults: `inventory` in `assets/config/defaults.jsonc`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InventoryOptions {
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    /// Gitignore-style globs (relative to the root) of files and folders the user excludes
    /// from analysis (module docs).
    pub exclude: Vec<String>,
}

/// One inventoried file (not yet hashed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryEntry {
    /// Repository-relative, `/`-separated.
    pub path: String,
    pub abs: PathBuf,
    pub language: Option<Language>,
    pub is_config: bool,
    pub size: u64,
    pub mtime_ns: u64,
}

/// Result of a scan: sources (known language) and configuration files, sorted by path.
#[derive(Clone, Debug, Default)]
pub struct Inventory {
    pub root: PathBuf,
    pub sources: Vec<InventoryEntry>,
    pub configs: Vec<InventoryEntry>,
    /// Refused files with reasons, sorted by path.
    pub omitted: Vec<OmittedFile>,
    /// Non-file problems met during the walk (e.g. unparsable ignore files), for diagnostics.
    pub warnings: Vec<String>,
}

/// Hidden or excluded directory name (case-insensitive for excluded names).
pub fn is_excluded_dir(name: &str) -> bool {
    name.starts_with('.') || EXCLUDED_DIRS.iter().any(|d| d.eq_ignore_ascii_case(name))
}

/// Sensitive file name: `.env*`, [`SENSITIVE_NAMES`] or [`SENSITIVE_EXTENSIONS`].
pub fn is_sensitive_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with(".env") || SENSITIVE_NAMES.contains(&lower.as_str()) {
        return true;
    }
    Path::new(&lower)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SENSITIVE_EXTENSIONS.contains(&e))
}

/// Canonicalize a root directory and return it in display form (no `\\?\` prefix).
pub fn canonical_root(root: &Path) -> Result<PathBuf> {
    let canon = fs::canonicalize(root).map_err(|_| CoreError::InvalidRoot(root.to_path_buf()))?;
    if !canon.is_dir() {
        return Err(CoreError::InvalidRoot(root.to_path_buf()));
    }
    Ok(strip_verbatim(canon))
}

/// Remove the Windows verbatim prefix (`\\?\C:\x` -> `C:\x`, `\\?\UNC\s\x` -> `\\s\x`).
pub fn strip_verbatim(path: PathBuf) -> PathBuf {
    let Some(s) = path.to_str() else { return path };
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if rest.as_bytes().get(1) == Some(&b':') {
            return PathBuf::from(rest);
        }
    }
    path
}

fn mtime_ns(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// `/`-separated relative path of `path` under `root`; `None` if outside or not UTF-8.
fn relative_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let mut out = String::new();
    for c in rel.components() {
        match c {
            Component::Normal(part) => {
                if !out.is_empty() {
                    out.push('/');
                }
                out.push_str(part.to_str()?);
            }
            _ => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Display form of a path relative to root, lossy (for omitted entries only).
fn lossy_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Innermost path and whether the root cause is an I/O error.
fn classify_walk_error(err: &ignore::Error) -> (Option<&Path>, bool) {
    match err {
        ignore::Error::WithPath { path, err } => {
            let (inner, io) = classify_walk_error(err);
            (inner.or(Some(path.as_path())), io)
        }
        ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => {
            classify_walk_error(err)
        }
        ignore::Error::Loop { child, .. } => (Some(child.as_path()), true),
        ignore::Error::Io(_) => (None, true),
        ignore::Error::Partial(errs) => errs.first().map(classify_walk_error).unwrap_or((None, false)),
        _ => (None, false),
    }
}

/// The user's exclusion globs as a matcher rooted at `root` (config error on a bad glob).
pub fn exclusion_matcher(root: &Path, globs: &[String]) -> Result<Option<ignore::gitignore::Gitignore>> {
    if globs.is_empty() {
        return Ok(None);
    }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
    for glob in globs {
        builder
            .add_line(None, glob)
            .map_err(|e| CoreError::Config(format!("The exclude pattern \"{glob}\" is not valid: {e}")))?;
    }
    builder
        .build()
        .map(Some)
        .map_err(|e| CoreError::Config(format!("The exclude patterns are not valid: {e}")))
}

/// Whether `path` (below `root`) or one of its parents matches the exclusion globs.
fn is_user_excluded(matcher: Option<&ignore::gitignore::Gitignore>, path: &Path, is_dir: bool) -> bool {
    matcher.is_some_and(|m| m.matched_path_or_any_parents(path, is_dir).is_ignore())
}

/// Walk `root` (canonical, see [`canonical_root`]) and inventory sources and configuration
/// files. Read-only: only directory listings, metadata and ignore files are read.
pub fn scan(root: &Path, opts: &InventoryOptions) -> Result<Inventory> {
    if !root.is_dir() {
        return Err(CoreError::InvalidRoot(root.to_path_buf()));
    }
    ensure_not_forbidden(root)?;
    let blocked = forbidden_keys_below(root);
    let blocked_hits: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
    let excluded = exclusion_matcher(root, &opts.exclude)?.map(Arc::new);

    let mut builder = WalkBuilder::new(root);
    {
        let blocked_hits = Arc::clone(&blocked_hits);
        let excluded = excluded.clone();
        builder
            .hidden(true)
            .parents(true)
            .ignore(true)
            .git_ignore(true)
            .git_exclude(true)
            .git_global(false)
            .require_git(false)
            .follow_links(false)
            .same_file_system(false)
            .filter_entry(move |entry| {
                if entry.depth() == 0 || !entry.file_type().is_some_and(|t| t.is_dir()) {
                    return true;
                }
                if is_excluded_dir(&entry.file_name().to_string_lossy()) {
                    return false;
                }
                if is_user_excluded(excluded.as_deref(), entry.path(), true) {
                    return false;
                }
                if !blocked.is_empty() && blocked.contains(&path_key(entry.path())) {
                    blocked_hits
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(entry.path().to_path_buf());
                    return false;
                }
                true
            });
    }

    let mut inv = Inventory {
        root: root.to_path_buf(),
        ..Inventory::default()
    };
    let omit = |inv: &mut Inventory, path: String, reason: &str| {
        inv.omitted.push(OmittedFile {
            path,
            reason: reason.into(),
        })
    };
    let mut total: u64 = 0;
    for result in builder.build() {
        let entry = match result {
            Ok(e) => e,
            Err(err) => {
                match classify_walk_error(&err) {
                    (Some(path), true) => omit(&mut inv, lossy_relative(root, path), "unreadable"),
                    _ => inv.warnings.push(err.to_string()),
                }
                continue;
            }
        };
        if entry.depth() == 0 {
            continue;
        }
        let Some(rel) = relative_path(root, entry.path()) else {
            omit(&mut inv, lossy_relative(root, entry.path()), "non_utf8_path");
            continue;
        };
        if entry.path_is_symlink() {
            // Symlinks and junctions are never followed or read.
            omit(&mut inv, rel, "symlink");
            continue;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if rel.contains(':') {
            // `:` separates path and name in symbol ids; such files cannot be addressed.
            omit(&mut inv, rel, "unsupported_path");
            continue;
        }
        if is_user_excluded(excluded.as_deref(), entry.path(), false) {
            omit(&mut inv, rel, "excluded");
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if is_sensitive_name(&name) {
            omit(&mut inv, rel, "sensitive");
            continue;
        }
        let is_config = is_config_name(&name);
        let mut language = crate::languages::from_path(entry.path());
        if language.is_none() && entry.path().extension().is_none() {
            // Extension-less scripts: the interpreter named by the shebang line.
            language = crate::languages::from_shebang(&first_line(entry.path()));
        }
        if language.is_none() && !is_config {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => {
                omit(&mut inv, rel, "unreadable");
                continue;
            }
        };
        if meta.len() > opts.max_file_bytes {
            omit(&mut inv, rel, "file_size_limit");
            continue;
        }
        total = total.saturating_add(meta.len());
        if total > opts.max_total_bytes {
            return Err(CoreError::Limit(format!(
                "more than {} bytes of source; narrow the root",
                opts.max_total_bytes
            )));
        }
        let item = InventoryEntry {
            path: rel,
            abs: entry.path().to_path_buf(),
            language,
            is_config,
            size: meta.len(),
            mtime_ns: mtime_ns(&meta),
        };
        match (is_config, language.is_some()) {
            (true, true) => {
                inv.configs.push(item.clone());
                inv.sources.push(item);
            }
            (true, false) => inv.configs.push(item),
            _ => inv.sources.push(item),
        }
        if inv.sources.len() > opts.max_files {
            return Err(CoreError::Limit(format!(
                "more than {} source files; narrow the root",
                opts.max_files
            )));
        }
    }
    let hits = std::mem::take(&mut *blocked_hits.lock().unwrap_or_else(|e| e.into_inner()));
    for dir in hits {
        let rel = lossy_relative(root, &dir);
        omit(&mut inv, rel, "forbidden_root");
    }
    inv.sources.sort_by(|a, b| a.path.cmp(&b.path));
    inv.configs.sort_by(|a, b| a.path.cmp(&b.path));
    inv.omitted.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(inv)
}

/// Validate a repository-relative path component by component and return the absolute path.
/// Rejects absolute paths, `..`, hidden/excluded components, sensitive names and symlinks,
/// and anything whose canonical form escapes `root` (port of permissions.py `safe_source`).
/// `root` must be canonical (display form).
pub fn safe_source_path(root: &Path, rel: &str) -> Result<PathBuf> {
    if rel.is_empty() || rel.contains('\\') || rel.contains(':') || rel.starts_with('/') {
        return Err(CoreError::InvalidRelativePath(rel.to_string()));
    }
    let mut cursor = root.to_path_buf();
    let parts: Vec<&str> = rel.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() || *part == "." || *part == ".." {
            return Err(CoreError::InvalidRelativePath(rel.to_string()));
        }
        let last = i + 1 == parts.len();
        if (!last && is_excluded_dir(part)) || part.starts_with('.') || (last && is_sensitive_name(part)) {
            return Err(CoreError::Sensitive(rel.to_string()));
        }
        cursor.push(part);
        match fs::symlink_metadata(&cursor) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(CoreError::Symlink(rel.to_string())),
            Ok(_) => {}
            Err(e) => return Err(CoreError::io(&cursor, e)),
        }
    }
    let canon = strip_verbatim(fs::canonicalize(&cursor).map_err(|e| CoreError::io(&cursor, e))?);
    if !crate::paths::is_within(&canon, root) {
        return Err(CoreError::OutsideRoot(canon));
    }
    Ok(cursor)
}

/// A hashed inventory entry.
#[derive(Clone, Debug)]
pub struct HashedEntry {
    pub entry: InventoryEntry,
    pub hash: Hash32,
}

/// Hash entries in parallel (order-preserving). `reuse(path, size, mtime_ns)` may return a
/// previously known hash for unchanged metadata (incremental mode); `None` forces reading the
/// file. Entries that became symlinks since the scan are refused.
pub fn hash_entries(
    entries: &[InventoryEntry],
    reuse: impl Fn(&str, u64, u64) -> Option<Hash32> + Sync,
) -> Vec<Result<HashedEntry>> {
    entries
        .par_iter()
        .map(|e| {
            let hash = match reuse(&e.path, e.size, e.mtime_ns) {
                Some(h) => h,
                None => hash_file(e)?,
            };
            Ok(HashedEntry {
                entry: e.clone(),
                hash,
            })
        })
        .collect()
}

fn hash_file(e: &InventoryEntry) -> Result<Hash32> {
    let meta = fs::symlink_metadata(&e.abs).map_err(|err| CoreError::io(&e.abs, err))?;
    if meta.file_type().is_symlink() {
        return Err(CoreError::Symlink(e.path.clone()));
    }
    let bytes = fs::read(&e.abs).map_err(|err| CoreError::io(&e.abs, err))?;
    Ok(Hash32::of(&bytes))
}

/// Fingerprint of the whole inventory: sorted (path, hash) of sources then configs.
pub fn inventory_fingerprint<'a>(
    sources: impl IntoIterator<Item = (&'a str, &'a Hash32)>,
    configs: impl IntoIterator<Item = (&'a str, &'a Hash32)>,
) -> Hash32 {
    let mut h = PartsHasher::new();
    h.text("sources");
    for (p, hash) in sources {
        h.text(p).part(&hash.0);
    }
    h.text("configs");
    for (p, hash) in configs {
        h.text(p).part(&hash.0);
    }
    h.finish()
}

/// The first line of a file (at most 256 bytes; empty when unreadable).
fn first_line(path: &Path) -> Vec<u8> {
    use std::io::Read;
    let mut buf = [0u8; 256];
    let n = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut buf))
        .unwrap_or(0);
    let line = &buf[..n];
    let end = line.iter().position(|&b| b == b'\n').unwrap_or(line.len());
    line[..end].to_vec()
}

#[cfg(test)]
#[path = "../tests/unit/inventory.rs"]
mod tests;
