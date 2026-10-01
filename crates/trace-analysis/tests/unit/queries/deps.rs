use super::*;
use crate::test_support::{index, push_site, sym};
use trace_core::model::{Decision, SymbolKind};

fn f(id: u32, file: u32, path: &str, name: &str) -> trace_core::model::Symbol {
    sym(id, file, path, name, SymbolKind::Function, None, None)
}

/// I-14: a distance-1 result's evidence is the queried symbol's own edge, even when an
/// edge from another reached function into it comes first by id; the ordered edge list
/// starts with the reaching edges.
#[test]
fn rule_deps_result_evidence_is_the_reaching_edge() {
    // 0 start calls 1 (a) and 2 (b); a also calls b (edge 0 has the lowest id).
    let ix = index(
        &["a.go", "b.go"],
        vec![
            f(0, 0, "a.go", "start"),
            f(1, 0, "a.go", "a"),
            f(2, 1, "b.go", "b"),
            f(3, 1, "b.go", "c"),
        ],
        &[(1, 2), (0, 1), (0, 2), (2, 3)],
    );
    let graph = Graph::new(&ix);
    let reach = graph
        .reach(SymbolId(0), Direction::Forward, Tier::Inferred, &Bounds::default())
        .unwrap();
    let first = reaching_edges(&graph, &reach);
    let to_b = graph.edge(first[2].expect("b reached"));
    assert_eq!((to_b.from, to_b.to), (SymbolId(0), SymbolId(2)));
    let row = reached_row(&graph, SymbolId(2), 1, first[2]);
    assert_eq!(row.from.as_deref(), Some("a.go:start"));
    assert_eq!(row.at.as_ref().map(|a| (a.file.as_str(), a.line)), Some(("a.go", 1)));
    // Distance 2: reached from b.
    let row = reached_row(&graph, SymbolId(3), 2, first[3]);
    assert_eq!(row.from.as_deref(), Some("b.go:b"));
    // The reaching edges come first, every traversed edge once.
    let ordered = reaching_first([first[1], first[2], first[3]].into_iter().flatten(), &reach.edges);
    assert_eq!(ordered.len(), reach.edges.len());
    assert_eq!(ordered[1], first[2].unwrap());
    let e = graph.edge(ordered[1]);
    assert_eq!(e.from, SymbolId(0));
}

/// I-01: `deps` never says complete with an undecided dispatch at a call: the row of the
/// call is `undecided` and lists every implementation as a possible target next to the
/// proven declaration; a decided dispatch leaves the row alone.
#[test]
fn rule_deps_is_not_complete_with_undecided_dispatch() {
    let mut ix = index(
        &["core.rs", "sink.rs"],
        vec![
            f(0, 0, "core.rs", "Core.sink_matched"),
            f(1, 1, "sink.rs", "Sink.matched"),
            f(2, 1, "sink.rs", "JSON.matched"),
            f(3, 1, "sink.rs", "Standard.matched"),
        ],
        &[(0, 1)],
    );
    let site = push_site(&mut ix, "self.sink.matched", SiteCategory::Dispatch, 0, &[2, 3]) as usize;
    ix.sites[site].at = ix.edges[0].at;
    ix.sites[site].declared_target = Some(SymbolId(1));
    let owners: HashSet<SymbolId> = [SymbolId(0)].into_iter().collect();
    assert!(undecided(&ix, site));
    assert_eq!(undecided_call_keys(&ix, &owners).len(), 1);
    {
        let graph = Graph::new(&ix);
        let sources = SourceStore::new(&ix);
        let rows = call_rows(&graph, &sources, Tier::Inferred, SymbolId(0));
        assert_eq!(rows.len(), 1);
        assert!(rows[0].undecided);
        let targets: Vec<(&str, &str)> = rows[0].targets.iter().map(|t| (t.id.as_str(), t.tier)).collect();
        assert_eq!(
            targets,
            vec![
                ("sink.rs:Sink.matched", "proven"),
                ("sink.rs:JSON.matched", "possible"),
                ("sink.rs:Standard.matched", "possible")
            ]
        );
    }
    ix.decisions[site] = Decision {
        site: site as u32,
        status: DecisionStatus::Decided,
        targets: vec![SymbolId(2)],
        reason: None,
    };
    assert!(!undecided(&ix, site));
    let graph = Graph::new(&ix);
    let sources = SourceStore::new(&ix);
    let rows = call_rows(&graph, &sources, Tier::Inferred, SymbolId(0));
    assert!(!rows[0].undecided);
    assert_eq!(rows[0].targets.len(), 2, "the declaration and the decided implementation");
}
