use super::super::*;
use crate::test_support::{Decl, Fixture};
use trace_core::facts::{BindTarget, Scope};

/// `def app(): app = App(); return app` (the local shadows the module-level function
/// `app`), and `def test(app): def inner(): app.run()` (a named nested function reads the
/// enclosing parameter): with the reads proven local, `app.run()` has exactly `App.run`
/// and the fixture returns only the instance.
#[test]
fn rule_local_reads_follow_lexical_scoping() {
    let mut fx = Fixture::new();
    let f = fx.file("tests/test_app.py", Language::Python, None);
    let class = fx.decl(f, Decl::class("App"));
    let run = fx.decl(f, Decl::method("run", class).params(&["self"]));
    fx.receiver(f, run, "self", class, false);
    let factory = fx.decl(f, Decl::function("app"));
    // module level: `app` is the function (a module variable of that name).
    let module_value = fx.name_ref(f, "app", factory);
    fx.bind(
        f,
        BindTarget::Var {
            scope: Scope::Module,
            name: "app".into(),
        },
        module_value,
        Scope::Module,
    );
    let k = fx.name_ref(f, "App", class);
    let made = fx.call(k, vec![]);
    fx.bind(
        f,
        BindTarget::Var {
            scope: Scope::Decl(factory.decl),
            name: "app".into(),
        },
        made,
        Scope::Decl(factory.decl),
    );
    let returned = fx.name("app");
    let returned_span = returned.span().expect("span");
    fx.ret(f, factory, returned);
    fx.local(f, returned_span);
    let t = fx.decl(f, Decl::function("test_run").params(&["app"]).test());
    let inner = fx.decl(f, Decl::nested("inner", t));
    let recv = fx.name("app");
    let recv_span = recv.span().expect("span");
    let func = fx.attr(recv, "run");
    let call = fx.call(func, vec![]);
    let at = fx.eval(f, inner, call, "app.run");
    fx.local(f, recv_span);
    // the test's parameter holds what `app()` returns.
    let k = fx.name_ref(f, "app", factory);
    let value = fx.call(k, vec![]);
    fx.bind(
        f,
        BindTarget::Var {
            scope: Scope::Decl(t.decl),
            name: "app".into(),
        },
        value,
        Scope::Decl(t.decl),
    );
    let index = fx.build();
    let h = Hierarchy::build(&index);
    let flow = Flow::solve(&index, &h);
    let cands: Vec<_> = flow.candidates().into_iter().filter(|c| c.span == at).collect();
    assert_eq!(cands.len(), 1, "{cands:?}");
    assert_eq!(cands[0].candidates, vec![fx.id(run)]);
    let ret = flow.ev(View::Full).slot(Slot::Return(fx.id(factory), Recv::Unknown));
    let values: Vec<Value> = ret.map(|v| v.to_vals().iter().copied().collect()).unwrap_or_default();
    assert!(values.iter().all(|v| matches!(v, Value::Instance(_))), "{values:?}");
}
