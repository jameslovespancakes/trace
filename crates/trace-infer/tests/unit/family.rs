//! Tests for [`crate::family`] (declarations built by hand; nothing is read or executed).

use super::*;
use crate::test_support::{Decl, Fixture};
use trace_core::facts::ImportKind;

/// (kind, from, to) rows with qualified names (`path:qualified`).
fn rows(index: &Index, edges: &[Edge]) -> Vec<(String, String, String)> {
    let name = |s: SymbolId| {
        let sym = index.symbol(s);
        format!("{}:{}", index.file_path(sym.file), sym.qualified_name)
    };
    let mut out: Vec<(String, String, String)> = edges
        .iter()
        .map(|e| (e.kind.as_str().to_string(), name(e.from), name(e.to)))
        .collect();
    out.sort();
    out
}

fn row(kind: &str, from: &str, to: &str) -> (String, String, String) {
    (kind.into(), from.into(), to.into())
}

/// `DefaultJSONProvider(JSONProvider)` overrides `dumps` / `loads` (same file), and a
/// test's `CustomProvider(DefaultJSONProvider)` overrides `loads` through its import.
#[test]
fn rule_python_overrides_follow_bases_across_imports() {
    let mut fx = Fixture::new();
    let p = fx.file("src/flask/json/provider.py", Language::Python, None);
    let t = fx.file("tests/test_json.py", Language::Python, None);
    let base = fx.decl(p, Decl::class("JSONProvider"));
    fx.decl(p, Decl::method("dumps", base));
    fx.decl(p, Decl::method("loads", base));
    let default = fx.decl(p, Decl::class("DefaultJSONProvider").bases(&["JSONProvider"]));
    fx.decl(p, Decl::method("dumps", default));
    fx.decl(p, Decl::method("loads", default));
    fx.decl(p, Decl::method("response", default));
    fx.import(t, "DefaultJSONProvider", "flask.json.provider.DefaultJSONProvider", ImportKind::Member);
    let custom = fx.decl(t, Decl::class("CustomProvider").bases(&["DefaultJSONProvider"]));
    let loads = fx.decl(t, Decl::method("loads", custom));
    let index = fx.build();
    let edges = family_edges(&index);
    assert_eq!(
        rows(&index, &edges),
        vec![
            row(
                "overrides",
                "src/flask/json/provider.py:DefaultJSONProvider.dumps",
                "src/flask/json/provider.py:JSONProvider.dumps"
            ),
            row(
                "overrides",
                "src/flask/json/provider.py:DefaultJSONProvider.loads",
                "src/flask/json/provider.py:JSONProvider.loads"
            ),
            row(
                "overrides",
                "tests/test_json.py:CustomProvider.loads",
                "src/flask/json/provider.py:DefaultJSONProvider.loads"
            ),
        ]
    );
    for e in &edges {
        assert_eq!(e.tier, Tier::Proven);
        assert_eq!(e.provider, Provider::Rule("python-mro".into()));
        assert_eq!(e.resolution, Resolution::InheritanceRule);
        assert_eq!(e.at.bytes, index.symbol(e.from).name_span);
        assert_eq!(e.at.file, index.symbol(e.from).file);
        assert!(e.bridge.is_none() && e.site.is_none());
    }
    let from_loads = edges.iter().find(|e| e.from == fx.id(loads)).unwrap();
    assert_eq!(from_loads.kind, EdgeKind::Overrides);
    // The edges are valid index edges.
    let mut index = index;
    index.edges.extend(edges);
    trace_core::assemble::sort_edges(&mut index.edges);
    index.validate().unwrap();
}

/// Python MRO (C3): in `D(B, C)` with `B(A)`, `C(A)`, `D.m` overrides `C.m` (not `A.m`).
#[test]
fn diamond_follows_c3_order() {
    let mut fx = Fixture::new();
    let f = fx.file("m.py", Language::Python, None);
    let a = fx.decl(f, Decl::class("A"));
    fx.decl(f, Decl::method("m", a));
    fx.decl(f, Decl::class("B").bases(&["A"]));
    let c = fx.decl(f, Decl::class("C").bases(&["A"]));
    fx.decl(f, Decl::method("m", c));
    let d = fx.decl(f, Decl::class("D").bases(&["B", "C"]));
    fx.decl(f, Decl::method("m", d));
    let index = fx.build();
    let edges = family_edges(&index);
    assert_eq!(
        rows(&index, &edges),
        vec![
            row("overrides", "m.py:C.m", "m.py:A.m"),
            row("overrides", "m.py:D.m", "m.py:C.m"),
        ]
    );
}

/// Two unrelated files declare `Base`; a third file's `Child(Base)` has no import: the base
/// is ambiguous and nothing is emitted (never "same directory").
#[test]
fn ambiguous_bases_emit_nothing() {
    let mut fx = Fixture::new();
    let x = fx.file("pkg/x.py", Language::Python, None);
    let y = fx.file("pkg/y.py", Language::Python, None);
    let z = fx.file("pkg/z.py", Language::Python, None);
    for f in [x, y] {
        let b = fx.decl(f, Decl::class("Base"));
        fx.decl(f, Decl::method("m", b));
    }
    let child = fx.decl(z, Decl::class("Child").bases(&["Base"]));
    fx.decl(z, Decl::method("m", child));
    let index = fx.build();
    assert!(family_edges(&index).is_empty());
}

/// A compiler fact in the class header (semantic value reference at the base spelling)
/// resolves an otherwise ambiguous base.
#[test]
fn compiler_facts_resolve_ambiguous_bases() {
    let mut fx = Fixture::new();
    let x = fx.file("pkg/x.py", Language::Python, None);
    let y = fx.file("pkg/y.py", Language::Python, None);
    let z = fx.file("pkg/z.py", Language::Python, None);
    fx.decl(x, Decl::class("Base"));
    let yb = fx.decl(y, Decl::class("Base"));
    let ym = fx.decl(y, Decl::method("m", yb));
    let child = fx.decl(
        z,
        Decl::class("Child")
            .bases(&["Base"])
            .span(trace_core::ByteSpan::new(10, 40)),
    );
    let cm = fx.decl(z, Decl::method("m", child).span(trace_core::ByteSpan::new(20, 30)));
    fx.value_ref(z, trace_core::ByteSpan::new(12, 16), yb);
    let index = fx.build();
    let edges = family_edges(&index);
    assert_eq!(edges.len(), 1);
    assert_eq!((edges[0].from, edges[0].to), (fx.id(cm), fx.id(ym)));
}

/// Rust: `impl Shape for Square { fn area }` implements the trait method; inherent methods
/// do not.
#[test]
fn rust_trait_impls_implement_trait_methods() {
    use trace_core::ByteSpan;
    let mut fx = Fixture::new();
    let f = fx.file("src/lib.rs", Language::Rust, None);
    let shape = fx.decl(f, Decl::interface("Shape"));
    let area = fx.decl(f, Decl::method("area", shape).stub());
    fx.decl(f, Decl::class("Square"));
    let sq_area = fx.decl(
        f,
        Decl::function("area")
            .container("Square")
            .span(ByteSpan::new(120, 150)),
    );
    fx.decl(
        f,
        Decl::function("perimeter")
            .container("Square")
            .span(ByteSpan::new(220, 250)),
    );
    fx.impl_block(f, "Square", "Shape", ByteSpan::new(100, 200));
    fx.impl_block(f, "Square", "Debug", ByteSpan::new(210, 260));
    let index = fx.build();
    let edges = family_edges(&index);
    assert_eq!(edges.len(), 1);
    assert_eq!((edges[0].from, edges[0].to), (fx.id(sq_area), fx.id(area)));
    assert_eq!(edges[0].kind, EdgeKind::Implements);
    assert_eq!(edges[0].provider, Provider::Rule("rust-trait-impl".into()));
}

/// TypeScript: `class Impl extends Base implements Greeter` overrides `Base.greet` and
/// implements `Greeter.greet`.
#[test]
fn typescript_extends_and_implements() {
    let mut fx = Fixture::new();
    let f = fx.file("src/a.ts", Language::TypeScript, None);
    let greeter = fx.decl(f, Decl::interface("Greeter"));
    fx.decl(f, Decl::method("greet", greeter).stub());
    let base = fx.decl(f, Decl::class("Base"));
    fx.decl(f, Decl::method("greet", base));
    let imp = fx.decl(f, Decl::class("Impl").bases(&["Base", "Greeter"]));
    fx.decl(f, Decl::method("greet", imp));
    fx.decl(f, Decl::constructor("constructor", imp));
    let index = fx.build();
    let edges = family_edges(&index);
    assert_eq!(
        rows(&index, &edges),
        vec![
            row("implements", "src/a.ts:Impl.greet", "src/a.ts:Greeter.greet"),
            row("overrides", "src/a.ts:Impl.greet", "src/a.ts:Base.greet"),
        ]
    );
    assert!(edges
        .iter()
        .all(|e| e.provider == Provider::Rule("ts-heritage".into())));
}

/// Go has no declared type relations: nothing is emitted; synthetic symbols never are.
#[test]
fn go_and_synthetic_symbols_are_skipped() {
    let mut fx = Fixture::new();
    let f = fx.file("a.go", Language::Go, None);
    let b = fx.decl(f, Decl::class("Base"));
    fx.decl(f, Decl::function("Run").container("Base"));
    fx.decl(f, Decl::class("Child").bases(&["Base"]));
    fx.decl(f, Decl::function("Run").container("Child"));
    let p = fx.file("m.py", Language::Python, None);
    let a = fx.decl(p, Decl::class("A"));
    fx.decl(p, Decl::method("<lambda>", a));
    let c = fx.decl(p, Decl::class("C").bases(&["A"]));
    fx.decl(p, Decl::method("<lambda>", c));
    fx.module(p);
    let index = fx.build();
    assert!(family_edges(&index).is_empty());
    let _ = b;
}

/// C: a header prototype and its single definition are one entity (proven
/// `stub_implementation`, `rule:c-prototype`); a name defined in two files (static helpers)
/// and C++ member prototypes with an out-of-line definition follow the same unique rule.
#[test]
fn c_prototypes_link_to_their_unique_definition() {
    let mut fx = Fixture::new();
    let h = fx.file("src/bytecode.h", Language::C, None);
    let c = fx.file("src/bytecode.c", Language::C, None);
    let a = fx.file("src/a.c", Language::C, None);
    let b = fx.file("src/b.c", Language::C, None);
    fx.decl(h, Decl::function("opcode_describe").stub());
    fx.decl(c, Decl::function("opcode_describe"));
    fx.decl(h, Decl::function("helper").stub());
    fx.decl(a, Decl::function("helper"));
    fx.decl(b, Decl::function("helper"));
    let hp = fx.file("w.hpp", Language::Cpp, None);
    let cc = fx.file("w.cpp", Language::Cpp, None);
    let widget = fx.decl(hp, Decl::class("Widget"));
    fx.decl(hp, Decl::method("draw", widget).stub());
    fx.decl(cc, Decl::function("draw").container("Widget"));
    let index = fx.build();
    let edges: Vec<Edge> = family_edges(&index)
        .into_iter()
        .filter(|e| e.kind == EdgeKind::StubImplementation)
        .collect();
    assert_eq!(
        rows(&index, &edges),
        vec![
            row("stub_implementation", "src/bytecode.h:opcode_describe", "src/bytecode.c:opcode_describe"),
            row("stub_implementation", "w.hpp:Widget.draw", "w.cpp:draw"),
        ]
    );
    assert!(edges.iter().all(|e| e.tier == Tier::Proven
        && e.provider == Provider::Rule("c-prototype".into())
        && e.resolution == Resolution::AbiNamingRule));
}

// --- general fixes: families across files, crates and packages ---------------------------

/// Rust: `impl Sink for JsonSink` in crate `b` names crate `a`'s trait through `use
/// sink_api::Sink` (the crate's manifest name differs from its directory, so no module path
/// rule resolves it; another trait `Sink` exists in crate `c`, so the name is not unique). The
/// server's `definition` of the trait identifier in the impl header (a value reference)
/// decides; `testutil.rs` impls resolve the same way, and the forwarding impl
/// `impl<S: Sink> Sink for &mut S` resolves by the same-file rule.
#[test]
fn rule_trait_impls_across_crates_use_header_compiler_facts() {
    use trace_core::ByteSpan;
    let build = |facts: bool| {
        let mut fx = Fixture::new();
        let a = fx.file("a/src/lib.rs", Language::Rust, None);
        let b = fx.file("b/src/lib.rs", Language::Rust, None);
        let t = fx.file("b/src/testutil.rs", Language::Rust, None);
        let c = fx.file("c/src/lib.rs", Language::Rust, None);
        let sink = fx.decl(a, Decl::interface("Sink"));
        let matched = fx.decl(a, Decl::method("matched", sink).stub());
        let context = fx.decl(a, Decl::method("context", sink));
        let other = fx.decl(c, Decl::interface("Sink"));
        fx.decl(c, Decl::method("matched", other).stub());
        // impl<S: Sink + ?Sized> Sink for &mut S { fn matched(..) { (**self).matched(..) } }
        let forward = fx.decl(
            a,
            Decl::function("matched")
                .container("S")
                .qualified("S.matched")
                .span(ByteSpan::new(520, 560)),
        );
        fx.impl_block(a, "S", "Sink", ByteSpan::new(500, 700));
        // crate b: use sink_api::Sink; impl Sink for JsonSink { fn matched; fn context }
        fx.import(b, "Sink", "sink_api::Sink", ImportKind::Member);
        fx.decl(b, Decl::class("JsonSink"));
        let json_matched = fx.decl(
            b,
            Decl::function("matched")
                .container("JsonSink")
                .qualified("JsonSink.matched")
                .span(ByteSpan::new(120, 150)),
        );
        let json_context = fx.decl(
            b,
            Decl::function("context")
                .container("JsonSink")
                .qualified("JsonSink.context")
                .span(ByteSpan::new(160, 190)),
        );
        fx.impl_block(b, "JsonSink", "Sink", ByteSpan::new(100, 300));
        // testutil.rs: impl Sink for KitchenSink { fn matched }
        fx.import(t, "Sink", "sink_api::Sink", ImportKind::Member);
        fx.decl(t, Decl::class("KitchenSink"));
        let kitchen = fx.decl(
            t,
            Decl::function("matched")
                .container("KitchenSink")
                .qualified("KitchenSink.matched")
                .span(ByteSpan::new(130, 160)),
        );
        fx.impl_block(t, "KitchenSink", "Sink", ByteSpan::new(100, 200));
        if facts {
            // `Sink` identifiers in the impl headers, resolved by the server.
            fx.value_ref(b, ByteSpan::new(105, 109), sink);
            fx.value_ref(t, ByteSpan::new(105, 109), sink);
        }
        let ids = [matched, context, forward, json_matched, json_context, kitchen];
        (fx, ids)
    };

    let (fx, [matched, context, forward, json_matched, json_context, kitchen]) = build(true);
    let index = fx.build();
    let edges = family_edges(&index);
    let got: BTreeSet<(SymbolId, SymbolId)> = edges.iter().map(|e| (e.from, e.to)).collect();
    let want: BTreeSet<(SymbolId, SymbolId)> = [
        (forward, matched),
        (json_matched, matched),
        (json_context, context),
        (kitchen, matched),
    ]
    .into_iter()
    .map(|(f, t)| (fx.id(f), fx.id(t)))
    .collect();
    assert_eq!(got, want, "{:?}", rows(&index, &edges));
    assert!(edges.iter().all(|e| e.kind == EdgeKind::Implements
        && e.tier == Tier::Proven
        && e.provider == Provider::Rule("rust-trait-impl".into())
        && e.at.bytes == index.symbol(e.from).name_span));
    assert_eq!(impl_traits(&index, &Hierarchy::build(&index)).len(), 3);

    // Without the compiler facts the cross-crate trait is ambiguous: only the same-file
    // forwarding impl is linked (never a guess).
    let (fx, [matched, _, forward, ..]) = build(false);
    let index = fx.build();
    let edges = family_edges(&index);
    let got: Vec<(SymbolId, SymbolId)> = edges.iter().map(|e| (e.from, e.to)).collect();
    assert_eq!(got, vec![(fx.id(forward), fx.id(matched))]);
}

/// Members declared in an extension / impl block of a type in another file and directory
/// attach to that type (container rule), among the types of the same language family: a
/// same-named class of another language never captures them.
#[test]
fn rule_extension_members_attach_to_type() {
    let mut fx = Fixture::new();
    let s = fx.blind_file("src/net/session.rs", Language::Rust, None);
    let x = fx.blind_file("src/extensions/retry.rs", Language::Rust, None);
    let py = fx.file("tools/session.py", Language::Python, None);
    let session = fx.decl(s, Decl::class("Session"));
    let body = fx.decl(s, Decl::method("start", session));
    let retry = fx.decl(
        x,
        Decl::function("retry")
            .container("Session")
            .qualified("Session.retry"),
    );
    let py_session = fx.decl(py, Decl::class("Session"));
    let index = fx.build();
    let h = Hierarchy::build(&index);
    assert_eq!(h.class_of(&index, fx.id(retry)), Some(fx.id(session)));
    assert_eq!(h.method(fx.id(session), "retry"), Some(fx.id(retry)));
    assert_eq!(h.method(fx.id(session), "start"), Some(fx.id(body)));
    assert_eq!(h.method(fx.id(py_session), "retry"), None);
}

/// C / C++: header prototypes and forward declarations link to the unique definition in
/// another file; owner paths match across namespaces written on either side (`namespace
/// shapes { class Box { int volume() const; }; }` and `int shapes::Box::volume() const {}`
/// whose namespace is part of the path, not of the container), while the same member of a
/// class in another namespace and a global function of the same name do not.
#[test]
fn rule_header_prototypes_link_to_definitions() {
    let mut fx = Fixture::new();
    let gh = fx.blind_file("c/geometry.h", Language::C, None);
    let gc = fx.blind_file("c/geometry.c", Language::C, None);
    fx.decl(gh, Decl::function("area").stub());
    fx.decl(gc, Decl::function("area"));
    let hpp = fx.blind_file("cpp/shapes.hpp", Language::Cpp, None);
    let cc = fx.blind_file("cpp/shapes.cc", Language::Cpp, None);
    let other = fx.blind_file("cpp/other.cc", Language::Cpp, None);
    let util = fx.blind_file("cpp/util.cc", Language::Cpp, None);
    let fwd = fx.blind_file("cpp/fwd.hpp", Language::Cpp, None);
    let box_ = fx.decl(hpp, Decl::class("Box").qualified("shapes.Box"));
    fx.decl(hpp, Decl::method("volume", box_).stub());
    fx.decl(hpp, Decl::function("scale").stub().qualified("shapes.scale"));
    fx.decl(hpp, Decl::class("Sphere").qualified("shapes.Sphere"));
    fx.decl(cc, Decl::function("volume").container("Box").qualified("Box.volume"));
    fx.decl(cc, Decl::function("scale").qualified("shapes.scale"));
    fx.decl(
        other,
        Decl::function("volume")
            .container("Box")
            .qualified("other.Box.volume"),
    );
    fx.decl(util, Decl::function("scale"));
    fx.decl(fwd, Decl::class("Sphere").stub().qualified("shapes.Sphere"));
    let index = fx.build();
    let edges = prototype_edges(&index, None);
    assert_eq!(
        rows(&index, &edges),
        vec![
            row("stub_implementation", "c/geometry.h:area", "c/geometry.c:area"),
            row("stub_implementation", "cpp/fwd.hpp:shapes.Sphere", "cpp/shapes.hpp:shapes.Sphere"),
            row("stub_implementation", "cpp/shapes.hpp:shapes.Box.volume", "cpp/shapes.cc:Box.volume"),
            row("stub_implementation", "cpp/shapes.hpp:shapes.scale", "cpp/shapes.cc:shapes.scale"),
        ]
    );
    assert!(edges.iter().all(|e| e.tier == Tier::Proven
        && e.provider == Provider::Rule("c-prototype".into())
        && e.resolution == Resolution::AbiNamingRule));
    // The same edges come out of the family rule.
    let all = family_edges(&index);
    assert_eq!(all.iter().filter(|e| e.kind == EdgeKind::StubImplementation).count(), 4);
}

/// Haskell: a type signature (`matches :: String -> String -> Bool`, a stub declaration)
/// links to every equation of the binding in the same module; a binding of the same name in
/// another module is a different function.
#[test]
fn rule_haskell_signatures_link_to_bindings() {
    let mut fx = Fixture::new();
    let m = fx.blind_file("hs/Match.hs", Language::Haskell, None);
    let o = fx.blind_file("hs/Other.hs", Language::Haskell, None);
    let signature = fx.decl(m, Decl::function("matches").stub());
    let first = fx.decl(m, Decl::function("matches"));
    let second = fx.decl(m, Decl::function("matches"));
    fx.decl(o, Decl::function("matches"));
    let index = fx.build();
    let edges = prototype_edges(&index, None);
    let got: Vec<(SymbolId, SymbolId)> = edges.iter().map(|e| (e.from, e.to)).collect();
    assert_eq!(got, vec![(fx.id(signature), fx.id(first)), (fx.id(signature), fx.id(second)),]);
    assert!(edges.iter().all(|e| e.kind == EdgeKind::StubImplementation
        && e.tier == Tier::Proven
        && e.provider == Provider::Rule("haskell-declaration".into())
        && e.resolution == Resolution::InheritanceRule));
}

/// TypeScript: overload signatures and their implementation share a qualified name in one
/// module (free functions and class methods); a `declare function` without implementation
/// and an interface member of the same name link to nothing.
#[test]
fn rule_ts_overload_signatures_link_to_implementation() {
    let mut fx = Fixture::new();
    let f = fx.file("ts/parse.ts", Language::TypeScript, None);
    let first = fx.decl(f, Decl::function("parse").stub());
    let second = fx.decl(f, Decl::function("parse").stub());
    let implementation = fx.decl(f, Decl::function("parse"));
    let client = fx.decl(f, Decl::class("Client"));
    let get_sig = fx.decl(f, Decl::method("get", client).stub());
    let get_impl = fx.decl(f, Decl::method("get", client));
    fx.decl(f, Decl::function("helper").stub());
    let getter = fx.decl(f, Decl::interface("Getter"));
    fx.decl(f, Decl::method("get", getter).stub());
    let index = fx.build();
    let edges: Vec<Edge> = prototype_edges(&index, None);
    let got: BTreeSet<(SymbolId, SymbolId)> = edges.iter().map(|e| (e.from, e.to)).collect();
    let want: BTreeSet<(SymbolId, SymbolId)> =
        [(first, implementation), (second, implementation), (get_sig, get_impl)]
            .into_iter()
            .map(|(a, b)| (fx.id(a), fx.id(b)))
            .collect();
    assert_eq!(got, want);
    assert!(edges
        .iter()
        .all(|e| e.provider == Provider::Rule("typescript-declaration".into())));
}

/// A pair the server already proved (`FileSemantics::implementations`, assembled into an
/// `implementation` edge) is not emitted again by the rule.
#[test]
fn rule_server_implementations_are_not_repeated_by_rules() {
    let mut fx = Fixture::new();
    let f = fx.file("src/a.ts", Language::TypeScript, None);
    let greeter = fx.decl(f, Decl::interface("Greeter"));
    let greet = fx.decl(f, Decl::method("greet", greeter).stub());
    let imp = fx.decl(f, Decl::class("Impl").bases(&["Greeter"]));
    let imp_greet = fx.decl(f, Decl::method("greet", imp));
    fx.implementation(f, greet, imp_greet, EdgeKind::Implements);
    let index = fx.build();
    let server: Vec<&Edge> = index
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Implements)
        .collect();
    assert_eq!(server.len(), 1);
    assert_eq!(server[0].resolution, Resolution::Implementation);
    assert_eq!((server[0].from, server[0].to), (fx.id(imp_greet), fx.id(greet)));
    assert!(family_edges(&index).is_empty());
}

/// Previous rule edges mapped onto the ids of `to` by uid (what the incremental link's
/// `IdRemap` does; edges naming a symbol that no longer exists are dropped).
fn remap_by_uid(edges: &[Edge], from: &Index, to: &Index) -> Vec<Edge> {
    let by_uid: std::collections::HashMap<&str, SymbolId> =
        to.symbols.iter().map(|s| (s.uid.as_str(), s.id)).collect();
    edges
        .iter()
        .filter_map(|e| {
            let mut e = e.clone();
            e.from = *by_uid.get(from.symbol(e.from).uid.as_str())?;
            e.to = *by_uid.get(from.symbol(e.to).uid.as_str())?;
            e.at.file = to.file_by_path(from.file_path(e.at.file))?;
            Some(e)
        })
        .collect()
}

/// PLAN decision 13: after an edit of a Python file (an override added), the incremental
/// family edges (Java group kept from the previous build, the Python group computed again)
/// equal the full rule on the new index.
#[test]
fn rule_incremental_family_edges_equal_full() {
    let build = |with_override: bool| {
        let mut fx = Fixture::new();
        let j = fx.file("src/Shape.java", Language::Java, None);
        let shape = fx.decl(j, Decl::interface("Shape"));
        fx.decl(j, Decl::method("area", shape).stub());
        let square = fx.decl(j, Decl::class("Square").bases(&["Shape"]));
        fx.decl(j, Decl::method("area", square));
        let p = fx.file("app/models.py", Language::Python, None);
        let base = fx.decl(p, Decl::class("Base"));
        fx.decl(p, Decl::method("save", base));
        fx.decl(p, Decl::method("load", base));
        let sub = fx.decl(p, Decl::class("Sub").bases(&["Base"]));
        fx.decl(p, Decl::method("save", sub));
        if with_override {
            fx.decl(p, Decl::method("load", sub));
        }
        fx.build()
    };
    let before = build(false);
    let after = build(true);
    let prev = remap_by_uid(&family_edges(&before), &before, &after);
    let delta = trace_core::delta::IndexDelta {
        modified: ["app/models.py".to_string()].into_iter().collect(),
        ..Default::default()
    };
    let h = Hierarchy::build(&after);
    let incremental = family_edges_delta(&after, &h, &delta, &prev, None);
    let full = family_edges(&after);
    assert_eq!(rows(&after, &incremental), rows(&after, &full));
    assert!(rows(&after, &full).contains(&row(
        "overrides",
        "app/models.py:Sub.load",
        "app/models.py:Base.load"
    )));
    assert!(rows(&after, &full).contains(&row(
        "implements",
        "src/Shape.java:Square.area",
        "src/Shape.java:Shape.area"
    )));
    // Nothing changed: the previous edges are the answer.
    let none = family_edges_delta(&after, &h, &trace_core::delta::IndexDelta::default(), &full, None);
    assert_eq!(rows(&after, &none), rows(&after, &full));
}

/// I-65 (Haskell, language rule): `class Pretty a where pretty :: a -> Doc` declares a stub
/// member; `instance Pretty Token where pretty = ...` (an out-of-line relation whose members
/// have the instance type as container) implements it, also for an instance type declared
/// outside the index (`instance Pretty Int`). Both instance members are implementations of
/// the class member (dispatch candidates); a function of the same name outside every
/// instance block is not.
#[test]
fn rule_instance_method_implements_class_method() {
    use trace_core::ByteSpan;
    let mut fx = Fixture::new();
    let f = fx.file("src/Pretty.hs", Language::Haskell, None);
    let class = fx.decl(f, Decl::interface("Pretty").span(ByteSpan::new(10, 60)));
    let sig = fx.decl(f, Decl::method("pretty", class).stub().span(ByteSpan::new(30, 50)));
    fx.decl(f, Decl::class("Token").span(ByteSpan::new(70, 90)));
    fx.impl_block(f, "Token", "Pretty", ByteSpan::new(100, 160));
    let token = fx.decl(
        f,
        Decl::function("pretty")
            .container("Token")
            .qualified("Token.pretty")
            .span(ByteSpan::new(120, 150)),
    );
    fx.impl_block(f, "Int", "Pretty", ByteSpan::new(200, 260));
    let int = fx.decl(
        f,
        Decl::function("pretty")
            .container("Int")
            .qualified("Int.pretty")
            .span(ByteSpan::new(220, 250)),
    );
    let loose = fx.decl(
        f,
        Decl::function("pretty")
            .qualified("pretty")
            .span(ByteSpan::new(300, 330)),
    );
    let index = fx.build();
    let edges = family_edges(&index);
    let implements: Vec<(SymbolId, SymbolId)> = edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Implements)
        .map(|e| (e.from, e.to))
        .collect();
    assert!(implements.contains(&(fx.id(token), fx.id(sig))), "{implements:?}");
    assert!(implements.contains(&(fx.id(int), fx.id(sig))), "{implements:?}");
    assert!(!implements.iter().any(|(from, _)| *from == fx.id(loose)));
    let h = Hierarchy::build(&index);
    let mut expected = vec![fx.id(int), fx.id(token)];
    expected.sort_by(|a, b| index.symbol(*a).uid.cmp(&index.symbol(*b).uid));
    assert_eq!(h.implementations(&index, fx.id(sig)), expected);
}

/// I-67 (R, language rule): a function `g.<class>` implements the S3 generic `g` (a stub:
/// its body calls `UseMethod`); the longest generic name wins (`as.frame.tbl` belongs to
/// `as.frame`, not to `as`); a dotted name without a generic prefix (`is.tidy`) implements
/// nothing. The methods are the generic's dispatch candidates.
#[test]
fn rule_s3_method_implements_its_generic() {
    let mut fx = Fixture::new();
    let g = fx.file("R/generics.R", Language::R, None);
    let m = fx.file("R/methods.R", Language::R, None);
    let summarise = fx.decl(g, Decl::function("summarise").stub());
    let as_ = fx.decl(g, Decl::function("as").stub());
    let as_frame = fx.decl(g, Decl::function("as.frame").stub());
    let df = fx.decl(m, Decl::function("summarise.data.frame"));
    let grouped = fx.decl(m, Decl::function("summarise.grouped_df"));
    let tbl = fx.decl(m, Decl::function("as.frame.tbl"));
    let other = fx.decl(m, Decl::function("is.tidy"));
    let index = fx.build();
    let edges = family_edges(&index);
    let s3: Vec<(SymbolId, SymbolId)> = edges
        .iter()
        .filter(|e| e.provider == Provider::Rule("r-s3".into()))
        .map(|e| (e.from, e.to))
        .collect();
    assert!(s3.contains(&(fx.id(df), fx.id(summarise))), "{s3:?}");
    assert!(s3.contains(&(fx.id(grouped), fx.id(summarise))));
    assert!(s3.contains(&(fx.id(tbl), fx.id(as_frame))));
    assert!(!s3.contains(&(fx.id(tbl), fx.id(as_))));
    assert!(!s3.iter().any(|(from, _)| *from == fx.id(other)));
    assert!(edges
        .iter()
        .filter(|e| e.provider == Provider::Rule("r-s3".into()))
        .all(|e| e.kind == EdgeKind::Implements));
    let h = Hierarchy::build(&index);
    assert_eq!(h.implementations(&index, fx.id(summarise)), vec![fx.id(df), fx.id(grouped)]);
}
