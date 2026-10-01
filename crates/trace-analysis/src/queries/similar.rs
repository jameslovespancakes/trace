//! Similar code with a persistent fingerprint cache (`<repo cache>/similar.bin`,
//! magic `TRACESIM`, key = (uid, file hash)). Candidates: same kind and language, not stubs,
//! span size within [1/3, 3] of the target; fingerprints computed per file with one parse
//! (rayon across files); Jaccard >= 0.6; sorted by similarity desc then uid; top `limit`.
//!
//! The cache is an optimization only: a missing, corrupt or version-mismatched file is
//! ignored and rewritten; entries of symbols that no longer exist are pruned on save.

use std::collections::{BTreeMap, HashMap, HashSet};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use trace_core::cache::{load_blob, save_blob, SIMILAR_MAGIC};
use trace_core::model::{FileId, Symbol};
use trace_core::paths::ensure_outside;
use trace_core::source::read_verified;
use trace_core::{Hash32, Index, SymbolId, SCHEMA_VERSION};
use trace_syntax::Fingerprint;

use crate::cards::card;
use crate::report::SimilarRow;
use crate::workspace::Workspace;
use crate::Result;

pub(crate) const THRESHOLD: f64 = 0.6;

/// Persisted cache.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct FingerprintCache {
    pub entries: HashMap<(String, Hash32), Option<Fingerprint>>,
}

impl FingerprintCache {
    fn key(index: &Index, s: &Symbol) -> (String, Hash32) {
        (s.uid.clone(), index.file(s.file).hash)
    }
}

pub fn similar(ws: &Workspace, target: SymbolId, limit: usize) -> Result<Vec<SimilarRow>> {
    Ok(similar_many(ws, &[target], limit)?
        .pop()
        .map(|(_, rows)| rows)
        .unwrap_or_default())
}

/// Candidate symbols for `target` (module docs).
fn candidates(index: &Index, target: SymbolId) -> Vec<SymbolId> {
    let t = index.symbol(target);
    let mine = u64::from(t.span.bytes.len());
    index
        .symbols
        .iter()
        .filter(|s| {
            let size = u64::from(s.span.bytes.len());
            s.id != target
                && !t.is_synthetic()
                && !s.is_synthetic()
                && s.kind == t.kind
                && s.language == t.language
                && !s.is_stub
                && size * 3 >= mine
                && size <= mine * 3
        })
        .map(|s| s.id)
        .collect()
}

/// Similar code for several targets with one cache load/save.
pub(crate) fn similar_many(
    ws: &Workspace,
    targets: &[SymbolId],
    limit: usize,
) -> Result<Vec<(SymbolId, Vec<SimilarRow>)>> {
    let index = ws.index()?;
    let mut cache: FingerprintCache =
        load_blob(&ws.paths.similar_file, SIMILAR_MAGIC, SCHEMA_VERSION).unwrap_or_default();
    let plans: Vec<(SymbolId, Vec<SymbolId>)> = targets
        .iter()
        .map(|&t| {
            let has_body = !index.symbol(t).span.bytes.is_empty();
            (
                t,
                if has_body {
                    candidates(index, t)
                } else {
                    Vec::new()
                },
            )
        })
        .collect();

    // Fingerprints still missing from the cache, grouped by file (one parse per file).
    let mut missing: BTreeMap<FileId, Vec<SymbolId>> = BTreeMap::new();
    let mut seen: HashSet<SymbolId> = HashSet::new();
    for (t, cands) in &plans {
        for &id in std::iter::once(t).chain(cands.iter()) {
            if !seen.insert(id) {
                continue;
            }
            let s = index.symbol(id);
            if !cache.entries.contains_key(&FingerprintCache::key(index, s)) {
                missing.entry(s.file).or_default().push(id);
            }
        }
    }
    let root = std::path::PathBuf::from(&index.header.root);
    let computed: Vec<Vec<(SymbolId, Option<Fingerprint>)>> = missing
        .par_iter()
        .filter_map(|(&fid, ids)| {
            let rec = index.file(fid);
            // A file that changed since indexing is skipped (not cached) for this run.
            let bytes = read_verified(&root, &rec.path, &rec.hash).ok()?;
            let spans: Vec<_> = ids.iter().map(|&i| index.symbol(i).span.bytes).collect();
            let fps = trace_syntax::similar::fingerprints(rec.language, &bytes, &spans);
            Some(ids.iter().copied().zip(fps).collect())
        })
        .collect();
    let dirty = !computed.is_empty();
    for (id, fp) in computed.into_iter().flatten() {
        let key = FingerprintCache::key(index, index.symbol(id));
        cache.entries.insert(key, fp.filter(|f| !f.is_empty()));
    }

    let lookup = |id: SymbolId| -> Option<&Fingerprint> {
        cache
            .entries
            .get(&FingerprintCache::key(index, index.symbol(id)))
            .and_then(Option::as_ref)
    };
    let mut out = Vec::with_capacity(plans.len());
    for (t, cands) in &plans {
        let Some(base) = lookup(*t) else {
            out.push((*t, Vec::new()));
            continue;
        };
        let mut rows: Vec<SimilarRow> = cands
            .iter()
            .filter_map(|&c| {
                let other = lookup(c)?;
                let score = trace_syntax::jaccard(base, other);
                (score >= THRESHOLD).then(|| SimilarRow {
                    card: card(index, index.symbol(c)),
                    similarity: (score * 1000.0).round() / 1000.0,
                })
            })
            .collect();
        rows.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.card.id.cmp(&b.card.id))
        });
        rows.truncate(limit);
        out.push((*t, rows));
    }

    if dirty {
        save(ws, index, cache);
    }
    Ok(out)
}

/// Prune entries of vanished symbols/file versions and persist (best effort).
fn save(ws: &Workspace, index: &Index, mut cache: FingerprintCache) {
    let valid: HashSet<(&str, Hash32)> = index
        .symbols
        .iter()
        .map(|s| (s.uid.as_str(), index.file(s.file).hash))
        .collect();
    cache
        .entries
        .retain(|(uid, hash), _| valid.contains(&(uid.as_str(), *hash)));
    if ensure_outside(&ws.paths.similar_file, &[&ws.paths.root]).is_ok() {
        let _ = save_blob(&ws.paths.similar_file, SIMILAR_MAGIC, SCHEMA_VERSION, &cache);
    }
}
