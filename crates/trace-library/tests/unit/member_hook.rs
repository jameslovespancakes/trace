use super::*;
use crate::model::{Channel, VerbSel};

fn hook_summary() -> FunctionSummary {
    FunctionSummary {
        symbol: "m.Lib.__getattr__".into(),
        qualified: "Lib.__getattr__".into(),
        line: 0,
        column: 0,
        params: vec![(
            ArgSel::PosOrKw(0, "name".into()),
            vec![Effect::Calls(ArgSel::PosOrKw(0, "name".into()))],
        )],
        effects: vec![Effect::Sends {
            channel: Channel::Ffi,
            key: ArgSel::PosOrKw(0, "name".into()),
            verb: VerbSel::Any,
        }],
    }
}

#[test]
fn rule_member_hook_call_looks_up_the_spelled_member() {
    let seen = at_call(Language::Python, hook_summary(), "compress_buf");
    assert!(seen.params.is_empty(), "the call's arguments never reach the hook's parameters");
    assert_eq!(
        seen.effects,
        vec![Effect::Sends {
            channel: Channel::Ffi,
            key: ArgSel::Member,
            verb: VerbSel::Any,
        }]
    );
}

#[test]
fn rule_member_hook_called_by_its_own_name_is_an_ordinary_call() {
    let seen = at_call(Language::Python, hook_summary(), "__getattr__");
    assert_eq!(seen, hook_summary());
    // Languages without a hook keep every summary.
    let seen = at_call(Language::Go, hook_summary(), "compress_buf");
    assert_eq!(seen, hook_summary());
}
