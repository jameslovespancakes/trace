//! Rule tests of the prototype object model (fixtures: `tests/fixtures/rule-derive-javascript/
//! objects.js`, the installed-package layout `tests/fixtures/rule-derive-objects`).

use std::path::{Path, PathBuf};

use trace_core::Language;

use super::super::{FileSummaries, FsLoader, FunctionSummary, Leaves};
use crate::channels::ChannelRow;
use crate::model::{ArgSel, Channel, Effect, VerbSel};
use crate::table::Section;
use crate::test_support::{derive_in, fixtures, summary, TestLeaves};

/// The HTTP server entry row the package fixture reaches (`http.createServer(listener)`).
fn entry_leaves() -> TestLeaves {
    TestLeaves::rows(vec![(
        Section::IoEntry,
        ChannelRow {
            symbol: Some("http.createServer".to_string()),
            channel: Some(Channel::Http),
            handler: Some(ArgSel::Last),
            ..ChannelRow::default()
        },
    )])
}

fn derive_path(path: &Path, roots: &[PathBuf]) -> FileSummaries {
    derive_with_leaves(path, roots, &entry_leaves())
}

fn derive_with_leaves(path: &Path, roots: &[PathBuf], leaves: &dyn Leaves) -> FileSummaries {
    let source = std::fs::read(path).unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()));
    derive_in(Language::JavaScript, path, &source, leaves, &FsLoader, roots)
}

/// One file of the installed package fixture, derived with the package's other files.
fn package_file(rel: &str) -> FileSummaries {
    let modules = fixtures().join("rule-derive-objects").join("node_modules");
    derive_path(&modules.join("objrouter").join(rel), std::slice::from_ref(&modules))
}

fn objects_js() -> FileSummaries {
    derive_path(&fixtures().join("rule-derive-javascript").join("objects.js"), &[])
}

fn effects_on(f: &FunctionSummary, sel: &ArgSel) -> Vec<&'static str> {
    f.params
        .iter()
        .filter(|(s, _)| s == sel)
        .flat_map(|(_, e)| e.iter().map(Effect::name))
        .collect()
}

fn registers(key: ArgSel, handler: ArgSel) -> Effect {
    Effect::Registers {
        channel: Channel::Http,
        key,
        handler,
        verb: VerbSel::Any,
    }
}

fn registrations(f: &FunctionSummary) -> Vec<&Effect> {
    f.effects
        .iter()
        .filter(|e| matches!(e, Effect::Registers { .. }))
        .collect()
}

#[test]
fn rule_object_member_functions_form_a_class_with_this_receiver() {
    let s = objects_js();
    // `registry.add = function (fn) { this.fns = fn }` and `registry.run` calls `this.fns`.
    let add = summary(&s, "registry.add");
    assert_eq!(effects_on(add, &ArgSel::Pos(0)), vec!["stored_then_called"], "{add:?}");
}

#[test]
fn rule_computed_member_definitions_are_methods_under_the_key_values() {
    let s = objects_js();
    // `obj[k](cb)`: the computed members of the object's class.
    let by_index = summary(&s, "viaIndex");
    assert_eq!(effects_on(by_index, &ArgSel::Pos(0)), vec!["calls"], "{by_index:?}");
    // `this.get(cb)`: `get` is an element of `["GET", "POST"].map(v => v.toLowerCase())`.
    let by_name = summary(&s, "obj.viaName");
    assert_eq!(effects_on(by_name, &ArgSel::Pos(0)), vec!["calls"], "{by_name:?}");
}

#[test]
fn rule_arguments_object_is_the_rest_parameter() {
    let s = objects_js();
    // `Array.prototype.slice.call(arguments, 1).forEach(fn => fn())`: the arguments after
    // the first are called, the first is not.
    let rest = summary(&s, "rest");
    assert!(effects_on(rest, &ArgSel::Rest(1)).contains(&"calls"), "{rest:?}");
    assert!(effects_on(rest, &ArgSel::Pos(0)).is_empty(), "{rest:?}");
    // `invoke.apply(null, arguments)`: every argument reaches the callee.
    let apply = summary(&s, "viaApply");
    assert_eq!(effects_on(apply, &ArgSel::Rest(0)), vec!["calls"], "{apply:?}");
}

#[test]
fn rule_receiver_binding_invocations_bind_the_arguments() {
    let s = objects_js();
    // `invoke.call(null, x)` is `invoke(x)`.
    let call = summary(&s, "viaCall");
    assert_eq!(effects_on(call, &ArgSel::Pos(0)), vec!["calls"], "{call:?}");
    // `cb.bind(o)()` calls `cb`.
    let bind = summary(&s, "viaBind");
    assert_eq!(effects_on(bind, &ArgSel::Pos(1)), vec!["calls"], "{bind:?}");
    assert!(effects_on(bind, &ArgSel::Pos(0)).is_empty(), "{bind:?}");
}

#[test]
fn rule_library_lowering_keeps_integer_literals() {
    let src = b"function f() { return g(arguments, 1); }";
    let facts = trace_syntax::extract(trace_syntax::SourceInput {
        path: "f.js",
        language: Language::JavaScript,
        source: src,
    })
    .expect("extract");
    let low = trace_syntax::lower::lower_library(Language::JavaScript, src, &facts).expect("lowering");
    let text = format!("{:?}", low.flow);
    assert!(text.contains(&format!("{}1", trace_syntax::lower::NUMBER_PREFIX)), "{text}");
}

#[test]
fn rule_computed_verb_member_registers_through_the_entry_object() {
    // `app[verb] = function (path) { route = this._router.route(path);
    // route[verb].apply(route, slice.call(arguments, 1)) }`: the route object holds `path`,
    // its computed member stores the rest of the arguments where dispatch code reached from
    // `http.createServer(app)` calls them (the app is a function object with the prototype's
    // members, the package entry builds it). The rest of the arguments is a variadic handler
    // list: it registers its last element (the ones before it run first).
    let s = package_file("lib/app.js");
    let verb = summary(&s, "<lambda>.<lambda>");
    assert_eq!(registrations(verb), vec![&registers(ArgSel::Pos(0), ArgSel::Last)], "{verb:?}");
}

#[test]
fn rule_nested_function_registers_the_enclosing_parameters() {
    // `fns.forEach(f => router.use(path, f))` with `path = fn` when the first argument is no
    // function: the key is the first argument, the handler the last of the variadic handler
    // list; the rest of the arguments is never a key.
    let s = package_file("lib/app.js");
    let used = summary(&s, "app.use");
    assert!(registrations(used).contains(&&registers(ArgSel::Pos(0), ArgSel::Last)), "{used:?}");
    assert!(
        !used.effects.iter().any(|e| matches!(
            e,
            Effect::Registers {
                key: ArgSel::Rest(_),
                ..
            }
        )),
        "{used:?}"
    );
}

#[test]
fn rule_member_storage_without_dispatch_is_not_registers() {
    // `this.kept = fn` is never called by dispatch code: no registration.
    let s = package_file("lib/app.js");
    let keep = summary(&s, "app.keep");
    assert!(registrations(keep).is_empty(), "{keep:?}");
}

#[test]
fn rule_module_value_and_prototype_link_type_the_function_object() {
    // `new Router()` is the module value (a constructor function whose result is linked to
    // the prototype object): `this.stack.push(layer)` of its members is the registry the
    // linked function object dispatches.
    let s = package_file("lib/router.js");
    let used = summary(&s, "proto.use");
    assert_eq!(registrations(used), vec![&registers(ArgSel::Pos(0), ArgSel::Pos(1))], "{used:?}");
}

#[test]
fn rule_computed_verb_member_without_entry_dispatch_is_not_registers() {
    // The same package without the server entry row: the stored handlers are called, but by
    // no code an entry reaches - no registration.
    let modules = fixtures().join("rule-derive-objects").join("node_modules");
    let s = derive_with_leaves(
        &modules.join("objrouter").join("lib/app.js"),
        std::slice::from_ref(&modules),
        &TestLeaves::default(),
    );
    let verb = summary(&s, "<lambda>.<lambda>");
    assert!(registrations(verb).is_empty(), "{verb:?}");
    assert!(registrations(summary(&s, "app.use")).is_empty());
}
