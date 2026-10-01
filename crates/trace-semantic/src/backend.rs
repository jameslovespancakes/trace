//! The backend trait, request/response types and the registry.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::SystemTime;

use trace_core::facts::FileFacts;
use trace_core::fingerprint::PartsHasher;
use trace_core::model::BackendRun;
use trace_core::paths::RepoPaths;
use trace_core::semantics::FileSemantics;
use trace_core::{Hash32, Language};

use crate::session::BackendSession;
use crate::tools::ToolEnv;
use crate::SemanticError;

/// One file of the backend's partition.
#[derive(Clone, Copy, Debug)]
pub struct SemanticFile<'a> {
    pub path: &'a str,
    pub language: Language,
    pub hash: Hash32,
    /// Exact bytes (already verified against `hash`).
    pub source: &'a [u8],
    pub facts: &'a FileFacts,
}

/// Everything a backend run needs.
pub struct SemanticRequest<'a> {
    pub repo: &'a RepoPaths,
    /// All files of the partition (all are copied into the snapshot).
    pub files: &'a [SemanticFile<'a>],
    /// Configuration files copied into the snapshot (never executed by trace).
    pub configs: &'a [(&'a str, &'a [u8])],
    /// Relative paths whose results must be (re)computed; others keep cached results.
    pub query: &'a HashSet<String>,
    pub tools: &'a ToolEnv,
    /// The backend's preflight result (DESIGN §1.7).
    pub prepared: &'a crate::languages::Prepared,
}

/// Results of one backend run.
pub struct BackendOutput {
    /// Fresh results for (at least) every file in `SemanticRequest::query`.
    pub files: HashMap<String, FileSemantics>,
    pub run: BackendRun,
}

/// A semantic backend. Implementations must be read-only on the target root.
pub trait Backend: Send + Sync {
    /// Stable id: `pyright`, `typescript`, `rust-analyzer`, `lsp:<server>`.
    fn id(&self) -> &str;
    /// Languages served (a partition).
    fn languages(&self) -> &[Language];
    /// Fingerprint of tool versions/configuration and the preflight (`prepared.fingerprint`);
    /// changes invalidate cached results.
    fn fingerprint(&self, tools: &ToolEnv, prepared: &crate::languages::Prepared) -> String;
    /// Analyze a snapshot of the partition (one-shot: processes and snapshot are gone when
    /// this returns).
    fn run(&self, request: &SemanticRequest<'_>) -> Result<BackendOutput, SemanticError>;
    /// Start a persistent session over a snapshot of `request`'s partition (processes start
    /// on the first update and stay alive until closed). `None` for one-shot-only backends.
    fn open_session(
        &self,
        request: &SemanticRequest<'_>,
    ) -> Result<Option<Box<dyn BackendSession>>, SemanticError> {
        let _ = request;
        Ok(None)
    }
    /// Configuration file names (basenames) copied into this backend's snapshots; their
    /// contents key the per-file semantic cache.
    fn snapshot_configs(&self) -> &[&str] {
        &[]
    }
    /// One-shot live find-references over a fresh snapshot of `request`'s partition
    /// (SPEC §8.9). `Ok(None)` = not supported (callers use the index).
    fn references(
        &self,
        request: &SemanticRequest<'_>,
        query: &crate::references::ReferenceQuery,
    ) -> Result<Option<crate::references::LiveReferences>, SemanticError> {
        let _ = (request, query);
        Ok(None)
    }
}

/// All backends, one per entry of `tools.registry` (the embedded `assets/backends/*.json` or
/// the `semantic.registry` override), in file order: `pyright`, `typescript_worker` and
/// `rust_analyzer` entries map to their dedicated Rust backends, every `lsp` entry to a
/// `backends::generic::GenericLsp` built from it. Every language is served by exactly one
/// entry ([`plan`]); whether that server can run here is decided by the preflight
/// (`crate::setup`), never by trying another backend.
pub fn registry(tools: &ToolEnv) -> Vec<Box<dyn Backend>> {
    use crate::registry::BackendKind;
    tools
        .registry
        .backends
        .iter()
        .map(|entry| -> Box<dyn Backend> {
            match entry.kind {
                BackendKind::Pyright => Box::new(crate::backends::pyright::Pyright),
                BackendKind::TypescriptWorker => Box::new(crate::backends::typescript::TypeScript),
                BackendKind::Lsp => Box::new(crate::backends::generic::GenericLsp::new(entry.clone())),
            }
        })
        .collect()
}

/// One backend and the languages it serves.
pub type Assignment<'b> = (&'b dyn Backend, Vec<Language>);

/// The one registry entry per present language (registry order; the registry validation
/// guarantees at most one entry per language). Languages no entry serves are absent: the
/// setup phase reports them (`crate::setup::unserved`), they are never analysed from syntax.
pub fn plan<'b>(backends: &'b [Box<dyn Backend>], present: &[Language]) -> Vec<Assignment<'b>> {
    let mut chosen: Vec<Vec<Language>> = vec![Vec::new(); backends.len()];
    let mut seen: HashSet<Language> = HashSet::new();
    for &language in present {
        if !seen.insert(language) {
            continue;
        }
        if let Some(i) = backends.iter().position(|b| b.languages().contains(&language)) {
            chosen[i].push(language);
        }
    }
    backends
        .iter()
        .zip(chosen)
        .filter(|(_, languages)| !languages.is_empty())
        .map(|(backend, languages)| (&**backend as &'b dyn Backend, languages))
        .collect()
}

/// Size and modification time (ns) of a file, for tool fingerprints.
pub(crate) fn file_stamp(path: &Path) -> (u64, u64) {
    std::fs::metadata(path)
        .map(|m| {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            (m.len(), mtime)
        })
        .unwrap_or((0, 0))
}

/// Hash a file's bytes into a fingerprint (missing files hash as empty with a marker).
pub(crate) fn hash_file_into(hasher: &mut PartsHasher, path: &Path) {
    match std::fs::read(path) {
        Ok(bytes) => {
            hasher.text("present").part(&bytes);
        }
        Err(_) => {
            hasher.text("missing");
        }
    }
}

/// Hash an executable's identity (path, size, mtime) into a fingerprint.
pub(crate) fn hash_executable_into(hasher: &mut PartsHasher, path: Option<&Path>) {
    match path {
        Some(path) => {
            let (size, mtime) = file_stamp(path);
            hasher.text(&path.to_string_lossy()).int(size).int(mtime);
        }
        None => {
            hasher.text("absent");
        }
    }
}

/// `version` field of an npm `package.json`.
pub(crate) fn package_version(package_json: &Path) -> Option<String> {
    let bytes = std::fs::read(package_json).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value.get("version")?.as_str().map(str::to_string)
}

/// Build the `BackendRun` of a successful run.
pub(crate) fn run_summary(
    id: &str,
    languages: &[Language],
    files: usize,
    queried: usize,
    requests: u64,
    started: std::time::Instant,
    tool_version: Option<String>,
) -> BackendRun {
    BackendRun {
        backend: id.to_string(),
        languages: languages.to_vec(),
        files: files as u32,
        queried_files: queried as u32,
        requests,
        seconds: started.elapsed().as_secs_f64(),
        ok: true,
        error: None,
        tool_version,
        ready: None,
    }
}

#[cfg(test)]
#[path = "../tests/unit/backend.rs"]
mod tests;
