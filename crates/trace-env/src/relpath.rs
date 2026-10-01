//! Repository-relative paths under a root directory (the path text rules are
//! `trace_core::relpath`) and bounded reads of small manifest files, shared by the ecosystems.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use trace_core::Language;

pub(crate) use trace_core::relpath::{ancestors, file_name, join, normalize, parent, relative_to, within};

/// `root.join(rel)` (`root` for `""`).
pub(crate) fn under(root: &Path, rel: &str) -> PathBuf {
    if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    }
}

/// `root` joined with every component of `rel` (native separators).
pub(crate) fn native(root: &Path, rel: &str) -> PathBuf {
    if rel.is_empty() {
        return root.to_path_buf();
    }
    rel.split('/').fold(root.to_path_buf(), |p, part| p.join(part))
}

/// Directories containing `name`, found by walking from the directory of every file of
/// `languages` up to the repository root. Sorted.
pub(crate) fn manifest_dirs(
    root: &Path,
    files: &[(&str, Language)],
    languages: &[Language],
    name: &str,
) -> Vec<String> {
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    let mut found: BTreeSet<String> = BTreeSet::new();
    for (path, language) in files {
        if !languages.contains(language) {
            continue;
        }
        let mut dir = parent(path);
        loop {
            if !visited.insert(dir) {
                break;
            }
            if under(root, dir).join(name).is_file() {
                found.insert(dir.to_string());
            }
            if dir.is_empty() {
                break;
            }
            dir = parent(dir);
        }
    }
    found.into_iter().collect()
}

/// A regular file of at most `max_bytes` as text.
pub(crate) fn read_small(path: &Path, max_bytes: u64) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max_bytes {
        return None;
    }
    fs::read_to_string(path).ok()
}

/// A text file without its UTF-8 byte order mark.
pub(crate) fn read_text(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    Some(match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_string(),
        None => text,
    })
}

/// A TOML file as JSON values.
pub(crate) fn read_toml(path: &Path) -> Option<serde_json::Value> {
    trace_core::formats::toml_value(&fs::read_to_string(path).ok()?)
}
