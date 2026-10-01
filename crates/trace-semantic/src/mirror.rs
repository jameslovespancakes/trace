//! Mirror workspaces (DESIGN §1.13; owner client): the file list of a persistent full-tree
//! copy of the repository for build-importing servers (Java/Scala, C#, Haskell, R, Rust,
//! C/C++, Go). [`crate::snapshot::Snapshot`] in
//! `WorkspaceMode::Mirror` copies what [`walk`] (or, for watcher hints, [`stat_paths`])
//! returns into `<repo cache>/workspaces/<backend>/tree` and deletes what left.
//!
//! What belongs in the mirror:
//! * every regular file the repository's ignore rules keep (`.gitignore`, `.ignore`,
//!   `.git/info/exclude`, parents included; hidden files included, e.g. `.mvn/`,
//!   `.cargo/config.toml`), plus git-ignored files matching the entry's `include_ignored`
//!   globs (restore outputs: `**/obj/project.assets.json`);
//! * never: `.git`, files matching the entry's `exclude` globs (`dist-newstyle/**`),
//!   sensitive files (inventory rules: `.env*`, keys, credential
//!   files) and symlinks / junctions (never followed).
//!
//! The mirror is capped at `semantic.max_mirror_mb` (4 GiB by default): a larger repository is the setup
//! error `This repository is too large for trace to analyze ({size}).`
//!
//! Globs ([`glob_match`]) are relative, `/`-separated patterns over path segments: `**`
//! matches any number of segments, `*` and `?` match within one segment; a pattern without
//! `/` matches the file name anywhere (like `.gitignore`). They are registry data (paths),
//! never source code.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ignore::WalkBuilder;
use trace_core::inventory::{is_excluded_dir, is_sensitive_name};

use crate::mapping::relative_to;

/// Largest mirror in bytes (sum of copied file sizes; setting `semantic.max_mirror_mb`).
pub(crate) fn max_mirror_bytes() -> u64 {
    trace_core::config::current().semantic.max_mirror_mb << 20
}

/// The entry's mirror globs (registry `workspace.include_ignored` / `workspace.exclude`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MirrorRules {
    pub include_ignored: Vec<String>,
    pub exclude: Vec<String>,
}

impl MirrorRules {
    /// Whether a repository-relative file may be copied at all (not `.git`, not excluded,
    /// not sensitive).
    pub fn allowed(&self, rel: &str) -> bool {
        let mut segments = rel.split('/');
        let name = rel.rsplit('/').next().unwrap_or(rel);
        !segments.any(|s| s == ".git")
            && !is_sensitive_name(name)
            && !self.exclude.iter().any(|p| glob_match(p, rel))
    }

    /// Whether a git-ignored file is copied anyway.
    pub fn included(&self, rel: &str) -> bool {
        self.include_ignored.iter().any(|p| glob_match(p, rel))
    }

    /// Whether a directory of the include walk can contain an `include_ignored` match.
    /// Hidden and conventionally excluded directories (`node_modules`, `.venv`, ...) are
    /// only entered when a pattern names them.
    fn may_contain(&self, dir_rel: &str) -> bool {
        let dir: Vec<&str> = dir_rel.split('/').filter(|s| !s.is_empty()).collect();
        let name = dir.last().copied().unwrap_or("");
        if name == ".git" || self.exclude.iter().any(|p| glob_match(p, dir_rel)) {
            return false;
        }
        let named = self
            .include_ignored
            .iter()
            .any(|p| p.split('/').any(|seg| seg == name));
        if is_excluded_dir(name) && !named {
            return false;
        }
        self.include_ignored.iter().any(|p| {
            let pat: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
            prefix_may_match(&pat, &dir)
        })
    }
}

/// One repository file of the mirror.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MirrorEntry {
    /// Repository-relative, `/`-separated.
    pub rel: String,
    pub abs: PathBuf,
    pub size: u64,
    pub mtime_ns: u64,
}

/// Every file of `root` that belongs in the mirror (module docs), sorted by path.
pub fn walk(root: &Path, rules: &MirrorRules) -> Vec<MirrorEntry> {
    let mut out: Vec<MirrorEntry> = Vec::new();
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .parents(true)
        .ignore(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        .require_git(false)
        .follow_links(false)
        .same_file_system(false)
        .filter_entry(|entry| {
            !(entry.depth() > 0
                && entry.file_type().is_some_and(|t| t.is_dir())
                && entry.file_name() == ".git")
        });
    collect(root, builder, rules, &mut out, &|_| true);
    if !rules.include_ignored.is_empty() {
        let mut ignored = WalkBuilder::new(root);
        let prune = rules.clone();
        let base = root.to_path_buf();
        ignored
            .hidden(false)
            .parents(false)
            .ignore(false)
            .git_ignore(false)
            .git_exclude(false)
            .git_global(false)
            .require_git(false)
            .follow_links(false)
            .same_file_system(false)
            .filter_entry(move |entry| {
                if entry.depth() == 0 || !entry.file_type().is_some_and(|t| t.is_dir()) {
                    return true;
                }
                relative_to(&base, entry.path()).is_some_and(|rel| prune.may_contain(&rel))
            });
        let mut extra: Vec<MirrorEntry> = Vec::new();
        collect(root, ignored, rules, &mut extra, &|rel| rules.included(rel));
        out.extend(extra);
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out.dedup_by(|a, b| a.rel == b.rel);
    out
}

/// Files of a walk that are regular (never symlinks), allowed and accepted by `keep`.
fn collect(
    root: &Path,
    builder: WalkBuilder,
    rules: &MirrorRules,
    out: &mut Vec<MirrorEntry>,
    keep: &dyn Fn(&str) -> bool,
) {
    for entry in builder.build().flatten() {
        let Some(kind) = entry.file_type() else { continue };
        if !kind.is_file() || kind.is_symlink() {
            continue;
        }
        let Some(rel) = relative_to(root, entry.path()) else { continue };
        if !rules.allowed(&rel) || !keep(&rel) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else { continue };
        if !meta.is_file() {
            continue;
        }
        out.push(MirrorEntry {
            rel,
            abs: entry.path().to_path_buf(),
            size: meta.len(),
            mtime_ns: mtime_ns(&meta),
        });
    }
}

/// Watcher hints: the hinted relative paths that are regular, allowed files now (to copy)
/// and those that no longer exist as regular files (to delete from the mirror).
pub fn stat_paths(root: &Path, rules: &MirrorRules, paths: &[String]) -> (Vec<MirrorEntry>, Vec<String>) {
    let mut present = Vec::new();
    let mut gone = Vec::new();
    for rel in paths {
        let Some(relative) = crate::registry::safe_relative(rel) else { continue };
        let abs = root.join(relative);
        match std::fs::symlink_metadata(&abs) {
            Ok(meta) if meta.is_file() && rules.allowed(rel) => present.push(MirrorEntry {
                rel: rel.clone(),
                abs,
                size: meta.len(),
                mtime_ns: mtime_ns(&meta),
            }),
            Ok(meta) if meta.is_file() => gone.push(rel.clone()),
            Ok(_) => {}
            Err(_) => gone.push(rel.clone()),
        }
    }
    present.sort_by(|a, b| a.rel.cmp(&b.rel));
    gone.sort();
    (present, gone)
}

/// Modification time in nanoseconds since the epoch (0 when unknown).
pub fn mtime_ns(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Human size for the size-cap error (`5.2 GB`).
pub fn size_text(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
}

/// Whether a relative `/`-separated `path` matches `pattern` (module docs).
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_start_matches("./");
    let pat: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if pat.len() == 1 && pat[0] != "**" {
        return segs.last().is_some_and(|name| segment_match(pat[0], name));
    }
    match_segments(&pat, &segs)
}

fn match_segments(pat: &[&str], segs: &[&str]) -> bool {
    match pat.split_first() {
        None => segs.is_empty(),
        Some((&"**", rest)) => (0..=segs.len()).any(|i| match_segments(rest, &segs[i..])),
        Some((p, rest)) => !segs.is_empty() && segment_match(p, segs[0]) && match_segments(rest, &segs[1..]),
    }
}

/// Whether a directory (segments) can still lead to a match of `pat`.
fn prefix_may_match(pat: &[&str], dir: &[&str]) -> bool {
    match (pat.split_first(), dir.split_first()) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some((&"**", _)), _) => true,
        (Some((p, prest)), Some((d, drest))) => {
            // The last pattern segment names a file, not a directory.
            !prest.is_empty() && segment_match(p, d) && prefix_may_match(prest, drest)
        }
    }
}

/// `*` / `?` wildcards within one segment (case-insensitive on Windows).
fn segment_match(pattern: &str, text: &str) -> bool {
    let fold = |s: &str| -> Vec<char> {
        if cfg!(windows) {
            s.to_lowercase().chars().collect()
        } else {
            s.chars().collect()
        }
    };
    let (p, t) = (fold(pattern), fold(text));
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
#[path = "../tests/unit/mirror.rs"]
mod tests;
