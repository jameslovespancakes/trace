//! Assembly: per-file syntax facts + per-file semantics -> one consistent [`Index`].
//!
//! Symbols come exclusively from syntax declarations (all languages). Semantic backends only
//! contribute edges, unresolved sites and value references mapped onto those symbols. Sites
//! and decisions are filled later by `trace-infer`.
//!
//! Invariants established here (checked again by [`Index::validate`] on load):
//! * files sorted by path; symbols contiguous per file in declaration order;
//! * `Index::edges` holds proven facts only — a backend can never inject inferred or
//!   possible kinds (such edges are dropped with a `non_proven_semantic_edge` diagnostic);
//! * semantic targets that no longer exist are dropped (`dangling_semantic_target`);
//! * server implementations (`FileSemantics::implementations`, SPEC §8.5a) become proven
//!   family edges `implementor -> base` (kind as recorded: `implements` / `overrides`;
//!   provider = the file's backend; resolution `implementation`; `at` = the implementor's
//!   name span in its own file); unknown uids are dropped like dangling targets, other kinds
//!   and self links are ignored. Rule edges of the same (from, to, kind) appended later
//!   (pipeline phase 4b) never duplicate them: the family rule skips pairs a server already
//!   proved, and [`sort_edges`] keeps the first (server) occurrence of equal keys;
//! * files without semantics (pending languages / sub-projects, inventoried files) contribute
//!   symbols only: they are analysed when a query first needs them, never answered from
//!   syntax;
//! * canonical C / C++ targets (language rule, [`c_prototype_definitions`]): an edge or value
//!   reference the server answered with a prototype / forward declaration whose unique
//!   definition exists in the index targets that definition (C has one global definition per
//!   name, C++ one per entity). clangd answers a call with the header prototype or the
//!   definition depending on which files it has open, so the canonical target makes full and
//!   incremental links agree. Family edges (`implements`, `overrides`, `stub_implementation`)
//!   and import / re-export edges keep their declaration.
//!
//! Incremental link ([`assemble_delta`]): only
//! the records of changed / removed / re-queried files are linked again, plus the files whose
//! cached semantics name a uid of a file whose facts changed (the dangling rule, found through
//! the reverse reference index of the `link` [`PhaseState`]) and the files whose
//! implementation edges can collide with a re-linked file's; every other edge, unknown and
//! value reference moves by [`IdRemap`]. The result equals [`assemble`] over the same records
//! (`rule_incremental_link_equals_full_link`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::delta::{IdRemap, IndexDelta, PhaseState};
use crate::facts::ParamKind;
use crate::fingerprint::Hash32;
use crate::languages::LanguageSupport;
use crate::model::{
    BackendRun, ByteSpan, Diagnostic, Edge, EdgeKind, FileId, FileRecord, Index, IndexHeader, Location,
    OmittedFile, Provider, Resolution, Symbol, SymbolId, SymbolKind, Tier, Unresolved, ValueRef,
};
use crate::semantics::FileSemantics;

mod c_targets;
mod relink;

pub use c_targets::*;
pub use relink::*;

/// [`PhaseState::phase`] of the link state.
pub const LINK_PHASE: &str = "link";
/// Version of the link state encoding; another version forces a full link.
pub(crate) const LINK_STATE_VERSION: u32 = 1;

/// Inputs of [`assemble`]. `files` should be sorted by path (sorted here if not).
pub struct AssembleInput {
    pub header: IndexHeader,
    pub files: Vec<FileRecord>,
    pub configs: Vec<(String, Hash32)>,
    pub omitted: Vec<OmittedFile>,
    pub support: Vec<LanguageSupport>,
    pub backend_runs: Vec<BackendRun>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Stable uid for the `occurrence`-th (1-based) declaration of `qualified` in `path`.
pub fn symbol_uid(path: &str, qualified: &str, occurrence: u32) -> String {
    if occurrence <= 1 {
        format!("{path}:{qualified}")
    } else {
        format!("{path}:{qualified}#{occurrence}")
    }
}

/// File part of a uid (`{path}:{qualified}`; paths never contain ':').
fn uid_path(uid: &str) -> &str {
    uid.split_once(':').map_or(uid, |(p, _)| p)
}

/// Global block of the link state: reverse reference indexes by file path.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct LinkGlobal {
    /// Target file path -> files whose cached semantics name a uid of that file (edge and
    /// value-reference targets, unresolved candidates, implementors).
    refs: BTreeMap<String, BTreeSet<String>>,
    /// Implementor file path -> files whose server implementations name an implementor there
    /// (their edges are located in the implementor's file).
    impls: BTreeMap<String, BTreeSet<String>>,
}

/// Per-file block of the link state (stored only when non-zero).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct LinkCounts {
    dangling: u64,
    non_proven: u64,
}

impl LinkCounts {
    fn is_zero(&self) -> bool {
        self.dangling == 0 && self.non_proven == 0
    }
}

/// Paths a file's semantics reference: (every referenced file, implementor files).
fn referenced_paths(sem: &FileSemantics) -> (BTreeSet<&str>, BTreeSet<&str>) {
    let mut refs: BTreeSet<&str> = BTreeSet::new();
    let mut impls: BTreeSet<&str> = BTreeSet::new();
    refs.extend(sem.edges.iter().map(|e| uid_path(&e.target)));
    refs.extend(sem.value_refs.iter().map(|r| uid_path(&r.target)));
    for u in &sem.unresolved {
        refs.extend(u.candidates.iter().map(|c| uid_path(c)));
    }
    for i in &sem.implementations {
        refs.insert(uid_path(&i.implementor));
        impls.insert(uid_path(&i.implementor));
    }
    (refs, impls)
}

impl LinkGlobal {
    fn add(&mut self, source: &str, sem: &FileSemantics) {
        let (refs, impls) = referenced_paths(sem);
        for p in refs {
            self.refs.entry(p.to_string()).or_default().insert(source.to_string());
        }
        for p in impls {
            self.impls
                .entry(p.to_string())
                .or_default()
                .insert(source.to_string());
        }
    }

    fn remove(&mut self, source: &str, sem: &FileSemantics) {
        let (refs, impls) = referenced_paths(sem);
        for p in refs {
            if let Some(set) = self.refs.get_mut(p) {
                set.remove(source);
                if set.is_empty() {
                    self.refs.remove(p);
                }
            }
        }
        for p in impls {
            if let Some(set) = self.impls.get_mut(p) {
                set.remove(source);
                if set.is_empty() {
                    self.impls.remove(p);
                }
            }
        }
    }
}

/// The decoded link state of an index (`None`: absent, other version or undecodable).
struct LinkState {
    global: LinkGlobal,
    counts: BTreeMap<String, LinkCounts>,
}

impl LinkState {
    fn of(index: &Index) -> Option<LinkState> {
        let state = index
            .phase_state
            .iter()
            .find(|s| s.phase == LINK_PHASE && s.version == LINK_STATE_VERSION)?;
        let global: LinkGlobal = postcard::from_bytes(&state.global).ok()?;
        let mut counts = BTreeMap::new();
        for (path, bytes) in &state.files {
            counts.insert(path.clone(), postcard::from_bytes::<LinkCounts>(bytes).ok()?);
        }
        Some(LinkState { global, counts })
    }

    fn encode(&self) -> PhaseState {
        PhaseState {
            phase: LINK_PHASE.to_string(),
            version: LINK_STATE_VERSION,
            // Encoding plain maps of strings and integers into a Vec cannot fail; an empty
            // block would only force the next update to link fully.
            global: postcard::to_stdvec(&self.global).unwrap_or_default(),
            files: self
                .counts
                .iter()
                .filter(|(_, c)| !c.is_zero())
                .filter_map(|(p, c)| postcard::to_stdvec(c).ok().map(|b| (p.clone(), b)))
                .collect(),
        }
    }
}

/// Linked facts of one file.
#[derive(Default)]
struct FileLink {
    edges: Vec<Edge>,
    unresolved: Vec<Unresolved>,
    value_refs: Vec<ValueRef>,
    counts: LinkCounts,
}

/// Link one file's semantics onto `symbols` (`lookup`: uid ->
/// symbol id of the new index).
fn link_file(
    fid: FileId,
    file: &FileRecord,
    symbols: &[Symbol],
    lookup: &dyn Fn(&str) -> Option<SymbolId>,
) -> FileLink {
    let mut out = FileLink::default();
    let owner_of = |decl: u32| file.symbol_of_decl(decl);
    let loc = |at: ByteSpan, line: u32| Location {
        file: fid,
        bytes: at,
        line,
    };
    if let Some(sem) = &file.semantic {
        for e in &sem.edges {
            if e.kind.tier() != Tier::Proven {
                out.counts.non_proven += 1;
                continue;
            }
            match (owner_of(e.owner), lookup(e.target.as_str())) {
                (Some(from), Some(to)) => out.edges.push(Edge {
                    from,
                    to,
                    kind: e.kind,
                    tier: Tier::Proven,
                    provider: sem.provider.clone(),
                    resolution: e.resolution,
                    at: loc(e.at, e.line),
                    site: None,
                    bridge: None,
                }),
                _ => out.counts.dangling += 1,
            }
        }
        for u in &sem.unresolved {
            let mut candidates: Vec<SymbolId> =
                u.candidates.iter().filter_map(|c| lookup(c.as_str())).collect();
            candidates.sort_unstable();
            candidates.dedup();
            out.unresolved.push(Unresolved {
                owner: u.owner.and_then(owner_of),
                kind: u.kind,
                at: loc(u.at, u.line),
                callee: u.callee.clone(),
                candidates,
            });
        }
        for r in &sem.value_refs {
            match lookup(r.target.as_str()) {
                Some(target) => out.value_refs.push(ValueRef {
                    at: loc(r.at, r.line),
                    target,
                }),
                None => out.counts.dangling += 1,
            }
        }
        for imp in &sem.implementations {
            if !matches!(imp.kind, EdgeKind::Implements | EdgeKind::Overrides) {
                continue;
            }
            let Some(base) = owner_of(imp.base) else {
                out.counts.dangling += 1;
                continue;
            };
            let Some(implementor) = lookup(imp.implementor.as_str()) else {
                out.counts.dangling += 1;
                continue;
            };
            if implementor == base {
                continue;
            }
            let s = &symbols[implementor.idx()];
            out.edges.push(Edge {
                from: implementor,
                to: base,
                kind: imp.kind,
                tier: Tier::Proven,
                provider: sem.provider.clone(),
                resolution: Resolution::Implementation,
                at: Location {
                    file: s.file,
                    bytes: s.name_span,
                    line: name_line(s),
                },
                site: None,
                bridge: None,
            });
        }
    }
    out
}

/// Sort `files` by path and drop duplicate paths (with a diagnostic), as [`assemble`] does.
fn sort_records(files: &mut Vec<FileRecord>, diagnostics: &mut Vec<Diagnostic>) {
    if files.windows(2).all(|w| w[0].path < w[1].path) {
        return;
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let before = files.len();
    files.dedup_by(|a, b| a.path == b.path);
    if files.len() != before {
        diagnostics.push(Diagnostic::new(
            "duplicate_file_record",
            None,
            format!("{} duplicate file records were dropped", before - files.len()),
        ));
    }
}

/// The link diagnostics of the whole index (totals over every file).
fn push_link_diagnostics(diagnostics: &mut Vec<Diagnostic>, totals: LinkCounts) {
    if totals.dangling > 0 {
        diagnostics.push(Diagnostic::new(
            "dangling_semantic_target",
            None,
            format!(
                "{} cached semantic results referenced symbols that no longer exist; \
                 they were dropped (the files are requeried on the next update)",
                totals.dangling
            ),
        ));
    }
    if totals.non_proven > 0 {
        diagnostics.push(Diagnostic::new(
            "non_proven_semantic_edge",
            None,
            format!(
                "{} semantic edges carried inferred/possible kinds and were dropped \
                 (backends may only report proven facts)",
                totals.non_proven
            ),
        ));
    }
}

/// Deterministic order, no duplicates (assembly's canonical order of linked facts).
fn finish_links(edges: &mut Vec<Edge>, unresolved: &mut Vec<Unresolved>, value_refs: &mut Vec<ValueRef>) {
    sort_edges(edges);
    unresolved.sort_by(|a, b| {
        (a.at.file, a.at.bytes, a.owner, &a.callee).cmp(&(b.at.file, b.at.bytes, b.owner, &b.callee))
    });
    unresolved.dedup_by(|a, b| a.at == b.at && a.owner == b.owner && a.kind == b.kind);
    value_refs.sort_by_key(|r| (r.at.file, r.at.bytes, r.target));
    value_refs.dedup();
}

/// Build the index. Dangling semantic targets are dropped with a diagnostic. The index
/// carries the `link` [`PhaseState`] for the next [`assemble_delta`].
pub fn assemble(input: AssembleInput) -> Index {
    let AssembleInput {
        header,
        mut files,
        mut configs,
        mut omitted,
        support,
        backend_runs,
        mut diagnostics,
    } = input;
    sort_records(&mut files, &mut diagnostics);
    configs.sort();
    omitted.sort_by(|a, b| a.path.cmp(&b.path));

    let symbols = build_symbols(&mut files);
    let by_uid: HashMap<&str, SymbolId> = symbols.iter().map(|s| (s.uid.as_str(), s.id)).collect();
    let lookup = |uid: &str| by_uid.get(uid).copied();

    // Semantic edges / unresolved / value refs.
    let mut edges: Vec<Edge> = Vec::new();
    let mut unresolved: Vec<Unresolved> = Vec::new();
    let mut value_refs: Vec<ValueRef> = Vec::new();
    let mut totals = LinkCounts::default();
    let mut state = LinkState {
        global: LinkGlobal::default(),
        counts: BTreeMap::new(),
    };
    for (fi, file) in files.iter().enumerate() {
        let linked = link_file(FileId(fi as u32), file, &symbols, &lookup);
        edges.extend(linked.edges);
        unresolved.extend(linked.unresolved);
        value_refs.extend(linked.value_refs);
        totals.dangling += linked.counts.dangling;
        totals.non_proven += linked.counts.non_proven;
        if let Some(sem) = &file.semantic {
            state.global.add(&file.path, sem);
        }
        if !linked.counts.is_zero() {
            state.counts.insert(file.path.clone(), linked.counts);
        }
    }
    push_link_diagnostics(&mut diagnostics, totals);
    canonical_c_targets(&symbols, &mut edges, &mut value_refs);
    finish_links(&mut edges, &mut unresolved, &mut value_refs);
    let link_state = state.encode();
    drop(by_uid);

    Index {
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
        phase_state: vec![link_state],
        stale: Default::default(),
        library_receivers: Vec::new(),
    }
}

/// Assembly's canonical edge order — (from, to, kind, file, bytes) — without duplicates of
/// that key (the first occurrence wins). Used again when phase 4b appends family edges.
pub fn sort_edges(edges: &mut Vec<Edge>) {
    edges.sort_by(|a, b| {
        (a.from, a.to, a.kind, a.at.file, a.at.bytes).cmp(&(b.from, b.to, b.kind, b.at.file, b.at.bytes))
    });
    let mut keys = HashSet::with_capacity(edges.len());
    edges.retain(|e| keys.insert((e.from, e.to, e.kind, e.at.file, e.at.bytes)));
}

/// Symbols from declarations; sets each file's `first_symbol` / `symbol_count`.
fn build_symbols(files: &mut [FileRecord]) -> Vec<Symbol> {
    let total: usize = files
        .iter()
        .filter_map(|f| f.facts.as_ref())
        .map(|f| f.declarations.len())
        .sum();
    let mut symbols: Vec<Symbol> = Vec::with_capacity(total);
    for (fi, file) in files.iter_mut().enumerate() {
        file_symbols(FileId(fi as u32), file, &mut symbols);
    }
    symbols
}

/// Append the symbols of one file (declaration order) and set its `first_symbol` /
/// `symbol_count`.
fn file_symbols(fid: FileId, file: &mut FileRecord, symbols: &mut Vec<Symbol>) {
    file.first_symbol = symbols.len() as u32;
    let Some(facts) = &file.facts else {
        file.symbol_count = 0;
        return;
    };
    let count = facts.declarations.len() as u32;
    let mut seen: HashMap<&str, u32> = HashMap::new();
    for (di, d) in facts.declarations.iter().enumerate() {
        let occurrence = seen.entry(d.qualified_name.as_str()).or_insert(0);
        *occurrence += 1;
        symbols.push(Symbol {
            id: SymbolId(symbols.len() as u32),
            uid: symbol_uid(&file.path, &d.qualified_name, *occurrence),
            file: fid,
            decl: di as u32,
            name: d.name.clone(),
            qualified_name: d.qualified_name.clone(),
            kind: d.kind,
            language: file.language,
            span: d.span,
            name_span: d.name_span,
            body_start: d.body_start,
            // Parents precede children (pre-order); anything else is ignored.
            parent: d
                .parent
                .filter(|&p| (p as usize) < di)
                .map(|p| SymbolId(file.first_symbol + p)),
            container: d.container.clone(),
            doc: d.doc.clone(),
            decorators: d.decorators.clone(),
            bases: d.bases.clone(),
            parameters: d
                .parameters
                .iter()
                .filter(|p| matches!(p.kind, ParamKind::Positional | ParamKind::KeywordOnly))
                .map(|p| p.name.clone())
                .collect(),
            execution: d.execution,
            is_stub: d.is_stub,
            is_test: d.is_test,
            declaration_lines: d.declaration_lines.clone(),
            semantic: file.semantic.is_some(),
        });
    }
    file.symbol_count = count;
}

#[cfg(test)]
#[path = "../../tests/unit/assemble/mod.rs"]
mod tests;
