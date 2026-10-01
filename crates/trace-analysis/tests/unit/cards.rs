use super::*;

#[test]
fn tiers_used_are_ordered_and_use_kinds_are_stable() {
    assert_eq!(tiers_used(["possible", "proven", "possible"]), vec!["proven", "possible"]);
    assert!(tiers_used([]).is_empty());
    assert_eq!(use_kind(EdgeKind::Calls), "call");
    assert_eq!(use_kind(EdgeKind::InferredCall), "call");
    assert_eq!(use_kind(EdgeKind::Writes), "write");
    assert_eq!(use_kind(EdgeKind::Imports), "import");
    assert_eq!(use_kind(EdgeKind::Reexports), "reexport");
    assert_eq!(use_kind(EdgeKind::PassesCallback), "callback");
    assert_eq!(use_kind(EdgeKind::Bridge), "bridge");
    assert_eq!(ref_use_kind(RefKind::Argument), "callback");
    assert_eq!(ref_use_kind(RefKind::Export), "reexport");
}

#[test]
fn executing_owner_is_the_innermost_executable() {
    let index = crate::test_support::project();
    // Session (class, 100..150) contains no executable; Session.login is 200..250.
    assert_eq!(executing_symbol_at(&index, FileId(1), 210), Some(SymbolId(2)));
    assert_eq!(executing_symbol_at(&index, FileId(1), 120), None);
}

#[test]
fn truncation_is_char_safe() {
    assert_eq!(truncate_chars("héllo", 2), "hé");
    assert_eq!(truncate_chars("ab", 5), "ab");
}

#[test]
fn first_edges_and_counts_follow_graph_rules() {
    use trace_core::model::{Decision, Site, SiteCategory, SiteId};
    let mut index = crate::test_support::project();
    // One undecided site owned by login_view with two candidates.
    index.sites.push(Site {
        id: SiteId("s".into()),
        category: SiteCategory::NoTarget,
        owner: SymbolId(0),
        declared_target: None,
        activation: trace_core::EdgeKind::Calls,
        at: index.edges[0].at,
        callee: "x.load".into(),
        candidates: vec![SymbolId(3), SymbolId(5)],
        flow_candidates: Vec::new(),
        field_only: Vec::new(),
        truncated_candidates: false,
        operation: None,
        argument: None,
        via: None,
        test_only: Vec::new(),
        receiver_exact: false,
        library: None,
        declared_library: None,
    });
    index.decisions.push(Decision {
        site: 0,
        status: DecisionStatus::Decided,
        targets: vec![SymbolId(5)],
        reason: None,
    });
    let counts = edge_counts(&index);
    let graph = Graph::new(&index);
    let g = graph.counts();
    assert_eq!((counts.proven, counts.inferred, counts.possible), (g.proven, g.inferred, g.possible));
    assert_eq!((counts.inferred, counts.possible), (1, 1));

    // Store.load is reached at distance 1 by the inferred edge and at distance 2 by the
    // proven chain; BFS distance 1 wins, so its first edge is the inferred one.
    let reach = graph
        .reach(SymbolId(0), trace_core::Direction::Forward, Tier::Inferred, &trace_core::Bounds::default())
        .unwrap();
    let first = first_edges(&graph, &reach);
    let e = graph.edge(first[5].unwrap());
    assert_eq!(e.tier, Tier::Inferred);
    let e = graph.edge(first[3].unwrap());
    assert_eq!((e.from, e.tier), (SymbolId(2), Tier::Proven));
    assert!(first[0].is_none());
}

#[test]
fn merged_bounds_union_hits() {
    let a = BoundsInfo {
        hit: vec!["depth"],
        complete: false,
        work: 3,
        depth: 4,
    };
    let b = BoundsInfo {
        hit: vec!["time"],
        complete: false,
        work: 2,
        depth: 4,
    };
    let m = merge_bounds(&[a, b], 4);
    assert_eq!(m.hit, vec!["depth", "time"]);
    assert!(!m.complete);
    assert_eq!(m.work, 5);
    assert!(merge_bounds(&[], 4).complete);
}
