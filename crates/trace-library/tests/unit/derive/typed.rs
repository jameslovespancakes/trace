//! Rule tests of typed dispatch (fixture: `tests/fixtures/rule-derive-typed`).

use std::path::Path;

use trace_core::Language;

use super::super::{FileSummaries, FunctionSummary};
use crate::channels::ChannelRow;
use crate::model::{ArgSel, Channel, Effect, VerbSel};
use crate::table::Section;
use crate::test_support::{derive_in, fixture_bytes, MapLoader, TestLeaves};

/// The entry protocol rows of the fixture's language (server protocol by specification).
fn entry_rows() -> TestLeaves {
    let row = |symbol: &str, pattern: &str| ChannelRow {
        symbol: Some(symbol.to_string()),
        pattern: Some(pattern.to_string()),
        channel: Some(Channel::Http),
        ..ChannelRow::default()
    };
    TestLeaves::rows(vec![
        (Section::IoEntry, row("net/http.Handler.ServeHTTP", "ServeHTTP/2")),
        (Section::IoEntry, row("router.Server.Serve", "Serve/2")),
    ])
}

fn router() -> FileSummaries {
    let src = fixture_bytes("rule-derive-typed/router.go");
    let path = Path::new("/virtual-library/router/router.go");
    derive_in(Language::Go, path, &src, &entry_rows(), &MapLoader::default(), &[])
}

fn method<'a>(s: &'a FileSummaries, qualified: &str) -> &'a FunctionSummary {
    s.by_qualified(qualified)
        .unwrap_or_else(|| panic!("no summary for {qualified}"))
}

fn registrations(f: &FunctionSummary) -> Vec<&Effect> {
    f.effects
        .iter()
        .filter(|e| matches!(e, Effect::Registers { .. }))
        .collect()
}

fn registers(key: u32, handler: ArgSel, verb: VerbSel) -> Effect {
    Effect::Registers {
        channel: Channel::Http,
        key: ArgSel::Pos(key),
        handler,
        verb,
    }
}

#[test]
fn rule_typed_dispatch_function_type_called_by_entry_registers_its_parameters() {
    let s = router();
    // `c.handlers[c.index](c)` reached from ServeHTTP: HandlerFunc is dispatched; a variadic
    // handler list registers its last element; the method name spells the verb (the same
    // registration with any verb is redundant).
    let get = method(&s, "Router.GET");
    assert_eq!(
        registrations(get),
        vec![&registers(0, ArgSel::Last, VerbSel::Const("GET".to_string()))],
        "{:?}",
        get.effects
    );
}

#[test]
fn rule_typed_dispatch_key_is_the_nearest_string_parameter_before_the_handler() {
    let s = router();
    let handle = method(&s, "Router.Handle");
    assert!(
        registrations(handle).contains(&&registers(1, ArgSel::Last, VerbSel::Any)),
        "{:?}",
        handle.effects
    );
    assert!(
        registrations(handle).iter().all(|e| matches!(
            e,
            Effect::Registers {
                key: ArgSel::Pos(1),
                ..
            }
        )),
        "{:?}",
        handle.effects
    );
}

#[test]
fn rule_typed_dispatch_middleware_types_register_nothing() {
    let s = router();
    // MiddlewareFunc is called by the entry too, but composes handler types.
    assert!(registrations(method(&s, "Router.Use")).is_empty());
}

#[test]
fn rule_typed_dispatch_entry_protocol_types_are_handlers() {
    let s = router();
    // The protocol interface itself, and a type declaring the protocol method.
    assert!(registrations(method(&s, "Router.Mount")).contains(&&registers(0, ArgSel::Pos(1), VerbSel::Any)));
    assert!(registrations(method(&s, "Router.HandleFunc")).contains(&&registers(
        0,
        ArgSel::Pos(1),
        VerbSel::Any
    )));
}

#[test]
fn rule_typed_dispatch_needs_a_call_from_entry_reachable_code() {
    let s = router();
    assert!(registrations(method(&s, "Router.OnStart")).is_empty());
}

#[test]
fn rule_typed_dispatch_group_prefix_mounts_the_result() {
    let s = router();
    let group = method(&s, "Router.Group");
    assert!(
        group.effects.contains(&Effect::Mounts {
            key: ArgSel::Pos(0),
            target: ArgSel::Receiver,
        }),
        "{:?}",
        group.effects
    );
    assert!(registrations(group).is_empty(), "a group's handlers are middleware: {:?}", group.effects);
}

#[test]
fn rule_typed_dispatch_unexported_methods_register_only_with_one_string_parameter() {
    let s = router();
    assert!(registrations(method(&s, "Router.register")).is_empty());
    assert!(registrations(method(&s, "Router.add")).contains(&&registers(0, ArgSel::Pos(1), VerbSel::Any)));
}

/// Go methods declared with a receiver are methods of the receiver type (calls on typed
/// receivers resolve to them), never package-level functions of their name.
#[test]
fn rule_go_receiver_methods_are_methods_of_their_type_not_globals() {
    let src = "package p\n\ntype T struct{ f func() }\n\nfunc (t *T) Run(cb func()) {\n\tt.store(cb)\n}\n\nfunc (t *T) store(cb func()) {\n\tcb()\n}\n\ntype Group struct{ name string }\n\nfunc (g *Group) Call(cb func()) {\n\tcb()\n}\n\nfunc (t *T) Group(name string, cb func()) *Group {\n\tg := &Group{name: name}\n\tg.Call(cb)\n\treturn g\n}\n";
    let path = Path::new("/virtual-library/p/p.go");
    let s = derive_in(Language::Go, path, src.as_bytes(), &entry_rows(), &MapLoader::default(), &[]);
    let run = method(&s, "T.Run");
    assert!(
        run.params
            .iter()
            .any(|(sel, e)| *sel == ArgSel::Pos(0) && e.contains(&Effect::Calls(ArgSel::Pos(0)))),
        "{:?}",
        run.params
    );
    // `Group{..}` inside `T.Group` is the type, not the method named like it.
    let group = method(&s, "T.Group");
    assert!(
        group
            .params
            .iter()
            .any(|(sel, e)| *sel == ArgSel::Pos(1) && e.contains(&Effect::Calls(ArgSel::Pos(1)))),
        "{:?}",
        group.params
    );
}

/// A method called on a parameter of a library-defined sequence / function / map type is
/// library code (`hs.Last()` on `Chain`), not a method of the caller's object.
#[test]
fn rule_typed_library_defined_types_are_not_open_receivers() {
    let s = router();
    let last = method(&s, "Router.Final");
    assert!(
        !last
            .params
            .iter()
            .flat_map(|(_, e)| e)
            .any(|e| matches!(e, Effect::CallsMethod { .. })),
        "{:?}",
        last.params
    );
}

/// A parameter declared with an anonymous function type whose parameter and result types
/// are the entry protocol method's receives handlers; another signature does not.
#[test]
fn rule_typed_dispatch_protocol_signature_function_types_are_handlers() {
    let s = router();
    let serve = method(&s, "Router.HandleServe");
    assert!(
        registrations(serve).contains(&&registers(0, ArgSel::Pos(1), VerbSel::Any)),
        "{:?}",
        serve.effects
    );
    assert!(registrations(method(&s, "Router.HandleWriter")).is_empty());
}
