//! Incremental link ([`assemble_delta`]): only the records of changed / removed / re-queried
//! files (and the files that name their uids) are linked again; everything else moves by
//! [`IdRemap`]. The result equals [`assemble`] over the same records.

use super::*;

/// Inputs of [`assemble_delta`]: only the changed + requeried file records, the removed
/// paths, and the global parts of the new index.
pub struct AssembleDeltaInput {
    pub header: IndexHeader,
    /// Changed + requeried files only (a record of an unchanged file is accepted and simply
    /// replaces the previous one).
    pub files: Vec<FileRecord>,
    pub removed: Vec<String>,
    pub configs: Vec<(String, Hash32)>,
    pub omitted: Vec<OmittedFile>,
    pub support: Vec<LanguageSupport>,
    pub backend_runs: Vec<BackendRun>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Where a file of the new index comes from.
#[derive(Clone, Copy, Debug)]
pub(super) enum Slot {
    /// A record of `prev` kept as is (index into `prev.files`).
    Kept(usize),
    /// A new record (index into the delta input's files).
    New(usize),
}

/// Incremental link (module docs): replace the records of changed / removed / requeried
/// files of `prev` and re-link only what can differ. Equals [`assemble`] over `prev`'s
/// untouched records plus the new ones. `prev`'s sites, decisions, bridges, rule edges
/// (family / import edges) and other phases' states are dropped: the later phases refill
/// them (`trace_analysis::pipeline::update` takes what it needs out of `prev` first).
///
/// `delta.full`, a missing link state or an internal inconsistency link everything
/// ([`assemble`]); the result is the same, only slower.
pub fn assemble_delta(prev: Index, input: AssembleDeltaInput, delta: &IndexDelta) -> (Index, IdRemap) {
    if delta.full {
        return full_link(prev, input, delta);
    }
    let Some(state) = LinkState::of(&prev) else {
        return full_link(prev, input, delta);
    };
    match plan_delta(&prev, &input, delta, state) {
        Some(plan) => apply_delta(prev, input, plan),
        None => full_link(prev, input, delta),
    }
}

/// Link everything: `prev`'s untouched records plus the new ones through [`assemble`], with
/// the old -> new id remap by path and uid.
pub(super) fn full_link(prev: Index, input: AssembleDeltaInput, delta: &IndexDelta) -> (Index, IdRemap) {
    let AssembleDeltaInput {
        header,
        files: new_files,
        removed,
        configs,
        omitted,
        support,
        backend_runs,
        diagnostics,
    } = input;
    let replaced: HashSet<String> = new_files
        .iter()
        .map(|f| f.path.clone())
        .chain(removed)
        .chain(delta.removed.iter().cloned())
        .collect();
    let old_files: Vec<String> = prev.files.iter().map(|f| f.path.clone()).collect();
    let old_symbols: Vec<String> = prev.symbols.iter().map(|s| s.uid.clone()).collect();
    let mut files: Vec<FileRecord> = prev
        .files
        .into_iter()
        .filter(|f| !replaced.contains(&f.path))
        .collect();
    files.extend(new_files);
    let index = assemble(AssembleInput {
        header,
        files,
        configs,
        omitted,
        support,
        backend_runs,
        diagnostics,
    });
    let file_ids: HashMap<&str, FileId> = index
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.path.as_str(), FileId(i as u32)))
        .collect();
    let symbol_ids: HashMap<&str, SymbolId> = index.symbols.iter().map(|s| (s.uid.as_str(), s.id)).collect();
    let remap = IdRemap {
        files: old_files.iter().map(|p| file_ids.get(p.as_str()).copied()).collect(),
        symbols: old_symbols
            .iter()
            .map(|u| symbol_ids.get(u.as_str()).copied())
            .collect(),
    };
    drop(file_ids);
    drop(symbol_ids);
    (index, remap)
}

/// Everything decided before `prev` is taken apart (a `None` plan links fully instead).
pub(super) struct DeltaPlan {
    /// Order of the new index's files.
    slots: Vec<Slot>,
    /// Duplicate new records dropped (for the diagnostic).
    duplicates: usize,
    /// First symbol id of each slot in the new index.
    firsts: Vec<u32>,
    /// Slots whose facts are linked again.
    relink: Vec<bool>,
    remap: IdRemap,
    /// Linked facts of kept, not re-linked files, remapped (in prev order).
    kept_edges: Vec<Edge>,
    kept_unresolved: Vec<Unresolved>,
    kept_value_refs: Vec<ValueRef>,
    /// Link counts of kept, not re-linked files.
    kept_totals: LinkCounts,
    state: LinkState,
    /// Old uids of replaced files that still exist (old symbol id, uid).
    replaced_uids: Vec<(usize, String)>,
}

pub(super) fn plan_delta(
    prev: &Index,
    input: &AssembleDeltaInput,
    delta: &IndexDelta,
    mut state: LinkState,
) -> Option<DeltaPlan> {
    // New records in path order, duplicates dropped (the first occurrence of a path wins
    // after a stable sort, exactly like `sort_records`).
    let mut new_order: Vec<usize> = (0..input.files.len()).collect();
    new_order.sort_by(|&a, &b| input.files[a].path.cmp(&input.files[b].path));
    let before = new_order.len();
    new_order.dedup_by(|a, b| input.files[*a].path == input.files[*b].path);
    // Strictly sorted input has no duplicates, so this equals `sort_records`' count.
    let duplicates = before - new_order.len();

    let removed: HashSet<&str> = input
        .removed
        .iter()
        .chain(delta.removed.iter())
        .map(String::as_str)
        .collect();
    let new_paths: HashSet<&str> = new_order.iter().map(|&i| input.files[i].path.as_str()).collect();
    let replaced = |path: &str| removed.contains(path) || new_paths.contains(path);

    // Merge kept prev files and new records by path.
    let mut slots: Vec<Slot> = Vec::with_capacity(prev.files.len() + new_order.len());
    let (mut i, mut j) = (0usize, 0usize);
    loop {
        while i < prev.files.len() && replaced(&prev.files[i].path) {
            i += 1;
        }
        match (prev.files.get(i), new_order.get(j)) {
            (Some(a), Some(&b)) => {
                if a.path < input.files[b].path {
                    slots.push(Slot::Kept(i));
                    i += 1;
                } else {
                    slots.push(Slot::New(b));
                    j += 1;
                }
            }
            (Some(_), None) => {
                slots.push(Slot::Kept(i));
                i += 1;
            }
            (None, Some(&b)) => {
                slots.push(Slot::New(b));
                j += 1;
            }
            (None, None) => break,
        }
    }

    // Symbol layout and the id remap of kept files.
    let mut firsts: Vec<u32> = Vec::with_capacity(slots.len());
    let mut remap = IdRemap {
        files: vec![None; prev.files.len()],
        symbols: vec![None; prev.symbols.len()],
    };
    let mut next = 0u32;
    let mut slot_of_path: HashMap<&str, usize> = HashMap::with_capacity(new_order.len());
    for (si, slot) in slots.iter().enumerate() {
        firsts.push(next);
        match *slot {
            Slot::Kept(pi) => {
                let rec = &prev.files[pi];
                remap.files[pi] = Some(FileId(si as u32));
                for k in 0..rec.symbol_count {
                    let old = (rec.first_symbol + k) as usize;
                    *remap.symbols.get_mut(old)? = Some(SymbolId(next + k));
                }
                next += rec.symbol_count;
            }
            Slot::New(ni) => {
                let rec = &input.files[ni];
                slot_of_path.insert(rec.path.as_str(), si);
                next += rec.facts.as_ref().map_or(0, |f| f.declarations.len() as u32);
            }
        }
    }

    // Replaced files with a previous record: their old uids (remapped by uid later) and the
    // new-record slot of the same path (a replaced file keeps its old FileId mapping).
    let mut replaced_uids: Vec<(usize, String)> = Vec::new();
    let mut facts_changed: BTreeSet<&str> = BTreeSet::new();
    for (pi, rec) in prev.files.iter().enumerate() {
        if !replaced(&rec.path) {
            continue;
        }
        match slot_of_path.get(rec.path.as_str()) {
            Some(&si) => {
                remap.files[pi] = Some(FileId(si as u32));
                let new_rec = match slots[si] {
                    Slot::New(ni) => &input.files[ni],
                    Slot::Kept(_) => return None,
                };
                if new_rec.facts != rec.facts {
                    facts_changed.insert(rec.path.as_str());
                }
                for s in prev.symbols_of(FileId(pi as u32)) {
                    replaced_uids.push((s.id.idx(), s.uid.clone()));
                }
            }
            None => {
                facts_changed.insert(rec.path.as_str());
            }
        }
    }
    for &ni in &new_order {
        let path = input.files[ni].path.as_str();
        if prev.file_by_path(path).is_none() {
            facts_changed.insert(path);
        }
    }
    // Canonical C / C++ targets depend on every prototype and definition of a name: when a
    // replaced, added or removed file changes one that a prototype of the index (or of a new
    // record) names, the carried edges may no longer be canonical -> link everything.
    if c_prototypes_changed(prev, input, &new_order, &removed) {
        return None;
    }

    // Files linked again: every new record; files naming a uid of a file whose facts changed
    // (dangling rule); files whose implementation edges live in a replaced file, and the
    // implementor files of replaced files' implementations (equal-key edges of two files
    // keep the first file's edge, so both sides are linked again in file order).
    let mut relink_paths: HashSet<&str> = new_paths.clone();
    for p in &facts_changed {
        if let Some(sources) = state.global.refs.get(*p) {
            relink_paths.extend(sources.iter().map(String::as_str));
        }
    }
    let replaced_all: Vec<&str> = new_paths.iter().copied().chain(removed.iter().copied()).collect();
    for p in &replaced_all {
        if let Some(sources) = state.global.impls.get(*p) {
            relink_paths.extend(sources.iter().map(String::as_str));
        }
    }
    let mut implementor_paths: Vec<&str> = Vec::new();
    for p in &replaced_all {
        if let Some(rec) = prev.file_by_path(p).map(|id| prev.file(id)) {
            if let Some(sem) = &rec.semantic {
                implementor_paths.extend(referenced_paths(sem).1);
            }
        }
    }
    for &ni in &new_order {
        if let Some(sem) = &input.files[ni].semantic {
            implementor_paths.extend(referenced_paths(sem).1);
        }
    }
    relink_paths.extend(implementor_paths);
    let relink: Vec<bool> = slots
        .iter()
        .map(|slot| match *slot {
            Slot::Kept(pi) => relink_paths.contains(prev.files[pi].path.as_str()),
            Slot::New(_) => true,
        })
        .collect();
    let kept_linked: Vec<bool> = {
        // Per prev file: kept AND not re-linked (its previous linked facts are reused).
        let mut v = vec![false; prev.files.len()];
        for (si, slot) in slots.iter().enumerate() {
            if let Slot::Kept(pi) = *slot {
                v[pi] = !relink[si];
                // Reused edges are told apart from rule edges by their provider: a backend
                // reporting a rule provider is linked fully instead.
                let rule_provider = prev.files[pi]
                    .semantic
                    .as_ref()
                    .is_some_and(|sem| matches!(sem.provider, Provider::Rule(_)));
                if v[pi] && rule_provider {
                    return None;
                }
            }
        }
        v
    };

    // Reuse the linked facts of kept, not re-linked files (rule edges of later phases are
    // dropped: they are recomputed by the family phase).
    let mut kept_edges: Vec<Edge> = Vec::new();
    for e in &prev.edges {
        if matches!(e.provider, Provider::Rule(_)) {
            continue;
        }
        let implementation = e.resolution == Resolution::Implementation
            && matches!(e.kind, EdgeKind::Implements | EdgeKind::Overrides);
        let source = if implementation {
            prev.symbols.get(e.to.idx())?.file
        } else {
            e.at.file
        };
        if !kept_linked.get(source.idx()).copied().unwrap_or(false) {
            continue;
        }
        let mut e = e.clone();
        e.from = remap.symbol(e.from)?;
        e.to = remap.symbol(e.to)?;
        e.at.file = remap.file(e.at.file)?;
        kept_edges.push(e);
    }
    let mut kept_unresolved: Vec<Unresolved> = Vec::new();
    for u in &prev.unresolved {
        if !kept_linked.get(u.at.file.idx()).copied().unwrap_or(false) {
            continue;
        }
        let mut u = u.clone();
        u.at.file = remap.file(u.at.file)?;
        u.owner = match u.owner {
            Some(o) => Some(remap.symbol(o)?),
            None => None,
        };
        for c in &mut u.candidates {
            *c = remap.symbol(*c)?;
        }
        kept_unresolved.push(u);
    }
    let mut kept_value_refs: Vec<ValueRef> = Vec::new();
    for r in &prev.value_refs {
        if !kept_linked.get(r.at.file.idx()).copied().unwrap_or(false) {
            continue;
        }
        kept_value_refs.push(ValueRef {
            at: Location {
                file: remap.file(r.at.file)?,
                ..r.at
            },
            target: remap.symbol(r.target)?,
        });
    }
    let mut kept_totals = LinkCounts::default();
    for (path, counts) in &state.counts {
        if let Some(id) = prev.file_by_path(path) {
            if kept_linked[id.idx()] {
                kept_totals.dangling += counts.dangling;
                kept_totals.non_proven += counts.non_proven;
            }
        }
    }

    // Link state: drop replaced files as sources (their old semantics); new records are
    // added after linking. Counts of re-linked and removed files are recomputed / dropped.
    for p in &replaced_all {
        if let Some(rec) = prev.file_by_path(p).map(|id| prev.file(id)) {
            if let Some(sem) = &rec.semantic {
                state.global.remove(&rec.path, sem);
            }
        }
        state.counts.remove(*p);
    }
    for (si, slot) in slots.iter().enumerate() {
        if let (Slot::Kept(pi), true) = (*slot, relink[si]) {
            state.counts.remove(&prev.files[pi].path);
        }
    }

    Some(DeltaPlan {
        slots,
        duplicates,
        firsts,
        relink,
        remap,
        kept_edges,
        kept_unresolved,
        kept_value_refs,
        kept_totals,
        state,
        replaced_uids,
    })
}

pub(super) fn apply_delta(prev: Index, input: AssembleDeltaInput, plan: DeltaPlan) -> (Index, IdRemap) {
    let AssembleDeltaInput {
        header,
        files: new_files,
        removed: _,
        mut configs,
        mut omitted,
        support,
        backend_runs,
        mut diagnostics,
    } = input;
    let DeltaPlan {
        slots,
        duplicates,
        firsts,
        relink,
        mut remap,
        kept_edges,
        kept_unresolved,
        kept_value_refs,
        kept_totals,
        mut state,
        replaced_uids,
    } = plan;
    if duplicates > 0 {
        diagnostics.push(Diagnostic::new(
            "duplicate_file_record",
            None,
            format!("{duplicates} duplicate file records were dropped"),
        ));
    }
    configs.sort();
    omitted.sort_by(|a, b| a.path.cmp(&b.path));

    // Move the records into the new order; kept symbols move with remapped ids.
    let prev_ranges: Vec<(u32, u32)> = prev.files.iter().map(|f| (f.first_symbol, f.symbol_count)).collect();
    let mut prev_files: Vec<Option<FileRecord>> = prev.files.into_iter().map(Some).collect();
    let mut new_slots: Vec<Option<FileRecord>> = new_files.into_iter().map(Some).collect();
    let mut old_symbols = prev.symbols.into_iter().enumerate().peekable();
    let mut files: Vec<FileRecord> = Vec::with_capacity(slots.len());
    let mut symbols: Vec<Symbol> = Vec::new();
    for (si, slot) in slots.iter().enumerate() {
        let fid = FileId(si as u32);
        let first = firsts[si];
        match *slot {
            Slot::Kept(pi) => {
                let mut rec = prev_files[pi].take().unwrap_or_else(|| unreachable_record(pi));
                let (old_first, count) = prev_ranges[pi];
                while old_symbols.peek().is_some_and(|(i, _)| (*i as u32) < old_first) {
                    old_symbols.next();
                }
                for _ in 0..count {
                    let Some((_, mut s)) = old_symbols.next() else { break };
                    s.id = SymbolId(first + s.decl);
                    s.file = fid;
                    s.parent = s.parent.map(|p| SymbolId(p.0 - old_first + first));
                    symbols.push(s);
                }
                rec.first_symbol = first;
                rec.symbol_count = count;
                files.push(rec);
            }
            Slot::New(ni) => {
                let mut rec = new_slots[ni].take().unwrap_or_else(|| unreachable_record(ni));
                file_symbols(fid, &mut rec, &mut symbols);
                files.push(rec);
            }
        }
    }
    drop(old_symbols);

    // Replaced files: old symbol ids map to the new symbol with the same uid.
    if !replaced_uids.is_empty() {
        let mut by_uid: HashMap<&str, SymbolId> = HashMap::new();
        for (si, slot) in slots.iter().enumerate() {
            if matches!(slot, Slot::New(_)) {
                let f = &files[si];
                for s in &symbols[f.first_symbol as usize..(f.first_symbol + f.symbol_count) as usize] {
                    by_uid.insert(s.uid.as_str(), s.id);
                }
            }
        }
        for (old, uid) in &replaced_uids {
            if let Some(slot) = remap.symbols.get_mut(*old) {
                *slot = by_uid.get(uid.as_str()).copied();
            }
        }
    }

    // Link the re-linked files; the uid lookup covers only the files they name.
    let mut wanted: BTreeSet<&str> = BTreeSet::new();
    for (si, file) in files.iter().enumerate() {
        if relink[si] {
            if let Some(sem) = &file.semantic {
                wanted.extend(referenced_paths(sem).0);
            }
        }
    }
    let mut by_uid: HashMap<&str, SymbolId> = HashMap::new();
    for path in wanted {
        if let Ok(fi) = files.binary_search_by(|f| f.path.as_str().cmp(path)) {
            let f = &files[fi];
            for s in &symbols[f.first_symbol as usize..(f.first_symbol + f.symbol_count) as usize] {
                by_uid.insert(s.uid.as_str(), s.id);
            }
        }
    }
    let lookup = |uid: &str| by_uid.get(uid).copied();
    let mut edges = kept_edges;
    let mut unresolved = kept_unresolved;
    let mut value_refs = kept_value_refs;
    let mut totals = kept_totals;
    for (si, file) in files.iter().enumerate() {
        if !relink[si] {
            continue;
        }
        let linked = link_file(FileId(si as u32), file, &symbols, &lookup);
        edges.extend(linked.edges);
        unresolved.extend(linked.unresolved);
        value_refs.extend(linked.value_refs);
        totals.dangling += linked.counts.dangling;
        totals.non_proven += linked.counts.non_proven;
        if !linked.counts.is_zero() {
            state.counts.insert(file.path.clone(), linked.counts);
        }
        if matches!(slots[si], Slot::New(_)) {
            if let Some(sem) = &file.semantic {
                state.global.add(&file.path, sem);
            }
        }
    }
    drop(by_uid);
    push_link_diagnostics(&mut diagnostics, totals);
    // Kept edges are canonical already (the plan links fully when a C / C++ declaration
    // that decides a canonical target changed); applying the rule again is idempotent.
    canonical_c_targets(&symbols, &mut edges, &mut value_refs);
    finish_links(&mut edges, &mut unresolved, &mut value_refs);

    let index = Index {
        header,
        files,
        configs,
        omitted,
        symbols,
        edges,
        unresolved,
        value_refs,
        sites: Vec::new(),
        decisions: Vec::new(),
        bridges: Vec::new(),
        support,
        backend_runs,
        diagnostics,
        phase_state: vec![state.encode()],
        stale: Default::default(),
        library_receivers: Vec::new(),
    };
    (index, remap)
}

/// A slot taken twice cannot happen (every slot names a distinct record); an empty record
/// keeps the code free of panics and is caught by `Index::validate` if it ever appeared.
pub(super) fn unreachable_record(_index: usize) -> FileRecord {
    FileRecord {
        path: String::new(),
        language: crate::languages::Language::Python,
        hash: Hash32::default(),
        size: 0,
        mtime_ns: 0,
        support: crate::languages::SupportLevel::Inventoried,
        facts: None,
        semantic: None,
        first_symbol: 0,
        symbol_count: 0,
        diagnostics: Vec::new(),
        pending: None,
    }
}
