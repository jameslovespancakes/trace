use super::*;

#[test]
fn views_are_nested_and_exclude_references() {
    for k in EdgeKind::ALL {
        if kinds_for(Tier::Proven).contains(k) {
            assert!(kinds_for(Tier::Inferred).contains(k));
        }
        if kinds_for(Tier::Inferred).contains(k) {
            assert!(kinds_for(Tier::Possible).contains(k));
        }
        assert!(!(USES.contains(k) && kinds_for(Tier::Possible).contains(k)));
        assert!(!(FAMILY.contains(k) && kinds_for(Tier::Possible).contains(k)));
    }
    assert!(!kinds_for(Tier::Proven).contains(EdgeKind::CreatesGenerator));
}

#[test]
fn groups_partition_all_kinds_by_tier() {
    let groups = [
        EXECUTION,
        DEFERRED,
        INFERRED,
        POSSIBLE,
        REFERENCE,
        INFERRED_REFERENCE,
        FAMILY,
        BRIDGE,
    ];
    for k in EdgeKind::ALL {
        let n = groups.iter().filter(|g| g.contains(k)).count();
        assert_eq!(n, 1, "{k} must be in exactly one group");
        let expected = if INFERRED.contains(k) || INFERRED_REFERENCE.contains(k) {
            Tier::Inferred
        } else if POSSIBLE.contains(k) {
            Tier::Possible
        } else {
            Tier::Proven
        };
        assert_eq!(k.tier(), expected);
    }
    assert_eq!(kinds_for(Tier::Proven).iter().count(), 8);
    assert!(kinds_for(Tier::Proven).contains(EdgeKind::Bridge));
    assert!(!kinds_for(Tier::Inferred).contains(EdgeKind::PossibleLink));
    assert!(!kinds_for(Tier::Possible).contains(EdgeKind::References));
}
