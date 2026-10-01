//! Tests for [`crate::flow::js_objects`]: CommonJS module values and the prototype built-ins of the
//! JavaScript object model in the repository value flow (facts lowered by hand).

use trace_core::facts::{BindTarget, Expr, Import, ImportKind, Scope};
use trace_core::model::SiteOperation;
use trace_core::{ByteSpan, Index, Language};
use trace_syntax::lower::LITERAL_PREFIX;

use crate::flow::{Flow, FlowCandidate};
use crate::hierarchy::Hierarchy;
use crate::test_support::{Decl, Fixture, D};

fn solve(index: &Index) -> Vec<FlowCandidate> {
    let h = Hierarchy::build(index);
    Flow::solve(index, &h).candidates()
}

fn targets(cands: &[FlowCandidate], at: ByteSpan) -> Vec<trace_core::SymbolId> {
    cands
        .iter()
        .filter(|c| c.span == at && c.operation == SiteOperation::Call && c.via.is_none())
        .flat_map(|c| c.candidates.iter().copied())
        .collect()
}

fn module_bind(fx: &mut Fixture, file: usize, target: BindTarget, value: Expr) {
    fx.bind(file, target, value, Scope::Module);
}

/// `module.exports` as a bind target.
fn module_exports(fx: &mut Fixture) -> BindTarget {
    BindTarget::FieldOf {
        object: fx.name("module"),
        name: "exports".into(),
    }
}

/// `require('<lit>')` (the literal text is left out of flow facts); returns the call.
fn require(fx: &mut Fixture) -> Expr {
    let loader = fx.name("require");
    let literal = fx.name(LITERAL_PREFIX);
    fx.call(loader, vec![literal])
}

/// `var <local> = require(..)` at module level; returns the call span (the import binding
/// the extractor records covers it).
fn bind_required(fx: &mut Fixture, file: usize, local: &str) -> ByteSpan {
    let call = require(fx);
    let span = call.span().expect("call span");
    module_bind(
        fx,
        file,
        BindTarget::Var {
            scope: Scope::Module,
            name: local.into(),
        },
        call,
    );
    span
}

/// Facts of the file at `path` (the built index orders files by path).
fn facts_of<'i>(index: &'i mut Index, path: &str) -> &'i mut trace_core::facts::FileFacts {
    let file = index.files.iter().position(|f| f.path == path).expect("file");
    index.files[file].facts.as_mut().expect("facts")
}

fn import(index: &mut Index, path: &str, local: &str, target: &str, span: ByteSpan) {
    let facts = facts_of(index, path);
    facts.imports.push(Import {
        local: local.into(),
        target: target.into(),
        kind: ImportKind::Module,
        scope: Scope::Module,
        span,
        line: 1,
    });
}

/// `main: <local>.<member>()` (no member: `<local>()`); returns the callee span.
fn call_in(fx: &mut Fixture, file: usize, main: D, local: &str, member: Option<&str>) -> ByteSpan {
    let object = fx.name(local);
    let func = match member {
        Some(m) => fx.attr(object, m),
        None => object,
    };
    let call = fx.call(func, vec![]);
    fx.eval(file, main, call, local)
}

/// Module rules: a relative `require` names the loaded file's module value - what it binds
/// to `module.exports` (a function), else its implicit `exports` object with the
/// `exports.x = v` stores; a whole-module re-export (`module.exports = require('./x')`)
/// passes the loaded value on; a directory specifier loads its `index` file. A package
/// specifier, or a specifier naming no repository file, is no module value.
#[test]
fn rule_js_relative_require_is_the_loaded_module_value() {
    let mut fx = Fixture::new();
    let helper_file = fx.file("lib/helper.js", Language::JavaScript, None);
    let helper = fx.decl(helper_file, Decl::function("helper"));
    let value = fx.name_ref(helper_file, "helper", helper);
    let target = module_exports(&mut fx);
    module_bind(&mut fx, helper_file, target, value);

    let util_file = fx.file("lib/util.js", Language::JavaScript, None);
    let fmt = fx.decl(util_file, Decl::function("fmt"));
    let value = fx.name_ref(util_file, "fmt", fmt);
    let target = BindTarget::FieldOf {
        object: fx.name("exports"),
        name: "fmt".into(),
    };
    module_bind(&mut fx, util_file, target, value);

    // lib/index.js: module.exports = require('./helper')
    let index_file = fx.file("lib/index.js", Language::JavaScript, None);
    let reexport = require(&mut fx);
    let reexport_at = reexport.span().expect("call span");
    let target = module_exports(&mut fx);
    module_bind(&mut fx, index_file, target, reexport);

    let main_file = fx.file("app/main.js", Language::JavaScript, None);
    let main = fx.decl(main_file, Decl::function("main"));
    let mut loads = Vec::new();
    for (local, spec) in [
        ("h", "../lib/helper"),
        ("u", "../lib/util.js"),
        ("d", "../lib"),
        ("p", "helper"),
        ("m", "../lib/missing"),
    ] {
        loads.push((local, spec, bind_required(&mut fx, main_file, local)));
    }
    let h_at = call_in(&mut fx, main_file, main, "h", None);
    let u_at = call_in(&mut fx, main_file, main, "u", Some("fmt"));
    let d_at = call_in(&mut fx, main_file, main, "d", None);
    let p_at = call_in(&mut fx, main_file, main, "p", None);
    let m_at = call_in(&mut fx, main_file, main, "m", None);

    let mut index = fx.build();
    for (local, spec, at) in loads {
        import(&mut index, "app/main.js", local, spec, at);
    }
    facts_of(&mut index, "lib/index.js")
        .exports
        .push(trace_core::facts::Export {
            exported: "*".into(),
            target: "./helper".into(),
            span: reexport_at,
            line: 1,
        });
    let cands = solve(&index);
    assert_eq!(targets(&cands, h_at), vec![fx.id(helper)], "{cands:?}");
    assert_eq!(targets(&cands, u_at), vec![fx.id(fmt)]);
    assert_eq!(targets(&cands, d_at), vec![fx.id(helper)], "directory index, re-exported");
    assert!(targets(&cands, p_at).is_empty(), "package specifier");
    assert!(targets(&cands, m_at).is_empty(), "no repository file");
}

/// The same `require` in a file of another language, or a `require` the server resolved to a
/// repository declaration (a local loader function), loads nothing.
#[test]
fn rule_js_require_applies_to_unbound_javascript_loaders_only() {
    let mut fx = Fixture::new();
    let helper_file = fx.file("lib/helper.js", Language::JavaScript, None);
    let helper = fx.decl(helper_file, Decl::function("helper"));
    let value = fx.name_ref(helper_file, "helper", helper);
    let target = module_exports(&mut fx);
    module_bind(&mut fx, helper_file, target, value);

    let main_file = fx.file("lib/main.js", Language::JavaScript, None);
    let main = fx.decl(main_file, Decl::function("main"));
    let own_loader = fx.decl(main_file, Decl::function("require"));
    // var h = require('./helper') where `require` is the file's own function.
    let loader = fx.name_ref(main_file, "require", own_loader);
    let literal = fx.name(LITERAL_PREFIX);
    let call = fx.call(loader, vec![literal]);
    let load_at = call.span().expect("call span");
    module_bind(
        &mut fx,
        main_file,
        BindTarget::Var {
            scope: Scope::Module,
            name: "h".into(),
        },
        call,
    );
    let h_at = call_in(&mut fx, main_file, main, "h", None);

    let ts_file = fx.file("lib/other.ts", Language::TypeScript, None);
    let ts_main = fx.decl(ts_file, Decl::function("main"));
    let ts_load = bind_required(&mut fx, ts_file, "h");
    let ts_at = call_in(&mut fx, ts_file, ts_main, "h", None);

    let mut index = fx.build();
    import(&mut index, "lib/main.js", "h", "./helper", load_at);
    import(&mut index, "lib/other.ts", "h", "./helper", ts_load);
    let cands = solve(&index);
    assert!(targets(&cands, h_at).is_empty(), "{cands:?}");
    assert!(targets(&cands, ts_at).is_empty());
}

/// Prototype built-ins are language rules of the object model: `Object.create(proto)` and
/// `Object.setPrototypeOf(o, proto)` link member lookups to `proto` without library
/// knowledge, unless the spelling's root is a repository binding.
#[test]
fn rule_js_prototype_builtins_link_unless_rebound() {
    let mut fx = Fixture::new();
    let f = fx.file("lib/proto.js", Language::JavaScript, None);
    let get = fx.decl(f, Decl::function("get"));
    let main = fx.decl(f, Decl::function("main"));
    let shadow = fx.decl(f, Decl::function("Object"));
    let member = fx.name_ref(f, "get", get);
    let object = fx.name(trace_syntax::lower::OBJECT_CALLEE);
    let literal = fx.call_kw(object, vec![], vec![("get", member)]);
    module_bind(
        &mut fx,
        f,
        BindTarget::Var {
            scope: Scope::Module,
            name: "proto".into(),
        },
        literal,
    );
    let mut ats = Vec::new();
    for (name, rebound) in [("a", false), ("b", true)] {
        let root = if rebound {
            fx.name_ref(f, "Object", shadow)
        } else {
            fx.name("Object")
        };
        let create = fx.attr(root, "create");
        let source = fx.name("proto");
        let made = fx.call(create, vec![source]);
        fx.bind(
            f,
            BindTarget::Var {
                scope: Scope::Decl(main.decl),
                name: name.into(),
            },
            made,
            Scope::Decl(main.decl),
        );
        ats.push(call_in(&mut fx, f, main, name, Some("get")));
    }
    let index = fx.build();
    let cands = solve(&index);
    assert_eq!(targets(&cands, ats[0]), vec![fx.id(get)], "{cands:?}");
    assert!(targets(&cands, ats[1]).is_empty(), "a repository `Object` is no built-in");
}
