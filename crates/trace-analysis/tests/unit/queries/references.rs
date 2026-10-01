use super::*;

#[test]
fn ranks_prefer_stronger_tiers() {
    assert!(tier_rank("proven") < tier_rank("inferred"));
    assert!(tier_rank("inferred") < tier_rank("possible"));
}

/// 600 bytes of 80-column blank lines with `inserts` placed at byte offsets.
fn source(inserts: &[(usize, &str)]) -> Vec<u8> {
    let mut b = vec![b' '; 600];
    for i in (79..b.len()).step_by(80) {
        b[i] = b'\n';
    }
    for (at, text) in inserts {
        b[*at..*at + text.len()].copy_from_slice(text.as_bytes());
    }
    b
}

/// Write the fixture sources and return the root.
fn write_sources(
    index: &mut trace_core::Index,
    contents: &[Vec<u8>],
) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = trace_core::inventory::strip_verbatim(std::fs::canonicalize(dir.path()).unwrap());
    for (rec, bytes) in index.files.iter_mut().zip(contents) {
        std::fs::write(root.join(&rec.path), bytes).unwrap();
        rec.hash = trace_core::Hash32::of(bytes);
    }
    (dir, root)
}

#[test]
fn rows_list_declarations_uses_and_overrides_at_the_identifier() {
    // Fixture spans: symbol k at k*100..k*100+50 (name at +4..+8); edge a -> b at
    // a*100+20..a*100+25 in a's file. api.py calls `s.login()` (0 -> 2 at 20..25),
    // auth.py calls `x.load()` (2 -> 5 at 220..225), Store.load overrides Session.login.
    let mut index = crate::test_support::project();
    let mut over = index.edges[2].clone();
    over.from = SymbolId(5);
    over.to = SymbolId(2);
    over.kind = EdgeKind::Overrides;
    over.at.bytes = ByteSpan::new(504, 508);
    over.at.file = FileId(2);
    index.edges.push(over);
    let contents = [source(&[(18, "s.login()")]), source(&[(219, "x.load()")]), source(&[])];
    let (_dir, root) = write_sources(&mut index, &contents);
    let graph = Graph::new(&index);
    let sources = SourceStore::with_root(&index, root);
    let family = target_family(&index, SymbolId(2), MAX_FAMILY);
    assert_eq!(family.ids(), vec![SymbolId(2), SymbolId(5)]);
    let rows = collect_rows(&graph, &sources, SymbolId(2), &family, Tier::Inferred).unwrap();
    type Row<'a> = (&'a str, &'a str, u32, u32, Option<&'a str>, Option<&'a str>);
    let got: Vec<Row> = rows
        .iter()
        .map(|(r, _)| (r.file.as_str(), r.kind, r.line, r.column, r.owner.as_deref(), r.via.as_deref()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("api.py", "call", 1, 21, Some("api.py:login_view"), None),
            ("auth.py", "declaration", 3, 45, Some("auth.py:Session.login"), None),
            ("auth.py", "call", 3, 62, Some("auth.py:Session.login"), Some("store.py:Store.load")),
            ("store.py", "override", 7, 25, Some("store.py:Store.load"), Some("store.py:Store.load")),
        ]
    );
    // The call is located at the member identifier and the line text is exact.
    let (call, _) = &rows[2];
    assert_eq!((call.start_byte, call.end_byte), (221, 225));
    assert_eq!(call.text.trim(), "x.load()");
    assert!(rows.iter().all(|(r, _)| r.tier == "proven" && r.source == "index"));
    assert_eq!(rows[1].0.resolution, "declaration");
    assert_eq!(rows[3].0.resolution, "call_hierarchy");
}

/// Rule 1: `uses` rows are proven and inferred only, even in the `possible` view.
#[test]
fn rule_uses_rows_are_proven_and_inferred_only() {
    use trace_core::model::SiteCategory;
    let mut index = crate::test_support::project();
    // An undecided site of login_view with candidates Session.login and Store.load:
    // possible links only.
    crate::test_support::push_site(&mut index, "x.login", SiteCategory::NoTarget, 0, &[2, 4]);
    let contents = [source(&[(18, "s.login()"), (30, "x.login")]), source(&[]), source(&[])];
    let (_dir, root) = write_sources(&mut index, &contents);
    let graph = Graph::new(&index);
    let sources = SourceStore::with_root(&index, root);
    let family = target_family(&index, SymbolId(2), MAX_FAMILY);
    let rows = collect_rows(&graph, &sources, SymbolId(2), &family, Tier::Possible).unwrap();
    assert!(rows.iter().all(|(r, _)| r.tier != "possible"), "{rows:?}");
    assert!(rows.iter().any(|(r, _)| r.kind == "call" && r.file == "api.py"));
}

/// TS `errorHandler` rule (c): a callback decided at a site whose receiving call is
/// `compose(this.handler)` is located at the argument (`handler` of `this.handler`), so
/// the member-binding rule rejects the link to the free function `handler`.
#[test]
fn rule_member_callback_evidence_is_the_argument() {
    use crate::test_support::{index as fixture_index, push_site, sym};
    use trace_core::facts::{Activation, CallSite, CallbackArg, FileFacts, MemberAccess};
    use trace_core::model::SiteCategory;
    use trace_core::SymbolKind::*;
    let mut handler = sym(2, 1, "handlers.ts", "handler", Function, None, None);
    handler.language = trace_core::Language::TypeScript;
    let symbols = vec![
        sym(0, 0, "app.ts", "App", Class, None, None),
        sym(1, 0, "app.ts", "App.run", Method, None, Some(0)),
        handler,
    ];
    let mut index = fixture_index(&["app.ts", "handlers.ts"], symbols, &[]);
    let mut facts = FileFacts::default();
    facts.calls.push(CallSite {
        owner: Some(1),
        lexical_owner: Some(1),
        span: ByteSpan::new(110, 132),
        callee_span: ByteSpan::new(110, 117),
        callee: "compose".into(),
        member: Some("compose".into()),
        receiver: None,
        line: 2,
        activation: Activation::Plain,
        is_new: false,
        arg_count: 1,
    });
    facts.callbacks.push(CallbackArg {
        call_callee_span: ByteSpan::new(110, 117),
        callee: "compose".into(),
        arg_span: ByteSpan::new(123, 130),
        argument: "this.handler".into(),
        name: "handler".into(),
        owner: Some(1),
        index: Some(0),
        keyword: None,
    });
    facts.member_accesses.push(MemberAccess {
        span: ByteSpan::new(123, 130),
        receiver_root: None,
        self_receiver: true,
    });
    index.files[0].facts = Some(facts);
    let s = push_site(&mut index, "compose", SiteCategory::Callback, 1, &[2]);
    index.sites[s as usize].at.bytes = ByteSpan::new(110, 117);
    index.sites[s as usize].argument = Some("this.handler".into());
    index.decisions[s as usize] = crate::test_support::decided(s, &[2]);
    let contents = [source(&[(110, "compose(this.handler)")]), source(&[])];
    let (_dir, root) = write_sources(&mut index, &contents);
    let family = target_family(&index, SymbolId(2), MAX_FAMILY);
    let graph = Graph::new(&index);
    let e = graph
        .edges()
        .iter()
        .find(|e| e.site == Some(s))
        .expect("the decided callback site is an edge");
    assert_eq!(callback_argument_span(&index, e), Some(ByteSpan::new(123, 130)));
    let sources = SourceStore::with_root(&index, root);
    let rows = collect_rows(&graph, &sources, SymbolId(2), &family, Tier::Inferred).unwrap();
    assert!(rows.iter().all(|(r, _)| r.file != "app.ts"), "{rows:?}");
}

/// Rule 5 on rows: an inferred link from a member access to a free function is not a
/// row; a proven one (server) is kept.
#[test]
fn rule_member_access_rows_are_dropped_unless_proven() {
    use crate::test_support::{index as fixture_index, push_site, sym};
    use trace_core::facts::{Activation, CallSite, FileFacts, MemberAccess};
    use trace_core::model::SiteCategory;
    use trace_core::SymbolKind::*;
    let mut handler = sym(2, 1, "handlers.py", "handler", Function, None, None);
    handler.language = trace_core::Language::TypeScript;
    let symbols = vec![
        sym(0, 0, "app.py", "App", Class, None, None),
        sym(1, 0, "app.py", "App.run", Method, None, Some(0)),
        handler,
    ];
    let mut index = fixture_index(&["app.py", "handlers.py"], symbols, &[]);
    let mut facts = FileFacts::default();
    facts.calls.push(CallSite {
        owner: Some(1),
        lexical_owner: Some(1),
        span: ByteSpan::new(110, 124),
        callee_span: ByteSpan::new(110, 122),
        callee: "this.handler".into(),
        member: Some("handler".into()),
        receiver: None,
        line: 2,
        activation: Activation::Plain,
        is_new: false,
        arg_count: 1,
    });
    facts.member_accesses.push(MemberAccess {
        span: ByteSpan::new(115, 122),
        receiver_root: None,
        self_receiver: true,
    });
    index.files[0].facts = Some(facts);
    let s = push_site(&mut index, "this.handler", SiteCategory::Flow, 1, &[2]);
    index.sites[s as usize].at.bytes = ByteSpan::new(110, 122);
    index.decisions[s as usize] = crate::test_support::decided(s, &[2]);
    let contents = [source(&[(110, "this.handler(e)")]), source(&[])];
    let (_dir, root) = write_sources(&mut index, &contents);
    let family = target_family(&index, SymbolId(2), MAX_FAMILY);
    {
        let graph = Graph::new(&index);
        let sources = SourceStore::with_root(&index, root.clone());
        let rows = collect_rows(&graph, &sources, SymbolId(2), &family, Tier::Inferred).unwrap();
        assert!(rows.iter().all(|(r, _)| r.file != "app.py"), "{rows:?}");
    }
    // The same link proven by a server stays a row.
    let mut proven = index.clone();
    proven.sites.clear();
    proven.decisions.clear();
    proven.edges.push(trace_core::Edge {
        from: SymbolId(1),
        to: SymbolId(2),
        kind: EdgeKind::Calls,
        tier: Tier::Proven,
        provider: trace_core::Provider::TypeScript,
        resolution: trace_core::Resolution::ResolvedSignature,
        at: trace_core::Location {
            file: FileId(0),
            bytes: ByteSpan::new(110, 122),
            line: 2,
        },
        site: None,
        bridge: None,
    });
    let graph = Graph::new(&proven);
    let sources = SourceStore::with_root(&proven, root);
    let rows = collect_rows(&graph, &sources, SymbolId(2), &family, Tier::Inferred).unwrap();
    assert!(rows.iter().any(|(r, _)| r.file == "app.py" && r.tier == "proven"), "{rows:?}");
}

/// TS `errorHandler` rule (I-23): the site names the flowed value (`handler`) while the
/// syntax argument is the member expression (`this.handler`): the argument of the same
/// call whose last member segment is the site's argument locates the use, so the
/// member-binding rule rejects the inferred link (no decoy row). Negative: two arguments
/// of that call matching by member segment locate nothing.
#[test]
fn rule_member_argument_matches_its_callback_span() {
    use crate::test_support::{index as fixture_index, push_site, sym};
    use trace_core::facts::{Activation, CallSite, CallbackArg, FileFacts, MemberAccess};
    use trace_core::model::SiteCategory;
    use trace_core::SymbolKind::*;
    let mut handler = sym(2, 1, "handlers.ts", "handler", Function, None, None);
    handler.language = trace_core::Language::TypeScript;
    let symbols = vec![
        sym(0, 0, "app.ts", "App", Class, None, None),
        sym(1, 0, "app.ts", "App.run", Method, None, Some(0)),
        handler,
    ];
    let mut index = fixture_index(&["app.ts", "handlers.ts"], symbols, &[]);
    index.files[0].language = trace_core::Language::TypeScript;
    index.files[1].language = trace_core::Language::TypeScript;
    let arg = |start: u32, argument: &str| CallbackArg {
        call_callee_span: ByteSpan::new(110, 117),
        callee: "compose".into(),
        arg_span: ByteSpan::new(start, start + 7),
        argument: argument.into(),
        name: "handler".into(),
        owner: Some(1),
        index: Some(0),
        keyword: None,
    };
    let mut facts = FileFacts::default();
    facts.calls.push(CallSite {
        owner: Some(1),
        lexical_owner: Some(1),
        span: ByteSpan::new(110, 132),
        callee_span: ByteSpan::new(110, 117),
        callee: "compose".into(),
        member: Some("compose".into()),
        receiver: None,
        line: 2,
        activation: Activation::Plain,
        is_new: false,
        arg_count: 1,
    });
    facts.callbacks.push(arg(123, "this.handler"));
    facts.member_accesses.push(MemberAccess {
        span: ByteSpan::new(123, 130),
        receiver_root: None,
        self_receiver: true,
    });
    index.files[0].facts = Some(facts);
    let s = push_site(&mut index, "compose", SiteCategory::Callback, 1, &[2]);
    index.sites[s as usize].at.bytes = ByteSpan::new(110, 117);
    index.sites[s as usize].argument = Some("handler".into());
    index.decisions[s as usize] = crate::test_support::decided(s, &[2]);
    let contents = [source(&[(110, "compose(this.handler)")]), source(&[])];
    let (_dir, root) = write_sources(&mut index, &contents);
    let family = target_family(&index, SymbolId(2), MAX_FAMILY);
    {
        let graph = Graph::new(&index);
        let e = graph
            .edges()
            .iter()
            .find(|e| e.site == Some(s))
            .expect("the decided callback site is an edge");
        assert_eq!(callback_argument_span(&index, e), Some(ByteSpan::new(123, 130)));
        let sources = SourceStore::with_root(&index, root);
        let rows = collect_rows(&graph, &sources, SymbolId(2), &family, Tier::Inferred).unwrap();
        assert!(rows.iter().all(|(r, _)| r.file != "app.ts"), "{rows:?}");
    }
    // Two arguments of the call end in `handler`: ambiguous, no location.
    let mut ambiguous = index.clone();
    ambiguous.files[0]
        .facts
        .as_mut()
        .unwrap()
        .callbacks
        .push(arg(140, "app.handler"));
    let graph = Graph::new(&ambiguous);
    let e = graph.edges().iter().find(|e| e.site == Some(s)).expect("edge");
    assert_eq!(callback_argument_span(&ambiguous, e), None);
    assert_eq!(member_tail("this.handler"), "handler");
    assert_eq!(member_tail("$this->handler"), "handler");
    assert_eq!(member_tail("handler"), "handler");
}
