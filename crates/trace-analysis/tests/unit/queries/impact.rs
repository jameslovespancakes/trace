use super::*;
use trace_syntax::uses::UseSite;

#[test]
fn transitive_chains_name_the_direct_caller_they_pass() {
    // Calls: login_view (0) -> Session.login (2) -> Store.load (5).
    let index = crate::test_support::project();
    let graph = Graph::new(&index);
    let members: HashSet<SymbolId> = [SymbolId(5)].into_iter().collect();
    let reach = graph
        .reach_many(
            &[SymbolId(5)],
            Direction::Reverse,
            trace_core::Tier::Inferred,
            &Bounds::default().with_depth(64),
        )
        .unwrap();
    let first = first_edges(&graph, &reach);
    let e = graph.edge(first[0].unwrap());
    assert_eq!((e.from, e.to), (SymbolId(0), SymbolId(2)));
    assert_eq!(
        walk_chain(&graph, &first, &members, SymbolId(0), e.to, 2),
        Some((SymbolId(5), Some(SymbolId(2))))
    );
    // Too few steps: the chain does not reach a member.
    assert_eq!(walk_chain(&graph, &first, &members, SymbolId(0), e.to, 1), None);
    // A chain that starts at a member has no intermediate caller.
    assert_eq!(walk_chain(&graph, &first, &members, SymbolId(2), SymbolId(5), 1), Some((SymbolId(5), None)));
}

#[test]
fn use_phrases_group_by_kind() {
    let ru = ResultUse {
        call: "token = f(user)".into(),
        line: 88,
        stored_in: vec!["token".into()],
        uses: vec![
            UseSite {
                kind: UseKind::ReadsAttribute,
                line: 90,
                code: "token.value".into(),
            },
            UseSite {
                kind: UseKind::ReadsAttribute,
                line: 91,
                code: "token.expiry".into(),
            },
            UseSite {
                kind: UseKind::ReturnsIt,
                line: 95,
                code: "return token".into(),
            },
        ],
    };
    assert_eq!(
        use_phrases(&ru),
        vec![
            "reads attribute (2x, e.g. line 90: token.value)".to_string(),
            "returns it (1x, e.g. line 95: return token)".to_string(),
        ]
    );
    let stored_only = ResultUse {
        call: "x = f()".into(),
        line: 1,
        stored_in: vec!["x".into()],
        uses: Vec::new(),
    };
    assert_eq!(use_phrases(&stored_only), vec!["stores it in x".to_string()]);
    let ignored = ResultUse {
        call: "f()".into(),
        line: 1,
        stored_in: Vec::new(),
        uses: vec![UseSite {
            kind: UseKind::IgnoresResult,
            line: 1,
            code: "f()".into(),
        }],
    };
    assert_eq!(use_phrases(&ignored), vec!["ignores the result".to_string()]);
}
