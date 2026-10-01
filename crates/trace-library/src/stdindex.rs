//! Per-toolchain module index of a stdlib source root (DESIGN §1.10a item 1):
//! when the server resolves a stdlib call to no location although the call names its module
//! deterministically (Python `json.dumps`), locate the
//! declaration by a unique module + function + arity match - never a name union: two
//! different declarations answering the same key is no answer. Built from syntax trees of the
//! root's files, cached per machine per toolchain version (and in memory per process).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use trace_core::semantics::LibraryFile;
use trace_core::text::LineIndex;
use trace_core::{Hash32, Language};
use trace_env::{LibraryKind, LibraryRoot};

use crate::languages;
use crate::ENGINE_VERSION;

/// Files indexed per root at most.
const MAX_FILES: usize = 20_000;
/// Directories visited per root at most.
const MAX_DIRS: usize = 20_000;
const INDEX_SCHEMA: u32 = 1;

/// One declaration of the index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Declared {
    file: u32,
    qualified: String,
    line: u32,
    column: u32,
    arity: Option<u32>,
}

/// Module + function -> declarations of one stdlib root.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StdIndex {
    schema: u32,
    files: Vec<PathBuf>,
    /// (module, function) -> declarations; `function` is both the qualified name below the
    /// module and its last segment.
    by_key: BTreeMap<(String, String), Vec<Declared>>,
}

impl StdIndex {
    /// Index every source file of `root` (bounded).
    pub fn build(language: Language, root: &Path) -> StdIndex {
        let Some(spec) = languages::adapter(language) else {
            return StdIndex::default();
        };
        // Bounded walk; symbolic links and junctions are not followed (a link back into the
        // root would repeat it), hidden directories are skipped.
        let mut files: Vec<PathBuf> = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        let mut dirs = 0usize;
        while let Some(dir) = stack.pop() {
            dirs += 1;
            if dirs > MAX_DIRS || files.len() >= MAX_FILES {
                break;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            let mut entries: Vec<(PathBuf, std::fs::FileType)> = entries
                .flatten()
                .filter_map(|e| e.file_type().ok().map(|t| (e.path(), t)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (p, kind) in entries {
                if files.len() >= MAX_FILES {
                    break;
                }
                if kind.is_symlink() {
                    continue;
                }
                if kind.is_dir() {
                    let hidden = p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
                    if !hidden {
                        stack.push(p);
                    }
                } else if kind.is_file() && languages::has_extension(&p, spec.extensions) {
                    files.push(p);
                }
            }
        }
        files.sort();
        let roots = [root.to_path_buf()];
        let mut index = StdIndex {
            schema: INDEX_SCHEMA,
            ..StdIndex::default()
        };
        // Files are read and parsed in parallel; their keys are added in file order.
        let per_file: Vec<Vec<((String, String), Declared)>> = files
            .par_iter()
            .enumerate()
            .map(|(fi, path)| file_keys(language, spec, &roots, fi as u32, path))
            .collect();
        for (key, declared) in per_file.into_iter().flatten() {
            index.by_key.entry(key).or_default().push(declared);
        }
        index.files = files;
        index
    }

    /// The unique declaration of `module.function` (with `arity` when given): `None` when
    /// absent or when different declarations answer (multiple clauses of one function in one
    /// file are one declaration).
    pub fn lookup(&self, module: &str, function: &str, arity: Option<u32>) -> Option<(PathBuf, u32, u32)> {
        let found = self.by_key.get(&(module.to_string(), function.to_string()))?;
        let matching: Vec<&Declared> = found
            .iter()
            .filter(|d| arity.is_none_or(|a| d.arity.is_none_or(|da| da == a)))
            .collect();
        let first = matching.first()?;
        let unique = matching
            .iter()
            .all(|d| d.file == first.file && d.qualified == first.qualified && d.arity == first.arity);
        if !unique {
            return None;
        }
        Some((self.files.get(first.file as usize)?.clone(), first.line, first.column))
    }

    /// The index of a root: from the process memo, else the per-machine cache file under
    /// `cache_dir` (keyed by language, toolchain version and root path), else built.
    pub fn load_or_build(cache_dir: Option<&Path>, language: Language, root: &LibraryRoot) -> Arc<StdIndex> {
        type Memo = Mutex<HashMap<(Language, PathBuf), Arc<StdIndex>>>;
        static MEMO: OnceLock<Memo> = OnceLock::new();
        let memo = MEMO.get_or_init(|| Mutex::new(HashMap::new()));
        let key = (language, root.path.clone());
        if let Some(found) = memo.lock().ok().and_then(|m| m.get(&key).cloned()) {
            return found;
        }
        let file = cache_dir.map(|dir| {
            let tag = Hash32::of(root.path.to_string_lossy().as_bytes()).hex_prefix(16);
            dir.join("stdindex").join(format!(
                "{}-{}-{tag}-e{ENGINE_VERSION}.bin",
                language.as_str(),
                root.version.as_deref().unwrap_or("unversioned")
            ))
        });
        let cached = file
            .as_ref()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| postcard::from_bytes::<StdIndex>(&b).ok())
            .filter(|i| i.schema == INDEX_SCHEMA);
        let index = Arc::new(match cached {
            Some(i) => i,
            None => {
                let built = StdIndex::build(language, &root.path);
                if let (Some(f), Ok(bytes)) = (&file, postcard::to_allocvec(&built)) {
                    if let Some(parent) = f.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let tmp = f.with_extension(format!("tmp{}", std::process::id()));
                    if std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, f).is_err() {
                        let _ = std::fs::remove_file(&tmp);
                    }
                }
                built
            }
        });
        if let Ok(mut m) = memo.lock() {
            m.insert(key, index.clone());
        }
        index
    }
}

/// The index keys of the callable declarations of one file (`fi`: its index in the file list).
fn file_keys(
    language: Language,
    spec: &languages::AdapterSpec,
    roots: &[PathBuf],
    fi: u32,
    path: &Path,
) -> Vec<((String, String), Declared)> {
    let mut out = Vec::new();
    let Ok(source) = std::fs::read(path) else { return out };
    let text = path.to_string_lossy();
    let Ok(facts) = trace_syntax::extract(trace_syntax::SourceInput {
        path: &text,
        language,
        source: &source,
    }) else {
        return out;
    };
    let lines = LineIndex::new(&source);
    let file_module = (spec.module_name)(path, roots);
    for (d, decl) in facts.declarations.iter().enumerate() {
        if facts.module_decl == Some(d as u32) || !decl.kind.is_callable() || decl.name.starts_with('<') {
            continue;
        }
        let line = lines.line0(decl.name_span.start);
        let start = lines.line_span(&source, line).map(|s| s.start).unwrap_or(0);
        let arity = Some(decl.parameters.len() as u32);
        let declared = Declared {
            file: fi,
            qualified: decl.qualified_name.clone(),
            line,
            column: decl.name_span.start.saturating_sub(start),
            arity,
        };
        let qualified = decl.qualified_name.as_str();
        let (prefix, last) = match qualified.rsplit_once('.') {
            Some((p, l)) => (Some(p), l),
            None => (None, qualified),
        };
        let mut keys: Vec<(String, String)> = Vec::new();
        if let Some(m) = &file_module {
            keys.push((m.clone(), qualified.to_string()));
            keys.push((m.clone(), last.to_string()));
        }
        if let Some(p) = prefix {
            keys.push((p.to_string(), last.to_string()));
        }
        keys.sort();
        keys.dedup();
        out.extend(keys.into_iter().map(|key| (key, declared.clone())));
    }
    out
}

/// The library file of a stdlib root that declares `path` (package = the language's stdlib).
pub(crate) fn stdlib_file(language: Language, root: &LibraryRoot, path: PathBuf) -> LibraryFile {
    let package = match root.kind {
        LibraryKind::Stdlib => format!("{}-stdlib", language.as_str()),
        LibraryKind::Dependency => path
            .strip_prefix(&root.path)
            .ok()
            .and_then(|rel| rel.components().next())
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    LibraryFile {
        path: path.to_string_lossy().into_owned(),
        package,
        version: root.version.clone(),
        stdlib: root.kind == LibraryKind::Stdlib,
        readable: true,
        language,
    }
}

#[cfg(test)]
#[path = "../tests/unit/stdindex.rs"]
mod tests;
