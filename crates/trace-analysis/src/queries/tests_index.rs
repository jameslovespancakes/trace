//! Relevant tests: tests that mention the target or an
//! affected caller by name. Test entries: indexed symbols with `is_test` plus syntax
//! `TestBlock`s; mentions = identifiers in their bodies. Names: the target (distance 0) and
//! reverse-reachable symbols within 4 steps at `include` (distance 1); dunder names ignored.
//! Output `test = "<path>::<qualified or block name>"`, `directness = direct | via_caller`,
//! sorted direct first then by test id. Relevance evidence, not coverage.
//!
//! A test symbol's own name never counts as a mention of itself.

use std::collections::{BTreeMap, HashMap};

use trace_core::{Bounds, Direction, Graph, Index, SymbolId, Tier};

use crate::report::TestRow;
use crate::Result;

/// Reverse depth for "affected caller" names.
pub(crate) const CALLER_DEPTH: u32 = 4;

/// Tests guarding `targets` (module docs) over an existing graph, at most `limit`.
pub(crate) fn tests_with_graph(
    graph: &Graph<'_>,
    include: Tier,
    targets: &[SymbolId],
    limit: usize,
) -> Result<Vec<TestRow>> {
    let index = graph.index;
    let names = mention_names(graph, include, targets)?;
    if names.is_empty() {
        return Ok(Vec::new());
    }
    // test id -> row (dedupe, keep the most direct mention).
    let mut rows: BTreeMap<String, (u8, TestRow)> = BTreeMap::new();
    let mut offer = |distance: u8, row: TestRow| match rows.get(&row.test) {
        Some((d, _)) if *d <= distance => {}
        _ => {
            rows.insert(row.test.clone(), (distance, row));
        }
    };
    for s in index.symbols.iter().filter(|s| s.is_test) {
        if targets.contains(&s.id) {
            continue;
        }
        let file = index.file(s.file);
        let Some(decl) = file.facts.as_ref().and_then(|f| f.declarations.get(s.decl as usize)) else {
            continue;
        };
        let hit = decl
            .identifiers
            .iter()
            .filter(|(ident, _)| *ident != s.name)
            .filter_map(|(ident, _)| names.get(ident.as_str()).map(|&d| (d, ident.as_str())))
            .min();
        if let Some((distance, via)) = hit {
            offer(distance, row(&file.path, &s.qualified_name, s.span.start_line, via, distance));
        }
    }
    for file in &index.files {
        let Some(facts) = &file.facts else { continue };
        for block in &facts.tests {
            let hit = block
                .mentions
                .iter()
                .filter_map(|m| names.get(m.as_str()).map(|&d| (d, m.as_str())))
                .min();
            if let Some((distance, via)) = hit {
                offer(distance, row(&file.path, &block.name, block.line, via, distance));
            }
        }
    }
    let mut out: Vec<(u8, TestRow)> = rows.into_values().collect();
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.test.cmp(&b.1.test)));
    out.truncate(limit);
    Ok(out.into_iter().map(|(_, r)| r).collect())
}

/// Names to look for: targets (0) and callers within [`CALLER_DEPTH`] reverse steps (1).
fn mention_names<'i>(graph: &Graph<'i>, include: Tier, targets: &[SymbolId]) -> Result<HashMap<&'i str, u8>> {
    let index: &'i Index = graph.index;
    let mut names: HashMap<&'i str, u8> = HashMap::new();
    let mut put = |id: SymbolId, distance: u8| {
        let name = index.symbol(id).name.as_str();
        if name.starts_with("__") || name.starts_with('<') || name.is_empty() {
            return;
        }
        let slot = names.entry(name).or_insert(distance);
        *slot = (*slot).min(distance);
    };
    let bounds = Bounds::default().with_depth(CALLER_DEPTH);
    for &t in targets {
        put(t, 0);
        let reach = graph.reach(t, Direction::Reverse, include, &bounds)?;
        for &(id, d) in &reach.nodes {
            if d > 0 && !index.symbol(id).is_test {
                put(id, 1);
            }
        }
    }
    Ok(names)
}

fn row(path: &str, name: &str, line: u32, via: &str, distance: u8) -> TestRow {
    TestRow {
        test: format!("{path}::{name}"),
        file: path.to_string(),
        line,
        mentions: via.to_string(),
        directness: if distance == 0 { "direct" } else { "via_caller" },
    }
}

#[cfg(test)]
#[path = "../../tests/unit/queries/tests_index.rs"]
mod tests;
