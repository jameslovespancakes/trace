//! The repository overview: areas, entry points and the most connected symbols.

use std::collections::{BTreeMap, HashMap, HashSet};

use trace_core::tiers::in_view;
use trace_core::{Index, SymbolId, SymbolKind, Tier};

use crate::cards::card;
use crate::report::{AreaRow, HubRow, Overview};

/// Rows per overview list (areas, entry points, hubs).
const TOP: usize = 10;
/// Decorator names that mark framework entry points.
const ROUTE_DECORATORS: &[&str] =
    &["get", "post", "put", "patch", "delete", "route", "command", "task", "fixture"];

/// Area of a file: first two path segments, or one area per module
/// in a flat package holding most of the files.
pub fn area(file: &str, dir_counts: &HashMap<&str, usize>, total: usize) -> String {
    let stem = file.rsplit_once('.').map(|(a, _)| a).unwrap_or(file);
    let parts: Vec<&str> = stem.split('/').collect();
    let directory = if parts.len() > 1 {
        parts[..parts.len() - 1].join("/")
    } else {
        parts[0].to_string()
    };
    let dir_key = file.rsplit_once('/').map(|(d, _)| d).unwrap_or(file);
    let same_dir = dir_counts.get(dir_key).copied().unwrap_or(0);
    if same_dir as f64 > (5.0f64).max(0.6 * total as f64) {
        return parts.join("/");
    }
    if parts.len() > 2 {
        parts[..2].join("/")
    } else {
        directory
    }
}

/// Overview data over the view at `include`.
pub fn overview(index: &Index, include: Tier) -> Overview {
    let files: Vec<&str> = index.files.iter().map(|f| f.path.as_str()).collect();
    let mut dir_counts: HashMap<&str, usize> = HashMap::new();
    for f in &files {
        let d = f.rsplit_once('/').map(|(d, _)| d).unwrap_or(f);
        *dir_counts.entry(d).or_insert(0) += 1;
    }
    let file_area: Vec<String> = files.iter().map(|f| area(f, &dir_counts, files.len())).collect();

    #[derive(Default)]
    struct Acc {
        files: HashSet<u32>,
        functions: usize,
        classes: usize,
    }
    let mut areas: BTreeMap<&str, Acc> = BTreeMap::new();
    for s in index.symbols.iter().filter(|s| !s.is_synthetic()) {
        let a = areas.entry(file_area[s.file.idx()].as_str()).or_default();
        a.files.insert(s.file.0);
        if s.kind.is_callable() {
            a.functions += 1;
        } else {
            a.classes += 1;
        }
    }
    let mut area_rows: Vec<AreaRow> = areas
        .into_iter()
        .map(|(area, acc)| AreaRow {
            area: area.to_string(),
            files: acc.files.len(),
            functions: acc.functions,
            classes: acc.classes,
        })
        .collect();
    area_rows.sort_by(|a, b| b.functions.cmp(&a.functions).then_with(|| a.area.cmp(&b.area)));
    area_rows.truncate(TOP);

    // Degrees over the execution view (materialized like the graph).
    let graph = trace_core::Graph::new(index);
    let n = index.symbols.len();
    let mut callers: Vec<HashSet<SymbolId>> = vec![HashSet::new(); n];
    let mut outdeg = vec![0usize; n];
    for e in graph.edges() {
        if !in_view(e, include) || e.from == e.to {
            continue;
        }
        callers[e.to.idx()].insert(e.from);
        outdeg[e.from.idx()] += 1;
    }
    let is_routed = |decorators: &[String]| {
        decorators.iter().any(|d| {
            let head = d.trim_start_matches('@').split('(').next().unwrap_or("");
            let last = head.rsplit('.').next().unwrap_or(head).trim();
            ROUTE_DECORATORS.contains(&last)
        })
    };
    let mut entries: Vec<SymbolId> = index
        .symbols
        .iter()
        .filter(|s| {
            s.kind == SymbolKind::Function
                && !s.is_synthetic()
                && s.parent.is_none()
                && callers[s.id.idx()].is_empty()
                && outdeg[s.id.idx()] > 0
                && !s.name.starts_with('_')
                && !s.is_test
        })
        .map(|s| s.id)
        .collect();
    entries.sort_by(|a, b| {
        outdeg[b.idx()]
            .cmp(&outdeg[a.idx()])
            .then_with(|| index.symbol(*a).uid.cmp(&index.symbol(*b).uid))
    });
    let mut entry_points: Vec<SymbolId> = index
        .symbols
        .iter()
        .filter(|s| s.kind.is_callable() && !s.is_synthetic() && is_routed(&s.decorators))
        .map(|s| s.id)
        .collect();
    for e in entries {
        if !entry_points.contains(&e) {
            entry_points.push(e);
        }
    }
    entry_points.truncate(TOP);

    let mut hubs: Vec<SymbolId> = (0..n)
        .map(|i| SymbolId(i as u32))
        .filter(|id| !callers[id.idx()].is_empty() && !index.symbol(*id).is_synthetic())
        .collect();
    hubs.sort_by(|a, b| {
        callers[b.idx()]
            .len()
            .cmp(&callers[a.idx()].len())
            .then_with(|| index.symbol(*a).uid.cmp(&index.symbol(*b).uid))
    });
    hubs.truncate(TOP);

    Overview {
        areas: area_rows,
        entry_points: entry_points.iter().map(|&id| card(index, index.symbol(id))).collect(),
        hubs: hubs
            .iter()
            .map(|&id| HubRow {
                card: card(index, index.symbol(id)),
                callers: callers[id.idx()].len(),
            })
            .collect(),
    }
}
