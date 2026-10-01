//! Rule tests of the process / FFI derivation rules (fixtures: `tests/fixtures/rule-derive-procffi`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use trace_core::Language;

use crate::channels::ChannelRow;
use crate::derive::{FileSummaries, FunctionSummary};
use crate::model::{ArgSel, Channel, Effect, VerbSel};
use crate::table::Section;
use crate::test_support::{derive_in, fixture_bytes, MapLoader, TestLeaves};

/// Irreducible rows of the fixtures: `_net.send` sends an HTTP request, `_proc.spawn` /
/// `proc.Spawn` start a program (key: the first argument), `_ffi.FuncPtr` looks up a symbol
/// (key: element 0 of the first argument).
fn rows() -> TestLeaves {
    let row = |symbol: &str, channel: Channel, key: ArgSel| ChannelRow {
        symbol: Some(symbol.to_string()),
        channel: Some(channel),
        key: Some(key),
        ..ChannelRow::default()
    };
    TestLeaves::rows(vec![
        (Section::IoSend, row("_net.send", Channel::Http, ArgSel::Pos(0))),
        (Section::IoSend, row("_proc.spawn", Channel::Process, ArgSel::Pos(0))),
        (Section::IoSend, row("proc.Spawn", Channel::Process, ArgSel::Pos(0))),
        (
            Section::FfiConventions,
            row(
                "_ffi.FuncPtr",
                Channel::Ffi,
                ArgSel::Field {
                    arg: 0,
                    field: "0".into(),
                },
            ),
        ),
    ])
}

fn derive(language: Language, name: &str) -> FileSummaries {
    let src = fixture_bytes(&format!("rule-derive-procffi/{name}"));
    let path = PathBuf::from(format!("/virtual-library/{name}"));
    derive_in(language, &path, &src, &rows(), &MapLoader::default(), &[])
}

fn summary<'a>(s: &'a FileSummaries, suffix: &str) -> &'a FunctionSummary {
    s.functions
        .values()
        .find(|f| f.qualified == suffix || f.qualified.ends_with(&format!(".{suffix}")))
        .unwrap_or_else(|| {
            let have: BTreeMap<&str, &Vec<Effect>> = s
                .functions
                .values()
                .map(|f| (f.qualified.as_str(), &f.effects))
                .collect();
            panic!("no summary {suffix}; have {have:#?}")
        })
}

fn sends(channel: Channel, key: ArgSel) -> Effect {
    Effect::Sends {
        channel,
        key,
        verb: VerbSel::Any,
    }
}

fn channel_effects(f: &FunctionSummary) -> Vec<&Effect> {
    f.effects.iter().filter(|e| e.is_channel()).collect()
}

#[test]
fn rule_spread_of_own_rest_parameter_forwards_the_send_key() {
    let s = derive(Language::Python, "spawn.py");
    assert_eq!(channel_effects(summary(&s, "run")), vec![&sends(Channel::Process, ArgSel::Rest(0))]);
    // The elements of a list parameter reach the key, not the parameter itself.
    assert!(channel_effects(summary(&s, "run_list")).is_empty());
    // Arguments after a spread land at unknown positions.
    assert!(channel_effects(summary(&s, "run_after")).is_empty(), "{:?}", summary(&s, "run_after").effects);
}

#[test]
fn rule_class_without_constructor_constructs_its_base_without_source() {
    let s = derive(Language::Python, "spawn.py");
    let name = |n: &str| ArgSel::PosOrKw(0, n.to_string());
    assert_eq!(
        channel_effects(summary(&s, "Library.__getitem__")),
        vec![&sends(Channel::Ffi, name("name_or_ordinal"))]
    );
    assert_eq!(channel_effects(summary(&s, "Library.__getattr__")), vec![&sends(Channel::Ffi, name("name"))]);
}

#[test]
fn rule_numeric_row_field_selects_the_sequence_element() {
    let s = derive(Language::Python, "spawn.py");
    // `_FuncPtr((lib, name))`: element 0 (`lib`) is the key, element 1 (`name`) never is.
    assert_eq!(
        channel_effects(summary(&s, "lookup_by_element")),
        vec![&sends(Channel::Ffi, ArgSel::PosOrKw(1, "lib".into()))]
    );
}

#[test]
fn rule_import_bound_module_variable_is_the_import() {
    let s = derive(Language::JavaScript, "spawn.js");
    assert_eq!(channel_effects(summary(&s, "direct")), vec![&sends(Channel::Process, ArgSel::Pos(0))]);
}

#[test]
fn rule_object_literal_fields_are_records() {
    let s = derive(Language::JavaScript, "spawn.js");
    // `parsed.file` is the record field filled with `file` (through two record factories).
    assert_eq!(channel_effects(summary(&s, "start")), vec![&sends(Channel::Process, ArgSel::Pos(0))]);
    // Reading another field never reaches `file`.
    assert_eq!(channel_effects(summary(&s, "startArgs")), vec![&sends(Channel::Process, ArgSel::Pos(1))]);
}

#[test]
fn rule_field_filled_by_constructor_and_sent_by_method_sends_the_parameter() {
    let s = derive(Language::Go, "cmd.go");
    assert_eq!(channel_effects(summary(&s, "Command")), vec![&sends(Channel::Process, ArgSel::Pos(0))]);
    // The method itself sends a field, no parameter of its own.
    assert!(channel_effects(summary(&s, "Start")).is_empty());
}

#[test]
fn rule_http_field_is_no_sent_field() {
    let s = derive(Language::Python, "spawn.py");
    assert!(channel_effects(summary(&s, "Client.__init__")).is_empty());
    assert!(channel_effects(summary(&s, "Client.get")).is_empty());
}
