use super::*;
use super::{access::*, family::*};
use trace_core::facts::Scope;
use trace_core::facts::{
    Activation, CallSite, FileFacts, Import, ImportKind, MemberAccess, RefKind, Reference,
};
use trace_core::model::{DecisionStatus, Provider, Resolution, SiteCategory};
use trace_core::model::{Location, Unresolved};
use trace_core::SymbolKind;

fn call(owner: u32, callee: &str, member: &str, end: u32) -> CallSite {
    CallSite {
        owner: Some(owner),
        lexical_owner: Some(owner),
        span: ByteSpan::new(end - callee.len() as u32, end + 2),
        callee_span: ByteSpan::new(end - callee.len() as u32, end),
        callee: callee.into(),
        member: Some(member.into()),
        receiver: None,
        line: 1,
        activation: Activation::Plain,
        is_new: false,
        arg_count: 0,
    }
}

fn unresolved(file: u32, start: u32, end: u32, kind: UnresolvedKind, owner: u32) -> Unresolved {
    Unresolved {
        owner: Some(SymbolId(owner)),
        kind,
        at: Location {
            file: FileId(file),
            bytes: ByteSpan::new(start, end),
            line: 1,
        },
        callee: "x".into(),
        candidates: Vec::new(),
    }
}

fn reference(start: u32, name: &str, local: bool) -> Reference {
    Reference {
        span: ByteSpan::new(start, start + name.len() as u32),
        name: name.into(),
        owner: None,
        in_decorator: false,
        local,
        kind: RefKind::Read,
    }
}

/// `project()` plus facts: in api.py, `s.login` (proven edge 0 -> 2, callee span 18..25),
/// `other.login` (no_semantic_target, 39..50) and `json.login` (external, 60..70); in
/// store.py (tests may make it a pending file) an unresolved-free reference read `login`
/// covered by nothing.
fn setup() -> Index {
    let mut index = crate::test_support::project();
    let mut api = FileFacts::default();
    api.calls.push(call(0, "s.login", "login", 25));
    api.calls.push(call(0, "other.login", "login", 50));
    api.calls.push(call(0, "json.login", "login", 70));
    api.calls.push(call(0, "s.logout", "logout", 90));
    index.files[0].facts = Some(api);
    index.files[1].facts = Some(FileFacts::default());
    index.files[2].facts = Some(FileFacts::default());
    index
        .unresolved
        .push(unresolved(0, 39, 50, UnresolvedKind::NoSemanticTarget, 0));
    index
        .unresolved
        .push(unresolved(0, 60, 70, UnresolvedKind::ExternalOrAmbiguous, 0));
    index
}

fn scan_with(index: &Index, family: &[SymbolId], spans: &[(FileId, ByteSpan)]) -> NameScan {
    let graph = Graph::new(index);
    let sources = SourceStore::new(index);
    name_scan(
        &graph,
        &sources,
        &NameQuery {
            family,
            include: Tier::Inferred,
            bounded: None,
            target_spans: spans,
        },
    )
}

fn scan(index: &Index, family: &[SymbolId]) -> Completeness {
    scan_with(index, family, &[]).completeness
}

#[test]
fn partial_lists_the_unresolved_same_name_site() {
    // The fixture edge 0 -> 2 is at 20..25 (test_support::index), ending with `s.login`.
    let index = setup();
    let c = scan(&index, &[SymbolId(2)]);
    assert_eq!(c.status, "partial", "{c:?}");
    assert_eq!((c.name_matches, c.resolved_to_target, c.resolved_elsewhere), (3, 1, 1));
    assert_eq!(c.elsewhere_reasons.get(SERVER), Some(&1));
    assert_eq!(c.unresolved.len(), 1);
    let u = &c.unresolved[0];
    assert_eq!((u.at.start_byte, u.at.end_byte, u.kind, u.reason), (45, 50, "call", "no_semantic_target"));
    assert_eq!(u.owner.as_deref(), Some("api.py:login_view"));
    assert_eq!(u.rank, 1);
    assert_eq!(c.summary, "partial: 1 same-name site unresolved (1 call)");
}

#[test]
fn complete_when_everything_resolves_and_unknown_when_bounded() {
    let mut index = setup();
    index.files[0].facts.as_mut().unwrap().calls.remove(1);
    index.unresolved.remove(0);
    let c = scan(&index, &[SymbolId(2)]);
    assert_eq!(c.status, "complete", "{c:?}");
    assert_eq!(c.summary, "complete: all 2 name matches resolved (1 to the target, 1 elsewhere)");
    assert!(c.unresolved.is_empty());

    let graph = Graph::new(&index);
    let sources = SourceStore::new(&index);
    let bounded = name_scan(
        &graph,
        &sources,
        &NameQuery {
            family: &[SymbolId(2)],
            include: Tier::Inferred,
            bounded: Some("family cut at 64 members".into()),
            target_spans: &[],
        },
    );
    assert_eq!(bounded.completeness.status, "unknown");
    // Proven-only view: the proven edge still resolves the call.
    let proven = name_scan(
        &graph,
        &sources,
        &NameQuery {
            family: &[SymbolId(2)],
            include: Tier::Proven,
            bounded: None,
            target_spans: &[],
        },
    );
    assert_eq!(proven.completeness.status, "complete");
}

/// A pending file's occurrences are unresolved `not_analyzed` and the summary names the
/// pending language and how to set it up.
#[test]
fn rule_pending_file_matches_are_not_analyzed() {
    let mut index = setup();
    index.files[0].facts.as_mut().unwrap().calls.truncate(1);
    index.unresolved.clear();
    index.files[2].support = SupportLevel::Pending;
    index.files[2].pending = Some("sub-project store: dependencies not installed".into());
    index.files[2]
        .facts
        .as_mut()
        .unwrap()
        .references
        .push(reference(410, "login", false));
    let c = scan(&index, &[SymbolId(2)]);
    assert_eq!(c.status, "partial");
    assert_eq!(c.unresolved[0].reason, "not_analyzed");
    assert_eq!(c.pending_languages, vec![Language::Python]);
    assert_eq!((c.pending_files, c.outside_build_files), (1, 0));
    assert_eq!(
        c.summary,
        "partial: 1 same-name site unresolved (1 read; 1 in 1 file not analyzed yet (Python); run a query on one of them to set it up)"
    );
    // A span counted as the target at the same identifier resolves it.
    let spans = [(FileId(2), ByteSpan::new(410, 415))];
    let scan = scan_with(&index, &[SymbolId(2)], &spans);
    assert_eq!(scan.completeness.status, "complete");
}

/// A file the server reported outside the build on this machine: its open occurrences
/// are `outside_build` and counted in the summary.
#[test]
fn rule_outside_build_file_matches_are_reported() {
    let mut index = setup();
    index.files[0].facts.as_mut().unwrap().calls.truncate(1);
    index.unresolved.clear();
    index.files[2]
        .facts
        .as_mut()
        .unwrap()
        .references
        .push(reference(410, "login", false));
    index.files[2].semantic = Some(trace_core::semantics::FileSemantics {
        provider: Provider::Pyright,
        tool_fingerprint: String::new(),
        edges: Vec::new(),
        unresolved: Vec::new(),
        value_refs: Vec::new(),
        diagnostics: Vec::new(),
        implementations: Vec::new(),
        resolved_elsewhere: Vec::new(),
        callback_params: Vec::new(),
        library_files: Vec::new(),
        library_calls: Vec::new(),
        outside_build: Some("other platform".into()),
        expanded: Vec::new(),
        library_dispatch: Vec::new(),
        library_bases: Vec::new(),
    });
    let c = scan(&index, &[SymbolId(2)]);
    assert_eq!(c.status, "partial");
    assert_eq!(c.unresolved[0].reason, "outside_build");
    assert_eq!(c.outside_build_files, 1);
    assert!(
        c.summary
            .ends_with("; 1 in 1 file outside the build on this machine)"),
        "{}",
        c.summary
    );
}

#[test]
fn dependencies_count_calls_inside() {
    let index = setup();
    let graph = Graph::new(&index);
    let sources = SourceStore::new(&index);
    let reached: HashSet<SymbolId> = [SymbolId(0), SymbolId(2)].into_iter().collect();
    let c = calls_completeness(&graph, &sources, &reached, Tier::Inferred, None);
    // api.py: four calls owned by login_view; one blind, one external.
    assert_eq!(c.name_matches, 4);
    assert_eq!(c.status, "partial");
    assert_eq!((c.resolved_elsewhere, c.unresolved.len()), (1, 1));
    assert_eq!(c.unresolved[0].reason, "no_semantic_target");
    assert_eq!((c.unresolved[0].rank, c.unresolved[0].scope), (1, NAME_ONLY));
    assert!(c.summary.starts_with("partial: 1 call inside unresolved"), "{}", c.summary);
}

/// Rule 1: every `uses` row's identifier span counts as the target, so a site is never
/// both a row and an unresolved entry.
#[test]
fn rule_uses_rows_never_contradict_check() {
    let index = setup();
    // A row at `other.login` (e.g. an inferred row the answer lists).
    let rows = [(FileId(0), ByteSpan::new(45, 50))];
    let scan = scan_with(&index, &[SymbolId(2)], &rows);
    let c = &scan.completeness;
    assert!(
        c.unresolved
            .iter()
            .all(|u| !(u.at.file == "api.py" && u.at.start_byte == 45)),
        "{c:?}"
    );
    assert_eq!(c.status, "complete", "{c:?}");
    assert_eq!(c.resolved_to_target, 2);
}

/// Rule 2: a local / parameter binding of the same name (and every use of it) is counted
/// as resolved elsewhere (`local_binding`), never listed — target evidence still wins.
#[test]
fn rule_check_drops_local_bindings() {
    let mut index = setup();
    index.unresolved.clear();
    index.files[0].facts.as_mut().unwrap().calls.truncate(1);
    let facts = index.files[2].facts.as_mut().unwrap();
    // `login = ...` binding and a read of it (flagged by `Reference::local`), plus a
    // bare call of the local closure (`local_spans` only).
    facts.references.push(reference(410, "login", true));
    facts.references.push(reference(430, "login", false));
    facts.calls.push(call(5, "login", "login", 455));
    facts.local_spans = vec![ByteSpan::new(430, 435), ByteSpan::new(450, 455)];
    let c = scan(&index, &[SymbolId(2)]);
    assert_eq!(c.status, "complete", "{c:?}");
    assert!(c.unresolved.is_empty());
    assert_eq!(c.elsewhere_reasons.get(LOCAL_BINDING), Some(&3), "{c:?}");
    // A local that a proven edge links to the target stays the target.
    let x = Evidence {
        target_proven: true,
        ..Evidence::default()
    };
    assert_eq!(decide(&x, true, false, &mut || None, "no_semantic_target"), State::Target);
}

/// TS `errorHandler` rule (b): at a local-binding occurrence (a destructured local
/// `errorHandler` called in another file) an inferred link to a same-name declaration
/// is not target evidence; the occurrence is resolved elsewhere (`local_binding`).
#[test]
fn rule_local_binding_occurrence_rejects_non_proven_rows() {
    let inferred = Evidence {
        target: true,
        ..Evidence::default()
    };
    assert_eq!(
        decide(&inferred, true, false, &mut || None, "no_semantic_target"),
        State::Elsewhere(LOCAL_BINDING)
    );
    // Not a local binding: the inferred link names the target.
    assert_eq!(decide(&inferred, false, false, &mut || None, "no_semantic_target"), State::Target);
    // A row of the answer is always the target (rows and check never contradict).
    let forced = Evidence {
        forced: true,
        ..Evidence::default()
    };
    assert_eq!(decide(&forced, true, false, &mut || None, "no_semantic_target"), State::Target);
}

/// Rule 2: a non-call use the server resolved outside the index (library / builtin) or
/// to a local binding (`FileSemantics::resolved_elsewhere`) is resolved elsewhere
/// (`server`).
#[test]
fn rule_check_drops_server_resolved_elsewhere() {
    let mut index = setup();
    index.unresolved.clear();
    index.files[0].facts.as_mut().unwrap().calls.truncate(1);
    index.files[2]
        .facts
        .as_mut()
        .unwrap()
        .references
        .push(reference(410, "login", false));
    let before = scan(&index, &[SymbolId(2)]);
    assert_eq!(before.status, "partial");
    index.files[2].semantic = Some(trace_core::semantics::FileSemantics {
        provider: Provider::Pyright,
        tool_fingerprint: String::new(),
        edges: Vec::new(),
        unresolved: Vec::new(),
        value_refs: Vec::new(),
        diagnostics: Vec::new(),
        implementations: Vec::new(),
        resolved_elsewhere: vec![ByteSpan::new(410, 415)],
        callback_params: Vec::new(),
        library_files: Vec::new(),
        library_calls: Vec::new(),
        outside_build: None,
        expanded: Vec::new(),
        library_dispatch: Vec::new(),
        library_bases: Vec::new(),
    });
    let c = scan(&index, &[SymbolId(2)]);
    assert_eq!(c.status, "complete", "{c:?}");
    assert_eq!(c.elsewhere_reasons.get(SERVER), Some(&1));
}

/// Rule 2: a member access whose receiver type is provably unrelated to the family is
/// resolved elsewhere (`unrelated_type`). The verdict comes from
/// `trace_infer::types::unrelated_to_family`; the decision itself is tested with the
/// verdict fixed.
#[test]
fn rule_check_drops_unrelated_receiver_types() {
    let open = Evidence {
        unresolved: Some("no_semantic_target"),
        ..Evidence::default()
    };
    assert_eq!(
        decide(&open, false, false, &mut || Some(UNRELATED_TYPE), "x"),
        State::Elsewhere(UNRELATED_TYPE)
    );
    assert_eq!(decide(&open, false, false, &mut || None, "x"), State::Unresolved("no_semantic_target"));
    // Target evidence wins over the receiver verdict; the verdict is not even asked.
    let target = Evidence {
        target: true,
        ..Evidence::default()
    };
    let mut asked = false;
    assert_eq!(
        decide(
            &target,
            false,
            false,
            &mut || {
                asked = true;
                Some(UNRELATED_TYPE)
            },
            "x"
        ),
        State::Target
    );
    assert!(!asked);
    // A server verdict comes first.
    let server = Evidence {
        elsewhere: true,
        ..Evidence::default()
    };
    assert_eq!(decide(&server, false, false, &mut || Some(UNRELATED_TYPE), "x"), State::Elsewhere(SERVER));
}

/// Rule 3: unresolved sites are ranked same module, then files importing the target,
/// then name-only matches; within a group by (file, line).
#[test]
fn rule_check_is_ranked_same_module_imports_name_only() {
    let mut index = setup();
    // auth.py declares the target; api.py imports it (proven imports edge into auth.py);
    // store.py only shares the name.
    let mut imports = index.edges[0].clone();
    imports.kind = EdgeKind::Imports;
    imports.at.file = FileId(0);
    index.edges.push(imports);
    let entry = |file: &str, line: u32| UnresolvedMatch {
        at: At {
            file: file.into(),
            line,
            start_byte: line * 10,
            end_byte: line * 10 + 5,
        },
        owner: None,
        kind: "call",
        text: String::new(),
        reason: "no_semantic_target",
        rank: 0,
        scope: NAME_ONLY,
    };
    let mut c = scan(&index, &[SymbolId(2)]);
    c.unresolved = vec![
        entry("store.py", 1),
        entry("api.py", 9),
        entry("auth.py", 7),
        entry("api.py", 2),
        entry("auth.py", 3),
    ];
    rank_unresolved(&index, SymbolId(2), &mut c);
    let got: Vec<(&str, u32, u32, &str)> = c
        .unresolved
        .iter()
        .map(|u| (u.at.file.as_str(), u.at.line, u.rank, u.scope))
        .collect();
    assert_eq!(
        got,
        vec![
            ("auth.py", 3, 1, SAME_MODULE),
            ("auth.py", 7, 2, SAME_MODULE),
            ("api.py", 2, 3, IMPORTS_TARGET),
            ("api.py", 9, 4, IMPORTS_TARGET),
            ("store.py", 1, 5, NAME_ONLY),
        ]
    );
}

/// `project()` with a free function `handler` (6, handlers.py) as the target and
/// member accesses `this.handler` (inferred link) / `ns.handler` in app.py.
fn member_binding_setup(module_import: bool) -> Index {
    use crate::test_support::{index, sym};
    use SymbolKind::*;
    let mut symbols = vec![
        sym(0, 0, "app.py", "App", Class, None, None),
        sym(1, 0, "app.py", "App.run", Method, None, Some(0)),
        sym(2, 1, "handlers.py", "handler", Function, None, None),
    ];
    symbols[2].language = Language::TypeScript;
    let mut ix = index(&["app.py", "handlers.py"], symbols, &[]);
    // The member accesses are TypeScript code (one language namespace with the target).
    ix.files[0].language = Language::TypeScript;
    let mut app = FileFacts::default();
    // `this.handler(e)` at 110..122 and `ns.handler(e)` at 140..150.
    app.calls.push(call(1, "this.handler", "handler", 122));
    app.calls.push(call(1, "ns.handler", "handler", 150));
    app.member_accesses = vec![
        MemberAccess {
            span: ByteSpan::new(115, 122),
            receiver_root: None,
            self_receiver: true,
        },
        MemberAccess {
            span: ByteSpan::new(143, 150),
            receiver_root: Some("ns".into()),
            self_receiver: false,
        },
    ];
    if module_import {
        app.imports.push(Import {
            local: "ns".into(),
            target: "./handlers".into(),
            kind: ImportKind::Module,
            scope: Scope::Module,
            span: ByteSpan::new(0, 30),
            line: 1,
        });
    }
    ix.files[0].facts = Some(app);
    ix.files[1].facts = Some(FileFacts::default());
    // Value flow linked `this.handler(e)` to the exported function (inferred).
    let s = crate::test_support::push_site(&mut ix, "this.handler", SiteCategory::Flow, 1, &[2]);
    ix.sites[s as usize].at = Location {
        file: FileId(0),
        bytes: ByteSpan::new(110, 122),
        line: 2,
    };
    ix.decisions[s as usize] = crate::test_support::decided(s, &[2]);
    ix
}

/// Rule 5: a member access (`this.handler`, `obj.handler`) never denotes a free /
/// exported function `handler`: an inferred link is rejected and the site is counted as
/// `member_binding`, never listed.
#[test]
fn rule_member_access_never_uses_a_free_function() {
    let ix = member_binding_setup(false);
    let b = Bindings::new(&ix, &[SymbolId(2)]);
    let self_access = Access::Member {
        receiver_root: None,
        self_receiver: true,
    };
    assert!(b.excludes(FileId(0), ByteSpan::new(115, 122), &self_access, SymbolId(2)));
    let c = scan(&ix, &[SymbolId(2)]);
    assert_eq!(c.status, "complete", "{c:?}");
    assert_eq!(c.resolved_to_target, 0, "{c:?}");
    assert_eq!(c.elsewhere_reasons.get(MEMBER_BINDING), Some(&2), "{c:?}");
    // The access fact is the one the rows use.
    let facts = ix.files[0].facts.as_ref().unwrap();
    assert_eq!(access_at(facts, ByteSpan::new(115, 122)), self_access);
    // Receivers that are other expressions are never judged; a bare name is.
    let other = Access::Member {
        receiver_root: None,
        self_receiver: false,
    };
    assert!(!b.excludes(FileId(0), ByteSpan::new(115, 122), &other, SymbolId(2)));
    assert!(!b.excludes(FileId(0), ByteSpan::new(115, 122), &Access::Bare, SymbolId(2)));
}

/// Rule 5 exception: a receiver bound by a module / namespace import (`import * as ns`)
/// reaches the module's function.
#[test]
fn rule_member_access_never_uses_a_free_function_unless_module_import() {
    let ix = member_binding_setup(true);
    let b = Bindings::new(&ix, &[SymbolId(2)]);
    let ns = Access::Member {
        receiver_root: Some("ns".into()),
        self_receiver: false,
    };
    assert!(!b.excludes(FileId(0), ByteSpan::new(143, 150), &ns, SymbolId(2)));
    let c = scan(&ix, &[SymbolId(2)]);
    // `this.handler` is still member_binding; `ns.handler` stays open.
    assert_eq!(c.elsewhere_reasons.get(MEMBER_BINDING), Some(&1), "{c:?}");
    assert_eq!(c.unresolved.len(), 1);
    assert_eq!(c.unresolved[0].at.start_byte, 143);
    // A bare name never denotes a member in JS/TS / Python outside the class body.
    let bare = Bindings::new(&ix, &[SymbolId(1)]);
    assert!(bare.excludes(FileId(1), ByteSpan::new(5, 8), &Access::Bare, SymbolId(1)));
}

/// Rule 7: sites whose candidates are all members of the family (overloads) are uses of
/// the family (inferred, `family_overloads`), unless a proven edge is at the site, a
/// candidate lies outside the family, or the set is truncated.
#[test]
fn rule_family_candidates_confirm_overload_calls() {
    use crate::test_support::{index, push_site, sym};
    use SymbolKind::*;
    let symbols = vec![
        sym(0, 0, "Node.cs", "Node", Class, None, None),
        sym(1, 0, "Node.cs", "Node.Clone", Method, None, Some(0)),
        sym(2, 0, "Node.cs", "Node.Clone", Method, None, Some(0)),
        sym(3, 1, "Use.cs", "Use.Copy", Method, None, None),
        sym(4, 1, "Use.cs", "Other.Clone", Method, None, None),
    ];
    let mut ix = index(&["Node.cs", "Use.cs"], symbols, &[]);
    let s = push_site(&mut ix, "node.Clone", SiteCategory::NoTarget, 3, &[1, 2]) as usize;
    let family: HashSet<SymbolId> = [1, 2].into_iter().map(SymbolId).collect();
    let edges = family_candidate_edges(&ix, &family, Tier::Inferred);
    assert_eq!(edges.len(), 1);
    let e = &edges[0];
    assert_eq!((e.from, e.to, e.tier, e.site), (SymbolId(3), SymbolId(1), Tier::Inferred, Some(s as u32)));
    assert_eq!(family_resolution(e), "family_overloads");
    assert!(family_candidate_edges(&ix, &family, Tier::Proven).is_empty());

    let mut outside = ix.clone();
    outside.sites[s].candidates.push(SymbolId(4));
    assert!(family_candidate_edges(&outside, &family, Tier::Inferred).is_empty());
    let mut truncated = ix.clone();
    truncated.sites[s].truncated_candidates = true;
    assert!(family_candidate_edges(&truncated, &family, Tier::Inferred).is_empty());
    let mut proven = ix.clone();
    proven.edges.push(Edge {
        from: SymbolId(3),
        to: SymbolId(4),
        kind: EdgeKind::Calls,
        tier: Tier::Proven,
        provider: Provider::Pyright,
        resolution: Resolution::CallHierarchy,
        at: proven.sites[s].at,
        site: None,
        bridge: None,
    });
    assert!(family_candidate_edges(&proven, &family, Tier::Inferred).is_empty());

    // A server ambiguity naming only family members (>= 2) is a family use too.
    let mut ambiguous = ix.clone();
    ambiguous.sites.clear();
    ambiguous.decisions.clear();
    let mut u = unresolved(1, 340, 350, UnresolvedKind::ExternalOrAmbiguous, 3);
    u.candidates = vec![SymbolId(1), SymbolId(2)];
    ambiguous.unresolved.push(u.clone());
    let edges = family_candidate_edges(&ambiguous, &family, Tier::Inferred);
    assert_eq!(edges.len(), 1);
    assert_eq!((edges[0].from, edges[0].site), (SymbolId(3), None));
    u.candidates.truncate(1);
    ambiguous.unresolved = vec![u];
    assert!(family_candidate_edges(&ambiguous, &family, Tier::Inferred).is_empty());
}

/// Rule 19: `complete` only when every same-name site is resolved; otherwise the summary
/// says exactly what is left.
#[test]
fn rule_complete_only_when_every_site_resolved() {
    let mut index = setup();
    index.files[2].support = SupportLevel::Pending;
    let facts = index.files[2].facts.as_mut().unwrap();
    facts.references.push(reference(410, "login", false));
    facts.calls.push(call(5, "store.login", "login", 460));
    facts.calls.push(call(5, "other.login", "login", 480));
    let c = scan(&index, &[SymbolId(2)]);
    assert_eq!(c.status, "partial");
    assert_eq!(
        c.summary,
        "partial: 4 same-name sites unresolved (3 calls, 1 read; 3 in 1 file not analyzed yet (Python); run a query on one of them to set it up)"
    );
    assert_eq!(c.unresolved.len(), 4);
    assert_eq!(c.resolved_elsewhere, c.elsewhere_reasons.values().sum::<usize>());
    // Resolve them: every row span counts as the target.
    let spans = [
        (FileId(0), ByteSpan::new(45, 50)),
        (FileId(2), ByteSpan::new(410, 415)),
        (FileId(2), ByteSpan::new(455, 460)),
        (FileId(2), ByteSpan::new(475, 480)),
    ];
    let done = scan_with(&index, &[SymbolId(2)], &spans).completeness;
    assert_eq!(done.status, "complete", "{done:?}");
    assert!(done.summary.starts_with("complete: all 6 name matches resolved"), "{}", done.summary);
}

#[test]
fn value_flow_inside_one_family_counts_for_the_family() {
    use crate::test_support::{index, push_site, sym};
    use SymbolKind::*;
    let symbols = vec![
        sym(0, 0, "provider.py", "JSONProvider", Class, None, None),
        sym(1, 0, "provider.py", "JSONProvider.loads", Method, None, Some(0)),
        sym(2, 0, "provider.py", "DefaultJSONProvider.loads", Method, None, None),
        sym(3, 1, "tests/test_basic.py", "test_json_dump_dataclass", Function, None, None),
        sym(4, 2, "tests/test_json.py", "CustomProvider.loads", Method, None, None),
        sym(5, 3, "tag.py", "TaggedJSONSerializer.loads", Method, None, None),
    ];
    let mut ix = index(&["provider.py", "tests/test_basic.py", "tests/test_json.py", "tag.py"], symbols, &[]);
    let s = push_site(&mut ix, "loads", SiteCategory::NoTarget, 3, &[1, 2, 4, 5]) as usize;
    ix.sites[s].flow_candidates = vec![SymbolId(2), SymbolId(4)];
    let family: HashSet<SymbolId> = [1, 2, 4].into_iter().map(SymbolId).collect();
    let edges = family_flow_edges(&ix, &family, Tier::Inferred);
    assert_eq!(edges.len(), 1);
    assert_eq!((edges[0].from, edges[0].tier, edges[0].site), (SymbolId(3), Tier::Inferred, Some(s as u32)));
    assert_eq!(family_resolution(&edges[0]), "family_flow");
    assert!(family_flow_edges(&ix, &family, Tier::Proven).is_empty());

    let mut outside = ix.clone();
    outside.sites[s].flow_candidates.push(SymbolId(5));
    assert!(family_flow_edges(&outside, &family, Tier::Inferred).is_empty());
    let mut truncated = ix.clone();
    truncated.sites[s].truncated_candidates = true;
    assert!(family_flow_edges(&truncated, &family, Tier::Inferred).is_empty());
    let mut weak = ix.clone();
    weak.sites[s].field_only = vec![SymbolId(2), SymbolId(4)];
    assert!(family_flow_edges(&weak, &family, Tier::Inferred).is_empty());
    let mut decided = ix.clone();
    decided.decisions[s].status = DecisionStatus::Decided;
    decided.decisions[s].targets = vec![SymbolId(2)];
    assert!(family_flow_edges(&decided, &family, Tier::Inferred).is_empty());
}

/// JS `picker.set_selection = function(row)` inside `internal.colorscheme`: a
/// store into a local value's field, not a separate entity; nested functions and methods
/// are declarations.
#[test]
fn field_stores_inside_functions_are_writes() {
    use crate::test_support::{index, sym};
    use SymbolKind::*;
    let symbols = vec![
        sym(0, 0, "b.js", "internal.colorscheme", Function, None, None),
        sym(1, 0, "b.js", "internal.colorscheme.picker.set_selection", Method, None, Some(0)),
        sym(2, 0, "b.js", "internal.colorscheme.helper", Function, None, Some(0)),
        sym(3, 1, "p.js", "Picker", Class, None, None),
        sym(4, 1, "p.js", "Picker.set_selection", Method, None, Some(3)),
    ];
    let ix = index(&["b.js", "p.js"], symbols, &[]);
    assert!(stores_into_value(&ix, SymbolId(1)));
    assert!(!stores_into_value(&ix, SymbolId(2)));
    assert!(!stores_into_value(&ix, SymbolId(4)));
    // Free functions vs members.
    assert!(is_free_function(&ix, SymbolId(2)));
    assert!(!is_free_function(&ix, SymbolId(1)));
    assert!(is_member(&ix, SymbolId(4)) && is_member(&ix, SymbolId(1)));
}

#[test]
fn call_access_without_member_facts_reads_the_callee_separator() {
    let facts = FileFacts::default();
    let c = call(0, "self.store.save", "save", 40);
    assert_eq!(
        call_access(&facts, &c, member_span(&c)),
        Access::Member {
            receiver_root: None,
            self_receiver: true
        }
    );
    let c = call(0, "obj->save", "save", 40);
    assert_eq!(
        call_access(&facts, &c, member_span(&c)),
        Access::Member {
            receiver_root: Some("obj".into()),
            self_receiver: false
        }
    );
    let c = call(0, "Type::save", "save", 40);
    assert_eq!(call_access(&facts, &c, member_span(&c)), Access::Unknown);
    let c = call(0, "save", "save", 40);
    assert_eq!(call_access(&facts, &c, member_span(&c)), Access::Bare);
}

// ------------------------------------------------------------ negative evidence

use trace_core::facts::{ArgSlot, Argument, CallDetail, Declaration, Expr, Param, ParamKind};
use trace_core::model::{ExecutionModel, LibraryReceiver, Span};
use trace_core::semantics::{FileSemantics, SemLibraryDispatch};

fn semantics() -> FileSemantics {
    FileSemantics {
        provider: Provider::Pyright,
        tool_fingerprint: String::new(),
        edges: Vec::new(),
        unresolved: Vec::new(),
        value_refs: Vec::new(),
        diagnostics: Vec::new(),
        implementations: Vec::new(),
        resolved_elsewhere: Vec::new(),
        callback_params: Vec::new(),
        library_files: Vec::new(),
        library_calls: Vec::new(),
        outside_build: None,
        expanded: Vec::new(),
        library_dispatch: Vec::new(),
        library_bases: Vec::new(),
    }
}

fn declaration(name: &str, qualified: &str, kind: SymbolKind, params: &[&str]) -> Declaration {
    Declaration {
        name: name.into(),
        qualified_name: qualified.into(),
        kind,
        span: Span {
            bytes: ByteSpan::new(0, 1),
            start_line: 1,
            end_line: 1,
        },
        name_span: ByteSpan::new(0, 1),
        body_start: 1,
        parent: None,
        container: None,
        doc: None,
        decorators: Vec::new(),
        bases: Vec::new(),
        parameters: params
            .iter()
            .map(|p| Param {
                name: p.to_string(),
                kind: ParamKind::Positional,
                has_default: false,
            })
            .collect(),
        execution: ExecutionModel::Ordinary,
        is_stub: false,
        is_test: false,
        declaration_lines: Vec::new(),
        identifiers: Vec::new(),
    }
}

/// Call details aligned with the calls of `facts`; call `i` gets `positional` plain
/// positional arguments plus, with `keyword`, one keyword argument.
fn arguments(facts: &mut FileFacts, i: usize, positional: u32, keyword: bool) {
    while facts.call_details.len() < facts.calls.len() {
        let call = facts.call_details.len() as u32;
        facts.call_details.push(CallDetail {
            call,
            receiver: None,
            arguments: Vec::new(),
            callee_path: None,
            not_identical: Vec::new(),
        });
    }
    let mut args: Vec<Argument> = (0..positional)
        .map(|index| Argument {
            slot: ArgSlot::Positional { index, exact: true },
            span: ByteSpan::new(0, 0),
            value: Expr::Opaque,
            has_string: false,
        })
        .collect();
    if keyword {
        args.push(Argument {
            slot: ArgSlot::Keyword("key".into()),
            span: ByteSpan::new(0, 0),
            value: Expr::Opaque,
            has_string: false,
        });
    }
    facts.calls[i].arg_count = args.len() as u32;
    facts.call_details[i].arguments = args;
}

/// Rule `other_language` (I-34): a same-name occurrence in a language that cannot name
/// any family member without a bridge is resolved elsewhere, never listed; languages
/// sharing one namespace (C / C++, JavaScript / TypeScript) and
/// contract files stay listed.
#[test]
fn rule_name_candidates_stay_in_the_language_family() {
    let mut index = setup();
    index.symbols[2].language = Language::Rust;
    index.files[1].language = Language::Rust;
    let c = scan(&index, &[SymbolId(2)]);
    assert_eq!(c.status, "complete", "{c:?}");
    assert_eq!(c.elsewhere_reasons.get(OTHER_LANGUAGE), Some(&1), "{c:?}");
    assert_eq!(c.elsewhere_reasons.get(SERVER), Some(&1), "{c:?}");
    // One namespace: a C caller of a C++ member stays listed.
    index.symbols[2].language = Language::Cpp;
    index.files[1].language = Language::Cpp;
    index.files[0].language = Language::C;
    let c = scan(&index, &[SymbolId(2)]);
    assert_eq!(c.status, "partial", "{c:?}");
    assert!(!c.elsewhere_reasons.contains_key(OTHER_LANGUAGE), "{c:?}");
    let set = |l: Language| -> BTreeSet<Language> { [l].into_iter().collect() };
    assert!(other_language(Language::Python, &set(Language::Rust)));
    assert!(!other_language(Language::TypeScript, &set(Language::JavaScript)));
    assert!(!other_language(Language::Cpp, &set(Language::C)));
    assert!(!other_language(Language::Proto, &set(Language::Rust)));
    assert!(!other_language(Language::Python, &set(Language::Proto)));
    // A possible bridge edge to the family keeps the occurrence listed.
    let bridged = Evidence {
        other_language: true,
        possible_target: true,
        possible_bridge: true,
        ..Evidence::default()
    };
    assert_eq!(decide(&bridged, false, false, &mut || None, "x"), State::Unresolved("possible_only"));
    let plain = Evidence {
        other_language: true,
        possible_target: true,
        ..Evidence::default()
    };
    assert_eq!(decide(&plain, false, false, &mut || None, "x"), State::Elsewhere(OTHER_LANGUAGE));
}

/// Rule `library_object` (I-03 b): a call whose receiver value comes only from objects a
/// library created (`Index::library_receivers`, value flow) never runs a repository
/// member; negative: a possible edge that value flow delivered to the family keeps it
/// listed (conflicting evidence), and without the flow fact it stays open.
#[test]
fn rule_library_created_receiver_rules_out_repository_members() {
    let mut index = setup();
    let open = scan(&index, &[SymbolId(2)]);
    assert_eq!(open.status, "partial", "{open:?}");
    index.library_receivers.push(LibraryReceiver {
        at: Location {
            file: FileId(0),
            bytes: ByteSpan::new(39, 50),
            line: 1,
        },
        library: "http.request".into(),
    });
    let c = scan(&index, &[SymbolId(2)]);
    assert_eq!(c.status, "complete", "{c:?}");
    assert_eq!(c.elsewhere_reasons.get(LIBRARY_OBJECT), Some(&1), "{c:?}");
    // Value flow delivers the family member at the same call: conflicting evidence,
    // the call stays listed (with the reason of its unresolved entry).
    let s = crate::test_support::push_site(&mut index, "other.login", SiteCategory::Flow, 0, &[2]) as usize;
    index.sites[s].at.bytes = ByteSpan::new(39, 50);
    index.sites[s].flow_candidates = vec![SymbolId(2)];
    let c = scan(&index, &[SymbolId(2)]);
    assert_eq!(c.status, "partial", "{c:?}");
    assert!(c.unresolved.iter().any(|u| u.at.start_byte == 45), "{c:?}");
}

/// Rule I-45: a possible edge to the target that value flow delivered is always listed
/// under `check:`, whatever a syntax rule says; a name-only candidate yields to the
/// syntax rule's positive evidence.
#[test]
fn rule_possible_edge_to_target_is_listed_under_check() {
    let flow = Evidence {
        possible_target: true,
        flow_target: true,
        ..Evidence::default()
    };
    let mut asked = false;
    let state = decide(
        &flow,
        false,
        false,
        &mut || {
            asked = true;
            Some(UNRELATED_TYPE)
        },
        "x",
    );
    assert_eq!(state, State::Unresolved("possible_only"));
    assert!(!asked, "conflicting evidence: the syntax rules are not asked");
    let by_name = Evidence {
        possible_target: true,
        ..Evidence::default()
    };
    assert_eq!(decide(&by_name, false, false, &mut || Some(ARITY), "x"), State::Elsewhere(ARITY));
    assert_eq!(decide(&by_name, false, false, &mut || None, "x"), State::Unresolved("possible_only"));
}

/// Rule `arity` (I-03 d): a call whose plain positional arguments no family member of
/// that name accepts is resolved elsewhere; negatives: an accepted count, and a call
/// with a keyword argument (not judged).
#[test]
fn rule_arity_mismatch_rules_out_a_name_match() {
    let mut index = setup();
    // auth.py: Session (decl 0), Session.login(self, a) (decl 1), Session._check (decl 2).
    for (i, s) in index.symbols.iter_mut().enumerate() {
        if s.file == FileId(1) {
            s.decl = i as u32 - 1;
        }
    }
    let auth = index.files[1].facts.as_mut().unwrap();
    auth.declarations = vec![
        declaration("Session", "Session", SymbolKind::Class, &[]),
        declaration("login", "Session.login", SymbolKind::Method, &["self", "a"]),
        declaration("_check", "Session._check", SymbolKind::Method, &["self"]),
    ];
    let with_args = |positional: u32, keyword: bool| {
        let mut ix = index.clone();
        arguments(ix.files[0].facts.as_mut().unwrap(), 1, positional, keyword);
        scan(&ix, &[SymbolId(2)])
    };
    let c = with_args(3, false);
    assert_eq!(c.status, "complete", "{c:?}");
    assert_eq!(c.elsewhere_reasons.get(ARITY), Some(&1), "{c:?}");
    // `other.login(1, 2)`: `Session.login(obj, a)` through the class accepts two.
    let c = with_args(2, false);
    assert_eq!(c.status, "partial", "{c:?}");
    assert!(!c.elsewhere_reasons.contains_key(ARITY), "{c:?}");
    let c = with_args(3, true);
    assert_eq!(c.status, "partial", "{c:?}");
    assert!(!c.elsewhere_reasons.contains_key(ARITY), "{c:?}");
}

/// Index with `app.py` (a bare call `redirect()` at 122..130 in `main`, optionally its
/// own top-level `def redirect`) and the target `helpers.py:redirect`; returns the index
/// and the target id.
fn scope_setup(own_def: bool) -> (Index, SymbolId) {
    use crate::test_support::{index, sym};
    use SymbolKind::*;
    let mut symbols = vec![sym(0, 0, "app.py", "main", Function, None, None)];
    if own_def {
        symbols.push(sym(1, 0, "app.py", "redirect", Function, None, None));
    }
    let target = symbols.len() as u32;
    symbols.push(sym(target, 1, "helpers.py", "redirect", Function, None, None));
    symbols.push(sym(target + 1, 2, "other.py", "redirect", Function, None, None));
    let mut ix = index(&["app.py", "helpers.py", "other.py"], symbols, &[]);
    let mut app = FileFacts::default();
    app.calls.push(call(0, "redirect", "redirect", 130));
    ix.files[0].facts = Some(app);
    ix.files[1].facts = Some(FileFacts::default());
    ix.files[2].facts = Some(FileFacts::default());
    (ix, SymbolId(target))
}

/// `from x import redirect` in app.py (statement 0..30, imported identifier 20..28).
fn import_redirect(ix: &mut Index) {
    let facts = ix.files[0].facts.as_mut().unwrap();
    facts.imports.push(Import {
        local: "redirect".into(),
        target: "x.redirect".into(),
        kind: ImportKind::Member,
        scope: Scope::Module,
        span: ByteSpan::new(0, 30),
        line: 1,
    });
    let mut r = reference(20, "redirect", false);
    r.kind = RefKind::Import;
    facts.references.push(r);
}

/// Rule `other_scope` (I-03 c): a bare name that a nearer binding of its own file denotes
/// (its own `def`, or an import proven to bind another symbol) is not the target;
/// negative: an import that proves nothing keeps the call listed.
#[test]
fn rule_nearer_scope_binding_rules_out_the_target() {
    let (ix, target) = scope_setup(true);
    let c = scan(&ix, &[target]);
    assert_eq!(c.status, "complete", "{c:?}");
    assert_eq!(c.elsewhere_reasons.get(OTHER_SCOPE), Some(&1), "{c:?}");

    // Without a nearer binding the bare call stays open.
    let (ix, target) = scope_setup(false);
    let c = scan(&ix, &[target]);
    assert_eq!(c.status, "partial", "{c:?}");

    // An import proven to bind another symbol (`other.py:redirect`).
    let (mut ix, target) = scope_setup(false);
    import_redirect(&mut ix);
    let mut unknown = ix.clone();
    let other = SymbolId(target.0 + 1);
    ix.edges.push(Edge {
        from: SymbolId(0),
        to: other,
        kind: EdgeKind::Imports,
        tier: Tier::Proven,
        provider: Provider::Pyright,
        resolution: Resolution::ImportPath,
        at: Location {
            file: FileId(0),
            bytes: ByteSpan::new(20, 28),
            line: 1,
        },
        site: None,
        bridge: None,
    });
    let c = scan(&ix, &[target]);
    assert_eq!(c.status, "complete", "{c:?}");
    assert_eq!(c.elsewhere_reasons.get(OTHER_SCOPE), Some(&1), "{c:?}");

    // The same import without evidence proves nothing: the call stays listed.
    let c = scan(&unknown, &[target]);
    assert_eq!(c.status, "partial", "{c:?}");
    assert!(c.unresolved.iter().any(|u| u.kind == "call"), "{c:?}");
    // The server resolved the imported name outside the index: another binding.
    let mut sem = semantics();
    sem.resolved_elsewhere = vec![ByteSpan::new(20, 28)];
    unknown.files[0].semantic = Some(sem);
    let c = scan(&unknown, &[target]);
    assert_eq!(c.elsewhere_reasons.get(OTHER_SCOPE), Some(&1), "{c:?}");
}

/// I-02: a call the server reported as a dispatch through a library-declared member,
/// with a family member among the implementations, is never `server` elsewhere: it is
/// listed (`possible_only`) until an edge decides it, in `uses` and `deps` alike;
/// negative: implementations outside the family leave the external verdict.
#[test]
fn rule_library_dispatch_occurrence_is_not_resolved_elsewhere() {
    let mut index = setup();
    index.files[0].facts.as_mut().unwrap().calls.remove(1);
    index.unresolved.remove(0);
    let before = scan(&index, &[SymbolId(2)]);
    assert_eq!(before.status, "complete", "{before:?}");
    let dispatch = |uid: &str| SemLibraryDispatch {
        owner: 0,
        at: ByteSpan::new(60, 70),
        line: 1,
        library_symbol: Some("json.Decoder.login".into()),
        implementations: vec![uid.to_string()],
    };
    let mut outside = index.clone();
    let mut sem = semantics();
    sem.library_dispatch = vec![dispatch("store.py:Store.load")];
    outside.files[0].semantic = Some(sem);
    assert_eq!(scan(&outside, &[SymbolId(2)]).status, "complete");

    let mut sem = semantics();
    sem.library_dispatch = vec![dispatch("auth.py:Session.login")];
    index.files[0].semantic = Some(sem);
    let c = scan(&index, &[SymbolId(2)]);
    assert_eq!(c.status, "partial", "{c:?}");
    let u = c.unresolved.iter().find(|u| u.at.start_byte == 65).expect("listed");
    assert_eq!(u.reason, "possible_only");
    // `deps`: the dispatch call inside a reached function is not external either.
    let graph = Graph::new(&index);
    let sources = SourceStore::new(&index);
    let reached: HashSet<SymbolId> = [SymbolId(0)].into_iter().collect();
    let d = calls_completeness(&graph, &sources, &reached, Tier::Inferred, None);
    assert_eq!(d.status, "partial", "{d:?}");
    assert!(d.unresolved.iter().any(|u| u.reason == "possible_only"), "{d:?}");
}
