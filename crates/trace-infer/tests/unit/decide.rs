use super::*;
use trace_core::model::{ByteSpan, EdgeKind, FileId, Location, SiteId};

/// The decision of one site outside an index (the data-model protocol rule never applies).
fn decide_one(site: &Site, site_index: u32) -> Decision {
    decide_site(site, site_index, &|_| false)
}

#[test]
fn rule_unique_candidate_decides() {
    let mut site = Site {
        id: SiteId("x".into()),
        category: SiteCategory::NoTarget,
        owner: SymbolId(0),
        declared_target: None,
        activation: EdgeKind::Calls,
        at: Location {
            file: FileId(0),
            bytes: ByteSpan::new(0, 1),
            line: 1,
        },
        callee: "f".into(),
        candidates: vec![SymbolId(1)],
        flow_candidates: vec![],
        field_only: vec![],
        truncated_candidates: false,
        operation: None,
        argument: None,
        via: None,
        test_only: vec![],
        receiver_exact: false,
        library: None,
        declared_library: None,
    };
    let d = decide_one(&site, 3);
    assert_eq!((d.site, d.status), (3, DecisionStatus::Decided));
    assert_eq!(d.targets, vec![SymbolId(1)]);
    site.field_only = vec![SymbolId(1)];
    let d = decide_one(&site, 3);
    assert_eq!(d.status, DecisionStatus::Unknown);
    assert_eq!(d.reason.as_deref(), Some("only field-name evidence"));
    site.candidates.push(SymbolId(2));
    assert_eq!(decide_one(&site, 3).reason.as_deref(), Some("abstain: not unique"));
    // Test-only candidates are never options.
    site.field_only.clear();
    site.test_only = vec![SymbolId(2)];
    let d = decide_one(&site, 3);
    assert_eq!(d.status, DecisionStatus::Decided);
    assert_eq!(d.targets, vec![SymbolId(1)]);
    site.test_only = vec![SymbolId(1), SymbolId(2)];
    let d = decide_one(&site, 3);
    assert_eq!(d.status, DecisionStatus::Unknown);
    assert_eq!(d.reason.as_deref(), Some("only test-origin candidates"));
    assert!(options(&site).is_empty());
    // A name pool with two options, value flow reaching exactly one of them.
    site.test_only.clear();
    site.flow_candidates = vec![SymbolId(2)];
    let d = decide_one(&site, 3);
    assert_eq!((d.status, d.targets.clone()), (DecisionStatus::Decided, vec![SymbolId(2)]));
    assert_eq!(d.reason.as_deref(), Some(FLOW_UNIQUE_REASON));
    // Field-name-only flow evidence, a bounded set or two flow targets: abstain.
    site.field_only = vec![SymbolId(2)];
    assert_eq!(decide_one(&site, 3).status, DecisionStatus::Unknown);
    site.field_only.clear();
    site.truncated_candidates = true;
    assert_eq!(decide_one(&site, 3).status, DecisionStatus::Unknown);
    site.truncated_candidates = false;
    site.flow_candidates = vec![SymbolId(1), SymbolId(2)];
    assert_eq!(decide_one(&site, 3).status, DecisionStatus::Unknown);
}

fn dispatch_site(candidates: &[u32]) -> Site {
    Site {
        id: SiteId("d".into()),
        category: SiteCategory::Dispatch,
        owner: SymbolId(0),
        declared_target: Some(SymbolId(9)),
        activation: EdgeKind::Calls,
        at: Location {
            file: FileId(0),
            bytes: ByteSpan::new(0, 1),
            line: 1,
        },
        callee: "self.sink.matched".into(),
        candidates: candidates.iter().map(|&c| SymbolId(c)).collect(),
        flow_candidates: vec![],
        field_only: vec![],
        truncated_candidates: false,
        operation: None,
        argument: None,
        via: None,
        test_only: vec![],
        receiver_exact: false,
        library: None,
        declared_library: None,
    }
}

/// I-01: a dispatch site whose receiver type is proven decides its one implementation
/// with the proven reason.
#[test]
fn rule_dispatch_with_proven_receiver_type_is_proven() {
    let mut site = dispatch_site(&[1, 2, 3]);
    site.receiver_exact = true;
    site.flow_candidates = vec![SymbolId(2)];
    let d = decide_one(&site, 0);
    assert_eq!((d.status, d.targets.clone()), (DecisionStatus::Decided, vec![SymbolId(2)]));
    assert_eq!(d.reason.as_deref(), Some(RECEIVER_TYPE_PROVEN_REASON));
    // The flag never decides every option of a dispatch site.
    assert!(!receiver_exact(&site));
}

/// I-01: receiver evidence reaching some implementations (value flow's known receivers,
/// or a type narrowing the family) decides each of them, inferred; the rest stay possible.
/// Test-only and field-name-only targets never count; a truncated set decides nothing.
#[test]
fn rule_dispatch_with_flow_types_is_inferred_per_type() {
    let mut site = dispatch_site(&[1, 2, 3]);
    site.flow_candidates = vec![SymbolId(1), SymbolId(3)];
    let d = decide_one(&site, 0);
    assert_eq!(d.status, DecisionStatus::Decided);
    assert_eq!(d.targets, vec![SymbolId(1), SymbolId(3)]);
    assert_eq!(d.reason.as_deref(), Some(RECEIVER_TYPES_REASON));
    site.test_only = vec![SymbolId(3)];
    site.field_only = vec![SymbolId(1)];
    assert_eq!(decide_one(&site, 0).status, DecisionStatus::Unknown);
    site.test_only.clear();
    site.field_only.clear();
    site.truncated_candidates = true;
    assert_eq!(decide_one(&site, 0).status, DecisionStatus::Unknown);
}

/// I-01 / I-02: without receiver evidence a dispatch site with several implementations
/// stays undecided (possible); one through a library-declared member stays undecided even
/// with a single repository implementation (the library may implement it too), while an
/// in-repository interface with one implementation keeps the unique rule.
#[test]
fn rule_undecided_dispatch_is_possible_and_listed() {
    let site = dispatch_site(&[1, 2]);
    assert_eq!(decide_one(&site, 0).status, DecisionStatus::Unknown);
    let mut single = dispatch_site(&[1]);
    assert_eq!(decide_one(&single, 0).status, DecisionStatus::Decided);
    single.declared_target = None;
    single.declared_library = Some("net/http.Handler.ServeHTTP".into());
    let d = decide_one(&single, 0);
    assert_eq!(d.status, DecisionStatus::Unknown);
    assert_eq!(d.reason.as_deref(), Some(LIBRARY_DISPATCH_REASON));
    // Receiver evidence decides the library dispatch too.
    single.flow_candidates = vec![SymbolId(1)];
    assert_eq!(decide_one(&single, 0).status, DecisionStatus::Decided);
}

/// boltons 06-exception-info: one constructor per receiver context decides every option;
/// test-only candidates stay excluded, field-only evidence disables it.
#[test]
fn receiver_exact_sites_decide_every_option() {
    let mut site = Site {
        id: SiteId("r".into()),
        category: SiteCategory::Flow,
        owner: SymbolId(0),
        declared_target: None,
        activation: EdgeKind::Calls,
        at: Location {
            file: FileId(0),
            bytes: ByteSpan::new(0, 1),
            line: 1,
        },
        callee: "cls".into(),
        candidates: vec![SymbolId(1), SymbolId(2), SymbolId(3)],
        flow_candidates: vec![SymbolId(1), SymbolId(2), SymbolId(3)],
        field_only: vec![],
        truncated_candidates: false,
        operation: Some(trace_core::model::SiteOperation::Call),
        argument: None,
        via: None,
        test_only: vec![SymbolId(3)],
        receiver_exact: true,
        library: None,
        declared_library: None,
    };
    assert!(receiver_exact(&site));
    let d = decide_one(&site, 0);
    assert_eq!(d.status, DecisionStatus::Decided);
    assert_eq!(d.targets, vec![SymbolId(1), SymbolId(2)]);
    assert_eq!(d.reason.as_deref(), Some("one target per receiver context"));
    site.field_only = vec![SymbolId(2)];
    assert!(!receiver_exact(&site));
    assert_eq!(decide_one(&site, 0).status, DecisionStatus::Unknown);
    site.field_only.clear();
    site.test_only = site.candidates.clone();
    assert!(!receiver_exact(&site));
}

/// server 08-webhook: the callback and the dispatch composed through it are both decided.
#[test]
fn composed_sites_follow_their_parent() {
    use crate::test_support::{Decl, Fixture};
    use trace_core::facts::CallbackArg;
    use trace_core::Language;
    let mut fx = Fixture::new();
    let f = fx.file("payments.py", Language::Python, None);
    let provider = fx.decl(f, Decl::class("PaymentProvider"));
    let parse = fx.decl(f, Decl::method("parse_webhook", provider).stub());
    let stripe = fx.decl(f, Decl::class("Stripe").bases(&["PaymentProvider"]));
    fx.decl(f, Decl::method("parse_webhook", stripe));
    let handle = fx.decl(f, Decl::function("handle"));
    let callee = fx.span();
    let call = fx.span();
    let arg = fx.span();
    fx.call_site(f, Some(handle), call, callee, "partial");
    fx.callback(
        f,
        CallbackArg {
            call_callee_span: callee,
            callee: "partial".into(),
            arg_span: arg,
            argument: "payments.parse_webhook".into(),
            name: "parse_webhook".into(),
            owner: Some(handle.decl),
            index: None,
            keyword: None,
        },
    );
    fx.edge(f, handle, parse, EdgeKind::PassesCallback, arg, 1);
    let mut index = fx.build();
    let sources = trace_core::source::SourceStore::new(&index);
    let sites = crate::sites::generate(&index, &sources).unwrap();
    index.sites = sites;
    assert_eq!(index.sites.len(), 2);
    let decisions = decide(&index);
    assert_eq!(decisions[0].status, DecisionStatus::Decided);
    assert_eq!(decisions[1].status, DecisionStatus::Decided);
    assert_eq!(decisions[1].targets, index.sites[1].candidates);
}

/// more-itertools 05: iterating one object runs its whole protocol.
#[test]
fn implicit_protocol_sites_decide_all_methods_of_one_class() {
    use crate::test_support::{Decl, Fixture};
    use trace_core::Language;
    let mut fx = Fixture::new();
    let f = fx.file("s.py", Language::Python, None);
    let seekable = fx.decl(f, Decl::class("seekable"));
    let iter = fx.decl(f, Decl::method("__iter__", seekable));
    let next = fx.decl(f, Decl::method("__next__", seekable));
    let other = fx.decl(f, Decl::class("peekable"));
    let other_next = fx.decl(f, Decl::method("__next__", other));
    let mut index = fx.build();
    let mut site = Site {
        id: SiteId("i".into()),
        category: SiteCategory::Implicit,
        owner: fx.id(seekable),
        declared_target: None,
        activation: EdgeKind::Calls,
        at: Location {
            file: FileId(0),
            bytes: ByteSpan::new(0, 1),
            line: 1,
        },
        callee: "self".into(),
        candidates: vec![fx.id(iter), fx.id(next)],
        flow_candidates: vec![],
        field_only: vec![],
        truncated_candidates: false,
        operation: Some(trace_core::model::SiteOperation::Iterate),
        argument: None,
        via: None,
        test_only: vec![],
        receiver_exact: false,
        library: None,
        declared_library: None,
    };
    index.sites = vec![site.clone()];
    let d = decide(&index);
    assert_eq!(d[0].status, DecisionStatus::Decided);
    assert_eq!(d[0].targets, vec![fx.id(iter), fx.id(next)]);
    site.candidates.push(fx.id(other_next));
    index.sites = vec![site];
    assert_eq!(decide(&index)[0].status, DecisionStatus::Unknown);
}

fn library(source: &str, effect: &str, inferred: bool) -> trace_core::model::LibraryBehaviour {
    trace_core::model::LibraryBehaviour {
        source: source.into(),
        effect: effect.into(),
        reason: String::new(),
        symbol: Some("lib.run".into()),
        inferred,
    }
}

/// A callback site of `owner` passing `target` to a library call.
fn callback_site(
    owner: SymbolId,
    target: SymbolId,
    library: Option<trace_core::model::LibraryBehaviour>,
) -> Site {
    Site {
        id: SiteId(format!("cb-{}-{}", owner.0, target.0)),
        category: SiteCategory::Callback,
        owner,
        declared_target: None,
        activation: EdgeKind::InvokedCallback,
        at: Location {
            file: FileId(0),
            bytes: ByteSpan::new(0, 3),
            line: 1,
        },
        callee: "lib.run".into(),
        candidates: vec![target],
        flow_candidates: vec![],
        field_only: vec![],
        truncated_candidates: false,
        operation: None,
        argument: Some("handler".into()),
        via: None,
        test_only: vec![],
        receiver_exact: false,
        library,
        declared_library: None,
    }
}

/// An index with two functions (`main` passes `handler` to a library call).
fn two_functions() -> (Index, SymbolId, SymbolId) {
    use crate::test_support::{Decl, Fixture};
    let mut fx = Fixture::new();
    let f = fx.file("app.py", trace_core::Language::Python, None);
    let handler = fx.decl(f, Decl::function("handler"));
    let main = fx.decl(f, Decl::function("main"));
    (fx.build(), fx.id(main), fx.id(handler))
}

/// DESIGN 1.10 item 6: a callback into a library call without evidence (no derived fact,
/// no declared function type, no table row), or with a derived fact below the precision
/// gate, stays possible (`check:`); an in-index callee keeps the
/// unique-candidate rule.
#[test]
fn rule_library_callback_without_evidence_stays_possible() {
    let (_, main, handler) = two_functions();
    for b in [library("none", "none", false), library("derived", "calls", false)] {
        let site = callback_site(main, handler, Some(b));
        let d = decide_one(&site, 0);
        assert_eq!(d.status, DecisionStatus::Unknown);
        assert_eq!(d.reason.as_deref(), Some(LIBRARY_UNKNOWN_REASON));
    }
    let plain = callback_site(main, handler, None);
    let d = decide_one(&plain, 0);
    assert_eq!((d.status, d.targets.clone()), (DecisionStatus::Decided, vec![handler]));
}

/// Tables: a `never_calls` row overrides the function-type rule on the same argument
/// (decision side): the site is not decided; a declared function type alone decides it
/// (inferred, never proven).
#[test]
fn rule_never_calls_row_overrides_type() {
    use trace_library::{ArgSel, Effect};
    let (_, main, handler) = two_functions();
    let effects = vec![Effect::Calls(ArgSel::Pos(0)), Effect::NeverCalls(ArgSel::Pos(0))];
    let effect = crate::behaviour::effect_for(&effects, Some(0), None, 1).map(Effect::name);
    assert_eq!(effect, Some(crate::behaviour::NEVER_CALLS));
    let never = callback_site(main, handler, Some(library("table", crate::behaviour::NEVER_CALLS, true)));
    let d = decide_one(&never, 0);
    assert_eq!(d.status, DecisionStatus::Unknown);
    assert_eq!(d.reason.as_deref(), Some(LIBRARY_UNKNOWN_REASON));
    let typed = callback_site(main, handler, Some(library("declared_type", "calls", true)));
    let d = decide_one(&typed, 0);
    assert_eq!((d.status, d.targets.clone()), (DecisionStatus::Decided, vec![handler]));
    assert!(
        d.reason
            .as_deref()
            .is_some_and(|r| r.starts_with(LIBRARY_RUNS_REASON)),
        "{:?}",
        d.reason
    );
}
