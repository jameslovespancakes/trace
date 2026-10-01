use super::*;

#[test]
fn rule_tokens_split_identifiers_and_drop_stop_words() {
    assert_eq!(tokens("parseHTTPResponse_code"), vec!["parse", "http", "response"]);
    assert_eq!(fold("registration"), "registr");
    assert_eq!(fold("entries"), "entry");
    assert_eq!(terms("How does login work?"), vec!["login", "work"]);
}

#[test]
fn bm25_ranks_names_and_filters_before_paging() {
    let index = crate::test_support::project();
    let search = SearchIndex::build(&index);
    assert_eq!(search.docs.len(), 6);
    let hits = search.search("how does login work", 5, 0);
    assert_eq!(index.symbol(hits[0].id).uid, "auth.py:Session.login");
    // Docstring words are searchable ("record" only appears in Store.load's doc).
    let hits = search.search("records", 5, 0);
    assert_eq!(index.symbol(hits[0].id).uid, "store.py:Store.load");
    // Class names are down-weighted but found; the callable filter excludes them.
    let all = search.search("session", 10, 0);
    assert!(all.iter().any(|h| index.symbol(h.id).kind.is_type()));
    let callables = search.search_where("session", 10, 0, |id| index.symbol(id).kind.is_callable());
    assert!(!callables.is_empty());
    assert!(callables.iter().all(|h| index.symbol(h.id).kind.is_callable()));
    // Paging is stable.
    let second = search.search_where("session", 1, 1, |id| index.symbol(id).kind.is_callable());
    assert_eq!(second.first().map(|h| h.id), callables.get(1).map(|h| h.id));
    assert!(search.search("zzzz", 5, 0).is_empty());
}
