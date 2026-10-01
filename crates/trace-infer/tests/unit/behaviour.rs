use super::*;
use trace_library::BehaviourSource;

fn behaviour(effects: Vec<Effect>, inferred: bool) -> CallBehaviour {
    CallBehaviour {
        symbol: Some("threading.Thread".into()),
        effects,
        source: BehaviourSource::Derived,
        inferred,
        reason: String::new(),
    }
}

#[test]
fn rule_callback_arg_selectors_match_index_and_keyword() {
    let target = ArgSel::PosOrKw(1, "target".into());
    assert!(selects(&target, Some(1), None, 2));
    assert!(selects(&target, None, Some("target"), 0));
    assert!(!selects(&target, Some(0), None, 2));
    assert!(selects(&ArgSel::Rest(1), Some(3), None, 4));
    assert!(!selects(&ArgSel::Rest(1), Some(0), None, 4));
    assert!(selects(&ArgSel::Last, Some(2), None, 3));
    assert!(!selects(&ArgSel::Last, Some(1), None, 3));
    assert!(!selects(&ArgSel::Receiver, Some(0), None, 1));
}

#[test]
fn rule_never_calls_effect_wins_for_its_argument() {
    let effects = vec![Effect::Calls(ArgSel::Pos(0)), Effect::NeverCalls(ArgSel::Pos(0))];
    assert_eq!(effect_for(&effects, Some(0), None, 1).map(Effect::name), Some(NEVER_CALLS));
    let effects = vec![Effect::Iterates(ArgSel::Pos(1)), Effect::Calls(ArgSel::Pos(0))];
    assert_eq!(effect_for(&effects, Some(0), None, 2).map(Effect::name), Some("calls"));
    assert_eq!(effect_for(&effects, Some(1), None, 2).map(Effect::name), Some("iterates"));
    assert_eq!(effect_for(&effects, Some(2), None, 3), None);
}

#[test]
fn rule_decorator_application_uses_the_inner_effect() {
    let b = behaviour(
        vec![Effect::Decorates {
            inner: Box::new(Effect::StoredThenCalled(ArgSel::Pos(0))),
        }],
        true,
    );
    assert_eq!(applied_effects(&b, true), vec![Effect::StoredThenCalled(ArgSel::Pos(0))]);
    assert!(applied_effects(&b, false).is_empty());
    assert!(runs(&Effect::StoredThenCalled(ArgSel::Pos(0))));
    assert!(!runs(&Effect::Returns(ArgSel::Pos(0))));
    assert!(runs_name("registers") && !runs_name(NEVER_CALLS));
}
