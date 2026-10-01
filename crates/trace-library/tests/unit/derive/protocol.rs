use std::path::Path;

use trace_core::Language;

use super::super::*;
use crate::channels::ChannelRow;
use crate::model::{ArgSel, Channel, Effect, VerbSel};
use crate::table::Section;
use crate::test_support::{derive_in, fixture_bytes, MapLoader, TestLeaves};

/// Only the protocol rows under test.
fn command_rows() -> TestLeaves {
    TestLeaves::rows(
        ["PUBLISH", "SPUBLISH"]
            .into_iter()
            .map(|token| {
                let row = ChannelRow {
                    pattern: Some(token.to_string()),
                    channel: Some(Channel::Message),
                    key: Some(ArgSel::Pos(1)),
                    ..ChannelRow::default()
                };
                (Section::IoSend, row)
            })
            .collect(),
    )
}

fn commands() -> FileSummaries {
    let src = fixture_bytes("rule-derive-protocol/commands.py");
    let path = Path::new("/virtual-library/commands.py");
    derive_in(Language::Python, path, &src, &command_rows(), &MapLoader::default(), &[])
}

fn sends(s: &FileSummaries, qualified: &str) -> Vec<Effect> {
    s.by_qualified(qualified)
        .unwrap_or_else(|| panic!("no summary for {qualified}"))
        .effects
        .iter()
        .filter(|e| matches!(e, Effect::Sends { .. }))
        .cloned()
        .collect()
}

fn message(i: u32, name: &str) -> Effect {
    Effect::Sends {
        channel: Channel::Message,
        key: ArgSel::PosOrKw(i, name.to_string()),
        verb: VerbSel::Any,
    }
}

#[test]
fn rule_protocol_command_token_argument_is_message_sends() {
    let s = commands();
    // The argument right after the token is the destination; the payload is not.
    assert_eq!(sends(&s, "Commands.publish"), vec![message(0, "channel")]);
    // Tokens compare case-insensitively.
    assert_eq!(sends(&s, "Commands.shard_publish"), vec![message(0, "shard")]);
    // A literal destination is no parameter; a token without a row sends nothing.
    assert!(sends(&s, "Commands.fixed").is_empty());
    assert!(sends(&s, "Commands.read").is_empty());
}

#[test]
fn rule_protocol_command_sends_compose_to_callers() {
    let s = commands();
    assert_eq!(sends(&s, "Facade.notify"), vec![message(0, "topic")]);
}
