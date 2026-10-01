use super::super::*;
use super::LibraryClass;
use crate::test_support::{Decl, Fixture};
use trace_core::facts::{BindTarget, Scope};

fn knowledge(path: &str, start: u32, symbol: &str, members: &[&str]) -> LibraryKnowledge {
    let mut k = LibraryKnowledge::default();
    k.classes.insert(
        (path.to_string(), start),
        LibraryClass {
            symbol: symbol.to_string(),
            members: members
                .iter()
                .map(|m| (m.to_string(), format!("{symbol}.{m}")))
                .collect(),
        },
    );
    k
}

/// `class AppClient(lib.Client)`; `client = AppClient(); client.get("/")`: `get` is
/// declared by no repository class along the MRO but by the library base, so the call
/// is a library call of `lib.Client.get`. `client.open()` (a repository override) and
/// `client.missing()` (declared nowhere) are not; neither is a member repository code
/// stored on the instance.
#[test]
fn rule_member_of_library_base_is_a_library_call() {
    let mut fx = Fixture::new();
    let f = fx.file("tests/test_client.py", Language::Python, None);
    let class = fx.decl(f, Decl::class("AppClient").bases(&["Client"]));
    let open = fx.decl(f, Decl::method("open", class).params(&["self"]));
    fx.receiver(f, open, "self", class, false);
    let helper = fx.decl(f, Decl::function("helper"));
    let t = fx.decl(f, Decl::function("test_get").test());
    let k = fx.name_ref(f, "AppClient", class);
    let made = fx.call(k, vec![]);
    fx.bind(
        f,
        BindTarget::Var {
            scope: Scope::Decl(t.decl),
            name: "client".into(),
        },
        made,
        Scope::Decl(t.decl),
    );
    let stored = fx.name_ref(f, "helper", helper);
    fx.field_store(f, t, "client", "post", stored);
    let mut ats = Vec::new();
    for member in ["get", "open", "missing", "post"] {
        let recv = fx.name("client");
        let func = fx.attr(recv, member);
        let path = fx.name("<lit>");
        let call = fx.call(func, vec![path]);
        ats.push(fx.eval(f, t, call, &format!("client.{member}")));
    }
    let header = fx.decl_span(class).start;
    let index = fx.build();
    let h = Hierarchy::build(&index);
    let k = knowledge("tests/test_client.py", header, "lib.Client", &["get", "open", "post"]);
    let flow = Flow::solve_rules(&index, &h, &k, &[]);
    let receivers = flow.library_receivers();
    let found: Vec<(ByteSpan, &str)> = receivers.iter().map(|r| (r.at.bytes, r.library.as_str())).collect();
    assert_eq!(found, vec![(ats[0], "lib.Client.get")], "{receivers:?}");
    // Without the library base nothing is known about `get`.
    let flow = Flow::solve(&index, &h);
    assert!(flow.library_receivers().iter().all(|r| r.at.bytes != ats[0]));
}

/// Python data model: a repository class declaring `__getattribute__` intercepts every
/// member access, so its library base's members are not what a call runs.
#[test]
fn rule_member_of_library_base_not_behind_attribute_hook() {
    let mut fx = Fixture::new();
    let f = fx.file("tests/test_proxy.py", Language::Python, None);
    let class = fx.decl(f, Decl::class("Proxy").bases(&["Client"]));
    let hook = fx.decl(f, Decl::method("__getattribute__", class).params(&["self", "name"]));
    fx.receiver(f, hook, "self", class, false);
    let t = fx.decl(f, Decl::function("test_proxy").test());
    let k = fx.name_ref(f, "Proxy", class);
    let made = fx.call(k, vec![]);
    fx.bind(
        f,
        BindTarget::Var {
            scope: Scope::Decl(t.decl),
            name: "proxy".into(),
        },
        made,
        Scope::Decl(t.decl),
    );
    let recv = fx.name("proxy");
    let func = fx.attr(recv, "get");
    let call = fx.call(func, vec![]);
    let at = fx.eval(f, t, call, "proxy.get");
    let header = fx.decl_span(class).start;
    let index = fx.build();
    let h = Hierarchy::build(&index);
    let k = knowledge("tests/test_proxy.py", header, "lib.Client", &["get"]);
    let flow = Flow::solve_rules(&index, &h, &k, &[]);
    assert!(flow.library_receivers().iter().all(|r| r.at.bytes != at));
}
