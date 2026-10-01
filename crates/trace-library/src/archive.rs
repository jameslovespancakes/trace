//! Library source inside archives: the `-sources.jar` a Maven / Gradle cache
//! keeps next to a dependency jar and the JDK's `lib/src.zip`, addressed as
//! `<archive>!/<entry>` paths so the derivation loads them like files. Read-only (the zip
//! central directory and the requested entries only; nothing is extracted to disk). Opened
//! archives are kept per process, bounded.

use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Separator between the archive path and the entry name.
const SEPARATOR: &str = "!/";
/// Archives kept open per process at most.
const MAX_OPEN: usize = 16;
/// Entries listed per archive lookup at most.
const MAX_LISTED: usize = 200_000;

type Shared = Arc<Mutex<zip::ZipArchive<BufReader<std::fs::File>>>>;

/// Whether `name` is an archive file name (`.jar`, `.zip`, case-insensitive).
fn is_archive_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".jar") || lower.ends_with(".zip")
}

/// `<archive>!/<entry>` for an entry name (`/`-separated).
pub fn entry_path(archive: &Path, entry: &str) -> PathBuf {
    PathBuf::from(format!("{}{SEPARATOR}{}", archive.to_string_lossy(), entry.trim_start_matches('/')))
}

/// Split an archive path into the archive file and the entry name (`/`-separated); `None`
/// for plain file paths.
pub fn split(path: &Path) -> Option<(PathBuf, String)> {
    let text = path.to_string_lossy();
    let bytes = text.as_bytes();
    for (i, _) in text.match_indices('!') {
        let after = bytes.get(i + 1).copied();
        if !matches!(after, Some(b'/') | Some(b'\\')) {
            continue;
        }
        let archive = &text[..i];
        if !is_archive_name(archive) {
            continue;
        }
        let entry = text[i + 2..].replace('\\', "/");
        let entry = entry.trim_start_matches('/').to_string();
        if entry.is_empty() {
            return None;
        }
        return Some((PathBuf::from(archive), entry));
    }
    None
}

fn open(archive: &Path) -> Option<Shared> {
    static OPEN: OnceLock<Mutex<HashMap<PathBuf, Shared>>> = OnceLock::new();
    let map = OPEN.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(found) = map.lock().ok()?.get(archive).cloned() {
        return Some(found);
    }
    let file = std::fs::File::open(archive).ok()?;
    let zip = zip::ZipArchive::new(BufReader::new(file)).ok()?;
    let shared: Shared = Arc::new(Mutex::new(zip));
    let mut m = map.lock().ok()?;
    if m.len() >= MAX_OPEN {
        m.clear();
    }
    m.insert(archive.to_path_buf(), shared.clone());
    Some(shared)
}

/// Bytes of an archive entry (`None`: no archive path, unreadable archive, missing entry, or
/// an entry larger than `max_bytes`).
pub fn read(path: &Path, max_bytes: u64) -> Option<Vec<u8>> {
    let (archive, entry) = split(path)?;
    let shared = open(&archive)?;
    let mut zip = shared.lock().ok()?;
    let mut file = zip.by_name(&entry).ok()?;
    if file.size() > max_bytes {
        return None;
    }
    let mut out = Vec::with_capacity(file.size() as usize);
    file.read_to_end(&mut out).ok()?;
    Some(out)
}

/// Whether `archive` holds `entry`.
pub(crate) fn has_entry(archive: &Path, entry: &str) -> bool {
    let Some(shared) = open(archive) else {
        return false;
    };
    let Ok(zip) = shared.lock() else {
        return false;
    };
    zip.index_for_name(entry).is_some()
}

/// Whether an archive path names an existing entry.
pub fn exists(path: &Path) -> bool {
    split(path).is_some_and(|(archive, entry)| has_entry(&archive, &entry))
}

/// The first entry (in archive order) whose name is `suffix` or ends with `/<suffix>`
/// (JDK `src.zip` keeps sources below their module: `java.base/java/util/List.java`).
pub fn find_suffix(archive: &Path, suffix: &str) -> Option<String> {
    let shared = open(archive)?;
    let zip = shared.lock().ok()?;
    let tail = format!("/{suffix}");
    let found = zip
        .file_names()
        .take(MAX_LISTED)
        .find(|name| *name == suffix || name.ends_with(&tail))
        .map(str::to_string);
    found
}

/// The other entries of the directory of an archive path with one of `extensions`, sorted.
pub fn siblings(path: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    let Some((archive, entry)) = split(path) else {
        return Vec::new();
    };
    let dir = entry
        .rsplit_once('/')
        .map(|(d, _)| format!("{d}/"))
        .unwrap_or_default();
    let Some(shared) = open(&archive) else {
        return Vec::new();
    };
    let Ok(zip) = shared.lock() else {
        return Vec::new();
    };
    let mut out: Vec<String> = zip
        .file_names()
        .take(MAX_LISTED)
        .filter(|name| *name != entry)
        .filter_map(|name| {
            let rest = name.strip_prefix(dir.as_str())?;
            let ext = rest.rsplit_once('.').map(|(_, e)| e)?;
            (!rest.contains('/') && extensions.contains(&ext)).then(|| name.to_string())
        })
        .collect();
    out.sort();
    out.into_iter().map(|name| entry_path(&archive, &name)).collect()
}

#[cfg(test)]
#[path = "../tests/unit/archive.rs"]
mod tests;
