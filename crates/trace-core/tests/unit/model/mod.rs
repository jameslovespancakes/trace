use super::*;
use crate::test_support::index::{decision, index_with, site};

#[test]
fn rule_template_dependent_and_inactive_code_are_named_and_not_blind() {
    for (k, name) in [
        (UnresolvedKind::TemplateDependent, "template_dependent"),
        (UnresolvedKind::InactiveCode, "inactive_code"),
    ] {
        assert_eq!(k.as_str(), name);
        assert_eq!(serde_json::to_string(&k).unwrap(), format!("\"{name}\""));
        assert!(!k.is_blind());
    }
}

#[test]
fn names_round_trip() {
    for k in EdgeKind::ALL {
        assert_eq!(k.as_str().parse::<EdgeKind>(), Ok(k));
        assert_eq!(serde_json::to_string(&k).unwrap(), format!("\"{k}\""));
    }
    for t in [Tier::Proven, Tier::Inferred, Tier::Possible] {
        assert_eq!(t.as_str().parse::<Tier>(), Ok(t));
    }
    assert!("everything".parse::<Tier>().is_err());
    assert_eq!(Tier::default(), Tier::Inferred);
    assert!(Tier::Proven < Tier::Inferred && Tier::Inferred < Tier::Possible);
    assert_eq!(EdgeKind::inferred_for(SiteCategory::Flow), EdgeKind::InferredCall);
    assert_eq!(Provider::Lsp("gopls".into()).label(), "lsp:gopls");
}

#[test]
fn spans() {
    let s = ByteSpan::new(2, 5);
    assert_eq!(s.len(), 3);
    assert!(s.contains(2) && !s.contains(5));
    assert!(s.encloses(ByteSpan::new(3, 5)));
    assert!(!s.encloses(ByteSpan::new(1, 3)));
    assert!(ByteSpan::new(5, 5).is_empty());
    assert_eq!(ByteSpan::new(5, 2).len(), 0);
}

#[test]
fn lookups() {
    let mut index = index_with(&["outer", "inner"], &[]);
    // Make `inner` nested inside `outer`.
    index.symbols[0].span.bytes = ByteSpan::new(0, 30);
    index.symbols[1].span.bytes = ByteSpan::new(10, 20);
    assert_eq!(index.symbol_at(FileId(0), 15), Some(SymbolId(1)));
    assert_eq!(index.symbol_at(FileId(0), 25), Some(SymbolId(0)));
    assert_eq!(index.symbol_at(FileId(0), 99), None);
    assert_eq!(index.file_by_path("a.py"), Some(FileId(0)));
    assert_eq!(index.file_by_path("b.py"), None);
    assert_eq!(index.symbols_of(FileId(0)).len(), 2);
    assert_eq!(index.file(FileId(0)).symbol_of_decl(1), Some(SymbolId(1)));
    assert_eq!(index.file(FileId(0)).symbol_of_decl(2), None);
}

#[test]
fn validate_catches_inconsistencies() {
    let mut index = index_with(&["a", "b"], &[(0, 1)]);
    index.sites.push(site(0, &[1]));
    index.decisions.push(decision(0, &[1]));
    index.validate().unwrap();

    let mut bad = index.clone();
    bad.edges[0].kind = EdgeKind::InferredCall;
    bad.edges[0].tier = Tier::Inferred;
    assert!(bad.validate().is_err(), "inferred edges are never stored as facts");

    let mut bad = index.clone();
    bad.decisions.push(decision(1, &[]));
    assert!(bad.validate().is_err(), "decisions must align with sites");

    let mut bad = index.clone();
    bad.files[0].symbol_count = 3;
    assert!(bad.validate().is_err());

    let mut bad = index.clone();
    bad.symbols[1].parent = Some(SymbolId(7));
    assert!(bad.validate().is_err());
}
