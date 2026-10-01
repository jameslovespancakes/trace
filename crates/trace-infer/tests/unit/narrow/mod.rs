//! Tests for [`crate::narrow`] (syntax facts built by hand; nothing is read or executed).

use super::*;
use crate::decide::decide;
use crate::test_support::{Decl, Fixture, D};
use trace_core::facts::{BindTarget, Expr, Scope};
use trace_core::facts::{ImportKind, ParamKind};
use trace_core::model::{DecisionStatus, Site};
use trace_core::source::SourceStore;
use trace_core::ByteSpan;

use ParamKind::{Positional, VarKeyword, VarPositional};

fn sites(index: &Index) -> Vec<Site> {
    let sources = SourceStore::new(index);
    crate::sites::generate(index, &sources).unwrap()
}

fn site_at<'s>(sites: &'s [Site], fx: &Fixture, at: trace_core::ByteSpan) -> Option<&'s Site> {
    let _ = fx;
    sites.iter().find(|s| s.at.bytes == at)
}

fn qnames(index: &Index, ids: &[SymbolId]) -> Vec<String> {
    ids.iter()
        .map(|&s| {
            let sym = index.symbol(s);
            format!("{}:{}", index.file_path(sym.file), sym.qualified_name)
        })
        .collect()
}

/// Go: a bare call never reaches methods, and a free function of another package is weak
/// evidence (possible, never decided alone).
#[test]
fn go_bare_calls() {
    let mut fx = Fixture::new();
    let main = fx.blind_file("cmd/main.go", Language::Go, None);
    let helpers = fx.blind_file("helpers/helpers.go", Language::Go, None);
    let run = fx.decl(main, Decl::function("run"));
    let t = fx.decl(helpers, Decl::class("Tool"));
    let tool_cleanup = fx.decl(helpers, Decl::function("cleanup").container("Tool"));
    let cleanup = fx.decl(helpers, Decl::function("cleanup"));
    let bare = fx.call_n(main, Some(run), "cleanup", 0);
    let index = fx.build();
    let all = sites(&index);
    let b = site_at(&all, &fx, bare).unwrap();
    assert_eq!(b.candidates, vec![fx.id(cleanup)], "methods never answer a bare call");
    assert_eq!(b.field_only, vec![fx.id(cleanup)], "another package: weak evidence");
    let _ = (t, tool_cleanup);
    // A unique but weak survivor is not decided.
    let mut index = index;
    let i = all.iter().position(|x| x.at.bytes == bare).unwrap();
    index.sites = all;
    assert_eq!(decide(&index)[i].status, DecisionStatus::Unknown);
}

/// gson: a bare call only reaches methods of the enclosing class family (implicit
/// receiver); a method of an unrelated class is dropped.
#[test]
fn java_bare_calls_reach_the_implicit_receiver_family() {
    let mut fx = Fixture::new();
    let g = fx.blind_file("gson/Other.java", Language::Java, None);
    let c = fx.blind_file("gson/Caller.java", Language::Java, None);
    fx.import(c, "List", "java.util.List", ImportKind::Member);
    let other = fx.decl(g, Decl::class("Other"));
    let other_helper = fx.decl(g, Decl::method("helper", other));
    let caller = fx.decl(c, Decl::class("Caller").bases(&["Base"]));
    let helper = fx.decl(c, Decl::method("helper", caller));
    let run = fx.decl(c, Decl::method("run", caller).param_specs(&[("writer", Positional, false)]));
    let bare = fx.call_n(c, Some(run), "helper", 0);
    let index = fx.build();
    let all = sites(&index);
    assert_eq!(site_at(&all, &fx, bare).unwrap().candidates, vec![fx.id(helper)]);
    let _ = (other_helper, VarPositional);
}

/// Java files without any import facts may use static imports: bare calls are not
/// narrowed to the class family there.
#[test]
fn java_without_import_facts_keeps_bare_candidates() {
    let mut fx = Fixture::new();
    let c = fx.blind_file("Caller.java", Language::Java, None);
    let g = fx.blind_file("Asserts.java", Language::Java, None);
    let caller = fx.decl(c, Decl::class("Caller"));
    let run = fx.decl(c, Decl::method("run", caller));
    let asserts = fx.decl(g, Decl::class("Asserts"));
    let check = fx.decl(g, Decl::method("check", asserts));
    let bare = fx.call_n(c, Some(run), "check", 0);
    let index = fx.build();
    let all = sites(&index);
    assert_eq!(site_at(&all, &fx, bare).unwrap().candidates, vec![fx.id(check)]);
}

/// Item 20 + visibility: `def joint(self, **joint): joint(...)` — the parameter shadows the
/// name: no method candidate, and a same-named function of another file is not visible
/// either (the parameter holds whatever value flow delivers).
#[test]
fn parameters_shadowing_a_method_name_produce_no_method_candidates() {
    let mut fx = Fixture::new();
    let f = fx.file("pkg/model.py", Language::Python, None);
    let g = fx.file("pkg/other.py", Language::Python, None);
    let model = fx.decl(f, Decl::class("Model"));
    let joint = fx.decl(
        f,
        Decl::method("joint", model)
            .param_specs(&[("self", Positional, false), ("joint", VarKeyword, false)]),
    );
    let sub = fx.decl(f, Decl::class("Sub").bases(&["Model"]));
    let sub_joint = fx.decl(f, Decl::method("joint", sub));
    let free = fx.decl(g, Decl::function("joint"));
    let at = fx.call_n(f, Some(joint), "joint", 1);
    fx.unresolved(f, joint, at, 1, "joint");
    let index = fx.build();
    let all = sites(&index);
    assert!(site_at(&all, &fx, at).is_none(), "nothing visible by name");
    let h = Hierarchy::build(&index);
    let u = index.unresolved.iter().find(|u| u.at.bytes == at).expect("unknown");
    let pool = h.functions.get("joint").unwrap().clone();
    let n = narrow_candidates(&index, &h, u, fx.id(joint), "joint", &pool, 21);
    assert!(n.dropped_by_scope.contains(&fx.id(sub_joint)));
    assert_eq!(n.dropped_by_visibility, vec![fx.id(free)]);
    assert!(n.kept.is_empty());
}

/// TS `errorHandler` rule (a): a local binding shadowing the name (`const [, errorHandler]
/// = ...; errorHandler(e)`) never reaches a same-named declaration of another module, and a
/// non-imported top-level binding of another ES module is not visible either (it stays
/// weak only for script files without imports / exports).
#[test]
fn rule_non_exported_binding_is_not_a_candidate_in_other_files() {
    let mut fx = Fixture::new();
    let base = fx.blind_file("src/hono-base.ts", Language::TypeScript, None);
    let render = fx.blind_file("src/jsx/render.ts", Language::TypeScript, None);
    let script = fx.blind_file("www/legacy.js", Language::JavaScript, None);
    let other_script = fx.blind_file("www/app.js", Language::JavaScript, None);
    fx.import(base, "compose", "./compose.compose", ImportKind::Member);
    fx.import(render, "h", "../h.h", ImportKind::Member);
    let handler = fx.decl(base, Decl::function("errorHandler").params(&["err", "c"]));
    let apply = fx.decl(render, Decl::function("apply").params(&["node"]));
    let run = fx.decl(render, Decl::function("run"));
    // apply: a destructured local `errorHandler` called (the syntax proves the binding).
    let local_call = fx.call_n(render, Some(apply), "errorHandler", 2);
    // run: a bare call of a name render.ts never imports.
    let bare = fx.call_n(render, Some(run), "errorHandler", 2);
    // Script files share one scope: the candidate stays, weak.
    let legacy = fx.decl(script, Decl::function("draw"));
    let main = fx.decl(other_script, Decl::function("main"));
    let script_call = fx.call_n(other_script, Some(main), "draw", 0);
    let mut index = fx.build();
    add_local(&mut index, "src/jsx/render.ts", local_call);
    let all = sites(&index);
    assert!(site_at(&all, &fx, local_call).is_none(), "local binding: nothing visible");
    assert!(site_at(&all, &fx, bare).is_none(), "not imported from an ES module");
    let s = site_at(&all, &fx, script_call).expect("script site");
    assert_eq!(s.candidates, vec![fx.id(legacy)]);
    assert_eq!(s.field_only, vec![fx.id(legacy)]);
    let _ = handler;
}

/// Item 20: a bare call reaches a same-named function of another file only through an
/// import; other same-named functions are weak (possible, never decided alone). Python bare
/// names never reach methods.
#[test]
fn same_named_functions_in_other_files_need_an_import() {
    let mut fx = Fixture::new();
    let a = fx.file("pkg/a.py", Language::Python, None);
    let b = fx.file("pkg/b.py", Language::Python, None);
    let c = fx.file("pkg/c.py", Language::Python, None);
    fx.import(a, "helper", "pkg.b.helper", ImportKind::Member);
    let main = fx.decl(a, Decl::function("main"));
    let helper_b = fx.decl(b, Decl::function("helper"));
    let helper_c = fx.decl(c, Decl::function("helper"));
    let k = fx.decl(c, Decl::class("K"));
    let helper_m = fx.decl(c, Decl::method("helper", k));
    let at = fx.call_n(a, Some(main), "helper", 0);
    fx.unresolved(a, main, at, 1, "helper");
    let index = fx.build();
    let all = sites(&index);
    let s = site_at(&all, &fx, at).unwrap();
    assert_eq!(s.candidates, vec![fx.id(helper_b), fx.id(helper_c)]);
    assert_eq!(s.field_only, vec![fx.id(helper_c)]);
    assert!(!s.candidates.contains(&fx.id(helper_m)));
}

/// An import of a barrel module reaches the function it re-exports (`export { helper } from
/// './b'`): one re-export hop, so the re-exported file is strong evidence and a same-named
/// function elsewhere stays weak.
#[test]
fn imports_follow_one_reexport_hop() {
    let mut fx = Fixture::new();
    let main = fx.blind_file("src/main.ts", Language::TypeScript, None);
    let barrel = fx.blind_file("src/lib/index.ts", Language::TypeScript, None);
    let b = fx.blind_file("src/lib/b.ts", Language::TypeScript, None);
    let c = fx.blind_file("src/other/c.ts", Language::TypeScript, None);
    fx.import(main, "helper", "./lib.helper", ImportKind::Member);
    fx.export(barrel, "helper", "./b.helper");
    let run = fx.decl(main, Decl::function("run"));
    let helper_b = fx.decl(b, Decl::function("helper"));
    let helper_c = fx.decl(c, Decl::function("helper"));
    let at = fx.call_n(main, Some(run), "helper", 0);
    let index = fx.build();
    let all = sites(&index);
    let s = site_at(&all, &fx, at).expect("site");
    assert!(s.candidates.contains(&fx.id(helper_b)), "{:?}", qnames(&index, &s.candidates));
    assert!(!s.field_only.contains(&fx.id(helper_b)), "re-exported: strong evidence");
    assert!(
        !s.candidates.contains(&fx.id(helper_c)) || s.field_only.contains(&fx.id(helper_c)),
        "a same-named function outside the import is never strong"
    );
}

/// Python class-body code may call a method of its own class by bare name.
#[test]
fn class_body_calls_reach_their_own_methods() {
    let mut fx = Fixture::new();
    let f = fx.file("m.py", Language::Python, None);
    let k = fx.decl(f, Decl::class("K"));
    let make = fx.decl(f, Decl::method("make", k));
    let run = fx.decl(f, Decl::function("run"));
    let module = fx.module(f);
    let at = fx.call_n(f, None, "make", 0);
    fx.lexical_owner(f, k);
    fx.unresolved(f, module, at, 1, "make");
    let at2 = fx.call_n(f, Some(run), "make", 0);
    fx.unresolved(f, run, at2, 1, "make");
    let index = fx.build();
    let all = sites(&index);
    assert_eq!(site_at(&all, &fx, at).unwrap().candidates, vec![fx.id(make)]);
    assert!(site_at(&all, &fx, at2).is_none(), "outside the class body: no method");
}

/// Nested functions are visible to bare calls only inside their enclosing function.
#[test]
fn nested_functions_are_lexically_scoped() {
    let mut fx = Fixture::new();
    let f = fx.file("m.py", Language::Python, None);
    let outer = fx.decl(f, Decl::function("outer"));
    let inner = fx.decl(f, Decl::nested("step", outer));
    let top = fx.decl(f, Decl::function("step"));
    let other = fx.decl(f, Decl::function("other"));
    let inside = fx.call_n(f, Some(outer), "step", 0);
    fx.unresolved(f, outer, inside, 1, "step");
    let outside = fx.call_n(f, Some(other), "step", 0);
    fx.unresolved(f, other, outside, 1, "step");
    let index = fx.build();
    let all = sites(&index);
    let a = site_at(&all, &fx, inside).unwrap();
    assert!(a.candidates.contains(&fx.id(inner)) && a.candidates.contains(&fx.id(top)));
    assert_eq!(site_at(&all, &fx, outside).unwrap().candidates, vec![fx.id(top)]);
}

#[test]
fn module_paths_resolve_by_language_rules() {
    let mut fx = Fixture::new();
    for (p, l) in [
        ("src/flask/json/provider.py", Language::Python),
        ("src/flask/json/__init__.py", Language::Python),
        ("src/app/util.ts", Language::TypeScript),
        ("src/app/lib/index.ts", Language::TypeScript),
        ("render/json.go", Language::Go),
    ] {
        fx.file(p, l, None);
    }
    let index = fx.build();
    let m = ModuleMap::new(&index);
    let path = |id: FileId| index.file_path(id).to_string();
    let (files, whole) = m
        .resolve("tests/test_json.py", Language::Python, "flask.json.provider.DefaultJSONProvider", true)
        .unwrap();
    assert_eq!(files.iter().map(|f| path(*f)).collect::<Vec<_>>(), vec!["src/flask/json/provider.py"]);
    assert!(!whole, "the last segment is a member");
    let (files, _) = m
        .resolve("src/flask/json/tag.py", Language::Python, ".provider", false)
        .unwrap();
    assert_eq!(path(files[0]), "src/flask/json/provider.py");
    let (files, _) = m
        .resolve("src/app/main.ts", Language::TypeScript, "./util.helper", true)
        .unwrap();
    assert_eq!(path(files[0]), "src/app/util.ts");
    let (files, _) = m
        .resolve("src/app/main.ts", Language::TypeScript, "./lib", false)
        .unwrap();
    assert!(files.iter().any(|f| path(*f) == "src/app/lib/index.ts"));
    let (files, _) = m
        .resolve("cmd/main.go", Language::Go, "\"github.com/acme/app/render\"", false)
        .unwrap();
    assert_eq!(path(files[0]), "render/json.go");
    assert!(m.resolve("a.py", Language::Python, "json.dumps", true).is_none());
    assert!(m
        .resolve("a.ts", Language::TypeScript, "react.useState", true)
        .is_none());
}

/// Bash: a script that defines `nvm_download` itself and sources nothing runs its own
/// definition; a script without its own definition keeps the other file's function.
#[test]
fn bash_own_definition_shadows_other_files() {
    let mut fx = Fixture::new();
    let nvm = fx.blind_file("nvm.sh", Language::Bash, None);
    let install = fx.blind_file("install.sh", Language::Bash, None);
    let mocks = fx.blind_file("update_test_mocks.sh", Language::Bash, None);
    let lib = fx.decl(nvm, Decl::function("nvm_download"));
    let own = fx.decl(install, Decl::function("nvm_download"));
    let main = fx.decl(install, Decl::function("install_nvm_as_script"));
    let in_install = fx.call_n(install, Some(main), "nvm_download", 2);
    let top = fx.module(mocks);
    let in_mocks = fx.call_n(mocks, Some(top), "nvm_download", 2);
    let index = fx.build();
    let all = sites(&index);
    let s = site_at(&all, &fx, in_install).expect("site");
    assert_eq!(s.candidates, vec![fx.id(own)]);
    let m = site_at(&all, &fx, in_mocks).expect("site");
    assert_eq!(qnames(&index, &m.candidates), vec!["install.sh:nvm_download", "nvm.sh:nvm_download"]);
    let _ = lib;
}

/// Bash: a script that sources `nvm.sh` (here through `"$NVM_DIR/nvm.sh"`, whose literal tail
/// trace-syntax keeps) sees that file's function, never install.sh's copy; an unresolvable
/// `source` keeps every candidate.
#[test]
fn bash_sourced_files_limit_the_candidates() {
    let mut fx = Fixture::new();
    let nvm = fx.blind_file("nvm.sh", Language::Bash, None);
    let install = fx.blind_file("install.sh", Language::Bash, None);
    let mocks = fx.blind_file("update_test_mocks.sh", Language::Bash, None);
    let other = fx.blind_file("scripts/other.sh", Language::Bash, None);
    let lib = fx.decl(nvm, Decl::function("nvm_download"));
    let _copy = fx.decl(install, Decl::function("nvm_download"));
    fx.import(mocks, "*", "nvm.sh", ImportKind::Wildcard);
    fx.import(other, "*", "/etc/profile", ImportKind::Wildcard);
    let top = fx.module(mocks);
    let in_mocks = fx.call_n(mocks, Some(top), "nvm_download", 2);
    let top2 = fx.module(other);
    let in_other = fx.call_n(other, Some(top2), "nvm_download", 2);
    let index = fx.build();
    let all = sites(&index);
    let m = site_at(&all, &fx, in_mocks)
        .map(|s| s.candidates.clone())
        .unwrap_or_default();
    assert_eq!(m, vec![fx.id(lib)], "only the sourced file's function");
    let o = site_at(&all, &fx, in_other)
        .map(|s| s.candidates.clone())
        .unwrap_or_default();
    assert_eq!(o.len(), 2, "an unresolvable source keeps every candidate");
}

/// C: a call matching a prototype and its linked definition has one candidate (the
/// definition), decided as a unique candidate.
#[test]
fn c_prototype_and_definition_collapse() {
    let mut fx = Fixture::new();
    let h = fx.blind_file("src/bytecode.h", Language::C, None);
    let c = fx.blind_file("src/bytecode.c", Language::C, None);
    let u = fx.blind_file("src/compile.c", Language::C, None);
    let proto = fx.decl(h, Decl::function("opcode_describe").stub().params(&["op"]));
    let def = fx.decl(c, Decl::function("opcode_describe").params(&["op"]));
    let gen = fx.decl(u, Decl::function("gen_op"));
    let at = fx.call_n(u, Some(gen), "opcode_describe", 1);
    let mut index = fx.build();
    let family = crate::family::family_edges(&index);
    index.edges.extend(family);
    trace_core::assemble::sort_edges(&mut index.edges);
    let all = sites(&index);
    let s = site_at(&all, &fx, at).expect("site");
    assert_eq!(s.candidates, vec![fx.id(def)]);
    let _ = proto;
}

/// A JavaScript call never reaches a Rust method (or a Python function) by name: the only
/// connection across that boundary is a bridge. JS -> TS and Scala -> Java stay candidates
/// (one module / class namespace).
#[test]
fn name_pools_stay_within_one_language_namespace() {
    let mut fx = Fixture::new();
    let js = fx.blind_file("www/index.js", Language::JavaScript, None);
    let ts = fx.blind_file("www/util.ts", Language::TypeScript, None);
    let rs = fx.blind_file("src/lib.rs", Language::Rust, None);
    let py = fx.blind_file("tools/sim.py", Language::Python, None);
    let universe = fx.decl(rs, Decl::class("Universe"));
    let rs_tick = fx.decl(rs, Decl::method("tick", universe));
    let board = fx.decl(ts, Decl::class("Board"));
    let ts_tick = fx.decl(ts, Decl::method("tick", board));
    let sim = fx.decl(py, Decl::class("Sim"));
    let py_tick = fx.decl(py, Decl::method("tick", sim));
    let render = fx.decl(js, Decl::function("renderLoop"));
    let at = fx.call_n(js, Some(render), "universe.tick", 0);
    let index = fx.build();
    let all = sites(&index);
    let s = site_at(&all, &fx, at).expect("site");
    assert_eq!(s.candidates, vec![fx.id(ts_tick)]);
    let _ = (rs_tick, py_tick);
    assert!(name_interop(Language::Scala, Language::Java));
    assert!(name_interop(Language::Cpp, Language::C));
    assert!(!name_interop(Language::JavaScript, Language::Rust));
    assert!(!name_interop(Language::Python, Language::Cpp));
}

/// An allocation expression as the receiver (`new K(..)` without arguments here).
fn allocation(fx: &mut Fixture, class: &str) -> Expr {
    let f = fx.name(class);
    match fx.call(f, Vec::new()) {
        Expr::Call {
            func,
            func_span,
            args,
            kwargs,
            span,
            ..
        } => Expr::Call {
            func,
            func_span,
            args,
            kwargs,
            span,
            is_new: true,
        },
        other => other,
    }
}

/// go-sqlite3 `error.go`: `Error{Code: ..}.Error()` in a cgo file the language server could
/// not type (a blind call of a *semantic* file). The composite literal names the receiver's
/// type, so only `Error.Error` remains; a plain receiver keeps both candidates, and a
/// literal whose type declares no candidate (a promoted method) drops nothing.
#[test]
fn allocation_receivers_narrow_every_blind_call() {
    let mut fx = Fixture::new();
    let f = fx.file("error.go", Language::Go, None);
    let errno = fx.decl(f, Decl::class("ErrNo"));
    let errno_error = fx.decl(f, Decl::function("Error").container("ErrNo"));
    let ext = fx.decl(f, Decl::class("ErrNoExtended"));
    let ext_error = fx.decl(f, Decl::function("Error").container("ErrNoExtended"));
    let error = fx.decl(f, Decl::class("Error"));
    let error_error = fx.decl(f, Decl::function("Error").container("Error"));
    let wrapper = fx.decl(f, Decl::class("Wrapper"));
    let literal = fx.call_n(f, Some(ext_error), "Error{Code: ErrNo(err)}.Error", 0);
    fx.unresolved(f, ext_error, literal, 1, "Error{Code: ErrNo(err)}.Error");
    let plain = fx.call_n(f, Some(ext_error), "e.Error", 0);
    fx.unresolved(f, ext_error, plain, 1, "e.Error");
    let promoted = fx.call_n(f, Some(ext_error), "Wrapper{}.Error", 0);
    fx.unresolved(f, ext_error, promoted, 1, "Wrapper{}.Error");
    let v = allocation(&mut fx, "Error");
    fx.call_receiver(f, literal, v);
    let v = allocation(&mut fx, "Wrapper");
    fx.call_receiver(f, promoted, v);
    let index = fx.build();
    let all = sites(&index);
    let ids = |at| {
        site_at(&all, &fx, at)
            .map(|s| s.candidates.clone())
            .unwrap_or_default()
    };
    assert_eq!(ids(literal), vec![fx.id(error_error)]);
    assert_eq!(ids(plain).len(), 2, "ErrNo.Error and Error.Error");
    assert_eq!(ids(promoted).len(), 2, "no candidate of Wrapper: nothing dropped");
    let _ = (errno, errno_error, ext, error, wrapper);
}

/// Java `new Reader().close()`: the run-time class is `Reader` or an anonymous subclass
/// (`new Reader() { .. }`), so another class's `close` is dropped while a subclass override
/// stays.
#[test]
fn allocation_receivers_keep_subclass_overrides() {
    let mut fx = Fixture::new();
    let f = fx.blind_file("io/Reader.java", Language::Java, None);
    let g = fx.blind_file("io/Writer.java", Language::Java, None);
    let u = fx.blind_file("app/Main.java", Language::Java, None);
    let reader = fx.decl(f, Decl::class("Reader"));
    let reader_close = fx.decl(f, Decl::method("close", reader));
    let sub = fx.decl(f, Decl::class("BufferedReader").bases(&["Reader"]));
    let sub_close = fx.decl(f, Decl::method("close", sub));
    let writer = fx.decl(g, Decl::class("Writer"));
    let writer_close = fx.decl(g, Decl::method("close", writer));
    let main = fx.decl(u, Decl::class("Main"));
    let run = fx.decl(u, Decl::method("run", main));
    let at = fx.call_n(u, Some(run), "new Reader().close", 0);
    let v = allocation(&mut fx, "Reader");
    fx.call_receiver(u, at, v);
    let index = fx.build();
    let all = sites(&index);
    let s = site_at(&all, &fx, at).expect("site");
    let mut want = vec![fx.id(reader_close), fx.id(sub_close)];
    want.sort_by_key(|&id| index.symbol(id).uid.clone());
    let mut got = s.candidates.clone();
    got.sort_by_key(|&id| index.symbol(id).uid.clone());
    assert_eq!(got, want);
    let _ = writer_close;
}

// ---- general fixes: rules before anything is possible (SPEC 7.11) ----

use crate::types::{receiver_type, ReceiverType};
use trace_core::facts::{FileFacts, TypeFact, TypeSource, TypeSubject};

/// Post-build facts of `path` (the fixture keeps its facts private; types, local spans and
/// member accesses are added after assembly, which never reads them).
fn facts_of<'x>(index: &'x mut Index, path: &str) -> &'x mut FileFacts {
    let id = index.file_by_path(path).expect("fixture file");
    index.files[id.idx()].facts.as_mut().expect("facts")
}

fn add_type(index: &mut Index, path: &str, subject: TypeSubject, type_name: &str, source: TypeSource) {
    let facts = facts_of(index, path);
    facts.types.push(TypeFact {
        subject,
        type_name: type_name.into(),
        span: ByteSpan::new(0, 0),
        source,
    });
}

fn add_local(index: &mut Index, path: &str, span: ByteSpan) {
    let facts = facts_of(index, path);
    facts.local_spans.push(span);
    facts.local_spans.sort_unstable_by_key(|s| (s.start, s.end));
    facts.local_spans.dedup();
}

fn var(scope: Scope, name: &str) -> TypeSubject {
    TypeSubject::Var {
        scope,
        name: name.into(),
    }
}

/// A call whose callee span has the callee's real length (member spans are derived from
/// it), at byte `at`, owned by `owner` (`None`: module level).
fn call_at(fx: &mut Fixture, file: usize, owner: Option<D>, callee: &str, at: u32) -> ByteSpan {
    let callee_span = ByteSpan::new(at, at + callee.len() as u32);
    let span = ByteSpan::new(at, callee_span.end + 2);
    fx.call_site(file, owner, span, callee_span, callee);
    callee_span
}

/// `name` as a receiver expression at `at`.
fn name_at(name: &str, at: u32) -> Expr {
    Expr::Name {
        name: name.into(),
        span: ByteSpan::new(at, at + name.len() as u32),
    }
}

/// `f()` as a value.
fn call_of(func: Expr, at: u32) -> Expr {
    let func_span = func.span().unwrap_or(ByteSpan::new(at, at + 1));
    Expr::Call {
        func: Box::new(func),
        func_span,
        args: Vec::new(),
        kwargs: Vec::new(),
        span: ByteSpan::new(func_span.start, func_span.end + 2),
        is_new: false,
    }
}

/// Narrow the blind call at `at` with the by-name pool of `member`.
fn narrowed(index: &Index, at: ByteSpan, owner: SymbolId, member: &str) -> Narrowing {
    let h = Hierarchy::build(index);
    let u = index
        .unresolved
        .iter()
        .find(|u| u.at.bytes == at)
        .expect("blind call");
    let pool = h.functions.get(member).cloned().unwrap_or_default();
    narrow_candidates(index, &h, u, owner, member, &pool, 21)
}

/// Rule 10 (package / namespace): an explicit Java import of a static member binds the simple
/// name to that declaration only. Module paths: Rust `crate::` / `super::`, PHP namespaces
/// (PSR-4), Haskell modules and JVM package directories.
#[test]
fn rule_narrowing_package_and_namespace() {
    let mut fx = Fixture::new();
    let lib = fx.blind_file("src/lib/Format.java", Language::Java, None);
    let util = fx.blind_file("src/app/Util.java", Language::Java, None);
    let main = fx.blind_file("src/app/Main.java", Language::Java, None);
    let screen = fx.blind_file("src/feature/Screen.java", Language::Java, None);
    let tool = fx.blind_file("src/tools/Tool.java", Language::Java, None);
    let lib_format = fx.decl(lib, Decl::function("format").params(&["s"]));
    let app_format = fx.decl(util, Decl::function("format").params(&["s"]));
    let run = fx.decl(main, Decl::function("run"));
    let show = fx.decl(screen, Decl::function("show"));
    let work = fx.decl(tool, Decl::function("work"));
    fx.import(screen, "format", "lib.format", ImportKind::Member);
    fx.import(tool, "*", "lib", ImportKind::Wildcard);
    let imported = fx.call_n(screen, Some(show), "format", 1);
    let index = fx.build();
    let all = sites(&index);
    let ids = |at| {
        site_at(&all, &fx, at)
            .map(|s| s.candidates.clone())
            .unwrap_or_default()
    };
    assert_eq!(ids(imported), vec![fx.id(lib_format)], "explicit import");
    let _ = (app_format, run, work, main);

    // Module path rules of the other languages.
    let mut fx = Fixture::new();
    for (p, l) in [
        ("src/lib.rs", Language::Rust),
        ("src/a.rs", Language::Rust),
        ("src/a/b.rs", Language::Rust),
        ("src/c.rs", Language::Rust),
        ("src/Models/User.php", Language::Php),
        ("src/Http/Controller.php", Language::Php),
        ("src/Data/Tree.hs", Language::Haskell),
        ("jv/lib/Models.java", Language::Java),
        ("jv/app/Main.java", Language::Java),
    ] {
        fx.blind_file(p, l, None);
    }
    let index = fx.build();
    let m = ModuleMap::new(&index);
    let paths = |files: &[FileId]| {
        files
            .iter()
            .map(|f| index.file_path(*f).to_string())
            .collect::<Vec<_>>()
    };
    let (files, whole) = m
        .resolve("src/c.rs", Language::Rust, "crate::a::f", true)
        .expect("crate path");
    assert_eq!((paths(&files), whole), (vec!["src/a.rs".to_string()], false));
    let split = m.member_paths("src/c.rs", Language::Rust, "crate::a::f");
    assert!(
        split
            .iter()
            .any(|(f, member)| paths(f) == ["src/a.rs"] && member == "f"),
        "{split:?}"
    );
    let split = m.member_paths("src/a/b.rs", Language::Rust, "super::g");
    assert!(
        split
            .iter()
            .any(|(f, member)| paths(f) == ["src/a.rs"] && member == "g"),
        "{split:?}"
    );
    let split = m.member_paths("src/Http/Controller.php", Language::Php, "App\\Models\\User");
    assert!(
        split
            .iter()
            .any(|(f, member)| paths(f) == ["src/Models/User.php"] && member == "User"),
        "{split:?}"
    );
    let split = m.member_paths("src/Main.hs", Language::Haskell, "Data.Tree.flatten");
    assert!(
        split
            .iter()
            .any(|(f, member)| paths(f) == ["src/Data/Tree.hs"] && member == "flatten"),
        "{split:?}"
    );
    let (files, whole) = m
        .resolve("jv/app/Main.java", Language::Java, "lib.Greeter", true)
        .expect("package");
    assert_eq!((paths(&files), whole), (vec!["jv/lib/Models.java".to_string()], false));
    assert!(m
        .resolve("jv/app/Main.java", Language::Java, "javax.inject.Inject", true)
        .is_none());
}

/// Rule 10 (class member vs local scope): a bare callee that the syntax facts prove to be a
/// local / parameter binding never calls a method, in every language (implicit-receiver
/// languages included); without the local fact the method stays a candidate.
#[test]
fn rule_narrowing_local_shadowing_every_language() {
    // One language per rule variant: locals shadow functions or not (`LanguageRules::
    // locals_shadow_functions`).
    for (language, path) in [(Language::Java, "src/K.java"), (Language::Scala, "src/K.scala")] {
        let mut fx = Fixture::new();
        let f = fx.blind_file(path, language, None);
        let k = fx.decl(f, Decl::class("K"));
        let step = fx.decl(f, Decl::method("step", k));
        let run = fx.decl(f, Decl::method("run", k).span(ByteSpan::new(100, 200)));
        let at = call_at(&mut fx, f, Some(run), "step", 120);
        let mut index = fx.build();
        let control = narrowed(&index, at, fx.id(run), "step");
        assert!(control.kept.contains(&fx.id(step)), "{language:?}: {control:?}");
        add_local(&mut index, path, at);
        let n = narrowed(&index, at, fx.id(run), "step");
        assert!(!n.kept.contains(&fx.id(step)), "{language:?}: {n:?}");
        assert!(n.dropped_by_scope.contains(&fx.id(step)), "{language:?}: {n:?}");
    }
}

/// Rule 11: conflicting facts (two unrelated declared types) prove nothing; related types
/// (a class and its base) stay; an unannotated parameter and an opaque binding are unknown.
#[test]
fn rule_receiver_type_unknown_on_conflict() {
    let mut fx = Fixture::new();
    let lib = fx.blind_file("lib/Types.java", Language::Java, None);
    let app = fx.blind_file("app/Main.java", Language::Java, None);
    fx.decl(lib, Decl::class("Picker"));
    fx.decl(lib, Decl::class("Finder"));
    let base = fx.decl(lib, Decl::class("Base"));
    let sub = fx.decl(lib, Decl::class("Sub").bases(&["Base"]));
    let run = fx.decl(
        app,
        Decl::function("run")
            .params(&["p", "q", "r"])
            .span(ByteSpan::new(100, 300)),
    );
    let member = |fx: &mut Fixture, receiver: &str, at: u32| {
        let callee = format!("{receiver}.go");
        let span = call_at(fx, app, Some(run), &callee, at);
        fx.call_receiver(app, span, name_at(receiver, at));
        ByteSpan::new(span.end - 2, span.end)
    };
    let p = member(&mut fx, "p", 110);
    let q = member(&mut fx, "q", 150);
    let r = member(&mut fx, "r", 190);
    let x = member(&mut fx, "x", 230);
    fx.bind(
        app,
        BindTarget::Var {
            scope: Scope::Decl(run.decl),
            name: "x".into(),
        },
        call_of(name_at("opaque", 220), 220),
        Scope::Decl(run.decl),
    );
    let mut index = fx.build();
    let scope = Scope::Decl(run.decl);
    add_type(&mut index, "app/Main.java", var(scope, "p"), "Picker", TypeSource::Declared);
    add_type(&mut index, "app/Main.java", var(scope, "p"), "Finder", TypeSource::Declared);
    add_type(&mut index, "app/Main.java", var(scope, "q"), "Base", TypeSource::Declared);
    add_type(&mut index, "app/Main.java", var(scope, "q"), "Sub", TypeSource::Declared);
    let h = Hierarchy::build(&index);
    let file = index.file_by_path("app/Main.java").unwrap();
    assert_eq!(receiver_type(&index, &h, file, p), ReceiverType::Unknown, "unrelated types conflict");
    let mut both = vec![fx.id(base), fx.id(sub)];
    both.sort_unstable();
    assert_eq!(receiver_type(&index, &h, file, q), ReceiverType::Index(both));
    assert_eq!(receiver_type(&index, &h, file, r), ReceiverType::Unknown, "unannotated parameter");
    assert_eq!(receiver_type(&index, &h, file, x), ReceiverType::Unknown, "opaque value");
}

/// Rule 2 (implicit receiver): a bare name inside a method of a type reads a field the type
/// inherits (declared on a base in another file); a field of the type itself shadows it.
#[test]
fn rule_bare_name_reads_an_inherited_field_type() {
    let mut fx = Fixture::new();
    let lib = fx.blind_file("lib/Base.java", Language::Java, None);
    let app = fx.blind_file("app/Sub.java", Language::Java, None);
    let conn = fx.decl(lib, Decl::class("Conn"));
    let other = fx.decl(lib, Decl::class("Other"));
    let base = fx.decl(lib, Decl::class("Base"));
    let sub = fx.decl(app, Decl::class("Sub").bases(&["Base"]).span(ByteSpan::new(10, 400)));
    let run = fx.decl(app, Decl::method("run", sub).span(ByteSpan::new(100, 300)));
    let own = fx.decl(app, Decl::class("Own").bases(&["Base"]).span(ByteSpan::new(500, 900)));
    let own_run = fx.decl(app, Decl::method("run", own).span(ByteSpan::new(600, 800)));
    let member = |fx: &mut Fixture, owner: D, at: u32| {
        let span = call_at(fx, app, Some(owner), "conn.go", at);
        fx.call_receiver(app, span, name_at("conn", at));
        ByteSpan::new(span.end - 2, span.end)
    };
    let inherited = member(&mut fx, run, 150);
    let shadowed = member(&mut fx, own_run, 650);
    let mut index = fx.build();
    let field = |class: D| TypeSubject::Field {
        class: class.decl,
        name: "conn".into(),
    };
    add_type(&mut index, "lib/Base.java", field(base), "Conn", TypeSource::Declared);
    add_type(&mut index, "app/Sub.java", field(own), "Other", TypeSource::Declared);
    let h = Hierarchy::build(&index);
    let file = index.file_by_path("app/Sub.java").unwrap();
    assert_eq!(receiver_type(&index, &h, file, inherited), ReceiverType::Index(vec![fx.id(conn)]));
    assert_eq!(receiver_type(&index, &h, file, shadowed), ReceiverType::Index(vec![fx.id(other)]));
}

/// Rule 11: a spelling shaped like a generic parameter (`DB`, `T`) names a type bound in the
/// file's package; elsewhere (only the repository-wide unique-name rule would find it) and
/// unbound it proves nothing, and is never an external type.
#[test]
fn rule_generic_looking_type_name_resolves_only_when_bound() {
    let mut fx = Fixture::new();
    let lib = fx.blind_file("lib/DB.java", Language::Java, None);
    let usage = fx.blind_file("lib/Use.java", Language::Java, None);
    let other = fx.blind_file("app/Other.java", Language::Java, None);
    let db = fx.decl(lib, Decl::class("DB"));
    let run = fx.decl(usage, Decl::function("run").span(ByteSpan::new(100, 300)));
    let far = fx.decl(other, Decl::function("far").span(ByteSpan::new(100, 300)));
    let member = |fx: &mut Fixture, file: usize, owner: D, receiver: &str, at: u32| {
        let span = call_at(fx, file, Some(owner), &format!("{receiver}.go"), at);
        fx.call_receiver(file, span, name_at(receiver, at));
        ByteSpan::new(span.end - 2, span.end)
    };
    let bound = member(&mut fx, usage, run, "p", 150);
    let unbound = member(&mut fx, other, far, "q", 150);
    let generic = member(&mut fx, other, far, "r", 200);
    let mut index = fx.build();
    add_type(&mut index, "lib/Use.java", var(Scope::Decl(run.decl), "p"), "DB", TypeSource::Declared);
    add_type(&mut index, "app/Other.java", var(Scope::Decl(far.decl), "q"), "DB", TypeSource::Declared);
    add_type(&mut index, "app/Other.java", var(Scope::Decl(far.decl), "r"), "T", TypeSource::Declared);
    let h = Hierarchy::build(&index);
    let usage = index.file_by_path("lib/Use.java").unwrap();
    let other = index.file_by_path("app/Other.java").unwrap();
    assert_eq!(receiver_type(&index, &h, usage, bound), ReceiverType::Index(vec![fx.id(db)]));
    assert_eq!(receiver_type(&index, &h, other, unbound), ReceiverType::Unknown);
    assert_eq!(receiver_type(&index, &h, other, generic), ReceiverType::Unknown);
}

/// Rule 11 (`unrelated_to_family` guards): an external type is unrelated to a family only
/// when no member's declaring type (or base) has an unresolved base spelling; structural
/// languages never exclude; containers without a class annotation prove nothing.
#[test]
fn rule_unrelated_to_family_external_type_guard() {
    use crate::types::unrelated_to_family;
    let mut fx = Fixture::new();
    let jv = fx.blind_file("lib/Greeter.java", Language::Java, None);
    let go = fx.blind_file("srv/server.go", Language::Go, None);
    let php = fx.blind_file("src/picker.php", Language::Php, None);
    let php2 = fx.blind_file("src/plain.php", Language::Php, None);
    let greeter = fx.decl(jv, Decl::class("Greeter"));
    let greet = fx.decl(jv, Decl::method("greet", greeter));
    let wrapped = fx.decl(jv, Decl::class("Wrapped").bases(&["ExternalBase"]));
    let wrapped_greet = fx.decl(jv, Decl::method("greet", wrapped));
    let other = fx.decl(jv, Decl::class("Other"));
    fx.decl(go, Decl::class("Server"));
    let serve = fx.decl(go, Decl::function("Write").container("Server"));
    let refresh = fx.decl(php, Decl::function("refresh").container("Picker"));
    let plain = fx.decl(php2, Decl::function("refresh").container("M"));
    let mut index = fx.build();
    add_type(&mut index, "src/picker.php", var(Scope::Module, "Picker"), "Picker", TypeSource::Comment);
    let h = Hierarchy::build(&index);
    let ext = |n: &str| ReceiverType::External(n.to_string());
    let unrelated = |ty: &ReceiverType, family: &[D]| {
        let ids: Vec<SymbolId> = family.iter().map(|d| fx.id(*d)).collect();
        unrelated_to_family(&index, &h, ty, &ids)
    };
    assert!(unrelated(&ext("Writer"), &[greet]), "nominal, every base resolved");
    assert!(!unrelated(&ext("Writer"), &[greet, wrapped_greet]), "Wrapped may extend Writer");
    assert!(!unrelated(&ext("Greeter"), &[greet]), "the declaring type's own name");
    assert!(!unrelated(&ext("Object"), &[greet]), "a top type proves nothing");
    assert!(!unrelated(&ReceiverType::Unknown, &[greet]));
    assert!(!unrelated(&ReceiverType::Index(vec![fx.id(greeter)]), &[greet]));
    assert!(unrelated(&ReceiverType::Index(vec![fx.id(other)]), &[greet]));
    assert!(!unrelated(&ext("Writer"), &[serve]), "Go interfaces are structural");
    assert!(unrelated(&ext("Finder"), &[refresh]), "annotated container of another class");
    assert!(!unrelated(&ext("Picker"), &[refresh]), "its own class");
    assert!(!unrelated(&ext("Finder"), &[plain]), "an unannotated container proves nothing");
}

// ---- negative evidence (arity, receivers) ----

use trace_core::facts::{ArgSlot, Argument, CallDetail};

/// `positional` plain positional arguments (plus one keyword argument with `keyword`) for
/// the call whose callee span is `at`; call details are filled up to stay aligned.
fn set_arguments(index: &mut Index, path: &str, at: ByteSpan, positional: u32, keyword: bool) {
    let facts = facts_of(index, path);
    while facts.call_details.len() < facts.calls.len() {
        let call = facts.call_details.len() as u32;
        facts.call_details.push(CallDetail {
            call,
            receiver: None,
            arguments: Vec::new(),
            callee_path: None,
            not_identical: Vec::new(),
        });
    }
    let i = facts
        .calls
        .iter()
        .position(|c| c.callee_span == at)
        .expect("call site");
    let mut args: Vec<Argument> = (0..positional)
        .map(|index| Argument {
            slot: ArgSlot::Positional { index, exact: true },
            span: ByteSpan::new(0, 0),
            value: Expr::Opaque,
            has_string: false,
        })
        .collect();
    if keyword {
        args.push(Argument {
            slot: ArgSlot::Keyword("key".into()),
            span: ByteSpan::new(0, 0),
            value: Expr::Opaque,
            has_string: false,
        });
    }
    facts.calls[i].arg_count = args.len() as u32;
    facts.call_details[i].arguments = args;
}

/// Rule `arity` (narrowing): a candidate whose parameter list accepts no binding of the
/// call's plain positional arguments is dropped (`dropped_by_arity`); a variadic candidate
/// stays. Negatives: a call with a keyword argument, and a candidate whose signature a
/// decorator may rewrite, are never judged.
#[test]
fn rule_narrowing_drops_candidates_by_arity() {
    let mut fx = Fixture::new();
    let lib = fx.blind_file("pkg/shapes.py", Language::Python, None);
    let app = fx.blind_file("pkg/app.py", Language::Python, None);
    let k = fx.decl(lib, Decl::class("K"));
    let k_m = fx.decl(lib, Decl::method("m", k).params(&["self", "a"]));
    let j = fx.decl(lib, Decl::class("J"));
    let j_m = fx.decl(lib, Decl::method("m", j).params(&["self"]));
    let l = fx.decl(lib, Decl::class("L"));
    let l_m = fx.decl(
        lib,
        Decl::method("m", l).param_specs(&[("self", Positional, false), ("rest", VarPositional, false)]),
    );
    let w = fx.decl(lib, Decl::class("W"));
    let w_m = fx.decl(lib, Decl::method("m", w).params(&["self"]).decorators(&["cached"]));
    let run = fx.decl(app, Decl::function("run").params(&["x"]));
    let at = fx.call_n(app, Some(run), "x.m", 3);
    let base = fx.build();

    let mut index = base.clone();
    set_arguments(&mut index, "pkg/app.py", at, 3, false);
    let n = narrowed(&index, at, fx.id(run), "m");
    let mut dropped = n.dropped_by_arity.clone();
    dropped.sort_unstable();
    let mut want = vec![fx.id(k_m), fx.id(j_m)];
    want.sort_unstable();
    assert_eq!(dropped, want, "{n:?}");
    assert!(n.kept.contains(&fx.id(l_m)), "variadic: {n:?}");
    assert!(n.kept.contains(&fx.id(w_m)), "decorated: {n:?}");

    // Two arguments: `K.m(obj, a)` through the class accepts them; `J.m` does not.
    let mut index = base.clone();
    set_arguments(&mut index, "pkg/app.py", at, 2, false);
    let n = narrowed(&index, at, fx.id(run), "m");
    assert_eq!(n.dropped_by_arity, vec![fx.id(j_m)], "{n:?}");

    // A keyword argument: never judged.
    let mut index = base;
    set_arguments(&mut index, "pkg/app.py", at, 3, true);
    let n = narrowed(&index, at, fx.id(run), "m");
    assert!(n.dropped_by_arity.is_empty(), "{n:?}");
    assert!(n.kept.contains(&fx.id(k_m)) && n.kept.contains(&fx.id(j_m)), "{n:?}");
}

/// Rule: an arity elimination never decides a site. When it leaves a single candidate (here a
/// decorated method, never judged by arity), that candidate has only its name (`field_only`)
/// and the unique-candidate rule does not decide the site: the call may run a same-named
/// method of a library object (a library receiver's `result.get()` is not the repository's
/// only surviving `get`).
#[test]
fn rule_arity_elimination_never_makes_a_unique_candidate() {
    let mut fx = Fixture::new();
    let lib = fx.blind_file("pkg/shapes.py", Language::Python, None);
    let app = fx.blind_file("pkg/app.py", Language::Python, None);
    let j = fx.decl(lib, Decl::class("J"));
    let j_m = fx.decl(lib, Decl::method("m", j).params(&["self"]));
    let w = fx.decl(lib, Decl::class("W"));
    let w_m = fx.decl(lib, Decl::method("m", w).params(&["self", "a"]).decorators(&["cached"]));
    let run = fx.decl(app, Decl::function("run").params(&["x"]));
    let at = fx.call_n(app, Some(run), "x.m", 2);
    let mut index = fx.build();
    set_arguments(&mut index, "pkg/app.py", at, 2, false);
    let n = narrowed(&index, at, fx.id(run), "m");
    assert_eq!(n.dropped_by_arity, vec![fx.id(j_m)], "{n:?}");
    let all = sites(&index);
    let site = site_at(&all, &fx, at).expect("no_target site");
    assert_eq!(site.candidates, vec![fx.id(w_m)]);
    assert_eq!(site.field_only, vec![fx.id(w_m)], "the survivor has only its name");
    index.sites = all;
    let decisions = decide(&index);
    let i = index.sites.iter().position(|s| s.at.bytes == at).unwrap();
    assert_ne!(decisions[i].status, DecisionStatus::Decided, "{:?}", decisions[i]);
}

/// Rule `unrelated_type` guard (Python `lambda self: self.set_selection(row)`): inside an
/// anonymous function that declares its own `self` parameter the name is that parameter,
/// bound by the caller, so its type is unknown, never the enclosing method's receiver;
/// an anonymous function without that parameter still sees the enclosing receiver.
#[test]
fn rule_self_parameter_of_a_callback_is_not_the_enclosing_receiver() {
    let mut fx = Fixture::new();
    let f = fx.blind_file("app/git.py", Language::Python, None);
    let status = fx.decl(
        f,
        Decl::function("status")
            .container("Git")
            .span(ByteSpan::new(100, 400)),
    );
    let callback = fx.decl(
        f,
        Decl::lambda(status)
            .qualified("status.<lambda>@150")
            .params(&["self"])
            .span(ByteSpan::new(150, 250)),
    );
    let plain = fx.decl(
        f,
        Decl::lambda(status)
            .qualified("status.<lambda>@260")
            .span(ByteSpan::new(260, 350)),
    );
    let inside = call_at(&mut fx, f, Some(callback), "self.set_selection", 160);
    fx.call_receiver(f, inside, name_at("self", 160));
    let captured = call_at(&mut fx, f, Some(plain), "self.refresh", 270);
    fx.call_receiver(f, captured, name_at("self", 270));
    let index = fx.build();
    let h = Hierarchy::build(&index);
    let file = index.file_by_path("app/git.py").unwrap();
    let member = |span: ByteSpan, name: &str| ByteSpan::new(span.end - name.len() as u32, span.end);
    assert_eq!(
        receiver_type(&index, &h, file, member(inside, "set_selection")),
        ReceiverType::Unknown,
        "the callback's own self parameter"
    );
    assert_eq!(
        receiver_type(&index, &h, file, member(captured, "refresh")),
        ReceiverType::External("Git".into()),
        "the enclosing method's receiver"
    );
}

/// Rule `unrelated_type` (server-typed receivers, I-03 a): a receiver identifier the
/// language server resolved to a type declaration (`Widget.refresh()`: a proven reference
/// to class `Widget`) has that type, so a member of an unrelated class is ruled out while
/// the type's own member is not; without the server fact the name proves nothing.
#[test]
fn rule_server_typed_receiver_outside_family_rules_out() {
    use crate::types::unrelated_to_family;
    use trace_core::EdgeKind;
    let mut fx = Fixture::new();
    let lib = fx.file("app/ui.py", Language::Python, None);
    let main = fx.file("app/main.py", Language::Python, None);
    let widget = fx.decl(lib, Decl::class("Widget"));
    let own = fx.decl(lib, Decl::method("refresh", widget).params(&["self"]));
    let picker = fx.decl(lib, Decl::class("Picker"));
    let other = fx.decl(lib, Decl::method("refresh", picker).params(&["self"]));
    let meta = fx.decl(lib, Decl::class("Meta").bases(&["metaclass=Base"]));
    let run = fx.decl(main, Decl::function("run").span(ByteSpan::new(100, 300)));
    let typed = call_at(&mut fx, main, Some(run), "Widget.refresh", 110);
    fx.call_receiver(main, typed, name_at("Widget", 110));
    fx.edge(main, run, widget, EdgeKind::References, ByteSpan::new(110, 116), 1);
    let plain = call_at(&mut fx, main, Some(run), "Other.refresh", 150);
    fx.call_receiver(main, plain, name_at("Other", 150));
    let dynamic = call_at(&mut fx, main, Some(run), "Meta.refresh", 190);
    fx.call_receiver(main, dynamic, name_at("Meta", 190));
    fx.edge(main, run, meta, EdgeKind::References, ByteSpan::new(190, 194), 1);
    let index = fx.build();
    let h = Hierarchy::build(&index);
    let file = index.file_by_path("app/main.py").unwrap();
    let member = |span: ByteSpan| ByteSpan::new(span.end - "refresh".len() as u32, span.end);
    let ty = receiver_type(&index, &h, file, member(typed));
    assert_eq!(ty, ReceiverType::Index(vec![fx.id(widget)]));
    assert!(unrelated_to_family(&index, &h, &ty, &[fx.id(other)]));
    assert!(!unrelated_to_family(&index, &h, &ty, &[fx.id(own)]));
    assert_eq!(receiver_type(&index, &h, file, member(plain)), ReceiverType::Unknown, "no server fact");
    assert_eq!(
        receiver_type(&index, &h, file, member(dynamic)),
        ReceiverType::Unknown,
        "a keyword base (metaclass) may add class-level members"
    );
}

/// Candidate pools stay in the calling language's namespace (I-34), including the
/// namespaces shared across languages (Vue / Svelte components with JS / TS, the JVM and
/// .NET families).
#[test]
fn rule_name_candidates_stay_in_the_language_family() {
    assert!(name_interop(Language::Vue, Language::TypeScript));
    assert!(name_interop(Language::Svelte, Language::JavaScript));
    assert!(name_interop(Language::Clojure, Language::Java));
    assert!(name_interop(Language::FSharp, Language::CSharp));
    assert!(!name_interop(Language::Python, Language::Rust));
    assert!(!name_interop(Language::Rust, Language::Python));
    assert!(!name_interop(Language::Go, Language::C));
}

/// Narrow the candidate pool of one blind call (over [`Narrower`]).
fn narrow_candidates(
    index: &Index,
    hierarchy: &Hierarchy,
    unresolved: &Unresolved,
    owner: SymbolId,
    member: &str,
    pool: &[SymbolId],
    limit: usize,
) -> Narrowing {
    let mut narrower = Narrower::new(index, hierarchy);
    let shape = narrower.shape(unresolved, owner, member);
    narrower.narrow(&shape, pool, limit)
}
