use super::*;
use crate::test_support::{Decl, Fixture, D};
use trace_core::facts::{Export, Import, Scope};

/// Post-build facts of `path` (the fixture keeps its facts private).
fn facts_mut<'x>(index: &'x mut Index, path: &str) -> &'x mut FileFacts {
    let id = index.file_by_path(path).expect("fixture file");
    index.files[id.idx()].facts.as_mut().expect("facts")
}

/// An import binding plus its `import` reference (the imported name inside the
/// binding's span), added after assembly.
fn add_import(
    index: &mut Index,
    path: &str,
    local: &str,
    target: &str,
    kind: ImportKind,
    at: u32,
) -> ByteSpan {
    let name = last_name(target).to_string();
    let span = ByteSpan::new(at, at + name.len().max(1) as u32);
    let facts = facts_mut(index, path);
    facts.imports.push(Import {
        local: local.into(),
        target: target.into(),
        kind,
        scope: Scope::Module,
        span,
        line: 1,
    });
    facts.references.push(Reference {
        span,
        name,
        owner: None,
        in_decorator: false,
        local: false,
        kind: RefKind::Import,
    });
    span
}

fn add_export(index: &mut Index, path: &str, exported: &str, target: &str, at: u32) -> ByteSpan {
    let span = ByteSpan::new(at, at + exported.len() as u32);
    let facts = facts_mut(index, path);
    facts.exports.push(Export {
        exported: exported.into(),
        target: target.into(),
        span,
        line: 2,
    });
    facts.references.push(Reference {
        span,
        name: exported.into(),
        owner: None,
        in_decorator: false,
        local: false,
        kind: RefKind::Export,
    });
    span
}

fn edge_at(edges: &[Edge], index: &Index, path: &str, span: ByteSpan) -> Option<Edge> {
    let file = index.file_by_path(path)?;
    edges
        .iter()
        .find(|e| e.at.file == file && e.at.bytes == span)
        .cloned()
}

fn module(fx: &mut Fixture, file: usize) -> D {
    fx.module(file)
}

/// Python relative member import, Java member import through package directories,
/// Rust `use crate::a::f`, TS `export { f } from './m'`: each names exactly one
/// declaration -> a proven edge (provider `rule:import-path`, resolution `import_path`)
/// from the statement's `<module>` owner.
#[test]
fn rule_import_of_exact_path_is_proven() {
    let mut fx = Fixture::new();
    // Python
    let helpers = fx.blind_file("pkg/helpers.py", Language::Python, None);
    let views = fx.blind_file("pkg/views.py", Language::Python, None);
    let other = fx.blind_file("other/helpers.py", Language::Python, None);
    let redirect = fx.decl(helpers, Decl::function("redirect"));
    fx.decl(other, Decl::function("redirect"));
    let views_module = module(&mut fx, views);
    // Java
    let format_file = fx.blind_file("src/lib/Format.java", Language::Java, None);
    let decoy_file = fx.blind_file("src/other/Format.java", Language::Java, None);
    let util_file = fx.blind_file("src/okio/TestUtil.java", Language::Java, None);
    let main_java = fx.blind_file("src/app/Main.java", Language::Java, None);
    let format = fx.decl(format_file, Decl::function("format"));
    fx.decl(decoy_file, Decl::function("format"));
    let util = fx.decl(util_file, Decl::class("TestUtil"));
    let random = fx.decl(util_file, Decl::method("randomBytes", util));
    let main_module = module(&mut fx, main_java);
    // Rust
    let lib = fx.blind_file("src/lib.rs", Language::Rust, None);
    let a = fx.blind_file("src/a.rs", Language::Rust, None);
    let b = fx.blind_file("src/b.rs", Language::Rust, None);
    fx.module(lib);
    let f = fx.decl(a, Decl::function("f"));
    let b_module = module(&mut fx, b);
    // TypeScript
    let m = fx.blind_file("web/m.ts", Language::TypeScript, None);
    let barrel = fx.blind_file("web/index.ts", Language::TypeScript, None);
    let ts_f = fx.decl(m, Decl::function("f"));
    let barrel_module = module(&mut fx, barrel);

    let mut index = fx.build();
    let py = add_import(&mut index, "pkg/views.py", "redirect", ".helpers.redirect", ImportKind::Member, 10);
    let jv = add_import(&mut index, "src/app/Main.java", "format", "lib.format", ImportKind::Member, 20);
    let jv2 = add_import(
        &mut index,
        "src/app/Main.java",
        "randomBytes",
        "okio.TestUtil.randomBytes",
        ImportKind::Member,
        40,
    );
    let rs = add_import(&mut index, "src/b.rs", "f", "crate::a::f", ImportKind::Member, 60);
    let ts = add_export(&mut index, "web/index.ts", "f", "./m.f", 80);

    let edges = import_path_edges(&index);
    let check = |path: &str, span: ByteSpan, kind: EdgeKind, from: D, to: D| {
        let e = edge_at(&edges, &index, path, span).unwrap_or_else(|| panic!("no edge at {path}: {edges:?}"));
        assert_eq!(e.kind, kind, "{path}");
        assert_eq!(e.tier, Tier::Proven);
        assert_eq!(e.provider, Provider::Rule(RULE.into()));
        assert_eq!(e.resolution, Resolution::ImportPath);
        assert_eq!(e.from, fx.id(from), "{path}: owner");
        assert_eq!(e.to, fx.id(to), "{path}: target");
        assert_eq!(e.at.line, if kind == EdgeKind::Reexports { 2 } else { 1 });
    };
    check("pkg/views.py", py, EdgeKind::Imports, views_module, redirect);
    check("src/app/Main.java", jv, EdgeKind::Imports, main_module, format);
    check("src/app/Main.java", jv2, EdgeKind::Imports, main_module, random);
    check("src/b.rs", rs, EdgeKind::Imports, b_module, f);
    check("web/index.ts", ts, EdgeKind::Reexports, barrel_module, ts_f);
    assert_eq!(edges.len(), 5, "{edges:?}");
    // Edges are proven facts the index accepts.
    let mut with = index.clone();
    with.edges.extend(edges);
    trace_core::assemble::sort_edges(&mut with.edges);
    with.validate().expect("valid index");
}

/// Several declarations answer the path (two `lib` package directories), a library
/// module, a wildcard import, a default import and a module import: no edge.
#[test]
fn rule_import_of_ambiguous_path_is_nothing() {
    let mut fx = Fixture::new();
    let one = fx.blind_file("a/lib/One.java", Language::Java, None);
    let two = fx.blind_file("b/lib/Two.java", Language::Java, None);
    let main_java = fx.blind_file("app/Main.java", Language::Java, None);
    fx.decl(one, Decl::function("format"));
    fx.decl(two, Decl::function("format"));
    fx.module(main_java);
    let views = fx.blind_file("pkg/views.py", Language::Python, None);
    let json = fx.blind_file("pkg/json.py", Language::Python, None);
    fx.decl(json, Decl::function("dumps"));
    fx.module(views);
    let m = fx.blind_file("web/m.ts", Language::TypeScript, None);
    let main_ts = fx.blind_file("web/main.ts", Language::TypeScript, None);
    fx.decl(m, Decl::function("f"));
    fx.module(main_ts);
    let mut index = fx.build();
    add_import(&mut index, "app/Main.java", "format", "lib.format", ImportKind::Member, 10);
    add_import(&mut index, "pkg/views.py", "dumps", "json.dumps", ImportKind::Member, 20);
    add_import(&mut index, "pkg/views.py", "*", ".json", ImportKind::Wildcard, 30);
    add_import(&mut index, "web/main.ts", "f", "./m.default", ImportKind::Member, 40);
    add_import(&mut index, "web/main.ts", "m", "./m", ImportKind::Module, 50);
    let edges = import_path_edges(&index);
    assert!(edges.is_empty(), "{edges:?}");
}

/// A server already proved the import at that span: the rule adds nothing.
#[test]
fn rule_server_import_edge_is_not_duplicated() {
    let mut fx = Fixture::new();
    let helpers = fx.file("pkg/helpers.py", Language::Python, None);
    let views = fx.file("pkg/views.py", Language::Python, None);
    let redirect = fx.decl(helpers, Decl::function("redirect"));
    let top = fx.module(views);
    let span = ByteSpan::new(10, 18);
    fx.edge(views, top, redirect, EdgeKind::Imports, span, 1);
    let mut index = fx.build();
    let facts = facts_mut(&mut index, "pkg/views.py");
    facts.imports.push(Import {
        local: "redirect".into(),
        target: ".helpers.redirect".into(),
        kind: ImportKind::Member,
        scope: Scope::Module,
        span,
        line: 1,
    });
    facts.references.push(Reference {
        span,
        name: "redirect".into(),
        owner: None,
        in_decorator: false,
        local: false,
        kind: RefKind::Import,
    });
    assert!(index
        .edges
        .iter()
        .any(|e| e.kind == EdgeKind::Imports && e.at.bytes == span));
    assert!(import_path_edges(&index).is_empty());
    // Without the server edge the rule proves it.
    let mut bare = index.clone();
    bare.edges.retain(|e| e.kind != EdgeKind::Imports);
    let edges = import_path_edges(&bare);
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].to, fx.id(redirect));
}

#[test]
fn last_names_of_every_path_form() {
    assert_eq!(last_name("a.b.c"), "c");
    assert_eq!(last_name("crate::a::f"), "f");
    assert_eq!(last_name("App\\Models\\User"), "User");
    assert_eq!(last_name("./m.f"), "f");
    assert_eq!(last_name("x"), "x");
}

/// PLAN decision 13: incremental import-path edges equal the full rule after an edit that
/// makes one import ambiguous (a second `render` declared in the imported module's
/// package is not read) and one unique (the imported file gains the declaration), with
/// an unrelated file keeping its previous edges.
#[test]
fn rule_incremental_import_edges_equal_full() {
    let build = |edited: bool| {
        let mut fx = Fixture::new();
        let helpers = fx.blind_file("pkg/helpers.py", Language::Python, None);
        let views = fx.blind_file("pkg/views.py", Language::Python, None);
        let other = fx.blind_file("pkg/other.py", Language::Python, None);
        let tools = fx.blind_file("tools/run.py", Language::Python, None);
        fx.decl(helpers, Decl::function("redirect"));
        if edited {
            fx.decl(helpers, Decl::function("render"));
        }
        fx.decl(other, Decl::function("main"));
        fx.decl(tools, Decl::function("cli"));
        module(&mut fx, views);
        module(&mut fx, tools);
        let mut index = fx.build();
        add_import(&mut index, "pkg/views.py", "redirect", ".helpers.redirect", ImportKind::Member, 10);
        add_import(&mut index, "pkg/views.py", "render", ".helpers.render", ImportKind::Member, 30);
        add_import(&mut index, "tools/run.py", "main", "pkg.other.main", ImportKind::Member, 10);
        index
    };
    let before = build(false);
    let after = build(true);
    let by_uid: std::collections::HashMap<&str, SymbolId> =
        after.symbols.iter().map(|s| (s.uid.as_str(), s.id)).collect();
    let prev: Vec<Edge> = import_path_edges(&before)
        .into_iter()
        .filter_map(|mut e| {
            e.from = *by_uid.get(before.symbol(e.from).uid.as_str())?;
            e.to = *by_uid.get(before.symbol(e.to).uid.as_str())?;
            e.at.file = after.file_by_path(before.file_path(e.at.file))?;
            Some(e)
        })
        .collect();
    let delta = trace_core::delta::IndexDelta {
        modified: ["pkg/helpers.py".to_string()].into_iter().collect(),
        ..Default::default()
    };
    let key = |edges: &[Edge]| {
        let mut v: Vec<(u32, u32, u32, u32, String)> = edges
            .iter()
            .map(|e| (e.at.file.0, e.at.bytes.start, e.from.0, e.to.0, e.kind.as_str().to_string()))
            .collect();
        v.sort();
        v
    };
    let full = import_path_edges(&after);
    let incremental = import_path_edges_delta(&after, &delta, &prev);
    assert_eq!(key(&incremental), key(&full));
    // The edit made `.helpers.render` name exactly one declaration.
    let render = ByteSpan::new(30, 36);
    assert!(edge_at(&full, &after, "pkg/views.py", render).is_some(), "{full:?}");
    assert!(edge_at(&import_path_edges(&before), &before, "pkg/views.py", render).is_none());
}
