//! Tests for [`crate::sites`] (no sources read, no paid calls).

use super::*;
use crate::test_support::{Decl, Fixture};
use trace_core::facts::CallbackArg;
use trace_core::Language;

#[test]
fn member_spellings() {
    assert_eq!(member_of("self.store.save"), Some("save"));
    assert_eq!(member_of("helper"), Some("helper"));
    assert_eq!(member_of("items[0]"), None);
    assert_eq!(member_of("make()()"), None);
    assert_eq!(member_of("Vec::new"), Some("new"));
}

#[test]
fn site_ids_are_stable_hex() {
    let id = site_id(&[json!("no_target"), json!("a.py:f"), json!("a.py"), json!(10)]);
    assert_eq!(id.0.len(), 20);
    assert!(id.0.chars().all(|c| c.is_ascii_hexdigit()));
    let again = site_id(&[json!("no_target"), json!("a.py:f"), json!("a.py"), json!(10)]);
    assert_eq!(id, again);
    let other = site_id(&[json!("no_target"), json!("a.py:f"), json!("a.py"), json!(11)]);
    assert_ne!(id, other);
}

/// tests_v3/test_inference.py fixture (dispatch, no_target, callback), no sources read.
#[test]
fn dispatch_no_target_and_callback_sites() {
    let mut fx = Fixture::new();
    let f = fx.file("m.py", Language::Python, None);
    let store = fx.decl(f, Decl::class("Store").bases(&["Protocol"]));
    let save = fx.decl(f, Decl::method("save", store).params(&["self", "value"]).stub());
    fx.decl(f, Decl::method("load", store).params(&["self"]).stub());
    let disk = fx.decl(f, Decl::class("DiskStore"));
    let disk_save = fx.decl(f, Decl::method("save", disk).params(&["self", "value"]));
    fx.decl(f, Decl::method("load", disk).params(&["self"]));
    let mem = fx.decl(f, Decl::class("MemoryStore"));
    let mem_save = fx.decl(f, Decl::method("save", mem).params(&["self", "value"]));
    fx.decl(f, Decl::method("load", mem).params(&["self"]));
    let use_ = fx.decl(f, Decl::function("use").params(&["store"]));
    let info = fx.decl(f, Decl::function("info"));
    let dynamic = fx.decl(f, Decl::function("dynamic").params(&["obj"]));
    let check = fx.decl(f, Decl::function("check").params(&["value"]));
    let run = fx.decl(f, Decl::function("run").params(&["value"]));

    let save_span = fx.span();
    let call_span = fx.span();
    fx.call_site(f, Some(use_), call_span, save_span, "store.save");
    fx.edge(f, use_, save, EdgeKind::Calls, save_span, 1);

    let info_span = fx.span();
    let info_call = fx.span();
    fx.call_site(f, Some(dynamic), info_call, info_span, "obj.info");
    fx.unresolved(f, dynamic, info_span, 1, "obj.info");

    let run_sync = fx.span();
    let run_call = fx.span();
    let arg = fx.span();
    fx.call_site(f, Some(run), run_call, run_sync, "anyio.to_thread.run_sync");
    fx.callback(
        f,
        CallbackArg {
            call_callee_span: run_sync,
            callee: "anyio.to_thread.run_sync".into(),
            arg_span: arg,
            argument: "check".into(),
            name: "check".into(),
            owner: Some(run.decl),
            index: None,
            keyword: None,
        },
    );
    fx.edge(f, run, check, EdgeKind::PassesCallback, arg, 1);

    let index = fx.build();
    let sources = SourceStore::new(&index);
    let sites = generate(&index, &sources).unwrap();
    let cats: Vec<SiteCategory> = sites.iter().map(|s| s.category).collect();
    assert_eq!(cats, vec![SiteCategory::Dispatch, SiteCategory::Callback, SiteCategory::NoTarget]);
    let d = &sites[0];
    assert_eq!(d.candidates, vec![fx.id(disk_save), fx.id(mem_save)]);
    assert_eq!(d.declared_target, Some(fx.id(save)));
    assert_eq!(d.callee, "store.save");
    assert_eq!(d.at.bytes, save_span);
    let cb = &sites[1];
    assert_eq!(cb.candidates, vec![fx.id(check)]);
    assert_eq!(cb.argument.as_deref(), Some("check"));
    assert_eq!(cb.activation, EdgeKind::InvokedCallback);
    assert_eq!(cb.at.bytes, run_sync);
    let nt = &sites[2];
    assert_eq!(nt.candidates, vec![fx.id(info)]);
    assert_eq!(nt.callee, "obj.info");
    // Candidates never become proven edges.
    assert!(!index.edges.iter().any(|e| e.to == fx.id(info)));
    // Ids are unique.
    let ids: HashSet<&SiteId> = sites.iter().map(|s| &s.id).collect();
    assert_eq!(ids.len(), sites.len());
}

#[test]
fn flow_candidates_merge_into_no_target_sites() {
    use trace_core::facts::{BindTarget, FlowFact, Scope};
    let mut fx = Fixture::new();
    let f = fx.file("b.py", Language::Python, None);
    let handler = fx.decl(f, Decl::function("handler").params(&["x"]));
    let other = fx.decl(f, Decl::function("fn"));
    let boxed = fx.decl(f, Decl::class("Box"));
    let init = fx.decl(f, Decl::method("__init__", boxed).params(&["self", "fn"]));
    let run = fx.decl(f, Decl::method("run", boxed).params(&["self"]));
    for m in [init, run] {
        fx.flow(
            f,
            FlowFact::ImplicitSelf {
                function: m.decl,
                param: "self".into(),
                class: boxed.decl,
                is_class: false,
            },
        );
    }
    // def __init__(self, fn=handler): self.fn = fn
    let default = fx.name_ref(f, "handler", handler);
    fx.flow(
        f,
        FlowFact::Bind {
            target: BindTarget::Var {
                scope: Scope::Decl(init.decl),
                name: "fn".into(),
            },
            value: default,
            scope: Scope::Module,
        },
    );
    let value = fx.name("fn");
    let obj = fx.name("self");
    fx.flow(
        f,
        FlowFact::Bind {
            target: BindTarget::FieldOf {
                object: obj,
                name: "fn".into(),
            },
            value,
            scope: Scope::Decl(init.decl),
        },
    );
    // def run(self): return self.fn(1)   -> no semantic target
    let recv = fx.name("self");
    let func = fx.attr(recv, "fn");
    let call = fx.call(func, vec![]);
    let at = fx.eval(f, run, call, "self.fn");
    fx.unresolved(f, run, at, 1, "self.fn");

    let index = fx.build();
    let sources = SourceStore::new(&index);
    let sites = generate(&index, &sources).unwrap();
    assert_eq!(sites.len(), 1, "flow merged into the no_target site");
    let s = &sites[0];
    assert_eq!(s.category, SiteCategory::NoTarget);
    assert_eq!(s.candidates, vec![fx.id(other), fx.id(handler)]);
    assert_eq!(s.flow_candidates, vec![fx.id(handler)]);
    assert!(s.field_only.is_empty());
}

/// server 08-webhook: `partial(payments.parse_webhook, ...)` passes an abstract method; the
/// callback site composes a dispatch site over its implementations.
#[test]
fn callbacks_to_abstract_methods_compose_dispatch() {
    let mut fx = Fixture::new();
    let f = fx.file("payments.py", Language::Python, None);
    let provider = fx.decl(f, Decl::class("PaymentProvider").bases(&["ABC"]));
    let parse = fx.decl(
        f,
        Decl::method("parse_webhook", provider)
            .params(&["self", "payload"])
            .stub(),
    );
    let stripe = fx.decl(f, Decl::class("StripePaymentProvider").bases(&["PaymentProvider"]));
    let stripe_parse = fx.decl(f, Decl::method("parse_webhook", stripe).params(&["self", "payload"]));
    let handle = fx.decl(f, Decl::function("handle").params(&["payments", "secret"]));
    let partial = fx.span();
    let call = fx.span();
    let arg = fx.span();
    fx.call_site(f, Some(handle), call, partial, "partial");
    fx.callback(
        f,
        CallbackArg {
            call_callee_span: partial,
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

    let index = fx.build();
    let sources = SourceStore::new(&index);
    let sites = generate(&index, &sources).unwrap();
    assert_eq!(sites.len(), 2);
    let cb = &sites[0];
    assert_eq!(cb.category, SiteCategory::Callback);
    assert_eq!(cb.candidates, vec![fx.id(parse)]);
    assert_eq!(cb.via, None);
    let d = &sites[1];
    assert_eq!(d.category, SiteCategory::Dispatch);
    assert_eq!(d.declared_target, Some(fx.id(parse)));
    assert_eq!(d.candidates, vec![fx.id(stripe_parse)]);
    assert_eq!(d.via, Some(0));
    assert_eq!(d.at, cb.at);
    assert_eq!(d.activation, EdgeKind::InvokedCallback);
    assert_eq!(d.argument.as_deref(), Some("payments.parse_webhook"));
}

/// A callback to an overridden method composes an override-dispatch site.
#[test]
fn callbacks_to_overridden_methods_compose_overrides() {
    let mut fx = Fixture::new();
    let f = fx.file("jobs.py", Language::Python, None);
    let base = fx.decl(f, Decl::class("Job"));
    let run = fx.decl(f, Decl::method("run", base).params(&["self"]));
    let nightly = fx.decl(f, Decl::class("NightlyJob").bases(&["Job"]));
    let nightly_run = fx.decl(f, Decl::method("run", nightly).params(&["self"]));
    let start = fx.decl(f, Decl::method("start", base).params(&["self"]));
    let submit = fx.span();
    let call = fx.span();
    let arg = fx.span();
    fx.call_site(f, Some(start), call, submit, "executor.submit");
    fx.callback(
        f,
        CallbackArg {
            call_callee_span: submit,
            callee: "executor.submit".into(),
            arg_span: arg,
            argument: "self.run".into(),
            name: "run".into(),
            owner: Some(start.decl),
            index: None,
            keyword: None,
        },
    );
    fx.edge(f, start, run, EdgeKind::PassesCallback, arg, 1);
    let index = fx.build();
    let sources = SourceStore::new(&index);
    let sites = generate(&index, &sources).unwrap();
    assert_eq!(sites.len(), 2);
    let o = &sites[1];
    assert_eq!(o.category, SiteCategory::Flow);
    assert_eq!(o.operation, Some(SiteOperation::OverrideDispatch));
    assert_eq!(o.candidates, vec![fx.id(nightly_run)]);
    assert_eq!(o.declared_target, Some(fx.id(run)));
    assert_eq!(o.via, Some(0));
}

/// boltons 07-remap: same-named test helpers never compete with product candidates.
/// (The call names a free function: a call through a parameter of the same name denotes
/// that binding, DESIGN §4.11 item 4a, and gets its candidates from value flow.)
#[test]
fn test_code_candidates_are_test_only() {
    let mut fx = Fixture::new();
    let f = fx.file("boltons/iterutils.py", Language::Python, None);
    let c = fx.file("boltons/cli.py", Language::Python, None);
    let t = fx.file("tests/test_iterutils.py", Language::Python, None);
    let remap = fx.decl(f, Decl::function("remap").params(&["root"]));
    let product_exit = fx.decl(c, Decl::function("exit").params(&["code"]));
    let test_a = fx.decl(t, Decl::function("test_a").test());
    let exit_a = fx.decl(t, Decl::nested("exit", test_a).params(&["p", "k", "v"]));
    let helper_exit = fx.decl(t, Decl::function("exit").params(&["p", "k", "v"]));
    let callee = fx.span();
    let call = fx.span();
    fx.call_site(f, Some(remap), call, callee, "exit");
    fx.unresolved(f, remap, callee, 1, "exit");
    // A test calling an unresolved `exit` keeps normal candidates.
    let t_callee = fx.span();
    let t_call = fx.span();
    fx.call_site(t, Some(test_a), t_call, t_callee, "exit");
    fx.unresolved(t, test_a, t_callee, 1, "exit");

    let index = fx.build();
    let sources = SourceStore::new(&index);
    let sites = generate(&index, &sources).unwrap();
    let product = sites.iter().find(|s| s.owner == fx.id(remap)).unwrap();
    // `exit_a` is nested in `test_a`: not visible to a bare call elsewhere (lexical nesting).
    assert_eq!(product.candidates, vec![fx.id(product_exit), fx.id(helper_exit)]);
    assert_eq!(product.test_only, vec![fx.id(helper_exit)]);
    let in_test = sites.iter().find(|s| s.owner == fx.id(test_a)).unwrap();
    assert!(in_test.test_only.is_empty());
    // Inside `test_a` its nested `exit` is visible.
    assert!(in_test.candidates.contains(&fx.id(exit_a)), "{:?}", in_test.candidates);
}

/// server 10-compaction: a lambda passed to a consuming parameter is a callback site; the
/// call inside the settling function is a flow site over the lambda.
#[test]
fn lambda_callbacks_become_sites() {
    let mut fx = Fixture::new();
    let f = fx.file("compaction.py", Language::Python, None);
    let settle = fx.decl(f, Decl::function("settle").params(&["fn"]));
    let compact = fx.decl(f, Decl::function("compact"));
    let lambda = fx.decl(f, Decl::lambda(compact));
    let fn_ = fx.name("fn");
    let call = fx.call(fn_, vec![]);
    fx.eval(f, settle, call, "fn");
    let callee = fx.name_ref(f, "settle", settle);
    let arg = fx.lambda_expr(lambda);
    let call = fx.call(callee, vec![arg]);
    let at = fx.eval(f, compact, call, "settle");
    fx.edge(f, compact, settle, EdgeKind::Calls, at, 1);
    let index = fx.build();
    let sources = SourceStore::new(&index);
    let sites = generate(&index, &sources).unwrap();
    let cats: Vec<SiteCategory> = sites.iter().map(|s| s.category).collect();
    assert_eq!(cats, vec![SiteCategory::Callback, SiteCategory::Flow]);
    assert_eq!(sites[0].owner, fx.id(compact));
    assert_eq!(sites[0].candidates, vec![fx.id(lambda)]);
    assert_eq!(sites[0].activation, EdgeKind::InvokedCallback);
    assert_eq!(sites[1].owner, fx.id(settle));
    assert_eq!(sites[1].candidates, vec![fx.id(lambda)]);
}

/// Rust calls the server leaves unresolved reach the trait method through the receiver's
/// declared shape: `(**self).matched(..)` inside `impl<S: Sink + ?Sized> Sink for &mut S`,
/// a parameter of a generic type bounded by `Sink` (`fn search<S: Sink>(sink: S)`), `&mut
/// dyn Sink` and `Box<dyn Sink>` (dereferenced) parameters. An unrelated receiver type and
/// `self.matched()` inside `impl Sink for JsonSink` (a declared type) get nothing. The
/// candidate merges into the call's no_target site as its only flow candidate.
#[test]
fn rule_deref_and_generic_bound_receivers_reach_the_trait_method() {
    use trace_core::facts::{RefKind, Scope, TypeSource, TypeSubject};
    use trace_core::ByteSpan;
    let mut fx = Fixture::new();
    let f = fx.file("a/src/lib.rs", Language::Rust, None);
    let b = fx.file("b/src/lib.rs", Language::Rust, None);
    let sink = fx.decl(f, Decl::interface("Sink"));
    let matched = fx.decl(f, Decl::method("matched", sink).params(&["self", "line"]).stub());
    let context = fx.decl(f, Decl::method("context", sink).params(&["self", "line"]));
    let var = |d: crate::test_support::D, name: &str| TypeSubject::Var {
        scope: Scope::Decl(d.decl),
        name: name.into(),
    };
    // impl<S: Sink + ?Sized> Sink for &mut S { fn matched(&mut self, line) { (**self).matched(line) } }
    fx.impl_block(f, "S", "Sink", ByteSpan::new(500, 700));
    fx.reference_at(f, None, "Sink", RefKind::Type, ByteSpan::new(508, 512));
    let forward = fx.decl(
        f,
        Decl::function("matched")
            .container("S")
            .qualified("S.matched")
            .params(&["self", "line"])
            .span(ByteSpan::new(540, 600)),
    );
    let deref_at = fx.call_n(f, Some(forward), "(**self).matched", 1);
    fx.unresolved(f, forward, deref_at, 1, "(**self).matched");
    // fn search<S: Sink>(mut sink: S) { sink.matched(..) }
    let search = fx.decl(
        f,
        Decl::function("search")
            .params(&["sink"])
            .span(ByteSpan::new(800, 900)),
    );
    fx.reference_at(f, Some(search), "Sink", RefKind::Type, ByteSpan::new(812, 816));
    fx.type_fact(f, var(search, "sink"), "S", ByteSpan::new(830, 831), TypeSource::Declared);
    let generic_at = fx.call_n(f, Some(search), "sink.matched", 1);
    fx.unresolved(f, search, generic_at, 1, "sink.matched");
    // fn run_dyn(sink: &mut dyn Sink) { sink.context(..) }
    let run_dyn = fx.decl(
        f,
        Decl::function("run_dyn")
            .params(&["sink"])
            .span(ByteSpan::new(1000, 1100)),
    );
    fx.type_fact(f, var(run_dyn, "sink"), "&mut dyn Sink", ByteSpan::new(1010, 1023), TypeSource::Declared);
    let dyn_at = fx.call_n(f, Some(run_dyn), "sink.context", 1);
    fx.unresolved(f, run_dyn, dyn_at, 1, "sink.context");
    // fn run_boxed(sink: Box<dyn Sink>) { (*sink).matched(..) }
    let run_boxed = fx.decl(
        f,
        Decl::function("run_boxed")
            .params(&["sink"])
            .span(ByteSpan::new(1200, 1300)),
    );
    fx.type_fact(f, var(run_boxed, "sink"), "Box", ByteSpan::new(1210, 1224), TypeSource::Declared);
    fx.reference_at(f, Some(run_boxed), "Sink", RefKind::Type, ByteSpan::new(1218, 1222));
    let boxed_at = fx.call_n(f, Some(run_boxed), "(*sink).matched", 1);
    fx.unresolved(f, run_boxed, boxed_at, 1, "(*sink).matched");
    // Not reached: an unrelated receiver type.
    let other = fx.decl(f, Decl::function("other").params(&["v"]).span(ByteSpan::new(1400, 1500)));
    fx.type_fact(f, var(other, "v"), "Vec", ByteSpan::new(1410, 1413), TypeSource::Declared);
    let vec_at = fx.call_n(f, Some(other), "v.matched", 1);
    fx.unresolved(f, other, vec_at, 1, "v.matched");
    // crate b: impl Sink for JsonSink { fn matched(&mut self) { self.matched() } }
    fx.decl(b, Decl::class("JsonSink"));
    fx.impl_block(b, "JsonSink", "Sink", ByteSpan::new(100, 300));
    fx.value_ref(b, ByteSpan::new(105, 109), sink);
    let json = fx.decl(
        b,
        Decl::function("matched")
            .container("JsonSink")
            .qualified("JsonSink.matched")
            .params(&["self"])
            .span(ByteSpan::new(120, 200)),
    );
    let self_at = fx.call_n(b, Some(json), "self.matched", 0);
    fx.unresolved(b, json, self_at, 1, "self.matched");
    let index = fx.build();

    let h = Hierarchy::build(&index);
    let rules = crate::flow::receiver_rule_candidates(&index, &h);
    let at = |span: ByteSpan| -> Vec<SymbolId> {
        rules
            .iter()
            .filter(|c| c.span == span)
            .flat_map(|c| c.candidates.iter().copied())
            .collect()
    };
    assert_eq!(at(deref_at), vec![fx.id(matched)]);
    assert_eq!(at(generic_at), vec![fx.id(matched)]);
    assert_eq!(at(dyn_at), vec![fx.id(context)]);
    assert_eq!(at(boxed_at), vec![fx.id(matched)]);
    assert!(at(vec_at).is_empty());
    assert!(at(self_at).is_empty());
    let owner_of = |span: ByteSpan| rules.iter().find(|c| c.span == span).map(|c| c.owner);
    assert_eq!(owner_of(deref_at), Some(fx.id(forward)));

    let sources = SourceStore::new(&index);
    let sites = generate(&index, &sources).unwrap();
    let site = sites
        .iter()
        .find(|s| s.at.bytes == deref_at && s.owner == fx.id(forward))
        .expect("site of (**self).matched");
    assert_eq!(site.flow_candidates, vec![fx.id(matched)]);
    assert!(site.candidates.contains(&fx.id(matched)));
    // Candidates are never proven edges.
    assert!(!index
        .edges
        .iter()
        .any(|e| e.to == fx.id(matched) && !trace_core::tiers::FAMILY.contains(e.kind)));
}

/// Library knowledge of one call (`path`, callee start) with `effects`.
fn knowledge_of(
    path: &str,
    start: u32,
    effects: Vec<trace_library::Effect>,
    inferred: bool,
) -> LibraryKnowledge {
    let mut k = LibraryKnowledge::default();
    k.by_call.insert(
        (path.to_string(), start),
        trace_library::CallBehaviour {
            symbol: Some("lib.run".into()),
            effects,
            source: trace_library::BehaviourSource::Derived,
            inferred,
            reason: String::new(),
        },
    );
    k
}

/// `main` passes `handler` (argument 0) to `lib.run`, which the server answered only with
/// a declaration in an installed library.
fn library_callback_fixture(language: Language, path: &str) -> (Index, ByteSpan) {
    let mut fx = Fixture::new();
    let f = fx.file(path, language, None);
    let handler = fx.decl(f, Decl::function("handler"));
    let main = fx.decl(f, Decl::function("main"));
    let callee = fx.span();
    let call = fx.span();
    let arg = fx.span();
    fx.call_site(f, Some(main), call, callee, "lib.run");
    fx.callback(
        f,
        CallbackArg {
            call_callee_span: callee,
            callee: "lib.run".into(),
            arg_span: arg,
            argument: "handler".into(),
            name: "handler".into(),
            owner: Some(main.decl),
            index: Some(0),
            keyword: None,
        },
    );
    fx.edge(f, main, handler, EdgeKind::PassesCallback, arg, 1);
    fx.library_call(f, callee, "lib.run");
    (fx.build(), callee)
}

fn sites_with(index: &Index, knowledge: &LibraryKnowledge) -> Vec<Site> {
    let sources = SourceStore::new(index);
    let h = Hierarchy::build(index);
    let none = LibraryInputs::none();
    let library = LibraryInputs {
        knowledge,
        tables: none.tables,
        installed: none.installed,
    };
    generate_report(index, &sources, &h, library).unwrap().sites
}

/// DESIGN 1.10 item 6, every language: a callback site whose receiving call is a library
/// call carries the library behaviour of its argument (`Site::library`): derived "calls"
/// with the gate passed -> decided (inferred); the same fact below the gate or no knowledge
/// at all -> the site stays possible.
#[test]
fn rule_library_callbacks_carry_their_behaviour() {
    use trace_library::{ArgSel, Effect};
    for (language, path) in [
        (Language::Python, "app.py"),
        (Language::TypeScript, "src/app.ts"),
        (Language::Go, "main.go"),
        (Language::Php, "lib/app.php"),
    ] {
        let (mut index, callee) = library_callback_fixture(language, path);
        let runs = knowledge_of(path, callee.start, vec![Effect::Calls(ArgSel::Pos(0))], true);
        let sites = sites_with(&index, &runs);
        let cb = sites
            .iter()
            .find(|s| s.category == SiteCategory::Callback)
            .unwrap_or_else(|| panic!("{path}: callback site"));
        let b = cb.library.clone().expect("library behaviour");
        assert_eq!((b.source.as_str(), b.effect.as_str(), b.inferred), ("derived", "calls", true), "{path}");
        index.sites = sites;
        let d = crate::decide::decide(&index);
        assert_eq!(d[0].status, trace_core::model::DecisionStatus::Decided, "{path}");

        for k in [
            knowledge_of(path, callee.start, vec![Effect::Calls(ArgSel::Pos(0))], false),
            LibraryKnowledge::default(),
        ] {
            index.sites = sites_with(&index, &k);
            let b = index.sites[0].library.clone().expect("library call without evidence");
            assert!(!b.inferred, "{path}");
            let d = crate::decide::decide(&index);
            assert_eq!(d[0].status, trace_core::model::DecisionStatus::Unknown, "{path}");
            assert_eq!(d[0].reason.as_deref(), Some(crate::decide::LIBRARY_UNKNOWN_REASON));
        }
    }
}

/// PLAN decision 13: incremental site generation equals the full generation: unchanged
/// inputs reuse the previous sites (mapped by uid), a changed file regenerates them.
#[test]
fn rule_incremental_sites_equal_full() {
    use trace_core::delta::{IdRemap, IndexDelta};
    let build = |extra: bool| {
        let mut fx = Fixture::new();
        let f = fx.file("m.py", Language::Python, None);
        let store = fx.decl(f, Decl::class("Store").bases(&["Protocol"]));
        let save = fx.decl(f, Decl::method("save", store).params(&["self", "value"]).stub());
        let disk = fx.decl(f, Decl::class("DiskStore"));
        fx.decl(f, Decl::method("save", disk).params(&["self", "value"]));
        if extra {
            let memory = fx.decl(f, Decl::class("MemoryStore"));
            fx.decl(f, Decl::method("save", memory).params(&["self", "value"]));
        }
        let g = fx.file("run.py", Language::Python, None);
        let run = fx.decl(g, Decl::function("run"));
        let at = fx.span();
        fx.edge(g, run, save, EdgeKind::Calls, at, 3);
        fx.build()
    };
    let index = build(false);
    let sources = SourceStore::new(&index);
    let h = Hierarchy::build(&index);
    let full = generate_report(&index, &sources, &h, LibraryInputs::none()).unwrap();
    let remap = IdRemap::identity(index.files.len(), index.symbols.len());
    let (first, state) = generate_delta(
        &index,
        &sources,
        &h,
        LibraryInputs::none(),
        SitesState::default(),
        &remap,
        &IndexDelta::full(),
    )
    .unwrap();
    assert_eq!(first.sites, full.sites);
    assert!(!first.sites.is_empty());
    // Re-queried with identical answers: the previous sites are reused.
    let requeried = IndexDelta {
        requeried: ["run.py".to_string()].into_iter().collect(),
        ..IndexDelta::default()
    };
    let (again, state) =
        generate_delta(&index, &sources, &h, LibraryInputs::none(), state, &remap, &requeried).unwrap();
    assert!(again.stats.reused);
    assert_eq!(again.sites, full.sites);
    // Library receivers are carried by the reused state like the sites (incremental = full).
    assert_eq!(again.library_receivers, full.library_receivers);
    assert_eq!(first.library_receivers, full.library_receivers);
    // An edit adds an implementation: the sites are generated again.
    let edited = build(true);
    let sources = SourceStore::new(&edited);
    let h = Hierarchy::build(&edited);
    let modified = IndexDelta {
        modified: ["m.py".to_string()].into_iter().collect(),
        ..IndexDelta::default()
    };
    let (next, _) =
        generate_delta(&edited, &sources, &h, LibraryInputs::none(), state, &remap, &modified).unwrap();
    assert!(!next.stats.reused);
    let expected = generate_report(&edited, &sources, &h, LibraryInputs::none()).unwrap();
    assert_eq!(next.sites, expected.sites);
    assert_ne!(next.sites, full.sites);
}

/// I-23: several `passes_callback` edges inside one argument (the server reported the
/// argument at the identifier and at an inner position) are one callback site per target:
/// the site id names the argument, not the edge position.
#[test]
fn rule_one_callback_site_per_argument() {
    use trace_core::ByteSpan;
    let mut fx = Fixture::new();
    let f = fx.file("src/app.ts", Language::TypeScript, None);
    let handler = fx.decl(f, Decl::function("errorHandler"));
    let main = fx.decl(f, Decl::function("main"));
    let callee = ByteSpan::new(500, 510);
    fx.call_site(f, Some(main), ByteSpan::new(500, 540), callee, "app.onError");
    let arg = ByteSpan::new(515, 535);
    fx.callback(
        f,
        CallbackArg {
            call_callee_span: callee,
            callee: "app.onError".into(),
            arg_span: arg,
            argument: "this.errorHandler".into(),
            name: "errorHandler".into(),
            owner: Some(main.decl),
            index: Some(0),
            keyword: None,
        },
    );
    fx.edge(f, main, handler, EdgeKind::PassesCallback, arg, 1);
    fx.edge(f, main, handler, EdgeKind::PassesCallback, ByteSpan::new(523, 535), 1);
    let index = fx.build();
    let sources = SourceStore::new(&index);
    let sites = generate(&index, &sources).unwrap();
    let callbacks: Vec<&Site> = sites
        .iter()
        .filter(|s| s.category == SiteCategory::Callback)
        .collect();
    assert_eq!(callbacks.len(), 1, "{callbacks:?}");
    assert_eq!(callbacks[0].candidates, vec![fx.id(handler)]);
    assert_eq!(callbacks[0].at.bytes, callee);
}

/// A library abstract member called in `perform` (`r.serve()`), whose in-index
/// implementations the server reported (`FileSemantics::library_dispatch`).
fn library_dispatch_fixture(with_flow: bool) -> (Index, ByteSpan, [SymbolId; 3]) {
    let mut fx = Fixture::new();
    let f = fx.file("server.py", Language::Python, None);
    let engine = fx.decl(f, Decl::class("Engine"));
    let engine_serve = fx.decl(f, Decl::method("serve", engine).params(&["self"]));
    let other = fx.decl(f, Decl::class("Other"));
    let other_serve = fx.decl(f, Decl::method("serve", other).params(&["self"]));
    let perform = fx.decl(f, Decl::function("perform").params(&["r"]));
    let main = fx.decl(f, Decl::function("main"));
    fx.receiver(f, engine_serve, "self", engine, false);
    fx.receiver(f, other_serve, "self", other, false);
    // def perform(r): r.serve()
    let r = fx.name("r");
    let func = fx.attr(r, "serve");
    let call = fx.call(func, vec![]);
    let at = fx.eval(f, perform, call, "r.serve");
    fx.library_call(f, at, "lib.Handler.serve");
    if with_flow {
        // def main(): perform(Engine())
        let callee = fx.name_ref(f, "perform", perform);
        let ctor = fx.name_ref(f, "Engine", engine);
        let arg = fx.call(ctor, vec![]);
        let call = fx.call(callee, vec![arg]);
        let perform_at = fx.eval(f, main, call, "perform");
        fx.edge(f, main, perform, EdgeKind::Calls, perform_at, 1);
    }
    let ids = [fx.id(perform), fx.id(engine_serve), fx.id(other_serve)];
    let uids = [fx.uid(engine_serve), fx.uid(other_serve)];
    let mut index = fx.build();
    let file = index.file_by_path("server.py").expect("file");
    let perform_decl = index.symbol(ids[0]).decl;
    index.files[file.idx()]
        .semantic
        .as_mut()
        .expect("semantics")
        .library_dispatch
        .push(trace_core::semantics::SemLibraryDispatch {
            owner: perform_decl,
            at,
            line: 1,
            library_symbol: Some("lib.Handler.serve".into()),
            implementations: uids.to_vec(),
        });
    (index, at, ids)
}

/// I-02: a call of a library-declared abstract member whose repository implementations the
/// server reported is a dispatch site over them (`declared_target` None, `declared_library`
/// the library symbol); without receiver evidence it stays possible, even though the
/// library may call its own implementations. A receiver that provably is a library-created
/// object gets no dispatch site.
#[test]
fn rule_library_interface_call_becomes_dispatch_site() {
    let (mut index, at, [perform, engine_serve, other_serve]) = library_dispatch_fixture(false);
    let sources = SourceStore::new(&index);
    let sites = generate(&index, &sources).unwrap();
    let d = sites
        .iter()
        .find(|s| s.category == SiteCategory::Dispatch)
        .expect("library dispatch site");
    assert_eq!(d.owner, perform);
    assert_eq!(d.at.bytes, at);
    assert_eq!(d.declared_target, None);
    assert_eq!(d.declared_library.as_deref(), Some("lib.Handler.serve"));
    assert_eq!(d.activation, EdgeKind::Calls);
    let mut expected = vec![engine_serve, other_serve];
    expected.sort_by(|a, b| index.symbol(*a).uid.cmp(&index.symbol(*b).uid));
    assert_eq!(d.candidates, expected);
    assert_eq!(sites.iter().filter(|s| s.at.bytes == at).count(), 1);
    index.sites = sites;
    let decisions = crate::decide::decide(&index);
    let i = index
        .sites
        .iter()
        .position(|s| s.category == SiteCategory::Dispatch)
        .unwrap();
    assert_eq!(decisions[i].status, trace_core::model::DecisionStatus::Unknown);
    let file = index.file_by_path("server.py").unwrap();
    index.library_receivers.push(trace_core::LibraryReceiver {
        at: Location {
            file,
            bytes: at,
            line: 1,
        },
        library: "lib.make".into(),
    });
    let sources = SourceStore::new(&index);
    let sites = generate(&index, &sources).unwrap();
    assert!(!sites.iter().any(|s| s.category == SiteCategory::Dispatch));
}

/// I-01 / I-02: value flow reaching the receiver (`perform(Engine())` binds `r` to an
/// `Engine`) merges into the library dispatch site and decides `Engine.serve` (inferred);
/// the other implementation stays possible. No separate flow site remains at the call.
#[test]
fn rule_library_dispatch_decided_by_argument_flow() {
    let (mut index, at, [_, engine_serve, other_serve]) = library_dispatch_fixture(true);
    let sources = SourceStore::new(&index);
    let sites = generate(&index, &sources).unwrap();
    let at_call: Vec<&Site> = sites.iter().filter(|s| s.at.bytes == at && s.via.is_none()).collect();
    assert_eq!(at_call.len(), 1, "{at_call:?}");
    let d = at_call[0];
    assert_eq!(d.category, SiteCategory::Dispatch);
    assert_eq!(d.flow_candidates, vec![engine_serve]);
    assert!(d.candidates.contains(&other_serve));
    index.sites = sites;
    let decisions = crate::decide::decide(&index);
    let i = index
        .sites
        .iter()
        .position(|s| s.category == SiteCategory::Dispatch)
        .unwrap();
    assert_eq!(decisions[i].status, trace_core::model::DecisionStatus::Decided);
    assert_eq!(decisions[i].targets, vec![engine_serve]);
    assert_eq!(decisions[i].reason.as_deref(), Some(crate::decide::RECEIVER_TYPES_REASON));
}

/// I-02 family fallback: a server without implementation answers (no `library_dispatch`
/// entry in the index): a repository class whose base names no index type implements the
/// library base's members of the same name; a library call whose symbol names
/// `<base>.<member>` becomes a dispatch site over them. A call of another member does not,
/// and constructors never implement a library member.
#[test]
fn rule_library_base_family_without_implementation_capability() {
    let mut fx = Fixture::new();
    let f = fx.file("handlers.py", Language::Python, None);
    let mine = fx.decl(f, Decl::class("MyHandler").bases(&["socketserver.BaseRequestHandler"]));
    let handle = fx.decl(f, Decl::method("handle", mine).params(&["self"]));
    fx.decl(f, Decl::method("__init__", mine).params(&["self"]));
    let run = fx.decl(f, Decl::function("run").params(&["h"]));
    let at = fx.call_n(f, Some(run), "h.handle", 0);
    fx.library_call(f, at, "socketserver.BaseRequestHandler.handle");
    let other = fx.call_n(f, Some(run), "h.finish", 0);
    fx.library_call(f, other, "socketserver.BaseRequestHandler.finish");
    let index = fx.build();
    let h = Hierarchy::build(&index);
    let expected = vec![fx.id(handle)];
    assert_eq!(h.library_implementations("socketserver.BaseRequestHandler.handle"), expected.as_slice());
    assert!(h
        .library_implementations("socketserver.BaseRequestHandler.__init__")
        .is_empty());
    let sources = SourceStore::new(&index);
    let sites = generate(&index, &sources).unwrap();
    let d: Vec<&Site> = sites
        .iter()
        .filter(|s| s.category == SiteCategory::Dispatch)
        .collect();
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].at.bytes, at);
    assert_eq!(d[0].candidates, expected);
    assert_eq!(d[0].owner, fx.id(run));
    assert_eq!(d[0].declared_library.as_deref(), Some("socketserver.BaseRequestHandler.handle"));
}

/// I-35: a library call whose (derived) behaviour calls a method of an argument
/// (`calls_method`) links to that method of the argument's flow types: a callback site with
/// the library behaviour, decided (inferred) when the fact passed the gate.
#[test]
fn rule_library_calling_argument_method_links_to_the_repository_method() {
    use trace_library::{ArgSel, Effect};
    let mut fx = Fixture::new();
    let path = "app.py";
    let f = fx.file(path, Language::Python, None);
    let engine = fx.decl(f, Decl::class("Engine"));
    let handle = fx.decl(f, Decl::method("handle", engine).params(&["self", "request"]));
    fx.decl(f, Decl::class("Other"));
    let main = fx.decl(f, Decl::function("main"));
    fx.receiver(f, handle, "self", engine, false);
    // def main(): lib.serve(Engine())
    let ctor = fx.name_ref(f, "Engine", engine);
    let arg = fx.call(ctor, vec![]);
    let arg_span = arg.span().expect("argument span");
    let lib = fx.name("lib");
    let func = fx.attr(lib, "serve");
    let call = fx.call(func, vec![arg]);
    let at = fx.eval(f, main, call, "lib.serve");
    fx.library_call(f, at, "lib.serve");
    fx.callback(
        f,
        CallbackArg {
            call_callee_span: at,
            callee: "lib.serve".into(),
            arg_span,
            argument: "Engine()".into(),
            name: "Engine".into(),
            owner: Some(main.decl),
            index: Some(0),
            keyword: None,
        },
    );
    let mut index = fx.build();
    let knowledge = knowledge_of(
        path,
        at.start,
        vec![Effect::CallsMethod {
            arg: ArgSel::Pos(0),
            method: "handle".into(),
        }],
        true,
    );
    let sites = sites_with(&index, &knowledge);
    let to_handle = |s: &Site| s.category == SiteCategory::Callback && s.candidates == vec![fx.id(handle)];
    let cb = sites
        .iter()
        .find(|s| to_handle(s))
        .unwrap_or_else(|| panic!("callback site to Engine.handle: {sites:?}"));
    let b = cb.library.clone().expect("library behaviour");
    assert_eq!((b.effect.as_str(), b.inferred), ("calls_method", true));
    index.sites = sites;
    let decisions = crate::decide::decide(&index);
    let i = index.sites.iter().position(to_handle).unwrap();
    assert_eq!(decisions[i].status, trace_core::model::DecisionStatus::Decided);
}

/// I-01: a dispatch call whose receiver has a declared concrete type in a statically typed
/// language (`A a = ...; a.matched()` resolved by the server to `Sink.matched`) is decided
/// by the receiver-type rule: `receiver_exact`, the one implementation `A.matched`.
#[test]
fn rule_dispatch_with_declared_concrete_receiver_is_proven() {
    use trace_core::facts::{Expr, Scope, TypeSource, TypeSubject};
    use trace_core::ByteSpan;
    let mut fx = Fixture::new();
    let f = fx.file("src/Sinks.java", Language::Java, None);
    let sink = fx.decl(f, Decl::interface("Sink").span(ByteSpan::new(10, 60)));
    let matched = fx.decl(f, Decl::method("matched", sink).stub().span(ByteSpan::new(20, 40)));
    let a = fx.decl(f, Decl::class("A").bases(&["Sink"]).span(ByteSpan::new(100, 200)));
    let a_matched = fx.decl(f, Decl::method("matched", a).span(ByteSpan::new(120, 180)));
    let b = fx.decl(f, Decl::class("B").bases(&["Sink"]).span(ByteSpan::new(300, 400)));
    let b_matched = fx.decl(f, Decl::method("matched", b).span(ByteSpan::new(320, 380)));
    let run = fx.decl(f, Decl::function("run").span(ByteSpan::new(650, 800)));
    let callee = ByteSpan::new(700, 709);
    fx.call_site(f, Some(run), ByteSpan::new(700, 711), callee, "a.matched");
    fx.call_receiver(
        f,
        callee,
        Expr::Name {
            name: "a".into(),
            span: ByteSpan::new(700, 701),
        },
    );
    fx.type_fact(
        f,
        TypeSubject::Var {
            scope: Scope::Decl(run.decl),
            name: "a".into(),
        },
        "A",
        ByteSpan::new(660, 661),
        TypeSource::Declared,
    );
    fx.edge(f, run, matched, EdgeKind::Calls, callee, 1);
    let index = fx.build();
    let sources = SourceStore::new(&index);
    let sites = generate(&index, &sources).unwrap();
    let d = sites
        .iter()
        .find(|s| s.category == SiteCategory::Dispatch)
        .expect("dispatch site");
    assert_eq!(d.candidates, vec![fx.id(a_matched), fx.id(b_matched)]);
    assert!(d.receiver_exact, "{d:?}");
    assert_eq!(d.flow_candidates, vec![fx.id(a_matched)]);
}

/// Step 1: a dispatch site sits at the call whose callee span is the edge's span: in a chain
/// `c.db().stop()` the outer callee starts where the inner `c.db` does, and the site (its
/// callee text and receiver evidence) is the outer call's.
#[test]
fn rule_dispatch_site_is_the_call_the_edge_spans() {
    use trace_core::ByteSpan;
    let mut fx = Fixture::new();
    let f = fx.file("src/Run.java", Language::Java, None);
    let db = fx.decl(f, Decl::class("Db").span(ByteSpan::new(10, 60)));
    let stop = fx.decl(f, Decl::method("stop", db).stub().span(ByteSpan::new(20, 40)));
    let native = fx.decl(f, Decl::class("NativeDb").bases(&["Db"]).span(ByteSpan::new(100, 200)));
    let native_stop = fx.decl(f, Decl::method("stop", native).span(ByteSpan::new(120, 180)));
    let run = fx.decl(f, Decl::function("run").span(ByteSpan::new(650, 800)));
    let inner = ByteSpan::new(700, 704);
    let outer = ByteSpan::new(700, 711);
    fx.call_site(f, Some(run), ByteSpan::new(700, 706), inner, "c.db");
    fx.call_site(f, Some(run), ByteSpan::new(700, 713), outer, "c.db().stop");
    fx.edge(f, run, stop, EdgeKind::Calls, outer, 1);
    let index = fx.build();
    let sources = SourceStore::new(&index);
    let sites = generate(&index, &sources).unwrap();
    let d = sites
        .iter()
        .find(|s| s.category == SiteCategory::Dispatch)
        .expect("dispatch site");
    assert_eq!(d.at.bytes, outer);
    assert_eq!(d.callee, "c.db().stop");
    assert_eq!(d.candidates, vec![fx.id(native_stop)]);
}

/// Value-flow settings other than the defaults are part of the sites key: changing them
/// regenerates the sites instead of reusing ones solved under other bounds; the defaults
/// leave the key as it is.
#[test]
fn rule_non_default_flow_settings_regenerate_sites() {
    let (index, _) = library_callback_fixture(Language::Python, "m.py");
    let none = LibraryInputs::none();
    let defaults = trace_core::config::defaults().flow.clone();
    let key = global_fingerprint_with(&index, &none, &defaults);
    assert_eq!(key, global_fingerprint(&index, &none));
    let mut other = defaults;
    other.max_iterations += 1;
    assert_ne!(global_fingerprint_with(&index, &none, &other), key);
}
