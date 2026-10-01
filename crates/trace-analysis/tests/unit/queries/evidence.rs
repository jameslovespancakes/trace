use super::*;

#[test]
fn evidence_lists_both_directions_and_owned_sites() {
    let mut index = crate::test_support::project();
    crate::test_support::push_site(&mut index, "x.load", trace_core::model::SiteCategory::NoTarget, 2, &[5]);
    let graph = Graph::new(&index);
    let sources = SourceStore::new(&index);
    // Session.login: called by login_view, calls _check and Store.load.
    let ev = evidence_of(&graph, &sources, SymbolId(2));
    let incoming: Vec<&str> = ev.incoming.iter().map(|r| r.other.id.as_str()).collect();
    assert_eq!(incoming, vec!["api.py:login_view"]);
    assert_eq!(ev.incoming[0].edge.to, "auth.py:Session.login");
    let outgoing: Vec<&str> = ev.outgoing.iter().map(|r| r.other.id.as_str()).collect();
    assert!(outgoing.contains(&"auth.py:Session._check"));
    assert!(outgoing.contains(&"store.py:Store.load"));
    assert_eq!(ev.sites.len(), 1);
    assert_eq!(ev.sites[0].candidates, vec!["store.py:Store.load".to_string()]);
    assert_eq!(ev.sites[0].decision.status, "unknown");
    // Fixture files do not exist on disk: the exact line text is empty, never an error.
    assert!(ev.incoming[0].edge.text.is_empty());
    assert!(ev.unresolved.is_empty());
}
