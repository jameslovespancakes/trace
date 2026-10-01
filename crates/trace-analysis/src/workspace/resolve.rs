//! Index access and selector resolution: the loaded index, graph, sources and search index;
//! `resolve` / `resolve_scope` with suggestions for unknown selectors.

use trace_core::source::SourceStore;
use trace_core::{CoreError, Graph, Index, SymbolId};

use super::Workspace;
use crate::report::CandidateRef;
use crate::search::SearchIndex;
use crate::{AnalysisError, Result};

impl Workspace {
    /// The loaded index (error `NotIndexed` if read-only and absent).
    pub fn index(&self) -> Result<&Index> {
        self.index
            .as_ref()
            .ok_or_else(|| AnalysisError::NotIndexed(self.paths.root_display()))
    }

    /// Graph over the loaded index (bridge traversal as configured, `OpenOptions::no_bridges`).
    pub fn graph(&self) -> Result<Graph<'_>> {
        let mut graph = Graph::new(self.index()?);
        graph.set_bridges(self.bridges);
        Ok(graph)
    }

    /// Whether bridge edges are traversed.
    pub fn bridges_enabled(&self) -> bool {
        self.bridges
    }

    /// Hash-verified source access.
    pub fn sources(&self) -> Result<SourceStore<'_>> {
        Ok(SourceStore::new(self.index()?))
    }

    /// BM25 search index over the loaded index (built once per loaded index).
    pub(crate) fn search(&self) -> Result<&SearchIndex> {
        let index = self.index()?;
        Ok(self.search.get_or_init(|| SearchIndex::build(index)))
    }

    /// Up to 3 symbol names close to `reference` (search index), for `symbol_not_found`.
    fn suggestions(&self, reference: &str) -> Vec<String> {
        match (self.index(), self.search()) {
            (Ok(index), Ok(search)) => suggest(index, search, reference),
            _ => Vec::new(),
        }
    }

    /// Resolve a symbol reference (SPEC §9.4) with a warm uid table. An unknown name is
    /// `symbol_not_found` with up to 3 suggestions.
    pub fn resolve(&self, reference: &str) -> Result<SymbolId> {
        self.resolve_impl(reference, true)
    }

    /// Exact owner/name matching for audit retrieval; never discards a wrong qualifier.
    pub fn resolve_exact(&self, reference: &str) -> Result<SymbolId> {
        self.resolve_impl(reference, false)
    }

    fn resolve_impl(&self, reference: &str, relaxed_owner: bool) -> Result<SymbolId> {
        let index = self.index()?;
        let uids = self
            .uids
            .get_or_init(|| index.symbols.iter().map(|s| (s.uid.clone(), s.id)).collect());
        let found = match trace_core::resolve::resolve(index, reference, |uid| uids.get(uid).copied()) {
            Err(CoreError::SymbolNotFound(r)) if relaxed_owner => match relaxed(index, &r) {
                Relaxed::One(id) => Ok(id),
                Relaxed::Many(candidates) => Err(CoreError::AmbiguousSymbol {
                    reference: r,
                    candidates,
                }),
                Relaxed::None => Err(CoreError::SymbolNotFound(r)),
            },
            other => other,
        };
        match found {
            Ok(id) => Ok(id),
            Err(CoreError::SymbolNotFound(r)) => Err(AnalysisError::SymbolNotFound {
                suggestions: if r.trim().is_empty() {
                    Vec::new()
                } else {
                    self.suggestions(&r)
                },
                reference: r,
            }),
            Err(CoreError::AmbiguousSymbol {
                reference,
                candidates,
            }) => Err(AnalysisError::Ambiguous {
                reference,
                candidates: candidates
                    .iter()
                    .enumerate()
                    .map(|(i, uid)| {
                        let s = uids.get(uid).map(|&id| index.symbol(id));
                        CandidateRef {
                            n: i + 1,
                            id: uid.clone(),
                            kind: s.map_or("symbol", |s| s.kind.as_str()),
                            file: s.map_or_else(String::new, |s| index.file_path(s.file).to_string()),
                            line: s.map_or(0, |s| s.span.start_line),
                        }
                    })
                    .collect(),
            }),
            Err(e) => Err(e.into()),
        }
    }

    /// [`Workspace::resolve`] for traversal start/end points (`path`, `deps`): a
    /// `<file>:<line>` in code owned by no named symbol resolves to the innermost executing
    /// scope there (`<lambda>` / `<module>`, `trace_core::resolve::innermost_scope_at`)
    /// instead of failing. Every other reference behaves exactly like `resolve`.
    pub(crate) fn resolve_scope(&self, reference: &str) -> Result<SymbolId> {
        match self.resolve(reference) {
            Ok(id) => Ok(id),
            Err(e) => {
                let fallback = self
                    .index()
                    .ok()
                    .and_then(|index| trace_core::resolve::innermost_scope_at(index, reference));
                match (e.kind(), fallback) {
                    ("symbol_not_found", Some(id)) => Ok(id),
                    _ => Err(e),
                }
            }
        }
    }
}

/// Up to 3 names of real symbols the search index ranks for the last segment of
/// `reference` (`parse_config` -> `load_config`, `parse_args`), never the name itself.
/// Outcome of a lenient selector match ([`relaxed`]).
#[derive(Debug, PartialEq)]
pub enum Relaxed {
    One(SymbolId),
    /// Candidate uids (sorted).
    Many(Vec<String>),
    None,
}

/// Lenient match of a `file:Qualified.name` selector nothing matched exactly (a wrong or
/// extra qualifier inside the right file, `gin.go:Engine.redirectFixedPath` for the function
/// `redirectFixedPath` of `gin.go`): the last name segment among the named symbols of that
/// file (the file path or a path suffix), exact names first, then names equal ignoring case.
/// One match resolves; several are an ambiguity (numbered candidates). Selectors without a
/// file part and `file:line` selectors are never relaxed (rule 18: no guess across files;
/// their error suggests the exact-name matches, [`suggest`]).
pub(crate) fn relaxed(index: &Index, reference: &str) -> Relaxed {
    let reference = reference.trim();
    let (file, rest) = match reference.rsplit_once(':') {
        // `a::b` is a path separator, not a file prefix.
        Some((f, r)) if !f.ends_with(':') && !r.is_empty() && (f.contains('.') || f.contains('/')) => {
            (Some(f), r)
        }
        _ => return Relaxed::None,
    };
    if rest.trim().parse::<u32>().is_ok() {
        return Relaxed::None;
    }
    let wanted = rest.rsplit([':', '.']).next().unwrap_or(rest).trim();
    if wanted.is_empty() {
        return Relaxed::None;
    }
    let in_file = |s: &trace_core::model::Symbol| {
        file.is_none_or(|f| {
            let path = index.file_path(s.file);
            path == f || path.ends_with(&format!("/{f}"))
        })
    };
    for exact in [true, false] {
        let found: Vec<&trace_core::model::Symbol> = index
            .symbols
            .iter()
            .filter(|s| !s.is_synthetic() && in_file(s))
            .filter(|s| {
                if exact {
                    s.name == wanted
                } else {
                    s.name.eq_ignore_ascii_case(wanted)
                }
            })
            .collect();
        match found.len() {
            0 => continue,
            1 => return Relaxed::One(found[0].id),
            _ => {
                let mut uids: Vec<String> = found.iter().map(|s| s.uid.clone()).collect();
                uids.sort();
                uids.dedup();
                return Relaxed::Many(uids);
            }
        }
    }
    Relaxed::None
}

pub(crate) fn suggest(index: &Index, search: &SearchIndex, reference: &str) -> Vec<String> {
    let wanted = reference.rsplit([':', '.']).next().unwrap_or(reference).trim();
    if wanted.is_empty() {
        return Vec::new();
    }
    // Symbols named exactly like the selector's last segment first (by id: the selector's
    // qualifier or file was wrong), then close names from the search index.
    let mut out: Vec<String> = index
        .symbols
        .iter()
        .filter(|s| !s.is_synthetic() && s.name == wanted)
        .map(|s| s.uid.clone())
        .take(3)
        .collect();
    for hit in search.search(wanted, 20, 0) {
        if out.len() == 3 {
            break;
        }
        let s = index.symbol(hit.id);
        if s.is_synthetic() || s.name == wanted || out.contains(&s.name) {
            continue;
        }
        out.push(s.name.clone());
        if out.len() == 3 {
            break;
        }
    }
    out
}
