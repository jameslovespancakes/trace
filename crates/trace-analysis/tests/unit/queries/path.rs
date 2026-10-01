use super::*;
use crate::test_support::{index, push_site, sym};
use trace_core::model::SiteCategory;
use trace_core::model::SymbolKind;

fn f(id: u32, file: u32, path: &str, name: &str) -> trace_core::model::Symbol {
    sym(id, file, path, name, SymbolKind::Function, None, None)
}

/// A path that exists only through `possible` edges: the default view finds none, the
/// `possible` view finds it (`path` then reports `possible_path` instead of `complete`).
#[test]
fn rule_path_through_possible_edges_is_reported_below_the_view() {
    let mut ix = index(
        &["web.ts", "api.py"],
        vec![
            f(0, 0, "web.ts", "load"),
            f(1, 0, "web.ts", "fetchItems"),
            f(2, 1, "api.py", "read_items"),
        ],
        &[(0, 1)],
    );
    push_site(&mut ix, "client.get", SiteCategory::NoTarget, 1, &[2]);
    let graph = Graph::new(&ix);
    let bounds = Bounds::default();
    assert!(graph
        .shortest_path(SymbolId(0), SymbolId(2), Tier::Inferred, &bounds)
        .unwrap()
        .paths
        .is_empty());
    assert!(!graph
        .shortest_path(SymbolId(0), SymbolId(2), Tier::Possible, &bounds)
        .unwrap()
        .paths
        .is_empty());
}

/// I-01: no path in the default view, but an undecided site on the way has a candidate
/// that reaches the target: `path` reports it instead of `complete`; the `possible` view
/// traverses it (nothing to report).
#[test]
fn rule_path_reports_undecided_frontier_instead_of_complete() {
    let mut ix = index(
        &["main.rs", "replace.rs"],
        vec![
            f(0, 0, "main.rs", "main"),
            f(1, 0, "main.rs", "run"),
            f(2, 1, "replace.rs", "Replacer.replace"),
            f(3, 1, "replace.rs", "Replacer.clear"),
            f(4, 1, "replace.rs", "Other.replace"),
        ],
        &[(0, 1), (2, 3)],
    );
    push_site(&mut ix, "sink.replace", SiteCategory::Dispatch, 1, &[2, 4]);
    let graph = Graph::new(&ix);
    assert_eq!(undecided_between(&graph, Tier::Inferred, SymbolId(0), SymbolId(3), 64), 1);
    assert_eq!(undecided_between(&graph, Tier::Possible, SymbolId(0), SymbolId(3), 64), 0);
    // A target no candidate reaches: nothing undecided on the way.
    assert_eq!(undecided_between(&graph, Tier::Inferred, SymbolId(0), SymbolId(0), 64), 0);
}
