//! Rule tests of the derivation engine (fixtures: `tests/fixtures/rule-derive-*`, temporary
//! library trees).

use std::path::{Path, PathBuf};

use trace_core::Language;

use super::*;
use crate::channels::ChannelRow;
use crate::model::{ArgSel, Channel, Effect, VerbSel};
use crate::table::Section;
use crate::test_support::{derive_in, fixture_bytes, summary, summary_ending, MapLoader, TestLeaves};

fn fixture(rel: &str) -> String {
    String::from_utf8(fixture_bytes(rel)).expect("utf-8 fixture")
}

fn derive_src(language: Language, name: &str, src: &str, leaves: &TestLeaves) -> FileSummaries {
    let path = PathBuf::from(format!("/virtual-library/{name}"));
    derive_in(language, &path, src.as_bytes(), leaves, &MapLoader::default(), &[])
}

fn python(rel: &str) -> FileSummaries {
    derive_src(Language::Python, "lib.py", &fixture(rel), &TestLeaves::default())
}

fn names(effects: &[Effect]) -> Vec<&'static str> {
    effects.iter().map(Effect::name).collect()
}

/// Positional index a selector names (`Pos(i)` / `PosOrKw(i, _)` / `Rest(i)`).
fn pos_of(sel: &ArgSel) -> Option<u32> {
    match sel {
        ArgSel::Pos(i) | ArgSel::PosOrKw(i, _) | ArgSel::Rest(i) => Some(*i),
        _ => None,
    }
}

/// Effect names on the parameter at positional index `i`.
fn at_pos(f: &FunctionSummary, i: u32) -> Vec<&'static str> {
    f.params
        .iter()
        .filter(|(sel, _)| pos_of(sel) == Some(i))
        .flat_map(|(_, e)| names(e))
        .collect()
}

#[test]
fn rule_parameter_called_directly() {
    let s = python("rule-derive-python/callbacks.py");
    let run = summary(&s, "run");
    assert_eq!(names(&run.param_effects("fn")), vec!["calls"]);
    assert!(run.param_effects("value").is_empty(), "data argument is not called");
    assert_eq!(run.params[0].0, ArgSel::PosOrKw(0, "fn".to_string()));
    assert_eq!(names(&summary(&s, "alias_call").param_effects("fn")), vec!["calls"]);
    // Interprocedural: forward passes fn to run, which calls it.
    assert!(names(&summary(&s, "forward").param_effects("fn")).contains(&"calls"));
}

#[test]
fn rule_parameter_stored_in_self_slot_then_called() {
    let s = python("rule-derive-python/callbacks.py");
    let worker = summary(&s, "Worker");
    assert_eq!(names(&worker.param_effects("target")), vec!["stored_then_called"]);
    assert!(worker.param_effects("name").is_empty());
    assert!(!names(&worker.param_effects("args")).contains(&"stored_then_called"));
    let on = summary(&s, "Emitter.on");
    assert_eq!(names(&on.param_effects("handler")), vec!["stored_then_called"]);
}

#[test]
fn rule_other_objects_attribute_is_not_a_self_slot() {
    let s = python("rule-derive-python/callbacks.py");
    assert!(summary(&s, "Box.put").param_effects("fn").is_empty());
}

#[test]
fn rule_returned_closure_calling_parameter_is_a_wrapper() {
    let s = python("rule-derive-python/callbacks.py");
    assert_eq!(names(&summary(&s, "wrap").param_effects("fn")), vec!["wraps"]);
}

#[test]
fn rule_returned_parameter_alone_is_returns_not_wrapper() {
    let s = python("rule-derive-python/callbacks.py");
    assert_eq!(names(&summary(&s, "identity").param_effects("fn")), vec!["returns"]);
}

#[test]
fn rule_iterated_parameter() {
    let s = python("rule-derive-python/callbacks.py");
    assert_eq!(names(&summary(&s, "total").param_effects("items")), vec!["iterates"]);
}

#[test]
fn rule_property_getter_parameter() {
    let s = python("rule-derive-python/callbacks.py");
    assert!(names(&summary(&s, "lazy").param_effects("func")).contains(&"property"));
}

#[test]
fn rule_unknown_receiver_is_never_a_method_name_union() {
    let s = python("rule-derive-python/callbacks.py");
    assert_eq!(names(&summary(&s, "Runner.submit").param_effects("fn")), vec!["calls"]);
    assert!(
        summary(&s, "schedule").param_effects("fn").is_empty(),
        "executor is untyped: its submit is not Runner.submit"
    );
}

#[test]
fn rule_partialmethod_and_cache_are_derived() {
    let s = python("rule-derive-python/callbacks.py");
    assert_eq!(names(&summary(&s, "cache").param_effects("user_function")), vec!["wraps"]);
    assert!(names(&summary(&s, "lru").param_effects("maxsize")).is_empty());
    assert_eq!(names(&summary(&s, "partialmethod").param_effects("func")), vec!["stored_then_called"]);
}

#[test]
fn rule_singledispatch_register_stores_then_calls() {
    let s = python("rule-derive-python/callbacks.py");
    assert!(names(&summary(&s, "dispatcher").param_effects("func")).contains(&"stored_then_called"));
    let register = summary_ending(&s, "register");
    assert!(names(&register.param_effects("impl")).contains(&"stored_then_called"));
}

#[test]
fn rule_weakref_finalize_calls_its_function() {
    let s = python("rule-derive-python/callbacks.py");
    let fin = summary(&s, "finalizer");
    assert!(names(&fin.param_effects("func")).contains(&"stored_then_called"));
    assert!(fin.param_effects("obj").is_empty());
}

#[test]
fn rule_data_parameters_are_not_called() {
    let s = python("rule-derive-python/negatives.py");
    let negatives: [(&str, &str); 30] = [
        ("Queue.put", "item"),
        ("Queue.put", "timeout"),
        ("Queue.put", "block"),
        ("Queue.put_nowait", "item"),
        ("Thread", "name"),
        ("Thread", "args"),
        ("Timer", "interval"),
        ("Timer", "args"),
        ("Event.wait", "timeout"),
        ("sleep", "delay"),
        ("sleep", "result"),
        ("Future.set_result", "result"),
        ("BaseEventLoop.call_later", "delay"),
        ("BaseEventLoop.call_at", "when"),
        ("ThreadPoolExecutor", "max_workers"),
        ("Counter", "iterable"),
        ("OrderedDict", "other"),
        ("partial", "args"),
        ("ExitStack.callback", "args"),
        ("scheduler.enter", "delay"),
        ("scheduler.enter", "priority"),
        ("scheduler.enter", "argument"),
        ("Response", "content"),
        ("Response", "status_code"),
        ("JSONResponse.render", "content"),
        ("Headers", "headers"),
        ("redirect", "location"),
        ("dumps", "obj"),
        ("App.make_response", "rv"),
        ("checkpoint", "x"),
    ];
    for (function, param) in negatives {
        let got = names(&summary(&s, function).param_effects(param));
        assert!(
            !got.contains(&"calls") && !got.contains(&"stored_then_called") && !got.contains(&"wraps"),
            "{function}({param}) is data, derived {got:?}"
        );
    }
    // The positive counterparts in the same file are derived.
    assert!(names(&summary(&s, "Thread").param_effects("target")).contains(&"stored_then_called"));
    assert!(
        names(&summary(&s, "ExitStack.callback").param_effects("callback")).contains(&"stored_then_called")
    );
    assert!(names(&summary(&s, "Counter").param_effects("iterable")).contains(&"iterates"));
}

#[test]
fn rule_derivation_composes_with_native_leaf_rows() {
    let leaves = TestLeaves {
        symbols: vec![(
            "_native.apply".to_string(),
            vec![Effect::Calls(ArgSel::Pos(0)), Effect::Iterates(ArgSel::Pos(1))],
        )],
        ..TestLeaves::default()
    };
    let s = derive_src(Language::Python, "lib.py", &fixture("rule-derive-python/callbacks.py"), &leaves);
    let f = summary(&s, "via_native");
    assert_eq!(names(&f.param_effects("fn")), vec!["calls"]);
    assert_eq!(names(&f.param_effects("data")), vec!["iterates"]);
    // Without the leaf row nothing is derived (no guess).
    let s = python("rule-derive-python/callbacks.py");
    assert!(summary(&s, "via_native").param_effects("fn").is_empty());
}

#[test]
fn rule_summary_positions_are_declaration_names() {
    let src = fixture("rule-derive-python/callbacks.py");
    let line = src.lines().position(|l| l.starts_with("def run(")).expect("def run") as u32;
    let s = derive_src(Language::Python, "lib.py", &src, &TestLeaves::default());
    let run = s.at(line, 4).expect("summary at the name");
    assert_eq!(run.qualified, "run");
    assert_eq!(run.symbol, "lib.run");
}

// ------------------------------------------------------------------ channel rules

fn channel_leaves() -> TestLeaves {
    TestLeaves {
        symbols: Vec::new(),
        rows: vec![
            (
                Section::IoEntry,
                ChannelRow {
                    pattern: Some("__call__/3".to_string()),
                    channel: Some(Channel::Http),
                    ..ChannelRow::default()
                },
            ),
            (
                Section::IoEntry,
                ChannelRow {
                    symbol: Some("_loop.run".to_string()),
                    channel: Some(Channel::Message),
                    handler: Some(ArgSel::Pos(0)),
                    ..ChannelRow::default()
                },
            ),
            (
                Section::IoSend,
                ChannelRow {
                    symbol: Some("_net.send".to_string()),
                    channel: Some(Channel::Http),
                    key: Some(ArgSel::Pos(1)),
                    verb: Some(VerbSel::Arg(ArgSel::Pos(0))),
                    ..ChannelRow::default()
                },
            ),
            (
                Section::IoSend,
                ChannelRow {
                    symbol: Some("_proc.spawn".to_string()),
                    channel: Some(Channel::Process),
                    key: Some(ArgSel::Pos(0)),
                    ..ChannelRow::default()
                },
            ),
        ],
    }
}

fn server() -> FileSummaries {
    derive_src(Language::Python, "server.py", &fixture("rule-derive-channels/server.py"), &channel_leaves())
}

fn kw(i: u32, name: &str) -> ArgSel {
    ArgSel::PosOrKw(i, name.to_string())
}

#[test]
fn rule_parameter_reaching_io_send_is_sends() {
    let s = server();
    assert!(summary(&s, "get").effects.contains(&Effect::Sends {
        channel: Channel::Http,
        key: kw(0, "url"),
        verb: VerbSel::Const("GET".to_string()),
    }));
    assert!(summary(&s, "request").effects.contains(&Effect::Sends {
        channel: Channel::Http,
        key: kw(1, "url"),
        verb: VerbSel::Arg(kw(0, "method")),
    }));
}

#[test]
fn rule_handler_stored_in_dispatch_registry_is_registers() {
    let s = server();
    let expected = Effect::Registers {
        channel: Channel::Http,
        key: kw(0, "path"),
        handler: kw(1, "endpoint"),
        verb: VerbSel::Any,
    };
    assert!(summary(&s, "Route").effects.contains(&expected), "{:?}", summary(&s, "Route").effects);
    assert!(summary(&s, "Router.add_route").effects.contains(&expected));
}

#[test]
fn rule_storage_without_io_entry_dispatch_is_not_registers() {
    let s = server();
    let keep = summary(&s, "Store.keep");
    assert!(!keep.effects.iter().any(|e| matches!(e, Effect::Registers { .. })));
    assert!(keep.param_effects("fn").is_empty());
}

#[test]
fn rule_sub_registry_under_prefix_is_mounts() {
    let s = server();
    assert!(summary(&s, "Router.mount").effects.contains(&Effect::Mounts {
        key: kw(0, "prefix"),
        target: kw(1, "router"),
    }));
}

#[test]
fn rule_decorator_returning_registrar_is_decorates() {
    let s = server();
    let route = summary(&s, "Router.route");
    assert!(
        route.effects.contains(&Effect::Decorates {
            inner: Box::new(Effect::Registers {
                channel: Channel::Http,
                key: kw(0, "path"),
                handler: ArgSel::Pos(0),
                verb: VerbSel::Any,
            }),
        }),
        "{:?}",
        route.effects
    );
}

#[test]
fn rule_consumer_loop_entry_makes_message_channel() {
    let s = server();
    assert!(summary(&s, "Consumer.subscribe")
        .effects
        .contains(&Effect::Registers {
            channel: Channel::Message,
            key: kw(0, "topic"),
            handler: kw(1, "handler"),
            verb: VerbSel::Any,
        }));
}

#[test]
fn rule_spawn_primitive_is_process_channel() {
    let s = server();
    assert!(summary(&s, "run_command").effects.contains(&Effect::Sends {
        channel: Channel::Process,
        key: kw(0, "cmd"),
        verb: VerbSel::Any,
    }));
}

// ------------------------------------------------------------------ adapters

fn derive_fixture(language: Language, rel: &str) -> FileSummaries {
    let name = Path::new(rel).file_name().and_then(|n| n.to_str()).unwrap_or("lib");
    derive_src(language, name, &fixture(rel), &TestLeaves::default())
}

#[test]
fn rule_python_adapter_derives_callback_call() {
    let s = python("rule-derive-python/callbacks.py");
    assert_eq!(at_pos(summary(&s, "run"), 0), vec!["calls"]);
}

#[test]
fn rule_javascript_adapter_derives_callback_call() {
    let s = derive_fixture(Language::JavaScript, "rule-derive-javascript/each.js");
    let each = summary_ending(&s, "each");
    assert_eq!(at_pos(each, 1), vec!["calls"]);
    assert_eq!(at_pos(each, 0), vec!["iterates"]);
    assert_eq!(at_pos(summary_ending(&s, "Emitter.on"), 0), vec!["stored_then_called"]);
    assert_eq!(at_pos(summary_ending(&s, "invoke"), 0), vec!["calls"]);
}

#[test]
fn rule_go_adapter_derives_callback_call() {
    let s = derive_fixture(Language::Go, "rule-derive-go/each.go");
    let each = summary_ending(&s, "Each");
    assert_eq!(at_pos(each, 1), vec!["calls"]);
    assert_eq!(at_pos(each, 0), vec!["iterates"]);
    assert_eq!(at_pos(summary_ending(&s, "On"), 0), vec!["stored_then_called"]);
}

#[test]
fn rule_rust_adapter_derives_callback_call() {
    let s = derive_fixture(Language::Rust, "rule-derive-rust/each.rs");
    let each = summary_ending(&s, "each");
    assert_eq!(at_pos(each, 1), vec!["calls"]);
    assert_eq!(at_pos(each, 0), vec!["iterates"]);
}

#[test]
fn rule_php_adapter_derives_callback_call() {
    let s = derive_fixture(Language::Php, "rule-derive-php/each.php");
    let each = summary_ending(&s, "each_item");
    assert_eq!(at_pos(each, 1), vec!["calls"]);
    assert_eq!(at_pos(each, 0), vec!["iterates"]);
    assert_eq!(at_pos(summary_ending(&s, "apply_now"), 0), vec!["calls"]);
}

#[test]
fn rule_r_adapter_derives_callback_call() {
    let s = derive_fixture(Language::R, "rule-derive-r/apply.R");
    assert_eq!(at_pos(summary_ending(&s, "apply_fun"), 1), vec!["calls"]);
    assert_eq!(at_pos(summary_ending(&s, "call_later"), 0), vec!["calls"]);
}

// ------------------------------------------------------------------ constructor chains, slots

#[test]
fn rule_stored_callback_through_super_init_is_stored_then_called() {
    let s = python("rule-derive-python/inheritance.py");
    // An explicit keyword through `super().__init__`.
    assert!(names(&summary(&s, "Flag").param_effects("callback")).contains(&"stored_then_called"));
    // Collected by `**attrs` and forwarded to the base constructor (`super()` and
    // `Base.__init__(self, ...)`): the keyword reaches the stored-then-called parameter.
    let option = summary(&s, "Option");
    assert!(names(&option.param_effects("callback")).contains(&"stored_then_called"), "{:?}", option.params);
    assert!(names(&summary(&s, "Argument").param_effects("callback")).contains(&"stored_then_called"));
    // Forwarded data keywords and own data parameters are not run.
    for (class, param) in [
        ("Option", "default"),
        ("Option", "show_default"),
        ("Option", "param_decls"),
        ("Argument", "required"),
    ] {
        let got = names(&summary(&s, class).param_effects(param));
        assert!(
            !got.contains(&"stored_then_called") && !got.contains(&"calls"),
            "{class}({param}) is data, derived {got:?}"
        );
    }
}

#[test]
fn rule_self_slot_of_other_object_is_not_this_object() {
    let s = python("rule-derive-python/inheritance.py");
    assert_eq!(names(&summary(&s, "Caller").param_effects("handler")), vec!["stored_then_called"]);
    // Keeper and Caller are sibling classes: Caller calling its own `handler` never calls a
    // Keeper's.
    assert!(summary(&s, "Keeper").param_effects("handler").is_empty());
    // Another object's attribute (an untyped parameter) is no slot of this object.
    assert!(summary(&s, "Relay.attach").param_effects("handler").is_empty());
    assert!(summary(&s, "Relay").param_effects("handler").is_empty());
}

// ------------------------------------------------------------------ methods of passed objects

/// Whether `f` calls `method` on the argument at position `i` (`calls_method`).
fn has_method(f: &FunctionSummary, i: u32, method: &str) -> bool {
    f.params.iter().any(|(sel, effects)| {
        pos_of(sel) == Some(i)
            && effects
                .iter()
                .any(|e| matches!(e, Effect::CallsMethod { method: m, .. } if m == method))
    })
}

#[test]
fn rule_stored_argument_method_call_is_derived() {
    let s = derive_fixture(Language::Go, "rule-derive-go/handler.go");
    // Called directly on an interface-typed parameter.
    assert!(has_method(summary(&s, "Dispatch"), 0, "ServeHTTP"));
    // Stored into a struct field by a composite literal, called later through that field
    // (`sh.srv.Handler.ServeHTTP(...)`, the field types type the receivers).
    let listen = summary(&s, "ListenAndServe");
    assert!(has_method(listen, 1, "ServeHTTP"), "{:?}", listen.params);
    assert!(listen.params.iter().all(|(sel, _)| pos_of(sel) != Some(0)), "the address is data");
    // A concrete library type: its method runs inside the library, not on a passed object.
    assert!(!has_method(summary(&s, "Reset"), 0, "Reset"));
}

#[test]
fn rule_java_functional_parameter_is_called() {
    let s = derive_fixture(Language::Java, "rule-derive-java/Pool.java");
    assert_eq!(at_pos(summary(&s, "Pool.submit"), 0), vec!["calls"]);
    assert!(at_pos(summary(&s, "Pool.apply"), 0).contains(&"calls"));
    // A single-method interface of the library is a function type too.
    assert!(at_pos(summary(&s, "Pool.register"), 0).contains(&"calls"));
    // Stored in a field (constructor) or a field's collection, run later.
    let pool = summary(&s, "Pool");
    assert!(at_pos(pool, 0).contains(&"stored_then_called"), "{:?}", pool.params);
    assert!(at_pos(summary(&s, "Pool.later"), 0).contains(&"stored_then_called"));
    // Data: a method of it is called, the value itself never runs.
    let data = at_pos(summary(&s, "Pool.name"), 0);
    assert!(!data.contains(&"calls") && !data.contains(&"stored_then_called"), "{data:?}");
}

// ------------------------------------------------------------------ member copies

fn copies_1_to_0() -> Effect {
    Effect::CopiesMembers {
        from: ArgSel::Pos(1),
        to: ArgSel::Pos(0),
    }
}

#[test]
fn rule_mixin_copies_members_into_the_target() {
    let s = derive_fixture(Language::JavaScript, "rule-derive-javascript/mixin.js");
    // `Object.getOwnPropertyNames(src).forEach(n => Object.defineProperty(dest, n, ...))`.
    let merge = summary(&s, "merge");
    assert!(merge.effects.contains(&copies_1_to_0()), "{:?}", merge.effects);
    // `for (const n of Object.getOwnPropertyNames(source)) Object.defineProperty(...)`.
    assert!(summary(&s, "mergeDescriptors").effects.contains(&copies_1_to_0()));
    // Composed through a call of a copying function.
    assert!(summary(&s, "mixin").effects.contains(&copies_1_to_0()));
    // Storing a constant under names taken from a list copies nothing.
    assert!(!summary(&s, "fill")
        .effects
        .iter()
        .any(|e| matches!(e, Effect::CopiesMembers { .. })));
}

#[test]
fn rule_for_in_member_copy_is_copies_members() {
    let s = derive_fixture(Language::JavaScript, "rule-derive-javascript/mixin.js");
    // `for (k in source) target[k] = source[k]`.
    let assign = summary(&s, "assignAll");
    assert!(assign.effects.contains(&copies_1_to_0()), "{:?}", assign.effects);
}

// ------------------------------------------------------------------ registrations through wrappers,
// parameters, receiver methods, table owners and import loading

/// The ASGI-shaped protocol entry (`__call__/3`) as the only irreducible row.
fn entry_leaves() -> TestLeaves {
    TestLeaves::rows(vec![(
        Section::IoEntry,
        ChannelRow {
            pattern: Some("__call__/3".to_string()),
            channel: Some(Channel::Http),
            ..ChannelRow::default()
        },
    )])
}

fn wrapped() -> FileSummaries {
    let path = Path::new("/virtual-library/wrapped.py");
    let src = fixture_bytes("rule-derive-channels/wrapped.py");
    derive_in(Language::Python, path, &src, &entry_leaves(), &FsLoader, &[])
}

fn route_registration() -> Effect {
    Effect::Registers {
        channel: Channel::Http,
        key: kw(0, "path"),
        handler: kw(1, "endpoint"),
        verb: VerbSel::Any,
    }
}

#[test]
fn rule_passed_function_is_reached_from_the_parameter_caller() {
    // `wrap(make_handler(self.endpoint))`: the endpoint runs when the wrapper's closure calls
    // its parameter, which the entry reaches through `handle` -> `self.app`.
    let s = wrapped();
    let e = &summary(&s, "WrappedRoute.__init__").effects;
    assert!(e.contains(&route_registration()), "{e:?}");
}

#[test]
fn rule_slot_passed_to_a_storing_parameter_flows_into_its_slot() {
    // `describe(path, call=self.endpoint)` stores `call` into `Record.call`, which the entry
    // calls through `run_record`: the endpoint slot is dispatched.
    let s = wrapped();
    let e = &summary(&s, "RecordRoute.__init__").effects;
    assert!(e.contains(&route_registration()), "{e:?}");
    assert!(summary(&s, "Router.add_recorded")
        .effects
        .contains(&route_registration()));
}

#[test]
fn rule_receiver_method_wins_over_container_vocabulary() {
    // `self.router.get(path)` calls the router's own `get`, not a container read.
    let s = wrapped();
    let e = &summary(&s, "App.get").effects;
    assert!(
        e.iter()
            .any(|x| matches!(x, Effect::Decorates { inner } if matches!(**inner, Effect::Registers { .. }))),
        "{e:?}"
    );
}

#[test]
fn rule_table_owner_plain_attributes_are_not_registrations() {
    // `self.lifespan = lifespan` / `self.default = default` on the object owning the route
    // table are its configuration, not entries registered under `routes`.
    let s = wrapped();
    let e = &summary(&s, "Router.__init__").effects;
    assert!(!e.iter().any(|x| matches!(x, Effect::Registers { .. })), "{e:?}");
}

#[test]
fn rule_typed_parameter_of_a_non_registry_class_is_no_mount_target() {
    let s = wrapped();
    let e = &summary(&s, "GroupRoute.__init__").effects;
    assert!(
        e.contains(&Effect::Mounts {
            key: kw(0, "path"),
            target: kw(2, "router"),
        }),
        "{e:?}"
    );
    assert!(
        !e.iter().any(|x| matches!(
            x,
            Effect::Mounts {
                target: ArgSel::PosOrKw(3, _),
                ..
            }
        )),
        "{e:?}"
    );
}

/// A library tree: `site/pkg/target.py` imports four modules of another root first, then its
/// own distribution's `helper` (which calls its parameter).
fn write_tree(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let site = dir.join("site");
    let other = dir.join("other");
    std::fs::create_dir_all(site.join("pkg")).expect("pkg dir");
    std::fs::create_dir_all(&other).expect("other dir");
    std::fs::write(site.join("pkg").join("__init__.py"), "").expect("init");
    for m in ["alpha", "beta", "gamma", "delta"] {
        std::fs::write(other.join(format!("{m}.py")), "def noop(x):\n    return x\n").expect("module");
    }
    std::fs::write(site.join("pkg").join("helper.py"), "def run(fn):\n    return fn()\n").expect("helper");
    let target = site.join("pkg").join("target.py");
    std::fs::write(
        &target,
        "import alpha\nimport beta\nimport gamma\nimport delta\nfrom pkg.helper import run\n\n\ndef go(cb):\n    return run(cb)\n",
    )
    .expect("target");
    (site, other, target)
}

#[test]
fn rule_imports_of_the_own_distribution_load_first() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (site, other, target) = write_tree(dir.path());
    let roots = vec![site, other];
    let src = std::fs::read(&target).expect("target source");
    // Budget: the target and one import.
    let limits = trace_core::config::DeriveSettings {
        max_units: 2,
        ..Default::default()
    };
    let cx = DeriveContext {
        leaves: &entry_leaves(),
        loader: &FsLoader,
        parsed: &ParsedFiles::default(),
        roots: &roots,
        limits: &limits,
    };
    let s = derive_with(Language::Python, &target, &src, &cx);
    let go = s.by_qualified("go").expect("go");
    assert!(
        go.params
            .iter()
            .any(|(_, e)| e.iter().any(|x| matches!(x, Effect::Calls(_)))),
        "{:?}",
        go.params
    );
}
