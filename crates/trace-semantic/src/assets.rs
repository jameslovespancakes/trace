//! Embedded helper scripts, materialized under the cache home (never inside a target).
//!
//! * `ts-worker/` — the TypeScript worker (ES modules: `main.mjs` entry, `project.mjs`,
//!   `analyze.mjs`, `fntype.mjs`, `external.mjs`, `assigned.mjs`); started as a copy of
//!   codepath_v3/backends/typescript/worker.mjs and trace-owned since the trace-next round
//!   (module-level owners, anonymous-function mapping, identifier uses, references mode;
//!   SPEC §8.5).
//! * `lsp_guard.cjs` — Node `--require` preload (started from codepath/lsp_guard.cjs) that
//!   refuses child processes, network and writes outside `CODEPATH_LSP_WRITE_ROOT` (the
//!   workspace) and the extra roots of `CODEPATH_LSP_WRITE_ROOTS` (trace's per-repository
//!   state dir, e.g. Intelephense's index storage). Every node-based language server runs
//!   under it: Pyright and the registry's `node_script` entries (Intelephense,
//!   bash-language-server).

use std::fs;
use std::path::{Path, PathBuf};

use trace_core::cache::write_atomic;
use trace_core::fingerprint::PartsHasher;
use trace_core::Hash32;

/// The TypeScript worker files (name, contents), materialized into `<assets>/ts-worker/`.
pub const TS_WORKER_FILES: [(&str, &str); 6] = [
    ("main.mjs", include_str!("../../../assets/ts-worker/main.mjs")),
    ("project.mjs", include_str!("../../../assets/ts-worker/project.mjs")),
    ("analyze.mjs", include_str!("../../../assets/ts-worker/analyze.mjs")),
    ("fntype.mjs", include_str!("../../../assets/ts-worker/fntype.mjs")),
    ("external.mjs", include_str!("../../../assets/ts-worker/external.mjs")),
    ("assigned.mjs", include_str!("../../../assets/ts-worker/assigned.mjs")),
];

/// Hex blake3 over every worker file (tool fingerprints).
pub fn ts_worker_hash() -> String {
    let mut h = PartsHasher::new();
    for (name, text) in TS_WORKER_FILES {
        h.text(name).text(text);
    }
    h.finish().to_hex()
}
pub const LSP_GUARD: &str = include_str!("../../../assets/lsp_guard.cjs");

/// Paths of materialized assets.
#[derive(Clone, Debug)]
pub struct AssetPaths {
    pub dir: PathBuf,
    pub ts_worker: PathBuf,
    pub lsp_guard: PathBuf,
}

/// Write assets to `<home>/assets/<blake3 of both contents, 16 hex>/` if not present
/// (atomic writes; existing files are verified by hash and rewritten if different).
pub fn materialize(home: &Path) -> Result<AssetPaths, crate::SemanticError> {
    let mut digest = PartsHasher::new();
    for (name, text) in TS_WORKER_FILES {
        digest.text(name).text(text);
    }
    let digest = digest.text(LSP_GUARD).finish().hex_prefix(16);
    let dir = home.join("assets").join(digest);
    let worker_dir = dir.join("ts-worker");
    fs::create_dir_all(&worker_dir)?;
    for (name, text) in TS_WORKER_FILES {
        ensure_file(&worker_dir.join(name), text.as_bytes())?;
    }
    let ts_worker = worker_dir.join("main.mjs");
    let lsp_guard = dir.join("lsp_guard.cjs");
    ensure_file(&lsp_guard, LSP_GUARD.as_bytes())?;
    Ok(AssetPaths {
        dir,
        ts_worker,
        lsp_guard,
    })
}

/// Hex blake3 of an embedded asset (tool fingerprints).
pub fn asset_hash(content: &str) -> String {
    Hash32::of(content.as_bytes()).to_hex()
}

fn ensure_file(path: &Path, bytes: &[u8]) -> Result<(), crate::SemanticError> {
    if let Ok(existing) = fs::read(path) {
        if Hash32::of(&existing) == Hash32::of(bytes) {
            return Ok(());
        }
    }
    write_atomic(path, bytes)?;
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/assets.rs"]
mod tests;
