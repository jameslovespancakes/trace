//! Retrieval-only data catalog: no additions to the call graph or its name resolver.
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use trace_core::facts::DataDefinition;
use trace_core::{FileId, Index, SymbolId};

use crate::report::{CandidateRef, Card};
use crate::{AnalysisError, Result, Workspace};

#[derive(Clone, Copy)]
pub(super) enum Selection {
    Symbol(SymbolId),
    Data(usize),
}

pub(super) struct DataRow<'a> {
    pub id: String,
    pub file: FileId,
    pub definition: Cow<'a, DataDefinition>,
    pub binding_count: usize,
    pub is_test: bool,
    pub kind: &'static str,
    pub label: Option<&'a str>,
    pub body: &'a [String],
}

impl DataRow<'_> {
    pub fn card(&self, index: &Index) -> Card {
        let file = index.file(self.file);
        let d = &self.definition;
        Card {
            id: self.id.clone(),
            name: d.name.clone(),
            qualified_name: d.name.clone(),
            file: file.path.clone(),
            kind: self.kind,
            language: file.language,
            line: d.span.start_line,
            end_line: d.span.end_line,
            start_byte: d.span.bytes.start,
            end_byte: d.span.bytes.end,
            summary: self.label.unwrap_or_default().into(),
            semantic: false,
        }
    }
}

pub(super) struct Catalog<'a> {
    pub rows: Vec<DataRow<'a>>,
    exact: HashMap<String, usize>,
}

impl<'a> Catalog<'a> {
    pub fn new(index: &'a Index) -> Self {
        let mut used: HashSet<String> = index.symbols.iter().map(|s| s.uid.clone()).collect();
        let mut rows = Vec::new();
        for (i, file) in index.files.iter().enumerate() {
            let Some(facts) = &file.facts else { continue };
            let fid = FileId(i as u32);
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for d in &facts.data_definitions {
                *counts.entry(&d.name).or_default() += 1;
            }
            for s in index.symbols_of(fid).iter().filter(|s| !s.is_synthetic()) {
                *counts.entry(&s.qualified_name).or_default() += 1;
            }
            let is_test = trace_syntax::is_test_path(&file.path, file.language, &Default::default());
            let mut ordinals: HashMap<&str, usize> = HashMap::new();
            for definition in &facts.data_definitions {
                let base = format!("{}:{}", file.path, definition.name);
                let ordinal = ordinals.entry(&definition.name).or_insert(1);
                let mut id = if *ordinal == 1 {
                    base.clone()
                } else {
                    format!("{base}#{ordinal}")
                };
                while !used.insert(id.clone()) {
                    *ordinal += 1;
                    id = format!("{base}#{ordinal}");
                }
                *ordinal += 1;
                rows.push(DataRow {
                    id,
                    file: fid,
                    definition: Cow::Borrowed(definition),
                    binding_count: counts[definition.name.as_str()],
                    is_test,
                    kind: "data",
                    label: None,
                    body: &[],
                });
            }
            for block in &facts.tests {
                // Source-position identity, not an invented callable/test-runner name.
                let name = format!("<test@{}:{}>", block.line, block.span.start);
                let id = format!("{}:{name}", file.path);
                if !used.insert(id.clone()) {
                    continue;
                } // duplicate query captures
                rows.push(DataRow {
                    id,
                    file: fid,
                    definition: Cow::Owned(DataDefinition {
                        name,
                        name_span: block.span,
                        span: trace_core::Span {
                            bytes: block.span,
                            start_line: block.line,
                            end_line: block.end_line,
                        },
                        conditional: false,
                    }),
                    binding_count: 1,
                    is_test: true,
                    kind: "test_block",
                    label: Some(&block.name),
                    body: &block.mentions,
                });
            }
        }
        let exact = rows.iter().enumerate().map(|(i, r)| (r.id.clone(), i)).collect();
        Self { rows, exact }
    }

    pub fn file(&self, index: &Index, selection: Selection) -> FileId {
        match selection {
            Selection::Symbol(id) => index.symbol(id).file,
            Selection::Data(i) => self.rows[i].file,
        }
    }

    pub fn resolve(&self, ws: &Workspace, reference: &str) -> Result<Selection> {
        // Exact IDs select source occurrences, never a guessed current runtime value.
        if let Some(&i) = self.exact.get(reference) {
            return Ok(Selection::Data(i));
        }
        let index = ws.index()?;
        let file_line = reference
            .rsplit_once(':')
            .and_then(|(p, n)| Some((index.file_by_path(p)?, n.parse::<u32>().ok()?)));
        let matches: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                let d = &r.definition;
                let hit = if let Some((file, line)) = file_line {
                    r.file == file && d.span.start_line <= line && line <= d.span.end_line
                } else {
                    // Module data has no lexical owner qualifier. Never drop a wrong owner or
                    // pretend an import alias declares a value in the importing file.
                    reference == d.name
                };
                hit.then_some(i)
            })
            .collect();
        let symbol = ws.resolve_exact(reference);
        if matches.is_empty() {
            return symbol.map(Selection::Symbol);
        }
        let mut candidates = match symbol {
            Ok(id) => {
                let s = index.symbol(id);
                vec![CandidateRef {
                    n: 0,
                    id: s.uid.clone(),
                    kind: s.kind.as_str(),
                    file: index.file_path(s.file).into(),
                    line: s.span.start_line,
                }]
            }
            Err(AnalysisError::Ambiguous { candidates, .. }) => candidates,
            Err(e) if e.kind() == "symbol_not_found" => Vec::new(),
            Err(e) => return Err(e),
        };
        if matches.len() == 1 && candidates.is_empty() {
            return Ok(Selection::Data(matches[0]));
        }
        candidates.extend(matches.into_iter().map(|i| {
            let r = &self.rows[i];
            CandidateRef {
                n: 0,
                id: r.id.clone(),
                kind: r.kind,
                file: index.file_path(r.file).into(),
                line: r.definition.span.start_line,
            }
        }));
        candidates.sort_by(|a, b| a.id.cmp(&b.id));
        candidates.dedup_by(|a, b| a.id == b.id);
        for (i, c) in candidates.iter_mut().enumerate() {
            c.n = i + 1;
        }
        Err(AnalysisError::Ambiguous {
            reference: reference.into(),
            candidates,
        })
    }
}
