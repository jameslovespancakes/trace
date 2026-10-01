//! Per-machine summary cache: one entry per library file, keyed by language,
//! `package@version`, the file's blake3 and the derivation context (engine version + native
//! tables), postcard with a schema header, outside every repository
//! (`<cache home>/library/v{ENGINE_VERSION}/<language>/<package>@<version>/<blake3>-<ctx>.bin`)
//! and shared across repositories. A cache entry from another schema / engine / key is
//! rebuilt, never misread. Bounded: [`SummaryCache::prune`] keeps the directory under a byte
//! budget (oldest entries first).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use trace_core::{Hash32, Language};

use crate::derive::FileSummaries;
use crate::model::LibraryError;
use crate::ENGINE_VERSION;

/// Cache schema (bump when `FileSummaries` changes shape).
pub const CACHE_SCHEMA: u32 = 1;

/// Key of one library file's summaries.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SummaryKey {
    pub language: Language,
    pub package: String,
    pub version: Option<String>,
    pub file_hash: Hash32,
    /// Hash of what else the summaries depend on (native tables, library roots).
    pub context: Hash32,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    schema: u32,
    engine: u32,
    language: String,
    package: String,
    version: Option<String>,
    file_hash: Hash32,
    context: Hash32,
    summaries: FileSummaries,
}

pub struct SummaryCache {
    dir: PathBuf,
}

/// File-name-safe form of a package or version (`@scope/pkg` -> `@scope_pkg`).
fn safe(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '@' | '+') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '.') {
        "_".to_string()
    } else {
        cleaned
    }
}

impl SummaryCache {
    pub fn open(dir: &Path) -> SummaryCache {
        SummaryCache {
            dir: dir.to_path_buf(),
        }
    }

    /// Path of one entry.
    pub fn entry_path(&self, key: &SummaryKey) -> PathBuf {
        let version = key.version.as_deref().unwrap_or("none");
        self.dir
            .join(key.language.as_str())
            .join(format!("{}@{}", safe(&key.package), safe(version)))
            .join(format!("{}-{}.bin", key.file_hash.to_hex(), key.context.hex_prefix(16)))
    }

    /// The cached summaries, when an entry with exactly this key exists and parses.
    pub fn get(&self, key: &SummaryKey) -> Option<FileSummaries> {
        let bytes = std::fs::read(self.entry_path(key)).ok()?;
        let entry: Entry = postcard::from_bytes(&bytes).ok()?;
        let matches = entry.schema == CACHE_SCHEMA
            && entry.engine == ENGINE_VERSION
            && entry.language == key.language.as_str()
            && entry.package == key.package
            && entry.version == key.version
            && entry.file_hash == key.file_hash
            && entry.context == key.context;
        matches.then_some(entry.summaries)
    }

    /// Store summaries (atomic write: temporary file + rename).
    pub fn put(&self, key: &SummaryKey, summaries: &FileSummaries) -> Result<(), LibraryError> {
        let path = self.entry_path(key);
        let err = |message: String| LibraryError::Cache {
            path: path.display().to_string(),
            message,
        };
        let entry = Entry {
            schema: CACHE_SCHEMA,
            engine: ENGINE_VERSION,
            language: key.language.as_str().to_string(),
            package: key.package.clone(),
            version: key.version.clone(),
            file_hash: key.file_hash,
            context: key.context,
            summaries: summaries.clone(),
        };
        let bytes = postcard::to_allocvec(&entry).map_err(|e| err(e.to_string()))?;
        let parent = path.parent().ok_or_else(|| err("no parent directory".to_string()))?;
        std::fs::create_dir_all(parent).map_err(|e| err(e.to_string()))?;
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, &bytes).map_err(|e| err(e.to_string()))?;
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            err(e.to_string())
        })
    }

    /// Keep the cache under `max_bytes`: when it is larger, delete the oldest entries until it
    /// is under 80% of the budget. Returns the number of removed entries.
    pub fn prune(&self, max_bytes: u64) -> usize {
        let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
        let mut stack = vec![self.dir.clone()];
        let mut total = 0u64;
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(meta) = entry.metadata() else { continue };
                if meta.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "bin") {
                    total += meta.len();
                    let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                    files.push((modified, meta.len(), path));
                }
            }
        }
        if total <= max_bytes {
            return 0;
        }
        files.sort();
        let target = max_bytes / 5 * 4;
        let mut removed = 0;
        for (_, len, path) in files {
            if total <= target {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(len);
                removed += 1;
            }
        }
        removed
    }
}

#[cfg(test)]
#[path = "../tests/unit/cache.rs"]
mod tests;
