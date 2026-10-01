//! Inventory scan: the hashed inventory of the root ([`Scanned`]) and whether an index is
//! current for it.

use trace_core::config::Settings;
use trace_core::inventory::{self, HashedEntry, InventoryEntry};
use trace_core::model::{Index, OmittedFile};
use trace_core::paths::RepoPaths;
use trace_core::{Hash32, SCHEMA_VERSION};

use super::IndexMode;
use crate::Result;

/// A hashed inventory of the root.
#[derive(Clone, Debug)]
pub(crate) struct Scanned {
    /// Sources with a known language, sorted by path.
    pub sources: Vec<HashedEntry>,
    /// Configuration files, sorted by path.
    pub configs: Vec<HashedEntry>,
    pub omitted: Vec<OmittedFile>,
    /// `inventory_fingerprint` over sources and configs.
    pub fingerprint: Hash32,
}

/// Scan and hash the inventory. Unless `Rebuild`, files whose size and mtime match `prev`
/// reuse the recorded hash (metadata shortcut); everything else is read and hashed.
pub(crate) fn scan(
    paths: &RepoPaths,
    config: &Settings,
    prev: Option<&Index>,
    mode: IndexMode,
) -> Result<Scanned> {
    let inv = inventory::scan(&paths.root, &config.inventory)?;
    let reuse_from = if mode == IndexMode::Rebuild { None } else { prev };
    let reuse = |path: &str, size: u64, mtime: u64| -> Option<Hash32> {
        let prev = reuse_from?;
        if mtime == 0 {
            return None;
        }
        let rec = prev.file(prev.file_by_path(path)?);
        (rec.size == size && rec.mtime_ns == mtime).then_some(rec.hash)
    };
    let mut omitted = inv.omitted;
    let sources: Vec<InventoryEntry> = inv.sources.into_iter().filter(|e| e.language.is_some()).collect();
    let sources = hashed(&sources, reuse, &mut omitted);
    let configs = hashed(&inv.configs, |_, _, _| None, &mut omitted);
    let fingerprint = inventory::inventory_fingerprint(
        sources.iter().map(|e| (e.entry.path.as_str(), &e.hash)),
        configs.iter().map(|e| (e.entry.path.as_str(), &e.hash)),
    );
    Ok(Scanned {
        sources,
        configs,
        omitted,
        fingerprint,
    })
}

fn hashed(
    entries: &[InventoryEntry],
    reuse: impl Fn(&str, u64, u64) -> Option<Hash32> + Sync,
    omitted: &mut Vec<OmittedFile>,
) -> Vec<HashedEntry> {
    let results = inventory::hash_entries(entries, reuse);
    let mut out = Vec::with_capacity(entries.len());
    for (entry, result) in entries.iter().zip(results) {
        match result {
            Ok(h) => out.push(h),
            Err(_) => omitted.push(OmittedFile {
                path: entry.path.clone(),
                reason: "unreadable".into(),
            }),
        }
    }
    out
}

/// True when `prev` was built from exactly this inventory with the current extractor and
/// inference versions (no update needed for query commands).
pub(crate) fn is_current(prev: &Index, scanned: &Scanned) -> bool {
    prev.header.inventory_fingerprint == scanned.fingerprint
        && prev.header.syntax_version == trace_syntax::EXTRACTOR_VERSION
        && prev.header.infer_version == trace_infer::INFER_VERSION
        && prev.header.bridge_version == trace_bridge::BRIDGE_VERSION
        && prev.header.schema == SCHEMA_VERSION
}
