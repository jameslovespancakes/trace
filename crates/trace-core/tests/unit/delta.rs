use super::*;

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn rule_full_delta_touches_everything() {
    let d = IndexDelta::full();
    assert!(!d.is_empty());
    assert!(d.file_changed("any.py"));
    assert!(d.symbol_touched("any"));
    assert!(IndexDelta::default().is_empty());
}

#[test]
fn rule_delta_merge_is_a_sequence_of_changes() {
    let mut d = IndexDelta {
        added: set(&["new.py"]),
        removed: set(&["gone.py"]),
        symbols_added: set(&["s1"]),
        ..IndexDelta::default()
    };
    d.merge(IndexDelta {
        removed: set(&["new.py"]),
        added: set(&["gone.py"]),
        modified: set(&["a.py"]),
        symbols_removed: set(&["s1", "s2"]),
        ..IndexDelta::default()
    });
    assert!(d.added.is_empty(), "added then removed = nothing");
    assert!(d.removed.is_empty());
    assert_eq!(d.modified, set(&["a.py", "gone.py"]));
    assert_eq!(d.symbols_removed, set(&["s2"]));
    assert!(d.symbols_added.is_empty());
    assert!(d.file_changed("a.py"));
    assert!(!d.file_changed("b.py"));
}

#[test]
fn rule_identity_remap_keeps_ids() {
    let r = IdRemap::identity(2, 3);
    assert_eq!(r.file(FileId(1)), Some(FileId(1)));
    assert_eq!(r.symbol(SymbolId(2)), Some(SymbolId(2)));
    assert_eq!(r.symbol(SymbolId(3)), None);
}
