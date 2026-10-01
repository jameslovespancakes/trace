//! Tests for [`crate::flow`]: the reference fixture plus one small fixture per
//! IMPROVEMENTS.md error pattern (facts lowered by hand, no sources read).

use super::*;
use crate::test_support::{Decl, Fixture, D};
use trace_core::facts::{BindTarget, Expr, FlowFact, ImplicitKind, Scope};

struct Names<'i>(&'i Index);

impl Names<'_> {
    fn of(&self, ids: &[SymbolId]) -> Vec<String> {
        ids.iter().map(|s| self.0.symbol(*s).qualified_name.clone()).collect()
    }
}

fn solve(index: &Index) -> Vec<FlowCandidate> {
    let h = Hierarchy::build(index);
    let flow = Flow::solve(index, &h);
    assert!(flow.iterations < trace_core::config::current().flow.max_iterations);
    flow.candidates()
}

/// Provider decorator of the test injection rule (a `runtime_dispatch` row's `symbol`).
const PROVIDER: &str = "runner.fixture";
/// Shared provider file of the test injection rule (the row's `glob`).
const SHARED: &str = "shared_fixtures.py";

/// The by-name injection row as the table states it (`runtime_dispatch`, pattern
/// `inject_by_parameter_name`, activated by its runner package).
fn injection_row() -> trace_library::table::IrreducibleRow {
    trace_library::table::IrreducibleRow {
        symbol: Some(PROVIDER.into()),
        pattern: Some(INJECT_BY_PARAMETER_NAME.into()),
        glob: Some(SHARED.into()),
        activated_by: Some("runner".into()),
        why_not_derivable: "reflection".into(),
        describe: "the runner injects provider values into parameters of the same name".into(),
        ..Default::default()
    }
}

/// Candidates with the injection row active (its package installed).
fn solve_injected(index: &Index) -> Vec<FlowCandidate> {
    let row = injection_row();
    let rules = injections([(Language::Python, &row)], &|_, package| package == "runner");
    assert_eq!(rules.len(), 1);
    let h = Hierarchy::build(index);
    let flow = Flow::solve_rules(index, &h, &LibraryKnowledge::default(), &rules);
    assert!(flow.iterations < trace_core::config::current().flow.max_iterations);
    flow.candidates()
}

/// Sites with the injection row active.
fn sites_injected(
    index: &Index,
    sources: &trace_core::source::SourceStore<'_>,
) -> Vec<trace_core::model::Site> {
    let row = injection_row();
    let rules = injections([(Language::Python, &row)], &|_, package| package == "runner");
    let h = Hierarchy::build(index);
    crate::sites::generate_rules(index, sources, &h, &LibraryKnowledge::default(), &rules)
        .unwrap()
        .sites
}

/// Candidates with library knowledge.
fn solve_known(index: &Index, knowledge: &LibraryKnowledge) -> Vec<FlowCandidate> {
    let h = Hierarchy::build(index);
    let flow = Flow::solve_rules(index, &h, knowledge, &[]);
    assert!(flow.iterations < trace_core::config::current().flow.max_iterations);
    flow.candidates()
}

/// Knowledge of one library call: `effects` at (path, callee start).
fn knowledge_at(entries: &[(&str, u32, Vec<Effect>, bool)]) -> LibraryKnowledge {
    let mut k = LibraryKnowledge::default();
    for (path, start, effects, inferred) in entries {
        k.by_call.insert(
            ((*path).to_string(), *start),
            trace_library::CallBehaviour {
                symbol: None,
                effects: effects.clone(),
                source: trace_library::BehaviourSource::Table,
                inferred: *inferred,
                reason: String::new(),
            },
        );
    }
    k
}

fn rows<'c>(cands: &'c [FlowCandidate], fx: &Fixture, owner: D, op: SiteOperation) -> Vec<&'c FlowCandidate> {
    cands
        .iter()
        .filter(|c| c.owner == fx.id(owner) && c.operation == op && c.via.is_none())
        .collect()
}

/// Port of tests_v3/test_flow.py `FlowCandidates` (facts lowered by hand).
#[test]
fn reference_flow_fixture() {
    let mut fx = Fixture::new();
    let f = fx.file("f.py", Language::Python, None);
    let make_key = fx.decl(f, Decl::function("make_key").params(&["args"]));
    let cached = fx.decl(f, Decl::class("Cached"));
    let cached_init = fx.decl(f, Decl::method("__init__", cached).params(&["self", "func", "key"]));
    let cached_call = fx.decl(f, Decl::method("__call__", cached).params(&["self"]));
    let default_enter = fx.decl(f, Decl::function("default_enter").params(&["value"]));
    let default_visit = fx.decl(f, Decl::function("default_visit").params(&["value"]));
    let remap = fx.decl(f, Decl::function("remap").params(&["root", "enter", "visit"]));
    let callpoint = fx.decl(f, Decl::class("Callpoint"));
    let callpoint_init = fx.decl(f, Decl::method("__init__", callpoint).params(&["self", "name"]));
    let from_tb = fx.decl(
        f,
        Decl::method("from_tb", callpoint)
            .params(&["cls", "tb"])
            .decorators(&["classmethod"]),
    );
    let info = fx.decl(f, Decl::class("Info"));
    let build = fx.decl(
        f,
        Decl::method("build", info)
            .params(&["cls", "tb"])
            .decorators(&["classmethod"]),
    );
    let store = fx.decl(f, Decl::class("Store"));
    let setitem = fx.decl(f, Decl::method("__setitem__", store).params(&["self", "key", "value"]));
    let lru = fx.decl(f, Decl::class("LRU").bases(&["Store"]));
    let fill = fx.decl(f, Decl::method("fill", lru).params(&["self", "key"]));
    let lazy = fx.decl(f, Decl::class("lazy"));
    let lazy_init = fx.decl(f, Decl::method("__init__", lazy).params(&["self", "func"]));
    let lazy_get = fx.decl(f, Decl::method("__get__", lazy).params(&["self", "obj", "objtype"]));
    let page = fx.decl(f, Decl::class("Page"));
    let title = fx.decl(f, Decl::method("title", page).params(&["self"]).decorators(&["lazy"]));
    let render = fx.decl(f, Decl::method("render", page).params(&["self"]));
    let base = fx.decl(f, Decl::class("Base"));
    let base_tell = fx.decl(f, Decl::method("tell", base).params(&["self"]));
    let check = fx.decl(f, Decl::method("check", base).params(&["self"]));
    let left = fx.decl(f, Decl::class("Left").bases(&["Base"]));
    let left_tell = fx.decl(f, Decl::method("tell", left).params(&["self"]));
    let right = fx.decl(f, Decl::class("Right").bases(&["Base"]));
    let right_tell = fx.decl(f, Decl::method("tell", right).params(&["self"]));

    for (m, c) in [
        (cached_init, cached),
        (cached_call, cached),
        (callpoint_init, callpoint),
        (setitem, store),
        (fill, lru),
        (lazy_init, lazy),
        (lazy_get, lazy),
        (title, page),
        (render, page),
        (base_tell, base),
        (check, base),
        (left_tell, left),
        (right_tell, right),
    ] {
        fx.receiver(f, m, "self", c, false);
    }
    fx.receiver(f, from_tb, "cls", callpoint, true);
    fx.receiver(f, build, "cls", info, true);

    // Cached.__init__: self.func = func; self.key_func = key or make_key
    let func = fx.name("func");
    fx.field_store(f, cached_init, "self", "func", func);
    let key = fx.name("key");
    let mk = fx.name_ref(f, "make_key", make_key);
    fx.field_store(f, cached_init, "self", "key_func", Expr::Choice(vec![key, mk]));
    // Cached.__call__: return self.key_func(args)
    let recv = fx.name("self");
    let func = fx.attr(recv, "key_func");
    let arg = fx.name("args");
    let call = fx.call(func, vec![arg]);
    fx.eval(f, cached_call, call, "self.key_func");

    // remap defaults + enter(root)
    for (p, target) in [("enter", default_enter), ("visit", default_visit)] {
        let v = fx.name_ref(f, p, target);
        fx.bind(
            f,
            BindTarget::Var {
                scope: Scope::Decl(remap.decl),
                name: p.into(),
            },
            v,
            Scope::Module,
        );
    }
    let enter = fx.name("enter");
    let root = fx.name("root");
    let call = fx.call(enter, vec![root]);
    fx.eval(f, remap, call, "enter");

    // @classmethod from_tb: cls(tb)
    for (method, class, name) in [(from_tb, callpoint, "from_tb"), (build, info, "build")] {
        let deco = fx.name("classmethod");
        fx.decorated_member(f, class, method, name, vec![deco]);
    }
    let cls = fx.name("cls");
    let tb = fx.name("tb");
    let call = fx.call(cls, vec![tb]);
    fx.eval(f, from_tb, call, "cls");

    // Info.point_type = Callpoint; build: cls.point_type.from_tb(tb)
    let v = fx.name_ref(f, "Callpoint", callpoint);
    fx.member(f, info, "point_type", v);
    let cls = fx.name("cls");
    let pt = fx.attr(cls, "point_type");
    let func = fx.attr(pt, "from_tb");
    let tb = fx.name("tb");
    let call = fx.call(func, vec![tb]);
    fx.eval(f, build, call, "cls.point_type.from_tb");

    // LRU.fill: self[key] = 1
    let subject = fx.name("self");
    fx.implicit(f, fill, ImplicitKind::SubscriptStore, subject);

    // lazy.__init__: self.func = func; lazy.__get__: self.func(obj)
    let v = fx.name("func");
    fx.field_store(f, lazy_init, "self", "func", v);
    let recv = fx.name("self");
    let func = fx.attr(recv, "func");
    let obj = fx.name("obj");
    let call = fx.call(func, vec![obj]);
    fx.eval(f, lazy_get, call, "self.func");

    // @lazy def title; render: self.title
    let deco = fx.name_ref(f, "lazy", lazy);
    fx.decorated_member(f, page, title, "title", vec![deco]);
    let recv = fx.name("self");
    let subject = fx.attr(recv, "title");
    fx.implicit(f, render, ImplicitKind::DescriptorGet, subject);

    // Base.check: self.tell() (proven to Base.tell)
    let recv = fx.name("self");
    let func = fx.attr(recv, "tell");
    let call = fx.call(func, vec![]);
    let at = fx.eval(f, check, call, "self.tell");
    fx.edge(f, check, base_tell, EdgeKind::Calls, at, 1);

    let index = fx.build();
    let cands = solve(&index);
    let names = Names(&index);
    let of = |owner: D, op: SiteOperation| -> (Vec<String>, Vec<String>) {
        let c = rows(&cands, &fx, owner, op);
        assert_eq!(c.len(), 1, "one candidate site for {owner:?} {op:?}");
        (names.of(&c[0].candidates), names.of(&c[0].field_only))
    };

    let (c, weak) = of(cached_call, SiteOperation::Call);
    assert_eq!(c, vec!["make_key"]);
    assert!(weak.is_empty(), "class-specific slot is strong evidence");
    assert_eq!(of(remap, SiteOperation::Call).0, vec!["default_enter"]);
    assert_eq!(of(from_tb, SiteOperation::Call).0, vec!["Callpoint.__init__"]);
    assert_eq!(of(build, SiteOperation::Call).0, vec!["Callpoint.from_tb"]);
    assert_eq!(of(fill, SiteOperation::SubscriptStore).0, vec!["Store.__setitem__"]);
    assert_eq!(of(render, SiteOperation::DescriptorGet).0, vec!["lazy.__get__"]);
    assert_eq!(of(lazy_get, SiteOperation::Call).0, vec!["Page.title"]);
    // Virtual self-call in the declared context: class-hierarchy overrides.
    assert_eq!(of(check, SiteOperation::OverrideDispatch).0, vec!["Left.tell", "Right.tell"]);
    // Descriptor access resolves through the allocation's stored function.
    let composed: Vec<&FlowCandidate> = cands
        .iter()
        .filter(|c| c.owner == fx.id(render) && c.via.is_some())
        .collect();
    assert_eq!(composed.len(), 1);
    assert_eq!(names.of(&composed[0].candidates), vec!["Page.title"]);
    assert_eq!(composed[0].via.map(|v| v.1), Some(fx.id(lazy_get)));
    // Candidates are never proven edges; nothing is test code here.
    assert!(index.edges.iter().all(|e| e.from != fx.id(cached_call)));
    assert!(cands.iter().all(|c| c.test_only.is_empty()));
    let _ = (lazy_init, setitem);
}

#[test]
fn field_name_only_evidence_is_marked() {
    let mut fx = Fixture::new();
    let f = fx.file("g.py", Language::Python, None);
    let handler = fx.decl(f, Decl::function("handler"));
    let setup = fx.decl(f, Decl::function("setup").params(&["obj"]));
    let run = fx.decl(f, Decl::function("run").params(&["other"]));
    // setup: obj.callback = handler   (obj has no known class)
    let v = fx.name_ref(f, "handler", handler);
    fx.field_store(f, setup, "obj", "callback", v);
    // run: other.callback()
    let other = fx.name("other");
    let func = fx.attr(other, "callback");
    let call = fx.call(func, vec![]);
    fx.eval(f, run, call, "other.callback");
    let index = fx.build();
    let cands = solve(&index);
    assert_eq!(cands.len(), 1);
    assert_eq!(cands[0].candidates, vec![fx.id(handler)]);
    assert_eq!(cands[0].field_only, vec![fx.id(handler)]);
    assert_eq!(cands[0].callee, "other.callback");
    assert_eq!(cands[0].kind, CandidateKind::Flow);
}

/// flask `flask.json.dumps(x)`: the compiler binds the call to the module function
/// `json.dumps`; the receiver expression `flask.json` may hold a provider instance by field
/// name (`self.json = Provider()`), but a module function has no overrides, so no
/// `override_dispatch` candidate (`Provider.dumps`) is produced. A proven *method* call on
/// the same receiver still dispatches.
#[test]
fn proven_module_functions_never_dispatch() {
    let mut fx = Fixture::new();
    let f = fx.file("app.py", Language::Python, None);
    let dumps = fx.decl(f, Decl::function("dumps").params(&["obj"]));
    let provider = fx.decl(f, Decl::class("Provider"));
    let provider_dumps = fx.decl(f, Decl::method("dumps", provider).params(&["self", "obj"]));
    let custom = fx.decl(f, Decl::class("Custom").bases(&["Provider"]));
    let custom_dumps = fx.decl(f, Decl::method("dumps", custom).params(&["self", "obj"]));
    let app = fx.decl(f, Decl::class("App"));
    let app_init = fx.decl(f, Decl::method("__init__", app).params(&["self"]));
    let use_fn = fx.decl(f, Decl::function("use").params(&["x"]));
    let use_method = fx.decl(f, Decl::function("use_method").params(&["other", "x"]));
    for (m, c) in [(provider_dumps, provider), (custom_dumps, custom), (app_init, app)] {
        fx.receiver(f, m, "self", c, false);
    }
    // App.__init__: self.json = Custom()
    let k = fx.name_ref(f, "Custom", custom);
    let alloc = fx.call(k, vec![]);
    fx.field_store(f, app_init, "self", "json", alloc);
    // use: flask.json.dumps(x)  (proven to the module function)
    let flask = fx.name("flask");
    let recv = fx.attr(flask, "json");
    let func = fx.attr(recv, "dumps");
    let x = fx.name("x");
    let call = fx.call(func, vec![x]);
    let at = fx.eval(f, use_fn, call, "flask.json.dumps");
    fx.edge(f, use_fn, dumps, EdgeKind::Calls, at, 1);
    // use_method: other.json.dumps(x)  (proven to the base method Provider.dumps)
    let other = fx.name("other");
    let recv = fx.attr(other, "json");
    let func = fx.attr(recv, "dumps");
    let x = fx.name("x");
    let call = fx.call(func, vec![x]);
    let at = fx.eval(f, use_method, call, "other.json.dumps");
    fx.edge(f, use_method, provider_dumps, EdgeKind::Calls, at, 1);
    let index = fx.build();
    let cands = solve(&index);
    assert!(rows(&cands, &fx, use_fn, SiteOperation::OverrideDispatch).is_empty());
    let over = rows(&cands, &fx, use_method, SiteOperation::OverrideDispatch);
    assert_eq!(over.len(), 1);
    assert_eq!(over[0].candidates, vec![fx.id(custom_dumps)]);
}

/// `test_json_dump(app, ...)`: `app` is a provider (fixture) of the shared provider file
/// `tests/shared_fixtures.py` returning `Flask(...)`; the runtime passes its value as the
/// test's `app` argument (the `runtime_dispatch` row), so `app.json.dumps(..)` (unresolved
/// by the analyzer: `app` is unannotated) is a flow candidate for the provider's `dumps`. A
/// same-named provider of the test's own file wins over the shared file, and a provider in
/// an unrelated directory is never visible.
#[test]
fn rule_by_name_injection_comes_from_runtime_dispatch_row() {
    let mut fx = Fixture::new();
    let lib = fx.file("src/app.py", Language::Python, None);
    let conftest = fx.file("tests/shared_fixtures.py", Language::Python, None);
    let test = fx.file("tests/test_json.py", Language::Python, None);
    let other = fx.file("other/shared_fixtures.py", Language::Python, None);
    let provider = fx.decl(lib, Decl::class("Provider"));
    let dumps = fx.decl(lib, Decl::method("dumps", provider).params(&["self", "obj"]));
    let flask = fx.decl(lib, Decl::class("Flask"));
    let init = fx.decl(lib, Decl::method("__init__", flask).params(&["self"]));
    let other_app = fx.decl(lib, Decl::class("OtherApp"));
    fx.receiver(lib, dumps, "self", provider, false);
    fx.receiver(lib, init, "self", flask, false);
    // Flask.__init__: self.json = Provider()
    let p = fx.name_ref(lib, "Provider", provider);
    let alloc = fx.call(p, vec![]);
    fx.field_store(lib, init, "self", "json", alloc);
    // shared file: @runner.fixture def app(): return Flask()
    let fixture = fx.decl(conftest, Decl::function("app").decorators(&[PROVIDER]));
    let k = fx.name_ref(conftest, "Flask", flask);
    let made = fx.call(k, vec![]);
    fx.ret(conftest, fixture, made);
    // other/shared_fixtures.py: an unrelated `app` provider (never visible from tests/).
    let unrelated = fx.decl(other, Decl::function("app").decorators(&["runner.fixture(autouse=True)"]));
    let k = fx.name_ref(other, "OtherApp", other_app);
    let made = fx.call(k, vec![]);
    fx.ret(other, unrelated, made);
    // test: def test_dump(app): app.json.dumps(1)
    let t = fx.decl(test, Decl::function("test_dump").params(&["app"]).test());
    let recv = fx.name("app");
    let json = fx.attr(recv, "json");
    let func = fx.attr(json, "dumps");
    let arg = fx.name("x");
    let call = fx.call(func, vec![arg]);
    fx.eval(test, t, call, "app.json.dumps");
    // A helper that is neither a test nor a provider receives nothing.
    let helper = fx.decl(test, Decl::function("helper").params(&["app"]));
    let recv = fx.name("app");
    let func = fx.attr(recv, "run");
    let call = fx.call(func, vec![]);
    fx.eval(test, helper, call, "app.run");
    let index = fx.build();
    let cands = solve_injected(&index);
    let c = rows(&cands, &fx, t, SiteOperation::Call);
    assert_eq!(c.len(), 1, "{cands:?}");
    assert_eq!(c[0].candidates, vec![fx.id(dumps)]);
    assert!(c[0].field_only.is_empty(), "the injected provider value is specific evidence");
    assert!(rows(&cands, &fx, helper, SiteOperation::Call).is_empty());
    // Without the row (no table knowledge) nothing is injected: no framework name in code.
    assert!(rows(&solve(&index), &fx, t, SiteOperation::Call)
        .iter()
        .all(|c| !c.candidates.contains(&fx.id(dumps)) || !c.field_only.is_empty()));
}

/// The injection row is active only when its `activated_by` package is an installed
/// dependency; a row without the pattern or without a provider decorator never applies.
#[test]
fn rule_runtime_dispatch_row_inactive_without_its_package() {
    let row = injection_row();
    assert!(injections([(Language::Python, &row)], &|_, _| false).is_empty());
    assert_eq!(injections([(Language::Python, &row)], &|_, p| p == "runner").len(), 1);
    let other = trace_library::table::IrreducibleRow {
        pattern: Some("queue_worker".into()),
        ..injection_row()
    };
    assert!(injections([(Language::Python, &other)], &|_, _| true).is_empty());
    let no_symbol = trace_library::table::IrreducibleRow {
        symbol: None,
        ..injection_row()
    };
    assert!(injections([(Language::Python, &no_symbol)], &|_, _| true).is_empty());
    // The embedded tables with nothing installed: no active injection rule.
    let tables = trace_library::table::Tables::builtin();
    let installed = trace_library::installed::InstalledPackages::default();
    let knowledge = LibraryKnowledge::default();
    let mut fx = Fixture::new();
    fx.file("tests/test_x.py", Language::Python, None);
    let index = fx.build();
    let library = crate::LibraryInputs {
        knowledge: &knowledge,
        tables: &tables,
        installed: &installed,
    };
    assert!(index_injections(&index, &library).is_empty());
}

/// The same injection when the fixture body is resolved by the analyzer (`app = App(..)`
/// proven to the class, `return app`) and the provider is stored by the constructor through
/// a class attribute (`self.json = self.provider_class(self)`).
#[test]
fn injected_provider_values_follow_proven_constructors() {
    let mut fx = Fixture::new();
    let lib = fx.file("src/app/core.py", Language::Python, None);
    let conftest = fx.file("tests/shared_fixtures.py", Language::Python, None);
    let test = fx.file("tests/test_json.py", Language::Python, None);
    let provider = fx.decl(lib, Decl::class("Provider"));
    let dumps = fx.decl(lib, Decl::method("dumps", provider).params(&["self", "obj"]));
    let default = fx.decl(lib, Decl::class("DefaultProvider").bases(&["Provider"]));
    let default_dumps = fx.decl(lib, Decl::method("dumps", default).params(&["self", "obj"]));
    let app = fx.decl(lib, Decl::class("App"));
    let init = fx.decl(lib, Decl::method("__init__", app).params(&["self", "name"]));
    for (m, c) in [(dumps, provider), (default_dumps, default), (init, app)] {
        fx.receiver(lib, m, "self", c, false);
    }
    // class App: provider_class = DefaultProvider
    let v = fx.name_ref(lib, "DefaultProvider", default);
    fx.member(lib, app, "provider_class", v);
    // App.__init__: self.json = self.provider_class(self)
    let recv = fx.name("self");
    let func = fx.attr(recv, "provider_class");
    let arg = fx.name("self");
    let made = fx.call(func, vec![arg]);
    fx.eval(lib, init, made.clone(), "self.provider_class");
    fx.field_store(lib, init, "self", "json", made);
    // shared file: @runner.fixture def app(): app = App("test"); return app
    let fixture = fx.decl(conftest, Decl::function("app").decorators(&[PROVIDER]));
    let k = fx.name("App");
    let made = fx.call(k, vec![]);
    let at = fx.eval(conftest, fixture, made.clone(), "App");
    fx.edge(conftest, fixture, app, EdgeKind::Calls, at, 1);
    fx.bind(
        conftest,
        BindTarget::Var {
            scope: Scope::Decl(fixture.decl),
            name: "app".into(),
        },
        made,
        Scope::Decl(fixture.decl),
    );
    let local = fx.name("app");
    fx.ret(conftest, fixture, local);
    // test: def test_dump(app): app.json.dumps(1)
    let t = fx.decl(test, Decl::function("test_dump").params(&["app"]).test());
    let recv = fx.name("app");
    let json = fx.attr(recv, "json");
    let func = fx.attr(json, "dumps");
    let arg = fx.name("x");
    let call = fx.call(func, vec![arg]);
    fx.eval(test, t, call, "app.json.dumps");
    let index = fx.build();
    let cands = solve_injected(&index);
    let c = rows(&cands, &fx, t, SiteOperation::Call);
    assert_eq!(c.len(), 1, "{cands:?}");
    assert_eq!(c[0].candidates, vec![fx.id(default_dumps)]);
}

/// sqlite-jdbc `sql.append(..).append(..)...` (hundreds of steps, each proven by the
/// analyzer): the receiver of every step is evaluated once, so solving is linear in the
/// chain length (it was exponential and exhausted memory).
#[test]
fn proven_builder_chains_are_evaluated_once_per_step() {
    let mut fx = Fixture::new();
    let f = fx.file("Meta.java", Language::Python, None);
    let builder = fx.decl(f, Decl::class("Builder"));
    let append = fx.decl(f, Decl::method("append", builder).params(&["self", "s"]));
    let run = fx.decl(f, Decl::function("run").params(&["sql"]));
    fx.receiver(f, append, "self", builder, false);
    let recv = fx.name("self");
    fx.ret(f, append, recv);
    let mut expr = fx.name("sql");
    let mut spans = Vec::new();
    for _ in 0..200 {
        let func = fx.attr(expr, "append");
        let arg = fx.name("s");
        expr = fx.call(func, vec![arg]);
        if let Expr::Call { func_span, .. } = &expr {
            spans.push(*func_span);
        }
    }
    let at = fx.eval(f, run, expr, "sql.append");
    for span in spans.iter().filter(|s| **s != at) {
        fx.edge(f, run, append, EdgeKind::Calls, *span, 1);
    }
    fx.edge(f, run, append, EdgeKind::Calls, at, 1);
    let index = fx.build();
    let started = std::time::Instant::now();
    let _ = solve(&index);
    assert!(started.elapsed() < std::time::Duration::from_secs(10), "{:?}", started.elapsed());
}

#[test]
fn module_scope_never_produces_candidates() {
    let mut fx = Fixture::new();
    let f = fx.file("h.py", Language::Python, None);
    let target = fx.decl(f, Decl::function("target"));
    let v = fx.name_ref(f, "target", target);
    fx.bind(
        f,
        BindTarget::Var {
            scope: Scope::Module,
            name: "alias".into(),
        },
        v,
        Scope::Module,
    );
    let alias = fx.name("alias");
    let call = fx.call(alias, vec![]);
    fx.flow(
        f,
        FlowFact::Eval {
            scope: Scope::Module,
            call,
        },
    );
    let index = fx.build();
    assert!(solve(&index).is_empty());
}

/// server 10-compaction: `settle_even_if_abandoned(lambda: record_compaction_usage(...))`.
/// The lambda is a function value: it flows into the called parameter, and the passing call
/// becomes a callback candidate for the lambda.
#[test]
fn lambdas_are_function_values() {
    let mut fx = Fixture::new();
    let f = fx.file("compaction.py", Language::Python, None);
    let settle = fx.decl(f, Decl::function("settle_even_if_abandoned").params(&["fn"]));
    let record = fx.decl(f, Decl::function("record_compaction_usage").params(&["session"]));
    let compact = fx.decl(f, Decl::function("compact").params(&["session"]));
    let lambda = fx.decl(f, Decl::lambda(compact));
    // settle: fn()
    let fn_ = fx.name("fn");
    let call = fx.call(fn_, vec![]);
    let fn_call = fx.eval(f, settle, call, "fn");
    // lambda body: record_compaction_usage(session)  (proven, owned by the lambda)
    let rec = fx.name_ref(f, "record_compaction_usage", record);
    let session = fx.name("session");
    let body = fx.call(rec, vec![session]);
    let at = fx.eval(f, lambda, body.clone(), "record_compaction_usage");
    fx.edge(f, lambda, record, EdgeKind::Calls, at, 1);
    fx.ret(f, lambda, body);
    // compact: settle_even_if_abandoned(lambda: ...)
    let callee = fx.name_ref(f, "settle_even_if_abandoned", settle);
    let arg = fx.lambda_expr(lambda);
    let call = fx.call(callee, vec![arg]);
    let settle_at = fx.eval(f, compact, call, "settle_even_if_abandoned");
    fx.edge(f, compact, settle, EdgeKind::Calls, settle_at, 1);

    let index = fx.build();
    let cands = solve(&index);
    let names = Names(&index);
    let inside = rows(&cands, &fx, settle, SiteOperation::Call);
    assert_eq!(inside.len(), 1);
    assert_eq!(inside[0].span, fn_call);
    assert_eq!(names.of(&inside[0].candidates), vec!["compact.<lambda>"]);
    let passed: Vec<&FlowCandidate> = cands.iter().filter(|c| c.owner == fx.id(compact)).collect();
    assert_eq!(passed.len(), 1);
    assert_eq!(passed[0].candidates, vec![fx.id(lambda)]);
    assert_eq!(passed[0].span, settle_at);
    assert_eq!(
        passed[0].kind,
        CandidateKind::Callback {
            arg: fx.decl_span(lambda)
        }
    );
}

/// boltons 02-url-render: every `@cachedproperty` decoration is its own allocation, so
/// `self.query_params` resolves through `cachedproperty.__get__` to exactly the function
/// stored in that allocation (not the 10 functions sharing the `func` field).
#[test]
fn decorations_are_separate_allocations() {
    let mut fx = Fixture::new();
    let f = fx.file("urlutils.py", Language::Python, None);
    let cp = fx.decl(f, Decl::class("cachedproperty"));
    let cp_init = fx.decl(f, Decl::method("__init__", cp).params(&["self", "func"]));
    let cp_get = fx.decl(f, Decl::method("__get__", cp).params(&["self", "obj", "objtype"]));
    let url = fx.decl(f, Decl::class("URL"));
    let query = fx.decl(f, Decl::method("query_params", url).params(&["self"]));
    let path = fx.decl(f, Decl::method("path_parts", url).params(&["self"]));
    let host = fx.decl(f, Decl::method("host_parts", url).params(&["self"]));
    let to_text = fx.decl(f, Decl::method("to_text", url).params(&["self"]));
    for (m, c) in [
        (cp_init, cp),
        (cp_get, cp),
        (query, url),
        (path, url),
        (host, url),
        (to_text, url),
    ] {
        fx.receiver(f, m, "self", c, false);
    }
    let func = fx.name("func");
    fx.field_store(f, cp_init, "self", "func", func);
    let recv = fx.name("self");
    let func = fx.attr(recv, "func");
    let obj = fx.name("obj");
    let call = fx.call(func, vec![obj]);
    fx.eval(f, cp_get, call, "self.func");
    for (m, name) in [(query, "query_params"), (path, "path_parts"), (host, "host_parts")] {
        let deco = fx.name_ref(f, "cachedproperty", cp);
        fx.decorated_member(f, url, m, name, vec![deco]);
    }
    let recv = fx.name("self");
    let subject = fx.attr(recv, "query_params");
    fx.implicit(f, to_text, ImplicitKind::DescriptorGet, subject);

    let index = fx.build();
    let cands = solve(&index);
    let names = Names(&index);
    let get = rows(&cands, &fx, to_text, SiteOperation::DescriptorGet);
    assert_eq!(get.len(), 1);
    assert_eq!(names.of(&get[0].candidates), vec!["cachedproperty.__get__"]);
    let composed: Vec<&FlowCandidate> = cands
        .iter()
        .filter(|c| c.owner == fx.id(to_text) && c.via.is_some())
        .collect();
    assert_eq!(composed.len(), 1);
    assert_eq!(names.of(&composed[0].candidates), vec!["URL.query_params"]);
    assert_eq!(composed[0].operation, SiteOperation::Call);
    let parent = composed[0].via.expect("composed").0;
    assert_eq!(cands[parent].operation, SiteOperation::DescriptorGet);
    // The shared descriptor body still sees every stored function (possible tier).
    let inside = rows(&cands, &fx, cp_get, SiteOperation::Call);
    assert_eq!(names.of(&inside[0].candidates), vec!["URL.host_parts", "URL.path_parts", "URL.query_params"]);
}

struct Tb {
    fx: Fixture,
    from_traceback: D,
    contextual_from_tb: D,
}

/// boltons tbutils: classmethods configured through class attributes of subclasses.
fn tbutils(with_contextual_caller: bool) -> Tb {
    let mut fx = Fixture::new();
    let f = fx.file("tbutils.py", Language::Python, None);
    let callpoint = fx.decl(f, Decl::class("Callpoint"));
    let from_tb = fx.decl(f, Decl::method("from_tb", callpoint).params(&["cls", "tb"]));
    let ccp = fx.decl(f, Decl::class("ContextualCallpoint").bases(&["Callpoint"]));
    let ccp_from_tb = fx.decl(f, Decl::method("from_tb", ccp).params(&["cls", "tb"]));
    let tbi = fx.decl(f, Decl::class("TracebackInfo"));
    let from_traceback = fx.decl(f, Decl::method("from_traceback", tbi).params(&["cls", "tb"]));
    let ctbi = fx.decl(f, Decl::class("ContextualTracebackInfo").bases(&["TracebackInfo"]));
    let exc = fx.decl(f, Decl::class("ExceptionInfo"));
    let from_exc_info = fx.decl(f, Decl::method("from_exc_info", exc).params(&["cls", "tb"]));
    let cexc = fx.decl(f, Decl::class("ContextualExceptionInfo").bases(&["ExceptionInfo"]));
    let caller = fx.decl(f, Decl::function("contextual_report").params(&["tb"]));
    for (m, c, name) in [
        (from_tb, callpoint, "from_tb"),
        (ccp_from_tb, ccp, "from_tb"),
        (from_traceback, tbi, "from_traceback"),
        (from_exc_info, exc, "from_exc_info"),
    ] {
        fx.receiver(f, m, "cls", c, true);
        let deco = fx.name("classmethod");
        fx.decorated_member(f, c, m, name, vec![deco]);
    }
    for (class, attr, value, name) in [
        (tbi, "callpoint_type", callpoint, "Callpoint"),
        (ctbi, "callpoint_type", ccp, "ContextualCallpoint"),
        (exc, "tb_info_type", tbi, "TracebackInfo"),
        (cexc, "tb_info_type", ctbi, "ContextualTracebackInfo"),
    ] {
        let v = fx.name_ref(f, name, value);
        fx.member(f, class, attr, v);
    }
    // from_traceback: cls.callpoint_type.from_tb(tb)   (pyright: Callpoint.from_tb)
    let cls = fx.name("cls");
    let ty = fx.attr(cls, "callpoint_type");
    let func = fx.attr(ty, "from_tb");
    let tb = fx.name("tb");
    let call = fx.call(func, vec![tb]);
    let at = fx.eval(f, from_traceback, call, "cls.callpoint_type.from_tb");
    fx.edge(f, from_traceback, from_tb, EdgeKind::Calls, at, 1);
    // from_exc_info: cls.tb_info_type.from_traceback(tb)   (pyright: TracebackInfo.from_traceback)
    let cls = fx.name("cls");
    let ty = fx.attr(cls, "tb_info_type");
    let func = fx.attr(ty, "from_traceback");
    let tb = fx.name("tb");
    let call = fx.call(func, vec![tb]);
    let at = fx.eval(f, from_exc_info, call, "cls.tb_info_type.from_traceback");
    fx.edge(f, from_exc_info, from_traceback, EdgeKind::Calls, at, 1);
    if with_contextual_caller {
        // contextual_report: ContextualExceptionInfo.from_exc_info(tb)
        let cls = fx.name_ref(f, "ContextualExceptionInfo", cexc);
        let func = fx.attr(cls, "from_exc_info");
        let tb = fx.name("tb");
        let call = fx.call(func, vec![tb]);
        let at = fx.eval(f, caller, call, "ContextualExceptionInfo.from_exc_info");
        fx.edge(f, caller, from_exc_info, EdgeKind::Calls, at, 1);
    }
    Tb {
        fx,
        from_traceback,
        contextual_from_tb: ccp_from_tb,
    }
}

/// boltons 06-exception-info: `cls` is specialised by the receiver class flowing from each
/// call site, so the non-contextual chain never reaches `ContextualCallpoint.from_tb` —
/// until some caller really uses the contextual classes.
#[test]
fn class_receivers_are_specialised_per_call_site() {
    let tb = tbutils(false);
    let index = tb.fx.build();
    let cands = solve(&index);
    let target = tb.fx.id(tb.contextual_from_tb);
    assert!(
        cands.iter().all(|c| !c.candidates.contains(&target)),
        "no receiver ever selects the contextual classes"
    );

    let tb = tbutils(true);
    let index = tb.fx.build();
    let cands = solve(&index);
    let over = rows(&cands, &tb.fx, tb.from_traceback, SiteOperation::OverrideDispatch);
    assert_eq!(over.len(), 1);
    assert_eq!(over[0].candidates, vec![tb.fx.id(tb.contextual_from_tb)]);
}

/// boltons 06-exception-info: `cls(...)` in `Callpoint.from_tb` constructs `Callpoint` in
/// its declared context and `ContextualCallpoint` when reached through `super().from_tb()`:
/// one constructor per receiver context, so the site is `receiver_exact`.
#[test]
fn one_target_per_receiver_context_is_receiver_exact() {
    let mut fx = Fixture::new();
    let f = fx.file("tbutils.py", Language::Python, None);
    let callpoint = fx.decl(f, Decl::class("Callpoint"));
    let init = fx.decl(f, Decl::method("__init__", callpoint).params(&["self", "name"]));
    let from_tb = fx.decl(f, Decl::method("from_tb", callpoint).params(&["cls", "tb"]));
    let ccp = fx.decl(f, Decl::class("ContextualCallpoint").bases(&["Callpoint"]));
    let ccp_init = fx.decl(f, Decl::method("__init__", ccp).params(&["self", "name"]));
    let ccp_from_tb = fx.decl(f, Decl::method("from_tb", ccp).params(&["cls", "tb"]));
    for (m, c, name) in [(from_tb, callpoint, "from_tb"), (ccp_from_tb, ccp, "from_tb")] {
        fx.receiver(f, m, "cls", c, true);
        let deco = fx.name("classmethod");
        fx.decorated_member(f, c, m, name, vec![deco]);
    }
    for (m, c) in [(init, callpoint), (ccp_init, ccp)] {
        fx.receiver(f, m, "self", c, false);
    }
    // Callpoint.from_tb: cls(tb)
    let cls = fx.name("cls");
    let tb = fx.name("tb");
    let call = fx.call(cls, vec![tb]);
    fx.eval(f, from_tb, call, "cls");
    // ContextualCallpoint.from_tb: super().from_tb(tb)   (pyright: Callpoint.from_tb)
    let sup = fx.name("super");
    let sup = fx.call(sup, vec![]);
    let func = fx.attr(sup, "from_tb");
    let tb = fx.name("tb");
    let call = fx.call(func, vec![tb]);
    let at = fx.eval(f, ccp_from_tb, call, "super().from_tb");
    fx.edge(f, ccp_from_tb, from_tb, EdgeKind::Calls, at, 1);

    let index = fx.build();
    let cands = solve(&index);
    let row = rows(&cands, &fx, from_tb, SiteOperation::Call);
    assert_eq!(row.len(), 1);
    assert_eq!(row[0].candidates, vec![fx.id(init), fx.id(ccp_init)]);
    assert!(row[0].receiver_exact);
}

/// boltons 07-remap: `if visit is _orig: ... else: visit(x)` — the guarded default is not a
/// callee of that call; a guard naming more than one object excludes nothing.
#[test]
fn identity_guards_exclude_the_guarded_object() {
    let run = |ambiguous: bool| -> Vec<String> {
        let mut fx = Fixture::new();
        let f = fx.file("iterutils.py", Language::Python, None);
        let default_visit = fx.decl(f, Decl::function("default_visit").params(&["x"]));
        let other = fx.decl(f, Decl::function("other_visit").params(&["x"]));
        let remap = fx.decl(f, Decl::function("remap").params(&["root", "visit"]));
        let user = fx.decl(f, Decl::function("user"));
        let module_var = |name: &str| BindTarget::Var {
            scope: Scope::Module,
            name: name.into(),
        };
        let orig = fx.name_ref(f, "default_visit", default_visit);
        fx.bind(f, module_var("_orig"), orig, Scope::Module);
        if ambiguous {
            let alt = fx.name_ref(f, "other_visit", other);
            fx.bind(f, module_var("_orig"), alt, Scope::Module);
        }
        let v = fx.name_ref(f, "default_visit", default_visit);
        let param = BindTarget::Var {
            scope: Scope::Decl(remap.decl),
            name: "visit".into(),
        };
        fx.bind(f, param, v, Scope::Module);
        // user(): remap(root, other_visit)
        let callee = fx.name_ref(f, "remap", remap);
        let arg = fx.name_ref(f, "other_visit", other);
        let call = fx.call(callee, vec![Expr::Opaque, arg]);
        fx.eval(f, user, call, "remap");
        // remap: `if visit is _orig: ... else: visit(root)`
        let visit = fx.name("visit");
        let root = fx.name("root");
        let call = fx.call(visit, vec![root]);
        let span = call.span().expect("call span");
        fx.eval(f, remap, call, "visit");
        let guard = fx.name("_orig");
        fx.guard(f, span, vec![guard]);

        let index = fx.build();
        let cands = solve(&index);
        let row = rows(&cands, &fx, remap, SiteOperation::Call);
        assert_eq!(row.len(), 1);
        assert!(!row[0].receiver_exact, "one context, several callees");
        Names(&index).of(&row[0].candidates)
    };
    assert_eq!(run(false), vec!["other_visit"]);
    assert_eq!(run(true), vec!["default_visit", "other_visit"]);
}

/// boltons 07-remap: a test's own `exit` function flows into `remap(exit=...)`; it is
/// reported as test-only (possible tier), the product default stays a normal candidate.
#[test]
fn test_origin_values_are_tagged() {
    let mut fx = Fixture::new();
    let f = fx.file("boltons/iterutils.py", Language::Python, None);
    let t = fx.file("tests/test_iterutils.py", Language::Python, None);
    let default_exit = fx.decl(f, Decl::function("default_exit").params(&["path", "key", "old"]));
    let remap = fx.decl(f, Decl::function("remap").params(&["root", "exit"]));
    let test_remap = fx.decl(t, Decl::function("test_remap").test());
    let test_exit = fx.decl(t, Decl::nested("exit", test_remap).params(&["path", "key", "old"]));
    let v = fx.name_ref(f, "default_exit", default_exit);
    fx.bind(
        f,
        BindTarget::Var {
            scope: Scope::Decl(remap.decl),
            name: "exit".into(),
        },
        v,
        Scope::Module,
    );
    let exit = fx.name("exit");
    let root = fx.name("root");
    let call = fx.call(exit, vec![root]);
    fx.eval(f, remap, call, "exit");
    // tests: remap({}, exit=exit)
    let callee = fx.name_ref(t, "remap", remap);
    let arg = fx.name_ref(t, "exit", test_exit);
    let call = fx.call_kw(callee, vec![Expr::Opaque], vec![("exit", arg)]);
    fx.eval(t, test_remap, call, "remap");

    let index = fx.build();
    let cands = solve(&index);
    let inside = rows(&cands, &fx, remap, SiteOperation::Call);
    assert_eq!(inside.len(), 1);
    assert_eq!(inside[0].candidates, vec![fx.id(default_exit), fx.id(test_exit)]);
    assert_eq!(inside[0].test_only, vec![fx.id(test_exit)]);
    assert!(!inside[0].receiver_exact);
    // The test's own passing call is test code: its callback candidate is not test-only.
    let passed: Vec<&FlowCandidate> = cands
        .iter()
        .filter(|c| c.owner == fx.id(test_remap) && matches!(c.kind, CandidateKind::Callback { .. }))
        .collect();
    assert_eq!(passed.len(), 1);
    assert_eq!(passed[0].candidates, vec![fx.id(test_exit)]);
    assert!(passed[0].test_only.is_empty());
}

/// more-itertools 01-nth-prime: `all(_strong_probable_prime(n, b) for b in bases)` — the
/// generator expression's body runs because `all()` consumes it.
#[test]
fn generator_expressions_run_when_consumed() {
    let mut fx = Fixture::new();
    let f = fx.file("more.py", Language::Python, None);
    let spp = fx.decl(f, Decl::function("_strong_probable_prime").params(&["n", "base"]));
    let is_prime = fx.decl(f, Decl::function("is_prime").params(&["n"]));
    let genexpr = fx.decl(f, Decl::genexpr(is_prime));
    let callee = fx.name_ref(f, "_strong_probable_prime", spp);
    let n = fx.name("n");
    let b = fx.name("b");
    let body = fx.call(callee, vec![n, b]);
    let at = fx.eval(f, genexpr, body, "_strong_probable_prime");
    fx.edge(f, genexpr, spp, EdgeKind::Calls, at, 1);
    let all = fx.name("all");
    let arg = fx.lambda_expr(genexpr);
    let call = fx.call(all, vec![arg]);
    let all_at = fx.eval(f, is_prime, call, "all");

    let index = fx.build();
    let k = knowledge_at(&[("more.py", all_at.start, vec![Effect::Iterates(ArgSel::Pos(0))], true)]);
    let cands = solve_known(&index, &k);
    let consumed = rows(&cands, &fx, is_prime, SiteOperation::Iterate);
    assert_eq!(consumed.len(), 1);
    assert_eq!(consumed[0].candidates, vec![fx.id(genexpr)]);
    assert_eq!(consumed[0].span, fx.decl_span(genexpr));
    assert_eq!(consumed[0].kind, CandidateKind::Implicit);
}

/// The syntax lowering of `all(f(b) for b in bases)`: the generator expression is its
/// synthetic function applied to the first iterable (`Call{Lambda(<genexpr>), [bases]}`);
/// the call creates the generator object, which `all()` consumes.
#[test]
fn lowered_generator_expressions_create_generator_objects() {
    let mut fx = Fixture::new();
    let f = fx.file("more.py", Language::Python, None);
    let spp = fx.decl(f, Decl::function("_strong_probable_prime").params(&["n", "base"]));
    let is_prime = fx.decl(f, Decl::function("is_prime").params(&["n", "bases"]));
    let genexpr = fx.decl(f, Decl::genexpr(is_prime));
    let callee = fx.name_ref(f, "_strong_probable_prime", spp);
    let n = fx.name("n");
    let b = fx.name("b");
    let body = fx.call(callee, vec![n, b]);
    let at = fx.eval(f, genexpr, body, "_strong_probable_prime");
    fx.edge(f, genexpr, spp, EdgeKind::Calls, at, 1);
    let bases = fx.name("bases");
    let created = fx.call(fx.lambda_expr(genexpr), vec![bases]);
    let all = fx.name("all");
    let call = fx.call(all, vec![created]);
    let all_at = fx.eval(f, is_prime, call, "all");

    let index = fx.build();
    let k = knowledge_at(&[("more.py", all_at.start, vec![Effect::Iterates(ArgSel::Pos(0))], true)]);
    let cands = solve_known(&index, &k);
    let consumed = rows(&cands, &fx, is_prime, SiteOperation::Iterate);
    assert_eq!(consumed.len(), 1);
    assert_eq!(consumed[0].candidates, vec![fx.id(genexpr)]);
    assert_eq!(consumed[0].kind, CandidateKind::Implicit);
}

/// more-itertools 04-intersperse: `iter(partial(take, n, it), [])` — `partial` returns a
/// wrapper equivalent to `take` and `iter(callable, sentinel)` calls it.
#[test]
fn library_partial_wrappers_are_called_by_iter() {
    let mut fx = Fixture::new();
    let f = fx.file("more.py", Language::Python, None);
    let take = fx.decl(f, Decl::function("take").params(&["n", "iterable"]));
    let chunked = fx.decl(f, Decl::function("chunked").params(&["iterable", "n"]));
    let partial = fx.name("partial");
    let take_ref = fx.name_ref(f, "take", take);
    let n = fx.name("n");
    let it = fx.name("iterable");
    let wrapped = fx.call(partial, vec![take_ref, n, it]);
    let wrapped_span = wrapped.span().expect("call span");
    let iter = fx.name("iter");
    let call = fx.call(iter, vec![wrapped.clone(), Expr::Opaque]);
    let iter_at = fx.eval(f, chunked, call, "iter");
    let partial_at = fx.eval(f, chunked, wrapped, "partial");

    let index = fx.build();
    let k = knowledge_at(&[
        ("more.py", iter_at.start, vec![Effect::Calls(ArgSel::Pos(0))], true),
        ("more.py", partial_at.start, vec![Effect::Partial(ArgSel::Pos(0))], true),
    ]);
    let cands = solve_known(&index, &k);
    let calls: Vec<&FlowCandidate> = cands.iter().filter(|c| c.owner == fx.id(chunked)).collect();
    assert_eq!(calls.len(), 1, "partial() itself does not call take");
    assert_eq!(calls[0].candidates, vec![fx.id(take)]);
    assert_eq!(calls[0].span, iter_at);
    assert_eq!(calls[0].kind, CandidateKind::Callback { arg: wrapped_span });
}

/// more-itertools 05-relative-seek: `consume(self)` forwards the instance to `islice`, which
/// iterates it: the passing call runs `seekable.__iter__` / `__next__`.
#[test]
fn instances_flowing_into_consuming_parameters_are_iterated() {
    let mut fx = Fixture::new();
    let f = fx.file("more.py", Language::Python, None);
    let consume = fx.decl(f, Decl::function("consume").params(&["iterator", "n"]));
    let seekable = fx.decl(f, Decl::class("seekable"));
    let iter = fx.decl(f, Decl::method("__iter__", seekable).params(&["self"]));
    let next = fx.decl(f, Decl::method("__next__", seekable).params(&["self"]));
    let seek = fx.decl(f, Decl::method("seek", seekable).params(&["self", "index"]));
    for m in [iter, next, seek] {
        fx.receiver(f, m, "self", seekable, false);
    }
    let me = fx.name("self");
    fx.ret(f, iter, me);
    // consume: next(islice(iterator, n, n), None)
    let islice = fx.name("islice");
    let a = fx.name("iterator");
    let n1 = fx.name("n");
    let n2 = fx.name("n");
    let sliced = fx.call(islice, vec![a, n1, n2]);
    let islice_at = fx.eval(f, consume, sliced, "islice");
    // seek: consume(self, index)
    let callee = fx.name_ref(f, "consume", consume);
    let me = fx.name("self");
    let me_span = me.span().expect("name span");
    let index_arg = fx.name("index");
    let call = fx.call(callee, vec![me, index_arg]);
    fx.eval(f, seek, call, "consume");

    let index = fx.build();
    let k = knowledge_at(&[("more.py", islice_at.start, vec![Effect::Iterates(ArgSel::Pos(0))], true)]);
    let cands = solve_known(&index, &k);
    let names = Names(&index);
    let consumed = rows(&cands, &fx, seek, SiteOperation::Iterate);
    assert_eq!(consumed.len(), 1);
    assert_eq!(consumed[0].span, me_span);
    assert_eq!(names.of(&consumed[0].candidates), vec!["seekable.__iter__", "seekable.__next__"]);
}

#[test]
fn library_property_runs_getters_on_access() {
    let mut fx = Fixture::new();
    let f = fx.file("p.py", Language::Python, None);
    let class = fx.decl(f, Decl::class("Config"));
    let getter = fx.decl(f, Decl::method("_load", class).params(&["self"]));
    let reader = fx.decl(f, Decl::method("read", class).params(&["self"]));
    for m in [getter, reader] {
        fx.receiver(f, m, "self", class, false);
    }
    // settings = property(_load)
    let property = fx.name("property");
    let load = fx.name_ref(f, "_load", getter);
    let wrapped = fx.call(property, vec![load]);
    let property_start = wrapped.span().expect("call span").start;
    fx.member(f, class, "settings", wrapped);
    // read: self.settings
    let me = fx.name("self");
    let subject = fx.attr(me, "settings");
    fx.implicit(f, reader, ImplicitKind::DescriptorGet, subject);
    let index = fx.build();
    let k = knowledge_at(&[(
        "p.py",
        property_start,
        vec![Effect::Property(ArgSel::PosOrKw(0, "fget".into()))],
        true,
    )]);
    let cands = solve_known(&index, &k);
    let get = rows(&cands, &fx, reader, SiteOperation::DescriptorGet);
    assert_eq!(get.len(), 1);
    assert_eq!(get[0].candidates, vec![fx.id(getter)]);
}

/// Library knowledge applies in every language (the Python-only gate is gone): a function
/// passed to a library call whose knowledge says it calls (or stores and later calls) the
/// argument is a callback candidate in TypeScript, Go and PHP alike; `never_calls` on the
/// same argument yields nothing, and without knowledge nothing is guessed.
#[test]
fn rule_derived_behaviour_applies_in_every_language() {
    for (path, language, effect) in [
        ("src/app.ts", Language::TypeScript, Effect::Calls(ArgSel::Pos(0))),
        ("main.go", Language::Go, Effect::StoredThenCalled(ArgSel::Pos(0))),
        ("lib/app.php", Language::Php, Effect::Calls(ArgSel::PosOrKw(0, "callback".into()))),
    ] {
        let mut fx = Fixture::new();
        let f = fx.file(path, language, None);
        let handler = fx.decl(f, Decl::function("handler"));
        let main = fx.decl(f, Decl::function("main"));
        let lib = fx.name("lib");
        let func = fx.attr(lib, "run");
        let arg = fx.name_ref(f, "handler", handler);
        let call = fx.call(func, vec![arg]);
        let at = fx.eval(f, main, call, "lib.run");
        let index = fx.build();
        let callbacks = |cands: &[FlowCandidate]| -> Vec<Vec<SymbolId>> {
            cands
                .iter()
                .filter(|c| c.owner == fx.id(main) && matches!(c.kind, CandidateKind::Callback { .. }))
                .map(|c| c.candidates.clone())
                .collect()
        };
        let k = knowledge_at(&[(path, at.start, vec![effect.clone()], true)]);
        assert_eq!(callbacks(&solve_known(&index, &k)), vec![vec![fx.id(handler)]], "{path}");
        let never = knowledge_at(&[(path, at.start, vec![effect, Effect::NeverCalls(ArgSel::Pos(0))], true)]);
        assert!(callbacks(&solve_known(&index, &never)).is_empty(), "{path}");
        assert!(callbacks(&solve(&index)).is_empty(), "{path}");
    }
}

/// A decorator factory whose knowledge says it returns a decorator that stores the
/// decorated function and calls it later (`@app.route("/")`, derived `Decorates`): the
/// decorator application carries the inner effect on the decorated function (argument 0).
#[test]
fn rule_decorator_factory_effect_runs_the_decorated_function() {
    let mut fx = Fixture::new();
    let f = fx.file("app.py", Language::Python, None);
    let setup = fx.decl(f, Decl::function("setup"));
    let index_fn = fx.decl(f, Decl::nested("index", setup));
    let recv = fx.name("app");
    let func = fx.attr(recv, "route");
    let arg = fx.name("path");
    let deco = fx.call(func, vec![arg]);
    let deco_start = deco.span().expect("call span").start;
    fx.decorated_in(f, setup, index_fn, "index", vec![(deco, Some("app.route"))]);
    let index = fx.build();
    let k = knowledge_at(&[(
        "app.py",
        deco_start,
        vec![Effect::Decorates {
            inner: Box::new(Effect::StoredThenCalled(ArgSel::Pos(0))),
        }],
        true,
    )]);
    let h = Hierarchy::build(&index);
    let flow = Flow::solve_rules(&index, &h, &k, &[]);
    let applied: Vec<&Vec<Effect>> = flow
        .constraints
        .iter()
        .filter_map(|c| match &c.rule {
            Rule::Decorated { decorators, .. } => Some(decorators),
            _ => None,
        })
        .flat_map(|d| d.iter().map(|(_, _, e)| e))
        .collect();
    assert_eq!(applied, vec![&vec![Effect::StoredThenCalled(ArgSel::Pos(0))]]);
    // Without knowledge the decorator passes the value through unchanged.
    let plain = Flow::solve(&index, &h);
    assert!(plain.constraints.iter().all(|c| match &c.rule {
        Rule::Decorated { decorators, .. } => decorators.iter().all(|(_, _, e)| e.is_empty()),
        _ => true,
    }));
}

/// Scale contract (SPEC §5.1a): 70 handlers stored under one field name saturate the
/// field-name slot at `flow.max_slot_values`; the candidate read from it is marked bounded and a
/// `flow_bound` diagnostic names the slot (nothing disappears silently). Items whose inputs
/// did not change are not re-evaluated.
#[test]
fn saturated_slots_are_bounded_and_reported() {
    let mut fx = Fixture::new();
    let f = fx.file("g.py", Language::Python, None);
    let setup = fx.decl(f, Decl::function("setup").params(&["obj"]));
    let run = fx.decl(f, Decl::function("run").params(&["other"]));
    for i in 0..70 {
        let h = fx.decl(f, Decl::function(&format!("handler{i:02}")));
        let v = fx.name_ref(f, "h", h);
        fx.field_store(f, setup, "obj", "callback", v);
    }
    let other = fx.name("other");
    let func = fx.attr(other, "callback");
    let call = fx.call(func, vec![]);
    fx.eval(f, run, call, "other.callback");
    let index = fx.build();
    let h = Hierarchy::build(&index);
    let flow = Flow::solve(&index, &h);
    assert!(flow.iterations < trace_core::config::current().flow.max_iterations);
    let cands = flow.candidates();
    assert_eq!(cands.len(), 1);
    assert_eq!(cands[0].candidates.len(), trace_core::config::current().flow.max_slot_values);
    assert!(cands[0].bounded);
    let stats = flow.stats();
    assert_eq!(stats.saturated_slots, 1);
    assert!(stats.skipped > 0, "unchanged items are skipped: {stats:?}");
    assert!(!stats.budget_exhausted);
    let diags = flow.diagnostics();
    assert!(diags
        .iter()
        .any(|d| d.kind == "flow_bound" && d.message.contains("field .callback")));
}

/// Module-level code of a file with a `<module>` declaration (extractor 5) produces
/// candidates owned by that synthetic symbol.
#[test]
fn module_code_is_owned_by_the_module_symbol() {
    let mut fx = Fixture::new();
    let f = fx.file("h.py", Language::Python, None);
    let target = fx.decl(f, Decl::function("target"));
    let v = fx.name_ref(f, "target", target);
    fx.bind(
        f,
        BindTarget::Var {
            scope: Scope::Module,
            name: "alias".into(),
        },
        v,
        Scope::Module,
    );
    let alias = fx.name("alias");
    let call = fx.call(alias, vec![]);
    fx.flow(
        f,
        FlowFact::Eval {
            scope: Scope::Module,
            call,
        },
    );
    let module = fx.module(f);
    let index = fx.build();
    let cands = solve(&index);
    assert_eq!(cands.len(), 1);
    assert_eq!(cands[0].owner, fx.id(module));
    assert_eq!(cands[0].candidates, vec![fx.id(target)]);
}

/// flask `test_redirect_with_app(app)`: `app.redirect = redirect` stores into the fixture's
/// `Flask` instance, whose class inherits the method `App.redirect`: a `field_write` flow
/// candidate (only that method), placed as a flow site decided deterministically and
/// materialized as an `inferred_write` edge (a non-call use: never traversed). A store to an
/// attribute no class defines as a member produces nothing.
#[test]
fn attribute_stores_over_methods_are_field_writes() {
    use trace_core::facts::RefKind;
    let mut fx = Fixture::new();
    let lib = fx.file("src/app.py", Language::Python, None);
    let conftest = fx.file("tests/shared_fixtures.py", Language::Python, None);
    let test = fx.file("tests/test_helpers.py", Language::Python, None);
    let app = fx.decl(lib, Decl::class("App"));
    let redirect = fx.decl(lib, Decl::method("redirect", app).params(&["self", "location"]));
    let flask = fx.decl(lib, Decl::class("Flask").bases(&["App"]));
    fx.receiver(lib, redirect, "self", app, false);
    let fixture = fx.decl(conftest, Decl::function("app").decorators(&[PROVIDER]));
    let k = fx.name_ref(conftest, "Flask", flask);
    let made = fx.call(k, vec![]);
    fx.ret(conftest, fixture, made);
    let t = fx.decl(test, Decl::function("test_redirect_with_app").params(&["app"]).test());
    let local = fx.decl(test, Decl::nested("redirect", t).params(&["location"]));
    // app.redirect = redirect
    let object = fx.name("app");
    let attr = fx.reference(test, Some(t), "redirect", RefKind::Write);
    let value = fx.name_ref(test, "redirect", local);
    fx.bind(
        test,
        BindTarget::FieldOf {
            object: object.clone(),
            name: "redirect".into(),
        },
        value,
        Scope::Decl(t.decl),
    );
    // app.config_value = 1 (no member of that name anywhere)
    fx.reference(test, Some(t), "config_value", RefKind::Write);
    let other = fx.name("x");
    fx.bind(
        test,
        BindTarget::FieldOf {
            object,
            name: "config_value".into(),
        },
        other,
        Scope::Decl(t.decl),
    );
    let mut index = fx.build();
    let cands = solve_injected(&index);
    let w = rows(&cands, &fx, t, SiteOperation::FieldWrite);
    assert_eq!(w.len(), 1, "{cands:?}");
    assert_eq!(w[0].candidates, vec![fx.id(redirect)]);
    assert_eq!(w[0].span.end, attr.end);

    let sources = trace_core::source::SourceStore::new(&index);
    let sites = sites_injected(&index, &sources);
    let s = sites
        .iter()
        .find(|s| s.operation == Some(SiteOperation::FieldWrite))
        .expect("field_write site");
    assert_eq!(s.candidates, vec![fx.id(redirect)]);
    index.sites = sites;
    index.decisions = crate::decide::decide(&index);
    let graph = trace_core::Graph::new(&index);
    let writes: Vec<_> = graph
        .edges()
        .iter()
        .filter(|e| e.to == fx.id(redirect) && e.from == fx.id(t))
        .collect();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].kind, EdgeKind::InferredWrite);
    assert_eq!(writes[0].tier, trace_core::Tier::Inferred);
    assert!(!trace_core::tiers::kinds_for(trace_core::Tier::Possible).contains(EdgeKind::InferredWrite));
}

// --- general fixes: calls reaching families through super, subclasses and decorators -----

/// The candidate row of one call (by callee span).
fn row_at(cands: &[FlowCandidate], span: ByteSpan) -> Vec<&FlowCandidate> {
    cands
        .iter()
        .filter(|c| c.span == span && c.operation == SiteOperation::Call && c.via.is_none())
        .collect()
}

/// `super().get(rule)` in `App.get` (Python) and `super.render()` in `Widget.render`
/// (TypeScript) reach the base member, never the override itself and never a CHA-widened
/// set; the syntax rule does the same for Java `super.get` and PHP `parent::handle` in files
/// without a server.
#[test]
fn rule_super_calls_reach_the_base_member() {
    let mut fx = Fixture::new();
    let f = fx.file("app.py", Language::Python, None);
    let base = fx.decl(f, Decl::class("Base"));
    let base_get = fx.decl(f, Decl::method("get", base).params(&["self", "rule"]));
    let app = fx.decl(f, Decl::class("App").bases(&["Base"]));
    let app_get = fx.decl(f, Decl::method("get", app).params(&["self", "rule"]));
    let sub = fx.decl(f, Decl::class("Sub").bases(&["App"]));
    let sub_get = fx.decl(f, Decl::method("get", sub).params(&["self", "rule"]));
    for (m, c) in [(base_get, base), (app_get, app), (sub_get, sub)] {
        fx.receiver(f, m, "self", c, false);
    }
    // App.get: return super().get(rule)   (no semantic target)
    let sup = fx.name("super");
    let sup = fx.call(sup, vec![]);
    let func = fx.attr(sup, "get");
    let rule = fx.name("rule");
    let call = fx.call(func, vec![rule]);
    let py_at = fx.eval(f, app_get, call, "super().get");
    // TypeScript: class Widget extends Component { render() { super.render() } }
    let t = fx.file("ui.ts", Language::TypeScript, None);
    let comp = fx.decl(t, Decl::class("Component"));
    let comp_render = fx.decl(t, Decl::method("render", comp));
    let widget = fx.decl(t, Decl::class("Widget").bases(&["Component"]));
    let widget_render = fx.decl(t, Decl::method("render", widget));
    fx.receiver(t, comp_render, "this", comp, false);
    fx.receiver(t, widget_render, "this", widget, false);
    let sup = fx.name("super");
    let func = fx.attr(sup, "render");
    let call = fx.call(func, vec![]);
    let ts_at = fx.eval(t, widget_render, call, "super.render");
    // Syntax only: Java, PHP.
    let k = fx.blind_file("src/App.java", Language::Java, None);
    let kb = fx.decl(k, Decl::class("Base"));
    let kb_get = fx.decl(k, Decl::method("get", kb));
    let ka = fx.decl(k, Decl::class("App").bases(&["Base"]));
    let ka_get = fx.decl(k, Decl::method("get", ka));
    let k_at = fx.call_n(k, Some(ka_get), "super.get", 1);
    let k_other = fx.call_n(k, Some(ka_get), "other.get", 1);
    let p = fx.blind_file("src/Admin.php", Language::Php, None);
    let pc = fx.decl(p, Decl::class("Controller"));
    let pc_handle = fx.decl(p, Decl::method("handle", pc));
    let pa = fx.decl(p, Decl::class("Admin").bases(&["Controller"]));
    let pa_handle = fx.decl(p, Decl::method("handle", pa));
    let p_at = fx.call_n(p, Some(pa_handle), "parent::handle", 0);
    let index = fx.build();

    let cands = solve(&index);
    let py = row_at(&cands, py_at);
    assert_eq!(py.len(), 1, "{cands:?}");
    assert_eq!(py[0].candidates, vec![fx.id(base_get)]);
    assert!(rows(&cands, &fx, app_get, SiteOperation::OverrideDispatch).is_empty());
    let ts = row_at(&cands, ts_at);
    assert_eq!(ts.len(), 1, "{cands:?}");
    assert_eq!(ts[0].candidates, vec![fx.id(comp_render)]);

    let h = Hierarchy::build(&index);
    let rules = receiver_rule_candidates(&index, &h);
    let by_span = |span: ByteSpan| -> Vec<SymbolId> {
        rules
            .iter()
            .filter(|c| c.span == span)
            .flat_map(|c| c.candidates.iter().copied())
            .collect()
    };
    assert_eq!(by_span(k_at), vec![fx.id(kb_get)]);
    assert_eq!(by_span(p_at), vec![fx.id(pc_handle)]);
    assert!(by_span(k_other).is_empty());
    for c in &rules {
        assert_eq!(c.kind, CandidateKind::Flow);
        assert!(c.field_only.is_empty() && c.via.is_none() && !c.receiver_exact);
    }
    let _ = sub_get;
}

/// `app.get("/")` on an `App()` instance where `App(Base)` inherits `get` reaches
/// `Base.get` (member lookup along the subclass's MRO), while an overriding subclass reaches
/// its override; the same in TypeScript with `new DiskStore()`.
#[test]
fn rule_subclass_instance_calls_reach_the_inherited_member() {
    let mut fx = Fixture::new();
    let f = fx.file("app.py", Language::Python, None);
    let base = fx.decl(f, Decl::class("Base"));
    let base_get = fx.decl(f, Decl::method("get", base).params(&["self", "rule"]));
    let app = fx.decl(f, Decl::class("App").bases(&["Base"]));
    let custom = fx.decl(f, Decl::class("Custom").bases(&["App"]));
    let custom_get = fx.decl(f, Decl::method("get", custom).params(&["self", "rule"]));
    let main = fx.decl(f, Decl::function("main"));
    fx.receiver(f, base_get, "self", base, false);
    fx.receiver(f, custom_get, "self", custom, false);
    let mut calls = Vec::new();
    for (var, class, name) in [("a", app, "App"), ("c", custom, "Custom")] {
        let k = fx.name_ref(f, name, class);
        let made = fx.call(k, vec![]);
        fx.bind(
            f,
            BindTarget::Var {
                scope: Scope::Decl(main.decl),
                name: var.into(),
            },
            made,
            Scope::Decl(main.decl),
        );
        let recv = fx.name(var);
        let func = fx.attr(recv, "get");
        let arg = fx.name("route");
        let call = fx.call(func, vec![arg]);
        calls.push(fx.eval(f, main, call, &format!("{var}.get")));
    }
    // TypeScript: const s = new DiskStore(); s.save()
    let t = fx.file("store.ts", Language::TypeScript, None);
    let store = fx.decl(t, Decl::class("Store"));
    let save = fx.decl(t, Decl::method("save", store));
    let disk = fx.decl(t, Decl::class("DiskStore").bases(&["Store"]));
    let persist = fx.decl(t, Decl::function("persist"));
    fx.receiver(t, save, "this", store, false);
    let k = fx.name_ref(t, "DiskStore", disk);
    let made = fx.call(k, vec![]);
    fx.bind(
        t,
        BindTarget::Var {
            scope: Scope::Decl(persist.decl),
            name: "s".into(),
        },
        made,
        Scope::Decl(persist.decl),
    );
    let recv = fx.name("s");
    let func = fx.attr(recv, "save");
    let call = fx.call(func, vec![]);
    let ts_at = fx.eval(t, persist, call, "s.save");
    let index = fx.build();
    let cands = solve(&index);
    let inherited = row_at(&cands, calls[0]);
    assert_eq!(inherited.len(), 1, "{cands:?}");
    assert_eq!(inherited[0].candidates, vec![fx.id(base_get)]);
    assert!(inherited[0].field_only.is_empty());
    let overridden = row_at(&cands, calls[1]);
    assert_eq!(overridden.len(), 1, "{cands:?}");
    assert_eq!(overridden[0].candidates, vec![fx.id(custom_get)]);
    let ts = row_at(&cands, ts_at);
    assert_eq!(ts.len(), 1, "{cands:?}");
    assert_eq!(ts[0].candidates, vec![fx.id(save)]);
}

/// `@app.get("/")` decorating a nested function in a test whose `app` parameter is fed by a
/// provider injected by name returning `App()` (`App(Base)` inherits `get`): the decorator call is a
/// call of the test and reaches `Base.get`, like the plain call `app.get("/x")`; merged
/// into the call's no_target site, its flow candidates are exactly the family member.
#[test]
fn rule_decorator_calls_on_fixture_instances_reach_the_family() {
    let mut fx = Fixture::new();
    let lib = fx.file("src/app.py", Language::Python, None);
    let conftest = fx.file("tests/shared_fixtures.py", Language::Python, None);
    let test = fx.file("tests/test_routes.py", Language::Python, None);
    let base = fx.decl(lib, Decl::class("Base"));
    let get = fx.decl(lib, Decl::method("get", base).params(&["self", "rule"]));
    let app = fx.decl(lib, Decl::class("App").bases(&["Base"]));
    fx.receiver(lib, get, "self", base, false);
    // shared file: @runner.fixture def app(): return App()
    let fixture = fx.decl(conftest, Decl::function("app").decorators(&[PROVIDER]));
    let k = fx.name_ref(conftest, "App", app);
    let made = fx.call(k, vec![]);
    fx.ret(conftest, fixture, made);
    // def test_index(app):
    //     @app.get("/")
    //     def index(): ...
    //     app.get("/x")
    let t = fx.decl(test, Decl::function("test_index").params(&["app"]).test());
    let index_fn = fx.decl(test, Decl::nested("index", t));
    let recv = fx.name("app");
    let func = fx.attr(recv, "get");
    let arg = fx.name("route");
    let deco = fx.call(func, vec![arg]);
    let spans = fx.decorated_in(test, t, index_fn, "index", vec![(deco, Some("app.get"))]);
    let deco_at = spans[0];
    fx.unresolved(test, t, deco_at, 1, "app.get");
    let recv = fx.name("app");
    let func = fx.attr(recv, "get");
    let arg = fx.name("path");
    let call = fx.call(func, vec![arg]);
    let call_at = fx.eval(test, t, call, "app.get");
    fx.unresolved(test, t, call_at, 1, "app.get");
    let index = fx.build();
    let cands = solve_injected(&index);
    for at in [deco_at, call_at] {
        let row = row_at(&cands, at);
        assert_eq!(row.len(), 1, "{cands:?}");
        assert_eq!(row[0].owner, fx.id(t));
        assert_eq!(row[0].candidates, vec![fx.id(get)]);
        assert!(row[0].field_only.is_empty());
    }

    let sources = trace_core::source::SourceStore::new(&index);
    let sites = sites_injected(&index, &sources);
    let site = sites
        .iter()
        .find(|s| s.at.bytes == deco_at && s.owner == fx.id(t))
        .expect("decorator call site");
    assert_eq!(site.flow_candidates, vec![fx.id(get)]);
    assert!(site.candidates.contains(&fx.id(get)));
}

// --- language fixes: fixtures, class-valued attributes, member copying, library objects ---

/// Injection rules of `row` (its package installed) solved over `index`.
fn solve_rows(index: &Index, row: &trace_library::table::IrreducibleRow) -> Vec<FlowCandidate> {
    let rules = injections([(Language::Python, row)], &|_, package| package == "runner");
    assert_eq!(rules.len(), 1);
    let h = Hierarchy::build(index);
    let flow = Flow::solve_rules(index, &h, &LibraryKnowledge::default(), &rules);
    assert!(flow.iterations < trace_core::config::current().flow.max_iterations);
    flow.candidates()
}

/// `<param>.<method>()` in test `name` of `file` taking `param`; returns (test, callee span).
fn fixture_test(fx: &mut Fixture, file: usize, name: &str, param: &str, method: &str) -> (D, ByteSpan) {
    let t = fx.decl(file, Decl::function(name).params(&[param]).test());
    let recv = fx.name(param);
    let func = fx.attr(recv, method);
    let call = fx.call(func, vec![]);
    let at = fx.eval(file, t, call, &format!("{param}.{method}"));
    (t, at)
}

/// A generator provider (`@runner.fixture def app(): yield App()`): the runtime runs it up to
/// its `yield`, so the injected parameter holds the yielded value.
#[test]
fn rule_generator_fixture_value_is_injected() {
    let mut fx = Fixture::new();
    let lib = fx.file("src/app.py", Language::Python, None);
    let shared = fx.file("tests/shared_fixtures.py", Language::Python, None);
    let test = fx.file("tests/test_app.py", Language::Python, None);
    let app = fx.decl(lib, Decl::class("App"));
    let run = fx.decl(lib, Decl::method("run", app).params(&["self"]));
    fx.receiver(lib, run, "self", app, false);
    let fixture = fx.decl(
        shared,
        Decl::function("app")
            .decorators(&[PROVIDER])
            .execution(ExecutionModel::Generator),
    );
    let k = fx.name_ref(shared, "App", app);
    let made = fx.call(k, vec![]);
    fx.bind(
        shared,
        BindTarget::Var {
            scope: Scope::Decl(fixture.decl),
            name: YIELDED.into(),
        },
        made,
        Scope::Decl(fixture.decl),
    );
    let (t, at) = fixture_test(&mut fx, test, "test_run", "app", "run");
    let index = fx.build();
    let cands = solve_injected(&index);
    let row = row_at(&cands, at);
    assert_eq!(row.len(), 1, "{cands:?}");
    assert_eq!(row[0].owner, fx.id(t));
    assert_eq!(row[0].candidates, vec![fx.id(run)]);
}

/// Providers requesting providers chain: `client(app)` receives `app`'s value and returns
/// `app.client()`, so the test's `client` parameter holds a `Client`.
#[test]
fn rule_fixture_chain_types_the_parameter() {
    let mut fx = Fixture::new();
    let lib = fx.file("src/app.py", Language::Python, None);
    let shared = fx.file("tests/shared_fixtures.py", Language::Python, None);
    let test = fx.file("tests/test_client.py", Language::Python, None);
    let app = fx.decl(lib, Decl::class("App"));
    let make_client = fx.decl(lib, Decl::method("client", app).params(&["self"]));
    let client = fx.decl(lib, Decl::class("Client"));
    let get = fx.decl(lib, Decl::method("get", client).params(&["self"]));
    fx.receiver(lib, make_client, "self", app, false);
    fx.receiver(lib, get, "self", client, false);
    let k = fx.name_ref(lib, "Client", client);
    let made = fx.call(k, vec![]);
    fx.ret(lib, make_client, made);
    // @runner.fixture def app(): return App()
    let app_fixture = fx.decl(shared, Decl::function("app").decorators(&[PROVIDER]));
    let k = fx.name_ref(shared, "App", app);
    let made = fx.call(k, vec![]);
    fx.ret(shared, app_fixture, made);
    // @runner.fixture def client(app): return app.client()
    let client_fixture = fx.decl(shared, Decl::function("client").params(&["app"]).decorators(&[PROVIDER]));
    let recv = fx.name("app");
    let func = fx.attr(recv, "client");
    let made = fx.call(func, vec![]);
    fx.ret(shared, client_fixture, made);
    let (_, at) = fixture_test(&mut fx, test, "test_get", "client", "get");
    let index = fx.build();
    let cands = solve_injected(&index);
    let row = row_at(&cands, at);
    assert_eq!(row.len(), 1, "{cands:?}");
    assert_eq!(row[0].candidates, vec![fx.id(get)]);
}

/// Providers of the shared file closest to the test win: `tests/sub/` tests see the
/// `tests/sub/` provider, other `tests/` tests the `tests/` one.
#[test]
fn rule_nearest_conftest_fixture_wins() {
    let mut fx = Fixture::new();
    let lib = fx.file("src/app.py", Language::Python, None);
    let outer = fx.file("tests/shared_fixtures.py", Language::Python, None);
    let inner = fx.file("tests/sub/shared_fixtures.py", Language::Python, None);
    let deep_test = fx.file("tests/sub/test_deep.py", Language::Python, None);
    let top_test = fx.file("tests/test_top.py", Language::Python, None);
    let mut runs = Vec::new();
    let mut classes = Vec::new();
    for name in ["OuterApp", "InnerApp"] {
        let class = fx.decl(lib, Decl::class(name));
        let run = fx.decl(lib, Decl::method("run", class).params(&["self"]));
        fx.receiver(lib, run, "self", class, false);
        classes.push(class);
        runs.push(run);
    }
    for (file, class) in [(outer, classes[0]), (inner, classes[1])] {
        let fixture = fx.decl(file, Decl::function("app").decorators(&[PROVIDER]));
        let k = fx.name_ref(file, "App", class);
        let made = fx.call(k, vec![]);
        fx.ret(file, fixture, made);
    }
    let (_, deep_at) = fixture_test(&mut fx, deep_test, "test_deep", "app", "run");
    let (_, top_at) = fixture_test(&mut fx, top_test, "test_top", "app", "run");
    let index = fx.build();
    let cands = solve_injected(&index);
    assert_eq!(row_at(&cands, deep_at)[0].candidates, vec![fx.id(runs[1])], "{cands:?}");
    assert_eq!(row_at(&cands, top_at)[0].candidates, vec![fx.id(runs[0])], "{cands:?}");
}

/// The provider decorator's renaming keyword (the row's `key`, `{"kw": "name"}`):
/// `@runner.fixture(name="client") def make_client()` provides `client`, not `make_client`.
/// Without the row's key the provider keeps its own name.
#[test]
fn rule_fixture_name_keyword_renames_it() {
    let mut fx = Fixture::new();
    let lib = fx.file("src/app.py", Language::Python, None);
    let shared = fx.file("tests/shared_fixtures.py", Language::Python, None);
    let test = fx.file("tests/test_client.py", Language::Python, None);
    let client = fx.decl(lib, Decl::class("Client"));
    let get = fx.decl(lib, Decl::method("get", client).params(&["self"]));
    fx.receiver(lib, get, "self", client, false);
    let provider =
        fx.decl(shared, Decl::function("make_client").decorators(&["runner.fixture(name=\"client\")"]));
    let k = fx.name_ref(shared, "Client", client);
    let made = fx.call(k, vec![]);
    fx.ret(shared, provider, made);
    // The lowered decorator: `runner.fixture(name=<lit>client)`.
    let runner = fx.name("runner");
    let head = fx.attr(runner, "fixture");
    let literal = fx.name("<lit>client");
    let deco = fx.call_kw(head, vec![], vec![("name", literal)]);
    fx.flow(
        shared,
        FlowFact::Decorated {
            scope: Scope::Module,
            target: BindTarget::Var {
                scope: Scope::Module,
                name: "make_client".into(),
            },
            function: provider.decl,
            decorators: vec![deco],
        },
    );
    let (_, renamed_at) = fixture_test(&mut fx, test, "test_get", "client", "get");
    let (_, own_at) = fixture_test(&mut fx, test, "test_own", "make_client", "get");
    let index = fx.build();
    let row = trace_library::table::IrreducibleRow {
        key: Some(serde_json::json!({"kw": "name"})),
        ..injection_row()
    };
    let cands = solve_rows(&index, &row);
    assert_eq!(row_at(&cands, renamed_at)[0].candidates, vec![fx.id(get)], "{cands:?}");
    assert!(row_at(&cands, own_at).is_empty(), "the provider no longer answers to its own name");
    let plain = solve_injected(&index);
    assert!(row_at(&plain, renamed_at).is_empty());
    assert_eq!(row_at(&plain, own_at)[0].candidates, vec![fx.id(get)]);
}

/// `self.response_class(rv)` where the class body stores a class (`response_class =
/// Response`): the call constructs the stored class (the class itself when it has no
/// constructor in the index); a subclass rebinding the attribute adds its class's
/// constructor.
#[test]
fn rule_class_valued_attribute_call_constructs_the_stored_class() {
    let mut fx = Fixture::new();
    let f = fx.file("src/app.py", Language::Python, None);
    let response = fx.decl(f, Decl::class("Response").bases(&["LibraryResponse"]));
    let json_response = fx.decl(f, Decl::class("JSONResponse"));
    let json_init = fx.decl(f, Decl::method("__init__", json_response).params(&["self", "rv"]));
    let app = fx.decl(f, Decl::class("App"));
    let make = fx.decl(f, Decl::method("make_response", app).params(&["self", "rv"]));
    let sub = fx.decl(f, Decl::class("JSONApp").bases(&["App"]));
    fx.receiver(f, json_init, "self", json_response, false);
    fx.receiver(f, make, "self", app, false);
    let v = fx.name_ref(f, "Response", response);
    fx.member(f, app, "response_class", v);
    let v = fx.name_ref(f, "JSONResponse", json_response);
    fx.member(f, sub, "response_class", v);
    let recv = fx.name("self");
    let func = fx.attr(recv, "response_class");
    let arg = fx.name("rv");
    let call = fx.call(func, vec![arg]);
    let at = fx.eval(f, make, call, "self.response_class");
    let index = fx.build();
    let cands = solve(&index);
    let row = row_at(&cands, at);
    assert_eq!(row.len(), 1, "{cands:?}");
    let mut expected = vec![fx.id(json_init), fx.id(response)];
    expected.sort_by(|a, b| index.symbol(*a).uid.cmp(&index.symbol(*b).uid));
    assert_eq!(row[0].candidates, expected);
    assert!(row[0].field_only.is_empty());
}

/// Binds `name = <object literal with fields>` at module level; returns nothing.
fn object_literal(fx: &mut Fixture, file: usize, name: &str, fields: Vec<(&str, Expr)>) {
    let object = fx.name(OBJECT_CALLEE);
    let literal = fx.call_kw(object, vec![], fields);
    fx.bind(
        file,
        BindTarget::Var {
            scope: Scope::Module,
            name: name.into(),
        },
        literal,
        Scope::Module,
    );
}

/// An object literal `{init: init}` (plain object) copied into a function object by a
/// library whose knowledge says it copies every member (`copies_members` from argument 1 to
/// argument 0, a mixin): `app.init()` on the function object reaches `init`. A function
/// object nothing was copied into has no such member.
#[test]
fn rule_mixin_copies_members_into_the_target() {
    let mut fx = Fixture::new();
    let f = fx.file("lib/app.js", Language::JavaScript, None);
    let init = fx.decl(f, Decl::function("init"));
    let create = fx.decl(f, Decl::function("createApp"));
    let handle = fx.decl(f, Decl::lambda(create));
    let plain = fx.decl(f, Decl::function("plain"));
    let bare = fx.decl(f, Decl::lambda(plain));
    let main = fx.decl(f, Decl::function("main"));
    let member = fx.name_ref(f, "init", init);
    object_literal(&mut fx, f, "proto", vec![("init", member)]);
    // createApp / plain: var app = function () {}; return app
    for (scope, lambda) in [(create, handle), (plain, bare)] {
        let value = fx.lambda_expr(lambda);
        fx.bind(
            f,
            BindTarget::Var {
                scope: Scope::Decl(scope.decl),
                name: "app".into(),
            },
            value,
            Scope::Decl(scope.decl),
        );
        let ret = fx.name("app");
        fx.ret(f, scope, ret);
    }
    // createApp: mixin(app, proto)
    let mixin = fx.name("mixin");
    let target = fx.name("app");
    let source = fx.name("proto");
    let call = fx.call(mixin, vec![target, source]);
    let mixin_at = fx.eval(f, create, call, "mixin");
    // main: createApp().init(); plain().init()
    let mut ats = Vec::new();
    for (maker, name) in [(create, "createApp"), (plain, "plain")] {
        let callee = fx.name_ref(f, name, maker);
        let made = fx.call(callee, vec![]);
        let func = fx.attr(made, "init");
        let call = fx.call(func, vec![]);
        ats.push(fx.eval(f, main, call, &format!("{name}().init")));
    }
    let index = fx.build();
    let k = knowledge_at(&[(
        "lib/app.js",
        mixin_at.start,
        vec![Effect::CopiesMembers {
            from: ArgSel::Pos(1),
            to: ArgSel::Pos(0),
        }],
        true,
    )]);
    let cands = solve_known(&index, &k);
    let copied = row_at(&cands, ats[0]);
    assert_eq!(copied.len(), 1, "{cands:?}");
    assert_eq!(copied[0].candidates, vec![fx.id(init)]);
    assert!(row_at(&cands, ats[1]).is_empty(), "nothing was copied into plain()'s object");
    // Without the knowledge nothing is copied.
    assert!(row_at(&solve(&index), ats[0]).is_empty());
}

/// `Object.create(proto)` (knowledge: `delegates_members` with the result as the object, or
/// the JavaScript language rule of the object model without knowledge): the new object's
/// member lookups continue in `proto`.
#[test]
fn rule_object_create_delegates_member_lookup() {
    let mut fx = Fixture::new();
    let f = fx.file("lib/request.js", Language::JavaScript, None);
    let get = fx.decl(f, Decl::function("get"));
    let main = fx.decl(f, Decl::function("main"));
    let member = fx.name_ref(f, "get", get);
    object_literal(&mut fx, f, "proto", vec![("get", member)]);
    let global = fx.name("Object");
    let create = fx.attr(global, "create");
    let source = fx.name("proto");
    let made = fx.call(create, vec![source]);
    let create_at = made.span().expect("call span").start;
    fx.bind(
        f,
        BindTarget::Var {
            scope: Scope::Decl(main.decl),
            name: "req".into(),
        },
        made,
        Scope::Decl(main.decl),
    );
    let recv = fx.name("req");
    let func = fx.attr(recv, "get");
    let call = fx.call(func, vec![]);
    let at = fx.eval(f, main, call, "req.get");
    let index = fx.build();
    let k = knowledge_at(&[(
        "lib/request.js",
        create_at,
        vec![Effect::DelegatesMembers {
            object: ArgSel::Result,
            to: ArgSel::Pos(0),
        }],
        true,
    )]);
    let cands = solve_known(&index, &k);
    assert_eq!(row_at(&cands, at)[0].candidates, vec![fx.id(get)], "{cands:?}");
    assert_eq!(row_at(&solve(&index), at)[0].candidates, vec![fx.id(get)]);
}

/// `Object.setPrototypeOf(o, proto)` (knowledge: `delegates_members` from argument 0 to
/// argument 1, or the language rule without knowledge) and the JavaScript `p.__proto__ =
/// proto` store (language rule) make the objects' lookups continue in `proto`.
#[test]
fn rule_set_prototype_delegates() {
    let mut fx = Fixture::new();
    let f = fx.file("lib/proto.js", Language::JavaScript, None);
    let get = fx.decl(f, Decl::function("get"));
    let main = fx.decl(f, Decl::function("main"));
    let member = fx.name_ref(f, "get", get);
    object_literal(&mut fx, f, "proto", vec![("get", member)]);
    for name in ["o", "p"] {
        let object = fx.name(OBJECT_CALLEE);
        let empty = fx.call_kw(object, vec![], vec![]);
        fx.bind(
            f,
            BindTarget::Var {
                scope: Scope::Decl(main.decl),
                name: name.into(),
            },
            empty,
            Scope::Decl(main.decl),
        );
    }
    // Object.setPrototypeOf(o, proto)
    let global = fx.name("Object");
    let set = fx.attr(global, "setPrototypeOf");
    let o = fx.name("o");
    let source = fx.name("proto");
    let call = fx.call(set, vec![o, source]);
    let set_at = fx.eval(f, main, call, "Object.setPrototypeOf");
    // p.__proto__ = proto
    let source = fx.name("proto");
    fx.field_store(f, main, "p", "__proto__", source);
    let mut ats = Vec::new();
    for name in ["o", "p"] {
        let recv = fx.name(name);
        let func = fx.attr(recv, "get");
        let call = fx.call(func, vec![]);
        ats.push(fx.eval(f, main, call, &format!("{name}.get")));
    }
    let index = fx.build();
    let k = knowledge_at(&[(
        "lib/proto.js",
        set_at.start,
        vec![Effect::DelegatesMembers {
            object: ArgSel::Pos(0),
            to: ArgSel::Pos(1),
        }],
        true,
    )]);
    let cands = solve_known(&index, &k);
    for at in &ats {
        assert_eq!(row_at(&cands, *at)[0].candidates, vec![fx.id(get)], "{cands:?}");
    }
    // Without the knowledge the language rules apply.
    let plain = solve(&index);
    for at in &ats {
        assert_eq!(row_at(&plain, *at)[0].candidates, vec![fx.id(get)]);
    }
}

/// `const C = {}; C.__index = C; C.m = function () {}; const o = attach({}, C)` where the
/// knowledge of `attach` says it delegates from argument 0 to the `__index` field of
/// argument 1 and returns argument 0: `o.m()` reaches `C.m`; an object without `__index`
/// delegates nothing.
#[test]
fn rule_field_delegation_delegates_member_lookup() {
    let mut fx = Fixture::new();
    let f = fx.file("src/picker.js", Language::JavaScript, None);
    let m = fx.decl(f, Decl::function("m").qualified("C.m"));
    let main = fx.decl(f, Decl::function("main"));
    for class in ["C", "D"] {
        object_literal(&mut fx, f, class, Vec::new());
        let method = fx.lambda_expr(m);
        let object = fx.name(class);
        fx.bind(
            f,
            BindTarget::FieldOf {
                object,
                name: "m".into(),
            },
            method,
            Scope::Module,
        );
    }
    // C.__index = C (D has none)
    let object = fx.name("C");
    let value = fx.name("C");
    fx.bind(
        f,
        BindTarget::FieldOf {
            object,
            name: "__index".into(),
        },
        value,
        Scope::Module,
    );
    let mut sets = Vec::new();
    let mut ats = Vec::new();
    for (var, class) in [("o", "C"), ("p", "D")] {
        let set = fx.name("attach");
        let object = fx.name(OBJECT_CALLEE);
        let empty = fx.call_kw(object, vec![], vec![]);
        let meta = fx.name(class);
        let made = fx.call(set, vec![empty, meta]);
        sets.push(made.span().expect("call span").start);
        fx.bind(
            f,
            BindTarget::Var {
                scope: Scope::Decl(main.decl),
                name: var.into(),
            },
            made,
            Scope::Decl(main.decl),
        );
        let recv = fx.name(var);
        let func = fx.attr(recv, "m");
        let call = fx.call(func, vec![]);
        ats.push(fx.eval(f, main, call, &format!("{var}:m")));
    }
    let index = fx.build();
    let effects = vec![
        Effect::DelegatesMembers {
            object: ArgSel::Pos(0),
            to: ArgSel::Field {
                arg: 1,
                field: "__index".into(),
            },
        },
        Effect::Returns(ArgSel::Pos(0)),
    ];
    let k = knowledge_at(&[
        ("src/picker.js", sets[0], effects.clone(), true),
        ("src/picker.js", sets[1], effects, true),
    ]);
    let cands = solve_known(&index, &k);
    assert_eq!(row_at(&cands, ats[0])[0].candidates, vec![fx.id(m)], "{cands:?}");
    assert!(row_at(&cands, ats[1]).is_empty(), "no __index: no delegation");
}

/// `var agent = make(); agent.get("/").expect(200)` where the server resolved `make` into
/// library code: the receivers of `get` and of `expect` are only library-created objects,
/// so both calls are library receivers with `make`'s library symbol as the evidence
/// (sorted, the same on every solve).
#[test]
fn rule_members_of_library_created_objects_are_library_calls() {
    let mut fx = Fixture::new();
    let f = fx.file("test/app.test.js", Language::JavaScript, None);
    let t = fx.decl(f, Decl::function("test_app").test());
    let make = fx.name("make");
    let made = fx.call(make, vec![]);
    let Expr::Call {
        func_span: make_at, ..
    } = &made
    else {
        unreachable!("a call expression");
    };
    fx.library_call(f, *make_at, "lib.make");
    fx.bind(
        f,
        BindTarget::Var {
            scope: Scope::Decl(t.decl),
            name: "agent".into(),
        },
        made,
        Scope::Decl(t.decl),
    );
    let recv = fx.name("agent");
    let func = fx.attr(recv, "get");
    let path = fx.name("<lit>");
    let get = fx.call(func, vec![path]);
    let get_at = fx.eval(f, t, get.clone(), "agent.get");
    let func = fx.attr(get, "expect");
    let status = fx.name("<lit>");
    let expect = fx.call(func, vec![status]);
    let expect_at = fx.eval(f, t, expect, "agent.get(...).expect");
    let index = fx.build();
    let h = Hierarchy::build(&index);
    let flow = Flow::solve(&index, &h);
    let receivers = flow.library_receivers();
    let at: Vec<ByteSpan> = receivers.iter().map(|r| r.at.bytes).collect();
    assert!(at.contains(&get_at), "{receivers:?}");
    assert!(at.contains(&expect_at), "{receivers:?}");
    assert!(receivers.iter().all(|r| r.library == "lib.make"));
    let mut sorted = receivers.clone();
    sorted.sort();
    assert_eq!(receivers, sorted);
    assert_eq!(flow.library_receivers(), receivers, "repeatable");
}

/// A library object whose member repository code stored (`agent.handler = helper`) keeps
/// that member (no library receiver there, the stored function is the candidate); a library
/// call receiving a repository object may hand it back, so its result is no library object.
#[test]
fn rule_patched_library_object_keeps_repository_members() {
    let mut fx = Fixture::new();
    let f = fx.file("test/patch.test.js", Language::JavaScript, None);
    let helper = fx.decl(f, Decl::function("helper"));
    let k = fx.decl(f, Decl::class("K"));
    let t = fx.decl(f, Decl::function("test_patch").test());
    let mut make_callees = Vec::new();
    for (var, arg) in [("agent", None), ("other", Some(k))] {
        let make = fx.name("make");
        let args = match arg {
            Some(class) => {
                let c = fx.name_ref(f, "K", class);
                vec![fx.call(c, vec![])]
            }
            None => Vec::new(),
        };
        let made = fx.call(make, args);
        if let Expr::Call { func_span, .. } = &made {
            make_callees.push(*func_span);
        }
        fx.bind(
            f,
            BindTarget::Var {
                scope: Scope::Decl(t.decl),
                name: var.into(),
            },
            made,
            Scope::Decl(t.decl),
        );
    }
    for at in make_callees {
        fx.library_call(f, at, "lib.make");
    }
    let stored = fx.name_ref(f, "helper", helper);
    fx.field_store(f, t, "agent", "handler", stored);
    let mut ats = Vec::new();
    for (var, member) in [("agent", "handler"), ("agent", "get"), ("other", "run")] {
        let recv = fx.name(var);
        let func = fx.attr(recv, member);
        let call = fx.call(func, vec![]);
        ats.push(fx.eval(f, t, call, &format!("{var}.{member}")));
    }
    let index = fx.build();
    let h = Hierarchy::build(&index);
    let flow = Flow::solve(&index, &h);
    let at: Vec<ByteSpan> = flow.library_receivers().iter().map(|r| r.at.bytes).collect();
    assert!(!at.contains(&ats[0]), "patched member");
    assert!(at.contains(&ats[1]), "unpatched member of the same object");
    assert!(!at.contains(&ats[2]), "a repository object went into the library call");
    let cands = flow.candidates();
    assert_eq!(row_at(&cands, ats[0])[0].candidates, vec![fx.id(helper)]);
}

/// A call of a parameter reaches every function passed for it at the call sites of its
/// function (`run(a); run(b)` -> `cb()` reaches `a` and `b`); a middleware only library code
/// runs (`use(mw)` whose knowledge stores and later calls it) gets its `next` argument from
/// the library, so `next()` is a library receiver. A function the repository also calls is
/// not.
#[test]
fn rule_parameter_call_reaches_every_passed_function() {
    let mut fx = Fixture::new();
    let f = fx.file("lib/router.js", Language::JavaScript, None);
    let a = fx.decl(f, Decl::function("a"));
    let b = fx.decl(f, Decl::function("b"));
    let run = fx.decl(f, Decl::function("run").params(&["cb"]));
    let mw = fx.decl(f, Decl::function("mw").params(&["req", "next"]));
    let mw2 = fx.decl(f, Decl::function("mw2").params(&["next"]));
    let main = fx.decl(f, Decl::function("main"));
    // run(cb) { cb() }
    let cb = fx.name("cb");
    let call = fx.call(cb, vec![]);
    let cb_at = fx.eval(f, run, call, "cb");
    // mw(req, next) { next() }, mw2(next) { next() }
    let mut next_ats = Vec::new();
    for owner in [mw, mw2] {
        let next = fx.name("next");
        let call = fx.call(next, vec![]);
        next_ats.push(fx.eval(f, owner, call, "next"));
    }
    // main: run(a); run(b)
    for (target, name) in [(a, "a"), (b, "b")] {
        let callee = fx.name_ref(f, "run", run);
        let arg = fx.name_ref(f, name, target);
        let call = fx.call(callee, vec![arg]);
        let at = fx.eval(f, main, call, "run");
        fx.edge(f, main, run, EdgeKind::Calls, at, 1);
    }
    // main: use(mw); use(mw2)
    let mut use_ats = Vec::new();
    for (target, name) in [(mw, "mw"), (mw2, "mw2")] {
        let callee = fx.name("use");
        let arg = fx.name_ref(f, name, target);
        let call = fx.call(callee, vec![arg]);
        let at = fx.eval(f, main, call, "use");
        fx.library_call(f, at, "lib.use");
        use_ats.push(at);
    }
    // main: mw2(a) (the repository calls mw2 itself)
    let callee = fx.name_ref(f, "mw2", mw2);
    let arg = fx.name_ref(f, "a", a);
    let call = fx.call(callee, vec![arg]);
    let direct = fx.eval(f, main, call, "mw2");
    fx.edge(f, main, mw2, EdgeKind::Calls, direct, 1);
    let index = fx.build();
    let stored = vec![Effect::StoredThenCalled(ArgSel::Pos(0))];
    let k = knowledge_at(&[
        ("lib/router.js", use_ats[0].start, stored.clone(), true),
        ("lib/router.js", use_ats[1].start, stored, true),
    ]);
    let h = Hierarchy::build(&index);
    let flow = Flow::solve_rules(&index, &h, &k, &[]);
    let cands = flow.candidates();
    let mut expected = vec![fx.id(a), fx.id(b)];
    expected.sort_by(|x, y| index.symbol(*x).uid.cmp(&index.symbol(*y).uid));
    assert_eq!(row_at(&cands, cb_at)[0].candidates, expected);
    let receivers = flow.library_receivers();
    let at: Vec<ByteSpan> = receivers.iter().map(|r| r.at.bytes).collect();
    assert!(
        receivers
            .iter()
            .any(|r| r.at.bytes == next_ats[0] && r.library == "lib.use"),
        "{receivers:?}"
    );
    assert!(!at.contains(&next_ats[1]), "mw2 is also called by the repository");
    assert!(!at.contains(&cb_at));
}
