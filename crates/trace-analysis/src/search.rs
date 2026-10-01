//! Structured BM25 symbol search.
//!
//! Fields and weights: name 3.0, qualified name 1.5, file path (without extension) 0.8,
//! own docstring 1.0, enclosing class docstring 0.6, identifiers used in the body 0.35.
//! Light suffix folding (registration/register, entries/entry); IDF down-weights words that
//! appear everywhere. Classes x0.8, dunder methods x0.85. `.pyi` stubs and synthetic scopes
//! (`<module>`, `<lambda>`, `<genexpr>`) are not indexed.
//! Syntax facts only; no regex.

use std::collections::HashMap;

use trace_core::{Index, SymbolId};

pub(crate) const STOP: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "can", "code", "do", "does", "find", "for", "from",
    "function", "how", "i", "in", "is", "it", "me", "of", "on", "or", "show", "that", "the", "this", "to",
    "trace", "what", "when", "where", "which", "with", "would",
];

const SUFFIXES: &[&str] = &[
    "ations", "ation", "ings", "ing", "ions", "ion", "ies", "ers", "er", "ed", "es", "s",
];

/// Field order: name, qualified, file, doc, class_doc, declaration identifiers,
/// body-only identifiers. The last field is used only for explicit body searches.
pub(crate) const FIELD_WEIGHT: [f64; 7] = [3.0, 1.5, 0.8, 1.0, 0.6, 0.35, 0.35];
const FIELDS: usize = 7;

/// Split identifiers and prose into lowercase words (camelCase, acronyms, snake_case,
/// punctuation), dropping stop words.
pub fn tokens(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut word = String::new();
    let mut previous = '\0';
    for (i, &c) in chars.iter().enumerate() {
        if c.is_alphanumeric() {
            let acronym_boundary = c.is_uppercase()
                && previous.is_uppercase()
                && chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if c.is_uppercase() && (previous.is_lowercase() || acronym_boundary) && !word.is_empty() {
                out.push(word.to_lowercase());
                word.clear();
            }
            word.push(c);
        } else if !word.is_empty() {
            out.push(word.to_lowercase());
            word.clear();
        }
        previous = c;
    }
    if !word.is_empty() {
        out.push(word.to_lowercase());
    }
    out.retain(|w| !STOP.contains(&w.as_str()));
    out
}

/// Fold common English suffixes when at least 4 characters remain.
pub fn fold(word: &str) -> String {
    let len = word.chars().count();
    for suffix in SUFFIXES {
        if word.ends_with(suffix) && len - suffix.chars().count() >= 4 {
            let stem = &word[..word.len() - suffix.len()];
            return if *suffix == "ies" {
                format!("{stem}y")
            } else {
                stem.to_string()
            };
        }
    }
    word.to_string()
}

/// Folded search terms of a text (tokens longer than one character).
pub fn terms(text: &str) -> Vec<String> {
    tokens(text)
        .into_iter()
        .filter(|t| t.chars().count() > 1)
        .map(|t| fold(&t))
        .collect()
}

/// One search hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Hit {
    pub id: SymbolId,
    pub score: f64,
}

/// Inverted BM25 index over symbols.
pub(crate) struct SearchIndex {
    term_ids: HashMap<String, u32>,
    /// term -> postings (doc, field, tf).
    postings: Vec<Vec<(u32, u8, u32)>>,
    idf: Vec<f64>,
    /// doc -> field lengths.
    lengths: Vec<[u32; FIELDS]>,
    avg: [f64; FIELDS],
    /// doc -> (symbol, multiplier, uid for tie-breaks).
    docs: Vec<(SymbolId, f64, String)>,
}

impl SearchIndex {
    pub fn build(index: &Index) -> SearchIndex {
        let mut term_ids: HashMap<String, u32> = HashMap::new();
        let mut postings: Vec<Vec<(u32, u8, u32)>> = Vec::new();
        let mut df: Vec<u32> = Vec::new();
        let mut lengths = Vec::new();
        let mut docs = Vec::new();
        for s in &index.symbols {
            let path = &index.file(s.file).path;
            if s.is_synthetic() || (s.is_stub && trace_core::Language::is_python_stub_path(path)) {
                continue;
            }
            let parent_doc = s
                .parent
                .map(|p| index.symbol(p))
                .filter(|p| p.kind.is_type())
                .and_then(|p| p.doc.as_deref())
                .unwrap_or("");
            let stem = path.rsplit_once('.').map(|(a, _)| a).unwrap_or(path);
            let mut body: Vec<String> = Vec::new();
            let mut body_only: Vec<String> = Vec::new();
            if let Some(decl) = index
                .file(s.file)
                .facts
                .as_ref()
                .and_then(|f| f.declarations.get(s.decl as usize))
            {
                for (ident, count) in &decl.identifiers {
                    for t in terms(ident) {
                        for _ in 0..*count {
                            body.push(t.clone());
                        }
                    }
                }
            }
            if let Some(facts) = &index.file(s.file).facts {
                if let Some(decl) = facts.declarations.get(s.decl as usize) {
                    if let Some(identifiers) = facts.body_identifiers.get(&decl.body_start) {
                        for (ident, count) in identifiers {
                            for t in terms(ident) {
                                for _ in 0..*count {
                                    body_only.push(t.clone());
                                }
                            }
                        }
                    }
                }
            }
            let fields: [Vec<String>; FIELDS] = [
                terms(&s.name),
                terms(&s.qualified_name),
                terms(stem),
                terms(s.doc.as_deref().unwrap_or("")),
                terms(parent_doc),
                body,
                body_only,
            ];
            let doc = docs.len() as u32;
            let mut seen_in_doc: Vec<u32> = Vec::new();
            let mut lens = [0u32; FIELDS];
            for (f, words) in fields.iter().enumerate() {
                lens[f] = words.len() as u32;
                let mut counts: HashMap<&str, u32> = HashMap::new();
                for w in words {
                    *counts.entry(w.as_str()).or_insert(0) += 1;
                }
                for (w, tf) in counts {
                    let id = *term_ids.entry(w.to_string()).or_insert_with(|| {
                        postings.push(Vec::new());
                        df.push(0);
                        (postings.len() - 1) as u32
                    });
                    postings[id as usize].push((doc, f as u8, tf));
                    if !seen_in_doc.contains(&id) {
                        seen_in_doc.push(id);
                        df[id as usize] += 1;
                    }
                }
            }
            lengths.push(lens);
            let mut mult = 1.0;
            if s.kind.is_type() {
                mult *= 0.8;
            }
            if s.name.starts_with("__") && s.name.ends_with("__") {
                mult *= 0.85;
            }
            docs.push((s.id, mult, s.uid.clone()));
        }
        let n = docs.len().max(1) as f64;
        let idf = df
            .iter()
            .map(|&c| (1.0 + (n - c as f64 + 0.5) / (c as f64 + 0.5)).ln())
            .collect();
        let mut avg = [0f64; FIELDS];
        for (f, slot) in avg.iter_mut().enumerate() {
            let total: u64 = lengths.iter().map(|l: &[u32; FIELDS]| l[f] as u64).sum();
            let a = total as f64 / n;
            *slot = if a == 0.0 { 1.0 } else { a };
        }
        SearchIndex {
            term_ids,
            postings,
            idf,
            lengths,
            avg,
            docs,
        }
    }

    /// Top `limit` hits after skipping `offset`, ordered by (-score, uid).
    pub fn search(&self, question: &str, limit: usize, offset: usize) -> Vec<Hit> {
        self.search_where(question, limit, offset, |_| true)
    }

    /// Like [`SearchIndex::search`], restricted to symbols accepted by `keep` (the filter is
    /// applied before paging, so pages are stable for a given filter).
    pub(crate) fn search_where(
        &self,
        question: &str,
        limit: usize,
        offset: usize,
        keep: impl Fn(SymbolId) -> bool,
    ) -> Vec<Hit> {
        self.search_fields(question, limit, offset, keep, false)
    }

    /// Search only indexed body identifiers, not names, paths or documentation.
    pub(crate) fn search_body_where(
        &self,
        question: &str,
        limit: usize,
        offset: usize,
        keep: impl Fn(SymbolId) -> bool,
    ) -> Vec<Hit> {
        self.search_fields(question, limit, offset, keep, true)
    }

    fn search_fields(
        &self,
        question: &str,
        limit: usize,
        offset: usize,
        keep: impl Fn(SymbolId) -> bool,
        body_only: bool,
    ) -> Vec<Hit> {
        let mut q: Vec<u32> = terms(question)
            .iter()
            .filter_map(|t| self.term_ids.get(t).copied())
            .collect();
        q.sort_unstable();
        q.dedup();
        let mut scores: HashMap<u32, f64> = HashMap::new();
        for t in q {
            let idf = self.idf[t as usize];
            for &(doc, field, tf) in &self.postings[t as usize] {
                if (body_only && field != 6) || (!body_only && field == 6) {
                    continue;
                }
                let f = field as usize;
                let length = self.lengths[doc as usize][f] as f64 / self.avg[f];
                let tf = tf as f64;
                *scores.entry(doc).or_insert(0.0) +=
                    FIELD_WEIGHT[f] * idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * length));
            }
        }
        let mut ranked: Vec<(f64, u32)> = scores
            .into_iter()
            .map(|(doc, s)| (s * self.docs[doc as usize].1, doc))
            .filter(|(s, doc)| *s > 0.0 && keep(self.docs[*doc as usize].0))
            .collect();
        ranked.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| self.docs[a.1 as usize].2.cmp(&self.docs[b.1 as usize].2))
        });
        ranked
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|(score, doc)| Hit {
                id: self.docs[doc as usize].0,
                score: (score * 1000.0).round() / 1000.0,
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "../tests/unit/search.rs"]
mod tests;
