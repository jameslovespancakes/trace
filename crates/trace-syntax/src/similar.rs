//! Normalized syntax fingerprints for SIMILAR CODE (port of codepath_v4/analysis.py
//! `fingerprint`/`similar`, language-generic).
//!
//! Fingerprint of a declaration span: pre-order walk of *named* nodes inside the span,
//! skipping comments and the leading docstring; each node contributes a token:
//! identifiers -> `"_"`, literals -> their node kind (e.g. `string`, `integer`), other nodes
//! -> their node kind; the declaration's own name, decorators and return annotation are
//! excluded. The fingerprint is the set of hashed 4-grams over this token sequence
//! (sorted, deduplicated `u64` = first 8 bytes of blake3 of the 4 tokens joined by `\0`).
//! Similarity = Jaccard(|A∩B| / |A∪B|).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use trace_core::model::ByteSpan;
use trace_core::Language;
use tree_sitter::Node;

use crate::grammar::grammar;
use crate::node::{named_children, nth_named};
use crate::spec::SyntaxSpec;

/// Sorted, deduplicated hashed 4-grams.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint(pub Vec<u64>);

impl Fingerprint {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Jaccard similarity of two sorted fingerprints (merge walk, no allocation).
pub fn jaccard(a: &Fingerprint, b: &Fingerprint) -> f64 {
    let (a, b) = (&a.0, &b.0);
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let (mut i, mut j, mut inter) = (0usize, 0usize, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                inter += 1;
                i += 1;
                j += 1;
            }
        }
    }
    inter as f64 / (a.len() + b.len() - inter) as f64
}

/// Maximum token count per declaration (bounds work on huge functions).
const MAX_TOKENS: usize = 200_000;

/// Fingerprints for several declaration spans of one file (single parse).
/// Entries are `None` when the span has no usable body.
pub fn fingerprints(language: Language, source: &[u8], spans: &[ByteSpan]) -> Vec<Option<Fingerprint>> {
    let Some(grammar) = grammar(language) else {
        return vec![None; spans.len()];
    };
    let Ok(tree) = crate::parse::parse(grammar, "", source) else {
        return vec![None; spans.len()];
    };
    let root = tree.root_node();
    spans
        .iter()
        .map(|span| fingerprint(grammar.spec, root, *span))
        .collect()
}

fn fingerprint(spec: &SyntaxSpec, root: Node<'_>, span: ByteSpan) -> Option<Fingerprint> {
    if span.is_empty() {
        return None;
    }
    let top = root.descendant_for_byte_range(span.start as usize, span.end as usize)?;
    let def = definition_node(top);
    let excluded = excluded_nodes(spec, top, def);
    let mut tokens: Vec<&'static str> = Vec::new();
    let mut stack = vec![top];
    while let Some(node) = stack.pop() {
        if tokens.len() >= MAX_TOKENS {
            break;
        }
        if node.is_extra() || spec.is_comment(node.kind()) || excluded.contains(&node.id()) {
            continue;
        }
        if node.start_byte() >= span.end as usize || node.end_byte() <= span.start as usize {
            continue;
        }
        let kind = node.kind();
        tokens.push(if spec.is_name_like(kind) { "_" } else { kind });
        let mut cursor = node.walk();
        let mut children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        children.reverse();
        stack.extend(children);
    }
    if tokens.len() < 4 {
        return None;
    }
    let mut grams: Vec<u64> = tokens
        .windows(4)
        .map(|w| {
            let mut hasher = blake3::Hasher::new();
            for (i, token) in w.iter().enumerate() {
                if i > 0 {
                    hasher.update(b"\0");
                }
                hasher.update(token.as_bytes());
            }
            let digest = hasher.finalize();
            let mut first = [0u8; 8];
            first.copy_from_slice(&digest.as_bytes()[..8]);
            u64::from_le_bytes(first)
        })
        .collect();
    grams.sort_unstable();
    grams.dedup();
    Some(Fingerprint(grams))
}

/// The declaration inside a wrapper (`decorated_definition`, `export_statement`, ...).
fn definition_node(top: Node<'_>) -> Node<'_> {
    top.child_by_field_name("definition")
        .or_else(|| top.child_by_field_name("declaration"))
        .unwrap_or(top)
}

/// Name, decorators, return annotation and leading docstring of the declaration.
fn excluded_nodes(spec: &SyntaxSpec, top: Node<'_>, def: Node<'_>) -> HashSet<usize> {
    let mut out = HashSet::new();
    for field in ["name", "return_type", "result"] {
        if let Some(n) = def.child_by_field_name(field) {
            out.insert(n.id());
        }
    }
    for holder in [top, def] {
        for child in named_children(holder) {
            let kind = child.kind();
            let annotation =
                kind.contains("annotation") && kind != "type_annotation" && holder.id() == def.id();
            if kind == "decorator" || spec.leading_attributes.contains(&kind) || annotation {
                out.insert(child.id());
            }
        }
    }
    // Leading docstring (Python): first body statement that is a lone string.
    if let Some(first) = def.child_by_field_name("body").and_then(|b| nth_named(b, 0)) {
        if first.kind() == "expression_statement" {
            let inner = named_children(first);
            if inner.len() == 1 && matches!(inner[0].kind(), "string" | "concatenated_string") {
                out.insert(first.id());
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "../tests/unit/similar.rs"]
mod tests;
