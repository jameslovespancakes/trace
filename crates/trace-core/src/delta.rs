//! Incremental updates.
//!
//! [`IndexDelta`] says what changed in one update; every `*_delta` phase function must
//! produce exactly what its full counterpart produces on the same inputs (`delta.full` ->
//! the full function). [`IdRemap`] moves positional ids after an incremental link;
//! [`PhaseState`] is the opaque per-phase incremental state persisted with the index.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::model::{FileId, SymbolId};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexDelta {
    /// Full rebuild (no previous index, version/config/fingerprint change, `IndexMode::Rebuild`).
    pub full: bool,
    pub added: BTreeSet<String>,
    pub modified: BTreeSet<String>,
    pub removed: BTreeSet<String>,
    /// Files whose semantics were (re)queried in this update (changed + resolved stale dependents).
    pub requeried: BTreeSet<String>,
    /// Files whose interface fingerprint changed (added and removed files included).
    pub interface_changed: BTreeSet<String>,
    /// Symbol uids that appeared, disappeared, or whose declaration header changed.
    pub symbols_added: BTreeSet<String>,
    pub symbols_removed: BTreeSet<String>,
    pub symbols_changed: BTreeSet<String>,
    /// Every file stale after this update: dependents left stale by an interface change,
    /// resolved in the following `index --watch` batches or by the next command (`Index::stale` of the new
    /// index; `trace_core::incremental::index_delta`).
    pub stale: BTreeSet<String>,
}

impl IndexDelta {
    /// A full rebuild.
    pub fn full() -> IndexDelta {
        IndexDelta {
            full: true,
            ..IndexDelta::default()
        }
    }

    /// Nothing changed (and not a full rebuild).
    pub fn is_empty(&self) -> bool {
        !self.full
            && self.added.is_empty()
            && self.modified.is_empty()
            && self.removed.is_empty()
            && self.requeried.is_empty()
            && self.interface_changed.is_empty()
            && self.symbols_added.is_empty()
            && self.symbols_removed.is_empty()
            && self.symbols_changed.is_empty()
            && self.stale.is_empty()
    }

    /// added | modified | removed | requeried (always true for a full rebuild).
    pub fn file_changed(&self, path: &str) -> bool {
        self.full
            || self.added.contains(path)
            || self.modified.contains(path)
            || self.removed.contains(path)
            || self.requeried.contains(path)
    }

    /// added | removed | changed (always true for a full rebuild).
    pub fn symbol_touched(&self, uid: &str) -> bool {
        self.full
            || self.symbols_added.contains(uid)
            || self.symbols_removed.contains(uid)
            || self.symbols_changed.contains(uid)
    }

    /// Fold a later delta into this one (sets are unions; a file added then removed is
    /// removed; a file removed then added is modified; `full` is sticky; `stale` is the
    /// later update's complete stale set).
    ///
    /// Note: `is_empty` on a delta that only carries a stale set is false (a stale set is
    /// persisted state that changed).
    pub fn merge(&mut self, later: IndexDelta) {
        self.full |= later.full;
        for path in later.removed {
            if !self.added.remove(&path) {
                self.removed.insert(path.clone());
            }
            self.modified.remove(&path);
            self.requeried.remove(&path);
        }
        for path in later.added {
            if self.removed.remove(&path) {
                self.modified.insert(path);
            } else {
                self.added.insert(path);
            }
        }
        for path in later.modified {
            if !self.added.contains(&path) {
                self.modified.insert(path);
            }
        }
        self.requeried.extend(later.requeried);
        self.interface_changed.extend(later.interface_changed);
        for uid in later.symbols_removed {
            if !self.symbols_added.remove(&uid) {
                self.symbols_removed.insert(uid.clone());
            }
            self.symbols_changed.remove(&uid);
        }
        for uid in later.symbols_added {
            if self.symbols_removed.remove(&uid) {
                self.symbols_changed.insert(uid);
            } else {
                self.symbols_added.insert(uid);
            }
        }
        for uid in later.symbols_changed {
            if !self.symbols_added.contains(&uid) {
                self.symbols_changed.insert(uid);
            }
        }
        self.stale = later.stale;
    }
}

/// Old -> new positional ids after an incremental link (a linear remap, no analysis work).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdRemap {
    pub files: Vec<Option<FileId>>,
    pub symbols: Vec<Option<SymbolId>>,
}

impl IdRemap {
    /// Every id maps to itself.
    pub fn identity(files: usize, symbols: usize) -> IdRemap {
        IdRemap {
            files: (0..files as u32).map(|i| Some(FileId(i))).collect(),
            symbols: (0..symbols as u32).map(|i| Some(SymbolId(i))).collect(),
        }
    }

    pub fn file(&self, old: FileId) -> Option<FileId> {
        self.files.get(old.0 as usize).copied().flatten()
    }

    pub fn symbol(&self, old: SymbolId) -> Option<SymbolId> {
        self.symbols.get(old.0 as usize).copied().flatten()
    }
}

/// Opaque per-phase incremental state persisted with the index (per-file blocks so a delta
/// rewrites only touched blocks). trace-core never interprets the bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseState {
    pub phase: String,
    pub version: u32,
    pub global: Vec<u8>,
    pub files: BTreeMap<String, Vec<u8>>,
}

#[cfg(test)]
#[path = "../tests/unit/delta.rs"]
mod tests;
