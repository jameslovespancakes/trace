use super::*;
use crate::test_support::{Decl, Fixture};

#[test]
fn base_names_are_bare() {
    assert_eq!(base_name("pkg.Base[T]"), "Base");
    assert_eq!(base_name(" Base<T, U> "), "Base");
    assert_eq!(base_name("public Base"), "Base");
    assert_eq!(base_name("std::fmt::Display"), "Display");
    assert_eq!(base_name("*Server"), "Server");
    assert_eq!(base_name("metaclass=ABCMeta"), "");
    assert_eq!(base_name("typing.Protocol"), "Protocol");
}

/// Rule: an abstract member's implementations include a bodiless member whose body is
/// across a language boundary (Java `native`: a `Uses` boundary names the declaration);
/// a bodiless member without such a fact (abstract) runs nothing.
#[test]
fn rule_native_member_implements_an_abstract_one() {
    use trace_core::facts::{BoundaryFact, BoundaryRole};
    use trace_core::model::BridgeKind;
    let mut fx = Fixture::new();
    let f = fx.file("src/Db.java", Language::Java, None);
    let db = fx.decl(f, Decl::class("Db"));
    let stop = fx.decl(f, Decl::method("stop", db).stub());
    let native = fx.decl(f, Decl::class("NativeDb").bases(&["Db"]));
    let native_stop = fx.decl(f, Decl::method("stop", native).stub());
    let other = fx.decl(f, Decl::class("OtherDb").bases(&["Db"]));
    fx.decl(f, Decl::method("stop", other).stub());
    let mut index = fx.build();
    let without = Hierarchy::build(&index);
    assert!(without.implementations(&index, fx.id(stop)).is_empty());
    let facts = index.files[0].facts.as_mut().unwrap();
    facts.boundaries.push(BoundaryFact {
        kind: BridgeKind::Jni,
        role: BoundaryRole::Uses,
        name: "Java_NativeDb_stop".into(),
        owner: None,
        decl: Some(native_stop.decl),
        span: trace_core::ByteSpan::new(0, 0),
        line: 1,
        detail: Vec::new(),
    });
    let h = Hierarchy::build(&index);
    assert_eq!(h.implementations(&index, fx.id(stop)), vec![fx.id(native_stop)]);
    assert!(h.runs_nothing(&index, fx.id(stop)));
    assert!(!h.runs_nothing(&index, fx.id(native_stop)));
}

#[test]
fn protocol_implementations_are_structural_and_nominal() {
    let mut fx = Fixture::new();
    let f = fx.file("m.py", Language::Python, None);
    let store = fx.decl(f, Decl::class("Store").bases(&["Protocol"]));
    let save = fx.decl(f, Decl::method("save", store).stub());
    fx.decl(f, Decl::method("load", store).stub());
    let disk = fx.decl(f, Decl::class("DiskStore"));
    let disk_save = fx.decl(f, Decl::method("save", disk));
    fx.decl(f, Decl::method("load", disk));
    let mem = fx.decl(f, Decl::class("MemoryStore"));
    let mem_save = fx.decl(f, Decl::method("save", mem));
    fx.decl(f, Decl::method("load", mem));
    let partial = fx.decl(f, Decl::class("Partial"));
    fx.decl(f, Decl::method("save", partial));
    let sub = fx.decl(f, Decl::class("Sub").bases(&["Store"]));
    let sub_save = fx.decl(f, Decl::method("save", sub));
    let index = fx.build();
    let h = Hierarchy::build(&index);
    let got: Vec<&str> = h
        .implementations(&index, fx.id(save))
        .into_iter()
        .map(|s| index.symbol(s).qualified_name.as_str())
        .collect();
    assert_eq!(got, vec!["DiskStore.save", "MemoryStore.save", "Sub.save"]);
    let _ = (disk_save, mem_save, sub_save);
    assert_eq!(h.class_of(&index, fx.id(save)), Some(fx.id(store)));
    assert!(h.family(fx.id(store)).contains(&fx.id(sub)));
    assert_eq!(h.mro(fx.id(sub)), vec![fx.id(sub), fx.id(store)]);
}

/// PLAN decision 13: the hierarchy after an update equals the hierarchy built for the
/// new index (a base added in an edit reaches the family, a removed class leaves it).
#[test]
fn rule_hierarchy_update_equals_rebuild() {
    let build = |with_sub: bool| {
        let mut fx = Fixture::new();
        let f = fx.file("m.py", Language::Python, None);
        let base = fx.decl(f, Decl::class("Base"));
        fx.decl(f, Decl::method("run", base).stub());
        let g = fx.file("n.py", Language::Python, None);
        if with_sub {
            let sub = fx.decl(g, Decl::class("Sub").bases(&["Base"]));
            fx.decl(g, Decl::method("run", sub));
        }
        fx.decl(g, Decl::function("helper"));
        fx.build()
    };
    let before = build(false);
    let after = build(true);
    let mut h = Hierarchy::build(&before);
    let remap = trace_core::delta::IdRemap::identity(before.files.len(), before.symbols.len());
    let delta = trace_core::delta::IndexDelta {
        modified: ["n.py".to_string()].into_iter().collect(),
        ..Default::default()
    };
    h.update(&after, &remap, &delta);
    let rebuilt = Hierarchy::build(&after);
    let sorted = |m: &HashMap<SymbolId, Vec<SymbolId>>| {
        let mut v: Vec<(SymbolId, Vec<SymbolId>)> = m.iter().map(|(k, v)| (*k, v.clone())).collect();
        v.sort();
        v
    };
    assert_eq!(sorted(&h.bases), sorted(&rebuilt.bases));
    assert_eq!(sorted(&h.children), sorted(&rebuilt.children));
    let mut fa: Vec<_> = h.functions.iter().collect();
    let mut fb: Vec<_> = rebuilt.functions.iter().collect();
    fa.sort();
    fb.sort();
    assert_eq!(fa, fb);
    let run = after
        .symbols
        .iter()
        .find(|s| s.qualified_name == "Base.run")
        .expect("stub")
        .id;
    assert_eq!(h.implementations(&after, run), rebuilt.implementations(&after, run));
    assert_eq!(h.implementations(&after, run).len(), 1);
}

#[test]
fn containers_resolve_methods_out_of_line() {
    let mut fx = Fixture::new();
    let f = fx.file("src/lib.rs", Language::Rust, None);
    let tr = fx.decl(f, Decl::interface("Shape"));
    let area = fx.decl(f, Decl::method("area", tr).stub());
    let sq = fx.decl(f, Decl::class("Square"));
    let sq_area = fx.decl(f, Decl::function("area").container("Square"));
    fx.impl_relation(f, "Square", "Shape");
    let index = fx.build();
    let h = Hierarchy::build(&index);
    assert_eq!(h.class_of(&index, fx.id(sq_area)), Some(fx.id(sq)));
    assert_eq!(h.bases.get(&fx.id(sq)), Some(&vec![fx.id(tr)]));
    assert_eq!(h.implementations(&index, fx.id(area)), vec![fx.id(sq_area)]);
}
