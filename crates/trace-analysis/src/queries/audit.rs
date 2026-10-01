//! Opt-in audit retrieval: complete sources, independent failures and exact-first discovery.
use super::show::{callers, calls};
use crate::cards::card;
use crate::report::{CandidateRef, Card, Envelope};
use crate::{AnalysisError, Result, Workspace};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use trace_core::{Index, SymbolId};

mod data;
use data::{Catalog, Selection};

#[derive(Clone, Debug, Serialize)]
pub struct SelectorError {
    pub requested: String,
    pub error_type: &'static str,
    pub error: String,
    pub suggestions: Vec<String>,
    pub candidates: Vec<CandidateRef>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DefinitionInfo {
    pub provenance: &'static str,
    pub callable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conditional: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_definitions: Option<usize>,
    pub notice: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct AuditItem {
    pub symbol: Card,
    pub source: String,
    /// Not applicable to data definitions, rather than a fabricated zero-call claim.
    pub callers: Option<usize>,
    pub calls: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<DefinitionInfo>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AuditShow {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub symbols: Vec<AuditItem>,
    pub errors: Vec<SelectorError>,
}

/// Canonical files win; only unambiguous suffixes normalize. Owners remain exact.
fn selector(index: &Index, input: &str) -> String {
    let normalized = input.trim().replace('\\', "/");
    let normalized = normalized.strip_prefix("./").unwrap_or(&normalized);
    let (file, tail) = normalized.rsplit_once(':').unwrap_or((normalized, "<module>"));
    if let Some(f) = index.files.iter().find(|f| f.path == file) {
        return format!("{}:{tail}", f.path);
    }
    let suffix = format!("/{file}");
    let mut matches = index.files.iter().filter(|f| f.path.ends_with(&suffix));
    match (matches.next(), matches.next()) {
        (Some(f), None) => format!("{}:{tail}", f.path),
        _ => normalized.to_string(),
    }
}

fn error(requested: &str, e: AnalysisError) -> SelectorError {
    let suggestions = match &e {
        AnalysisError::SymbolNotFound { suggestions, .. } => suggestions.clone(),
        _ => Vec::new(),
    };
    let candidates = match &e {
        AnalysisError::Ambiguous { candidates, .. } => candidates.clone(),
        _ => Vec::new(),
    };
    SelectorError {
        requested: requested.into(),
        error_type: e.kind(),
        error: e.to_string(),
        suggestions,
        candidates,
    }
}

pub fn show(ws: &mut Workspace, requested: &[String]) -> Result<AuditShow> {
    // Retain paths, not IDs/references, across lazy setup which may replace the index.
    let files = {
        let index = ws.index()?;
        let data = Catalog::new(index);
        let mut seen = HashSet::new();
        let mut files = Vec::new();
        for input in requested {
            match data.resolve(ws, &selector(index, input)) {
                Ok(selected) => {
                    let path = index.file_path(data.file(index, selected)).to_string();
                    if seen.insert(path.clone()) {
                        files.push(path);
                    }
                }
                Err(e) if matches!(e.kind(), "symbol_not_found" | "ambiguous_symbol") => {}
                Err(e) => return Err(e),
            }
        }
        files
    };
    for path in files {
        if let Some(file) = ws.index()?.file_by_path(&path) {
            ws.ensure_ready(&[file])?;
        }
    }
    let graph = ws.graph()?;
    let sources = ws.sources()?;
    let data = Catalog::new(graph.index);
    let mut errors = Vec::new();
    let mut symbols = Vec::new();
    let mut seen = HashSet::new();
    for input in requested {
        let selected = match data.resolve(ws, &selector(graph.index, input)) {
            Ok(selected) => selected,
            Err(e) if matches!(e.kind(), "symbol_not_found" | "ambiguous_symbol") => {
                errors.push(error(input, e));
                continue;
            }
            Err(e) => return Err(e),
        };
        let item = match selected {
            Selection::Symbol(id) => {
                let s = graph.index.symbol(id);
                if !seen.insert(s.uid.clone()) {
                    continue;
                }
                AuditItem {
                    symbol: card(graph.index, s),
                    source: sources.text(s.file, s.span.bytes)?,
                    callers: Some(callers(&graph, ws.include, id)),
                    calls: Some(calls(&graph, ws.include, id)),
                    definition: None,
                }
            }
            Selection::Data(i) => {
                let r = &data.rows[i];
                if !seen.insert(r.id.clone()) {
                    continue;
                }
                AuditItem {
                    symbol: r.card(graph.index),
                    source: sources.text(r.file, r.definition.span.bytes)?,
                    callers: None,
                    calls: None,
                    definition: Some(DefinitionInfo {
                        provenance: "syntax",
                        callable: false,
                        conditional: (r.kind == "data").then_some(r.definition.conditional),
                        binding_definitions: (r.kind == "data").then_some(r.binding_count),
                        notice: if r.kind == "test_block" {
                            "Syntax-discovered test block; not a callable definition or verified test collection/coverage."
                        } else {
                            "Source binding only; current runtime binding and value are not inferred. Imports and uses are not definitions."
                        },
                    }),
                }
            }
        };
        symbols.push(item);
    }
    Ok(AuditShow {
        envelope: ws.envelope("show"),
        symbols,
        errors,
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct SymbolMatch {
    pub id: String,
    pub kind: &'static str,
    pub file: String,
    pub line: u32,
    pub end_line: u32,
    pub is_test: bool,
    pub semantic: bool,
    /// Name/scope/framework heuristic, not a guarantee of test collection or coverage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_role: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Discovery {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub matches: Vec<SymbolMatch>,
    pub total: usize,
    pub eligible_symbols: usize,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub match_mode: &'static str,
    pub notice: &'static str,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiscoveryMode {
    #[default]
    Auto,
    Name,
    Body,
}

// Preserve access to helpers and other languages; only ranking and explicit role labels
// use this conservative Python convention. Nested functions are not collected test cases.
fn is_test_code(index: &Index, id: SymbolId) -> bool {
    if super::sites::is_test_code(index, id) {
        return true;
    }
    let s = index.symbol(id);
    let Some(facts) = &index.file(s.file).facts else {
        return false;
    };
    let mut scope = Some(id);
    while let Some(id) = scope {
        let s = index.symbol(id);
        if s.kind == trace_core::SymbolKind::Module {
            break;
        }
        if facts
            .tests
            .iter()
            .any(|t| t.span.encloses(s.span.bytes) || s.span.bytes.encloses(t.span))
        {
            return true;
        }
        scope = s.parent;
    }
    false
}

fn test_role(index: &Index, id: SymbolId) -> Option<&'static str> {
    let s = index.symbol(id);
    if !is_test_code(index, id) {
        return None;
    }
    if index.file(s.file).language != trace_core::Language::Python {
        return Some("test_code");
    }
    let mut parent = s.parent;
    while let Some(id) = parent {
        let owner = index.symbol(id);
        if owner.kind.is_callable() {
            return Some("helper_candidate");
        }
        parent = owner.parent;
    }
    if s.kind.is_callable() && (s.name == "test" || s.name.starts_with("test_")) {
        Some("case_candidate")
    } else if s.kind.is_type() {
        Some("container")
    } else {
        Some("helper_candidate")
    }
}

fn role_rank(role: Option<&str>) -> u8 {
    match role {
        Some("case_candidate" | "block_candidate") => 0,
        Some("helper_candidate") => 2,
        _ => 1,
    }
}

struct Entry {
    view: SymbolMatch,
    name: String,
    qualified: String,
    symbol: Option<SymbolId>,
    body: Vec<String>,
}

/// Default discovery. Known unambiguous names can go straight to batched `show`.
pub fn symbols(
    ws: &Workspace,
    query: Option<&str>,
    file: Option<&str>,
    tests: bool,
    limit: usize,
    offset: usize,
) -> Result<Discovery> {
    symbols_in_mode(ws, query, file, tests, DiscoveryMode::Auto, limit, offset)
}

pub fn symbols_in_mode(
    ws: &Workspace,
    query: Option<&str>,
    file: Option<&str>,
    tests: bool,
    requested_mode: DiscoveryMode,
    limit: usize,
    offset: usize,
) -> Result<Discovery> {
    if requested_mode == DiscoveryMode::Body && query.is_none_or(|q| q.trim().is_empty()) {
        return Err(AnalysisError::InvalidArgument("body mode requires a query".into()));
    }
    if !(1..=100).contains(&limit) {
        return Err(AnalysisError::InvalidArgument("limit must be between 1 and 100".into()));
    }
    let index = ws.index()?;
    let data = Catalog::new(index);
    let mut entries: Vec<Entry> = index
        .symbols
        .iter()
        .filter(|s| !s.is_synthetic())
        .map(|s| Entry {
            view: SymbolMatch {
                id: s.uid.clone(),
                kind: s.kind.as_str(),
                file: index.file_path(s.file).into(),
                line: s.span.start_line,
                end_line: s.span.end_line,
                is_test: is_test_code(index, s.id),
                semantic: s.semantic,
                test_role: test_role(index, s.id),
                label: None,
            },
            name: s.name.clone(),
            qualified: s.qualified_name.clone(),
            symbol: Some(s.id),
            body: Vec::new(),
        })
        .collect();
    entries.extend(data.rows.iter().map(|r| Entry {
        view: SymbolMatch {
            id: r.id.clone(),
            kind: r.kind,
            file: index.file_path(r.file).into(),
            line: r.definition.span.start_line,
            end_line: r.definition.span.end_line,
            is_test: r.is_test,
            semantic: false,
            test_role: r.is_test.then_some(if r.kind == "test_block" {
                "block_candidate"
            } else {
                "data"
            }),
            label: r.label.map(str::to_owned),
        },
        name: r.label.unwrap_or(&r.definition.name).to_owned(),
        qualified: r.label.unwrap_or(&r.definition.name).to_owned(),
        symbol: None,
        body: r.body.iter().flat_map(|s| crate::search::terms(s)).collect(),
    }));
    entries.retain(|e| (!tests || e.view.is_test) && file.is_none_or(|f| e.view.file.contains(f)));
    let eligible_symbols = entries.len();
    let mode = if let Some(q) = query.map(str::trim).filter(|q| !q.is_empty()) {
        let exact = |e: &Entry| e.name == q || e.qualified == q || e.view.id == q;
        let exact_hits: Vec<_> = entries.iter().filter(|e| exact(e)).collect();
        let explicit_helper = exact_hits
            .iter()
            .any(|e| e.view.id == q || (e.qualified == q && e.qualified != e.name));
        let helper_only = !exact_hits.is_empty()
            && exact_hits
                .iter()
                .all(|e| e.view.test_role == Some("helper_candidate"));
        let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
        let name_match = |e: &Entry| {
            let name = e.qualified.to_lowercase();
            words.iter().all(|word| name.contains(word))
        };
        if requested_mode != DiscoveryMode::Body
            && !exact_hits.is_empty()
            && (requested_mode == DiscoveryMode::Name || !tests || !helper_only || explicit_helper)
        {
            entries.retain(exact);
            "exact"
        } else if requested_mode == DiscoveryMode::Name
            || (requested_mode == DiscoveryMode::Auto && !tests && entries.iter().any(name_match))
        {
            entries.retain(name_match);
            "name"
        } else {
            let allowed: HashSet<_> = entries.iter().filter_map(|e| e.symbol).collect();
            let search = ws.search()?;
            let hits = if requested_mode == DiscoveryMode::Body {
                search.search_body_where(q, index.symbols.len(), 0, |id| allowed.contains(&id))
            } else {
                search.search_where(q, index.symbols.len(), 0, |id| allowed.contains(&id))
            };
            let ranks: HashMap<_, _> = hits.into_iter().enumerate().map(|(i, h)| (h.id, i)).collect();
            let terms = crate::search::terms(q);
            entries.retain(|e| {
                e.symbol.is_some_and(|id| ranks.contains_key(&id))
                    || (e.view.kind == "test_block"
                        && terms.iter().any(|t| {
                            e.body.contains(t)
                                || (requested_mode == DiscoveryMode::Auto
                                    && crate::search::terms(&e.name).contains(t))
                        }))
                    || (requested_mode == DiscoveryMode::Auto && tests && name_match(e))
            });
            entries.sort_by_key(|e| {
                (
                    if tests { role_rank(e.view.test_role) } else { 0 },
                    e.symbol.and_then(|id| ranks.get(&id).copied()).unwrap_or(usize::MAX),
                    e.view.id.clone(),
                )
            });
            if requested_mode == DiscoveryMode::Body {
                "body"
            } else if tests {
                "test_ranked"
            } else {
                "broad"
            }
        }
    } else {
        "all"
    };
    if matches!(mode, "exact" | "name" | "all") {
        entries.sort_by(|a, b| {
            let roles = if tests && requested_mode == DiscoveryMode::Auto {
                role_rank(a.view.test_role).cmp(&role_rank(b.view.test_role))
            } else {
                std::cmp::Ordering::Equal
            };
            roles.then_with(|| a.view.id.cmp(&b.view.id))
        });
    }
    let total = entries.len();
    let matches: Vec<_> = entries.into_iter().skip(offset).take(limit).map(|e| e.view).collect();
    let next = offset.saturating_add(matches.len());
    Ok(Discovery {
        envelope: ws.envelope("symbols"), matches, total, eligible_symbols, offset,
        next_offset: (next < total).then_some(next), match_mode: mode,
        notice: "Auto prefers exact identifiers; conceptual --tests queries retain broader doc/body matches and rank Python case candidates ahead of helpers. Test roles are name/scope heuristics, not verified collection or coverage. Name mode searches names only; body mode searches indexed body identifiers, not literals, docs or data initializers. Data definitions are Python module assignments and Go package bindings, not runtime values. Test blocks have syntax-only source-position identities, not invented callable names. If discovery misses a literal or scope, use bounded search/source. Name mode matches all query words within one name; use batched Show for independent names. Show accepts unambiguous bare/qualified names and returns canonical ids with complete source.",
    })
}
