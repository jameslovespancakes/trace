use super::*;
use crate::model::EdgeKind;
use crate::test_support::index::{decision, index_with, site};

fn names(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("f{i}")).collect()
}

fn chain_index(n: usize) -> crate::Index {
    let names = names(n);
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let calls: Vec<(u32, u32)> = (0..n as u32 - 1).map(|i| (i, i + 1)).collect();
    index_with(&refs, &calls)
}

#[test]
fn reach_reports_distances_and_depth_bound() {
    let index = chain_index(6);
    let g = Graph::new(&index);
    let full = g
        .reach(SymbolId(0), Direction::Forward, Tier::Proven, &Bounds::default())
        .unwrap();
    assert!(full.complete());
    assert_eq!(full.nodes.len(), 6);
    assert_eq!(full.nodes[5], (SymbolId(5), 5));
    assert_eq!(full.edges.len(), 5);

    let shallow = g
        .reach(SymbolId(0), Direction::Forward, Tier::Proven, &Bounds::default().with_depth(2))
        .unwrap();
    assert_eq!(shallow.nodes.len(), 3);
    assert!(shallow.hit.depth);
    assert!(!shallow.complete());
    assert_eq!(shallow.hit.names(), vec!["depth"]);

    let reverse = g
        .reach(SymbolId(5), Direction::Reverse, Tier::Proven, &Bounds::default())
        .unwrap();
    assert_eq!(reverse.nodes.len(), 6);
}

#[test]
fn reach_depth_not_hit_when_frontier_already_seen() {
    // 0 -> 1 -> 0 cycle with depth 1: the edge back to 0 does not reveal anything new.
    let index = index_with(&["a", "b"], &[(0, 1), (1, 0)]);
    let g = Graph::new(&index);
    let r = g
        .reach(SymbolId(0), Direction::Forward, Tier::Proven, &Bounds::default().with_depth(1))
        .unwrap();
    assert!(r.complete());
    assert_eq!(r.nodes.len(), 2);
}

#[test]
fn work_and_time_bounds_are_flagged() {
    let index = chain_index(5000);
    let g = Graph::new(&index);
    let work = Bounds {
        max_work: 10,
        max_depth: 10_000,
        ..Bounds::default()
    };
    let r = g.reach(SymbolId(0), Direction::Forward, Tier::Proven, &work).unwrap();
    assert!(r.hit.work);
    assert_eq!(r.work, 10);
    assert!(!r.complete());

    let time = Bounds {
        timeout: Duration::from_nanos(1),
        max_depth: 10_000,
        max_work: 1_000_000,
        ..Bounds::default()
    };
    let r = g.reach(SymbolId(0), Direction::Forward, Tier::Proven, &time).unwrap();
    assert!(r.hit.time);
    assert!(!r.complete());
}

#[test]
fn invalid_bounds_and_ids_are_rejected() {
    let index = chain_index(3);
    let g = Graph::new(&index);
    let zero = Bounds {
        max_work: 0,
        ..Bounds::default()
    };
    assert!(g.reach(SymbolId(0), Direction::Forward, Tier::Proven, &zero).is_err());
    assert!(g
        .reach(SymbolId(9), Direction::Forward, Tier::Proven, &Bounds::default())
        .is_err());
    assert!(g
        .reach_many(&[], Direction::Forward, Tier::Proven, &Bounds::default())
        .is_err());
}

#[test]
fn reach_many_starts_at_distance_zero() {
    let index = chain_index(5);
    let g = Graph::new(&index);
    let r = g
        .reach_many(
            &[SymbolId(4), SymbolId(2), SymbolId(4)],
            Direction::Reverse,
            Tier::Proven,
            &Bounds::default(),
        )
        .unwrap();
    assert_eq!(&r.nodes[..2], &[(SymbolId(4), 0), (SymbolId(2), 0)]);
    assert_eq!(r.nodes.len(), 5);
    assert_eq!(r.start, SymbolId(4));
}

#[test]
fn shortest_path_respects_tiers() {
    // 0 -> 1 proven; 1 -> 2 only via an inferred decision.
    let mut index = index_with(&["a", "b", "c"], &[(0, 1)]);
    index.sites.push(site(1, &[2]));
    index.decisions.push(decision(0, &[2]));
    let g = Graph::new(&index);
    let proven = g
        .shortest_path(SymbolId(0), SymbolId(2), Tier::Proven, &Bounds::default())
        .unwrap();
    assert!(!proven.found());
    assert!(proven.complete());
    let inferred = g
        .shortest_path(SymbolId(0), SymbolId(2), Tier::Inferred, &Bounds::default())
        .unwrap();
    assert!(inferred.found());
    let p = &inferred.paths[0];
    assert_eq!(p.nodes, vec![SymbolId(0), SymbolId(1), SymbolId(2)]);
    assert_eq!(g.edge(p.edges[1]).kind, EdgeKind::InferredCall);

    let same = g
        .shortest_path(SymbolId(1), SymbolId(1), Tier::Proven, &Bounds::default())
        .unwrap();
    assert_eq!(same.paths[0].nodes, vec![SymbolId(1)]);
    assert!(same.paths[0].edges.is_empty());
}

#[test]
fn shortest_path_depth_bound() {
    let index = chain_index(6);
    let g = Graph::new(&index);
    let r = g
        .shortest_path(SymbolId(0), SymbolId(5), Tier::Proven, &Bounds::default().with_depth(3))
        .unwrap();
    assert!(!r.found());
    assert!(r.hit.depth);
}

#[test]
fn all_paths_are_simple_and_bounded() {
    // Diamond with a cycle: 0->1, 0->2, 1->3, 2->3, 3->0, 1->2.
    let index = index_with(&["a", "b", "c", "d"], &[(0, 1), (0, 2), (1, 3), (2, 3), (3, 0), (1, 2)]);
    let g = Graph::new(&index);
    let r = g
        .all_paths(SymbolId(0), SymbolId(3), Tier::Proven, &Bounds::default())
        .unwrap();
    assert!(r.complete());
    assert_eq!(r.paths.len(), 3);
    // Shortest first.
    assert!(r.paths.windows(2).all(|w| w[0].edges.len() <= w[1].edges.len()));
    for p in &r.paths {
        let mut seen = p.nodes.clone();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), p.nodes.len(), "path must be simple");
        assert_eq!(p.nodes.len(), p.edges.len() + 1);
    }
    let one = Bounds {
        max_paths: 1,
        ..Bounds::default()
    };
    let r = g.all_paths(SymbolId(0), SymbolId(3), Tier::Proven, &one).unwrap();
    assert_eq!(r.paths.len(), 1);
    assert!(r.hit.paths);
}
