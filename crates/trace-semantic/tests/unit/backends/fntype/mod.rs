use super::*;
use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{json, Value};
use trace_core::facts::{CallSite, CallbackArg, FileFacts};
use trace_core::semantics::FnTypeVerdict;
use trace_core::Language;

use crate::SemanticError;
use trace_core::facts::Activation;
use trace_core::model::ByteSpan;

/// Classify a declared parameter type text of `language` (tree-sitter parse of a synthetic
/// declaration; function types, unions/optionals containing one, callable-bounded type
/// parameters, SAM/delegate/protocol-with-call where the language decides it by
/// assignability). Named types that need a definition hop give `Unknown` here.
fn classify(language: Language, type_text: &str, tables: &Tables) -> FnTypeVerdict {
    let text = type_text.trim();
    if text.is_empty() {
        return FnTypeVerdict::Unknown;
    }
    let class = match language {
        Language::Python | Language::Rust | Language::C | Language::Cpp => {
            standalone_type_class(language, text, tables)
        }
        Language::Haskell => haskell_signature(&format!("_x :: ({text}) -> ()"), 0, tables)
            .map(|(c, _)| c)
            .unwrap_or(Class::Unknown),
        _ => match wrap_as_param(language, text) {
            Some(label) => parse_label_param(language, &label, tables)
                .map(|p| p.class)
                .unwrap_or(Class::Unknown),
            None => Class::Unknown,
        },
    };
    settle(language, class).verdict()
}

/// A type text as a parameter label of `language` (for [`classify`]).
fn wrap_as_param(language: Language, type_text: &str) -> Option<String> {
    Some(match language {
        Language::Go => format!("_x {type_text}"),
        Language::Php => format!("{type_text} $_x"),
        Language::CSharp | Language::Java => format!("{type_text} _x"),
        Language::Scala => format!("_x: {type_text}"),
        _ => return None,
    })
}

const TYPESHED: &str = include_str!("../../../../../../tests/fixtures/rule-tables-fntype/typeshed.pyi");
const STD_THREAD: &str = include_str!("../../../../../../tests/fixtures/rule-tables-fntype/std_thread.rs");
const STDLIB_H: &str = include_str!("../../../../../../tests/fixtures/rule-tables-fntype/stdlib.h");
const ALGORITHM: &str = include_str!("../../../../../../tests/fixtures/rule-tables-fntype/algorithm.hpp");

/// A scripted session: answers by (method, uri, line); logs every request.
#[derive(Default)]
struct Fake {
    answers: Vec<(String, String, u32, Value)>,
    files: HashMap<String, Vec<u8>>,
    log: Vec<(String, Value)>,
    record_only: bool,
}

impl Fake {
    fn answer(&mut self, method: &str, uri: &str, line: u32, v: Value) {
        self.answers.push((method.into(), uri.into(), line, v));
    }
    fn file(&mut self, uri: &str, text: &str) {
        self.files.insert(uri.into(), text.as_bytes().to_vec());
    }
}

impl FnTypeSession for Fake {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, SemanticError> {
        self.log.push((method.to_string(), params.clone()));
        if self.record_only {
            return Err(SemanticError::Capability(format!("{method} (recorded)")));
        }
        let uri = params["textDocument"]["uri"].as_str().unwrap_or_default().to_string();
        let line = params["position"]["line"].as_u64().unwrap_or(u64::MAX);
        self.answers
            .iter()
            .find(|(m, u, l, _)| m == method && *u == uri && u64::from(*l) == line)
            .map(|(_, _, _, v)| v.clone())
            .ok_or_else(|| SemanticError::Capability(format!("{method} {uri}:{line} not scripted")))
    }
    fn uri_of(&self, rel: &str) -> Result<String, SemanticError> {
        Ok(format!("file:///repo/{rel}"))
    }
    fn read_location(&mut self, uri: &str) -> Option<(PathBuf, Vec<u8>)> {
        let bytes = self.files.get(uri)?.clone();
        let path = PathBuf::from(uri.trim_start_matches("file://"));
        Some((path, bytes))
    }
}

/// Line (0-based) of the first line containing `needle`.
fn line_of(text: &str, needle: &str) -> u32 {
    text.lines().position(|l| l.contains(needle)).expect("needle") as u32
}

/// Location JSON at the start of `needle` on its line.
fn loc(uri: &str, text: &str, needle: &str) -> Value {
    let line = line_of(text, needle);
    let col = text
        .lines()
        .nth(line as usize)
        .and_then(|l| l.find(needle))
        .unwrap_or(0);
    json!([{"uri": uri, "range": {"start": {"line": line, "character": col}, "end": {"line": line, "character": col + needle.len()}}}])
}

struct Call {
    source: String,
    facts: FileFacts,
    call: CallSite,
    arg: CallbackArg,
}

/// A call `callee(...)` in `source` passing `argument` at `index` / `keyword`.
fn call(
    source: &str,
    callee: &str,
    argument: &str,
    index: Option<u32>,
    keyword: Option<&str>,
    arg_count: u32,
) -> Call {
    let start = source.find(callee).expect("callee") as u32;
    let callee_span = ByteSpan::new(start, start + callee.len() as u32);
    let call_end = source[start as usize..]
        .find(')')
        .map_or(source.len(), |i| start as usize + i + 1) as u32;
    let arg_start = source[start as usize..].find(argument).expect("argument") as u32 + start;
    let member = callee.rsplit(['.', ':']).next().map(str::to_string);
    Call {
        source: source.to_string(),
        facts: FileFacts::default(),
        call: CallSite {
            owner: None,
            lexical_owner: None,
            span: ByteSpan::new(start, call_end),
            callee_span,
            callee: callee.to_string(),
            member,
            receiver: None,
            line: 1,
            activation: Activation::Plain,
            is_new: false,
            arg_count,
        },
        arg: CallbackArg {
            call_callee_span: callee_span,
            callee: callee.to_string(),
            arg_span: ByteSpan::new(arg_start, arg_start + argument.len() as u32),
            argument: argument.to_string(),
            name: argument.to_string(),
            owner: None,
            index,
            keyword: keyword.map(str::to_string),
        },
    }
}

fn query<'a>(c: &'a Call, language: Language, path: &'a str, route: FnTypeRoute) -> FnTypeQuery<'a> {
    FnTypeQuery {
        language,
        path,
        source: c.source.as_bytes(),
        facts: &c.facts,
        call: &c.call,
        arg: &c.arg,
        route,
    }
}

const STUB: &str = "file:///lib/typeshed.pyi";

fn python_session() -> Fake {
    let mut s = Fake::default();
    s.file(STUB, TYPESHED);
    s
}

#[test]
fn rule_function_typed_library_param_is_inferred_callback() {
    // Python declaration route: Thread(target=work) -> class Thread -> __init__.target.
    let c = call(
        "import threading\nthreading.Thread(target=work)\n",
        "threading.Thread",
        "work",
        None,
        Some("target"),
        0,
    );
    let mut s = python_session();
    s.answer("textDocument/definition", "file:///repo/app.py", 1, loc(STUB, TYPESHED, "Thread:"));
    let mut cache = FnTypeCache::default();
    let p = resolve(&query(&c, Language::Python, "app.py", FnTypeRoute::Declaration), &mut s, &mut cache)
        .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::FunctionType);
    assert_eq!(p.param_name.as_deref(), Some("target"));
    assert_eq!(p.param_type, "Callable[..., object] | None");
    assert_eq!(p.route, "declaration");
    assert_eq!(p.arg, c.arg.arg_span);
    assert_eq!(p.call, c.call.callee_span);

    // Go label route: sort.Slice(xs, less).
    let c = call("package main\nfunc f() { sort.Slice(xs, less) }\n", "sort.Slice", "less", Some(1), None, 2);
    let mut s = Fake::default();
    s.answer(
            "textDocument/signatureHelp",
            "file:///repo/main.go",
            1,
            json!({"signatures": [{"label": "Slice(x any, less func(i int, j int) bool)", "parameters": [{"label": "x any"}, {"label": "less func(i int, j int) bool"}]}], "activeParameter": 0}),
        );
    let p =
        resolve(&query(&c, Language::Go, "main.go", FnTypeRoute::Label), &mut s, &mut cache).expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::FunctionType);
    assert_eq!(p.param_name.as_deref(), Some("less"));
    assert_eq!(p.route, "label");

    // Rust declaration route: thread::spawn(work) -> F: FnOnce() -> T (where clause).
    const RS: &str = "file:///rust-src/std/thread/mod.rs";
    let c = call("fn main() { thread::spawn(work); }\n", "thread::spawn", "work", Some(0), None, 1);
    let mut s = Fake::default();
    s.file(RS, STD_THREAD);
    s.answer("textDocument/definition", "file:///repo/main.rs", 0, loc(RS, STD_THREAD, "spawn<F, T>"));
    let p = resolve(&query(&c, Language::Rust, "main.rs", FnTypeRoute::Declaration), &mut s, &mut cache)
        .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::FunctionType);

    // Rust method syntax skips `&self`: once.call_once(work) -> F: FnOnce().
    let c = call("fn main() { once.call_once(work); }\n", "once.call_once", "work", Some(0), None, 1);
    s.answer("textDocument/definition", "file:///repo/once.rs", 0, loc(RS, STD_THREAD, "call_once<F"));
    let p = resolve(&query(&c, Language::Rust, "once.rs", FnTypeRoute::Declaration), &mut s, &mut cache)
        .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::FunctionType);

    // C: qsort(.., cmp) -> int (*compar)(const void *, const void *).
    const H: &str = "file:///usr/include/stdlib.h";
    let c = call("void f(void) { qsort(xs, n, 4, cmp); }\n", "qsort", "cmp", Some(3), None, 4);
    let mut s = Fake::default();
    s.file(H, STDLIB_H);
    s.answer("textDocument/definition", "file:///repo/main.c", 0, loc(H, STDLIB_H, "qsort("));
    let p = resolve(&query(&c, Language::C, "main.c", FnTypeRoute::Declaration), &mut s, &mut cache)
        .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::FunctionType);
    assert_eq!(p.param_name.as_deref(), Some("compar"));
}

#[test]
fn rule_top_typed_param_is_not_a_function_type() {
    // Python print(work) -> *values: object.
    let c = call("print(work)\n", "print", "work", Some(0), None, 1);
    let mut s = python_session();
    s.answer("textDocument/definition", "file:///repo/app.py", 0, loc(STUB, TYPESHED, "print("));
    let p = resolve(
        &query(&c, Language::Python, "app.py", FnTypeRoute::Declaration),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::TopType);

    // Go fmt.Println(work) -> a ...any (variadic).
    let c = call(
        "package main\nfunc f() { fmt.Println(\"x\", work) }\n",
        "fmt.Println",
        "work",
        Some(1),
        None,
        2,
    );
    let mut s = Fake::default();
    s.answer(
            "textDocument/signatureHelp",
            "file:///repo/main.go",
            1,
            json!({"signatures": [{"label": "Println(a ...any) (n int, err error)", "parameters": [{"label": [8, 16]}]}]}),
        );
    let p =
        resolve(&query(&c, Language::Go, "main.go", FnTypeRoute::Label), &mut s, &mut FnTypeCache::default())
            .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::TopType);
    assert_eq!(p.param_type, "any");

    // C: printf("%p", fn) -> variadic `...`; memcpy(dest, ...) -> void *.
    const H: &str = "file:///usr/include/stdlib.h";
    let mut s = Fake::default();
    s.file(H, STDLIB_H);
    let c = call("void f(void) { printf(\"%p\", work); }\n", "printf", "work", Some(1), None, 2);
    s.answer("textDocument/definition", "file:///repo/main.c", 0, loc(H, STDLIB_H, "printf("));
    let p = resolve(
        &query(&c, Language::C, "main.c", FnTypeRoute::Declaration),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::TopType);

    // Rust show<T: Debug>(t: T): a non-Fn bound is not a function type.
    const RS: &str = "file:///rust-src/std/thread/mod.rs";
    let c = call("fn main() { show(work); }\n", "show", "work", Some(0), None, 1);
    let mut s = Fake::default();
    s.file(RS, STD_THREAD);
    s.answer("textDocument/definition", "file:///repo/main.rs", 0, loc(RS, STD_THREAD, "show<T"));
    let p = resolve(
        &query(&c, Language::Rust, "main.rs", FnTypeRoute::Declaration),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::TopType);

    // Classifier: top types of every label language.
    let t = tables();
    assert_eq!(classify(Language::CSharp, "object?", t), FnTypeVerdict::TopType);
    assert_eq!(classify(Language::Scala, "Any", t), FnTypeVerdict::TopType);
    assert_eq!(classify(Language::Php, "mixed", t), FnTypeVerdict::TopType);
    assert_eq!(classify(Language::Go, "interface{}", t), FnTypeVerdict::TopType);
    assert_eq!(classify(Language::Python, "object", t), FnTypeVerdict::TopType);
}

#[test]
fn rule_alias_param_type_resolves() {
    // signal.signal(SIGINT, handler) -> handler: _HANDLER -> _HANDLER = Callable[...] | int | None.
    let c = call("import signal\nsignal.signal(2, handler)\n", "signal.signal", "handler", Some(1), None, 2);
    let mut s = python_session();
    s.answer("textDocument/definition", "file:///repo/app.py", 1, loc(STUB, TYPESHED, "signal(signalnum"));
    let alias_use = line_of(TYPESHED, "def signal(");
    s.answer("textDocument/definition", STUB, alias_use, loc(STUB, TYPESHED, "_HANDLER ="));
    let p = resolve(
        &query(&c, Language::Python, "app.py", FnTypeRoute::Declaration),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::FunctionType);
    assert_eq!(p.param_type, "_HANDLER");

    // register(func: _F) with _F = TypeVar("_F", bound=Callable[..., Any]).
    let c = call("import atexit\natexit.register(work)\n", "atexit.register", "work", Some(0), None, 1);
    let mut s = python_session();
    s.answer("textDocument/definition", "file:///repo/app.py", 1, loc(STUB, TYPESHED, "register(func"));
    let use_line = line_of(TYPESHED, "def register(");
    s.answer("textDocument/definition", STUB, use_line, loc(STUB, TYPESHED, "_F = TypeVar"));
    let p = resolve(
        &query(&c, Language::Python, "app.py", FnTypeRoute::Declaration),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::FunctionType);

    // C typedef: signal(SIGINT, handler) -> sig_handler_t -> typedef void (*)(int).
    const H: &str = "file:///usr/include/stdlib.h";
    let c = call("void f(void) { signal(2, handler); }\n", "signal", "handler", Some(1), None, 2);
    let mut s = Fake::default();
    s.file(H, STDLIB_H);
    s.answer("textDocument/definition", "file:///repo/main.c", 0, loc(H, STDLIB_H, "signal(int"));
    let decl = line_of(STDLIB_H, "sig_handler_t signal(");
    s.answer("textDocument/definition", H, decl, loc(H, STDLIB_H, "sig_handler_t)(int)"));
    let p = resolve(
        &query(&c, Language::C, "main.c", FnTypeRoute::Declaration),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::FunctionType);

    // A function-pointer typedef declared earlier in the same file needs no hop.
    let mut s = Fake::default();
    s.file(H, STDLIB_H);
    s.answer("textDocument/definition", "file:///repo/main.c", 0, loc(H, STDLIB_H, "signal(int"));
    let p = resolve(
        &query(&c, Language::C, "main.c", FnTypeRoute::Declaration),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::FunctionType);
    assert_eq!(s.log.len(), 1, "only the callee's definition was asked");

    // A typedef of another file without the hop answer stays unknown (never guessed).
    const H2: &str = "file:///usr/include/signal_only.h";
    let header = "sig_handler_t signal(int sig, sig_handler_t handler);\n";
    let mut s = Fake::default();
    s.file(H2, header);
    s.answer("textDocument/definition", "file:///repo/main.c", 0, loc(H2, header, "signal(int"));
    let p = resolve(
        &query(&c, Language::C, "main.c", FnTypeRoute::Declaration),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::Unknown);
}

/// Rule (MSVC SAL): an annotation macro before a parameter's type (`_In_ CompareFn
/// _Compare`) is read by the grammar as the type; the parameter is classified by the
/// function-pointer typedef the annotated declarator names. Negatives: an annotated
/// value type and a non-function typedef stay non-function.
#[test]
fn rule_annotated_parameter_uses_the_function_pointer_typedef() {
    const H: &str = "file:///sdk/ucrt/search.h";
    let header = "typedef int (__cdecl* _CompareFn)(void const*, void const*);\n\
                      typedef struct _Pair { int a; } _PairT;\n\
                      void __cdecl sort_it(void* _Base, _In_ size_t _Count, _In_ _PairT _Pair, _In_ _CompareFn _CompareFunction);\n";
    let decl = "sort_it(";
    let run = |index: u32| {
        let c = call(
            "void f(void) { sort_it(xs, n, p, cmp); }\n",
            "sort_it",
            if index == 3 { "cmp" } else { "n" },
            Some(index),
            None,
            4,
        );
        let mut s = Fake::default();
        s.file(H, header);
        s.answer("textDocument/definition", "file:///repo/main.c", 0, loc(H, header, decl));
        resolve(
            &query(&c, Language::C, "main.c", FnTypeRoute::Declaration),
            &mut s,
            &mut FnTypeCache::default(),
        )
        .expect("answer")
        .verdict
    };
    assert_eq!(run(3), FnTypeVerdict::FunctionType, "annotated comparator");
    assert_ne!(run(1), FnTypeVerdict::FunctionType, "annotated size_t is a value");
    assert_ne!(run(2), FnTypeVerdict::FunctionType, "annotated struct typedef is a value");
    // The calling convention inside a typedef's declarator hides nothing.
    let tree = trace_syntax::parse_tree(Language::C, header.as_bytes()).unwrap();
    let typedefs = c_typedefs(tree.root_node(), header.as_bytes());
    assert_eq!(typedefs.get("_CompareFn").map(|t| t.0), Some(true));
    assert_eq!(typedefs.get("_PairT").map(|t| t.0), Some(false));
}

#[test]
fn rule_overloads_scan_all_signatures() {
    // sorted(xs, key=keyf): the first @overload has key: None, the second a Callable.
    let c = call("sorted(xs, key=keyf)\n", "sorted", "keyf", None, Some("key"), 1);
    let mut s = python_session();
    s.answer("textDocument/definition", "file:///repo/app.py", 0, loc(STUB, TYPESHED, "sorted(iterable"));
    let p = resolve(
        &query(&c, Language::Python, "app.py", FnTypeRoute::Declaration),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::FunctionType);

    // C#: activeSignature points at the (int, int) overload; another takes a Func.
    let c = call("class A { void F() { xs.Select(Twice); } }\n", "xs.Select", "Twice", Some(0), None, 1);
    let mut s = Fake::default();
    s.answer(
            "textDocument/signatureHelp",
            "file:///repo/A.cs",
            0,
            json!({"activeSignature": 0, "activeParameter": null, "signatures": [
                {"label": "IEnumerable<TResult> IEnumerable<int>.Select<int, TResult>(Func<int, int, TResult> selector)", "parameters": [{"label": "Func<int, int, TResult> selector"}]},
                {"label": "void Other(int count)", "parameters": [{"label": "int count"}]}
            ]}),
        );
    let p = resolve(
        &query(&c, Language::CSharp, "A.cs", FnTypeRoute::Label),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::FunctionType);

    // C++ overload set: every declaration of the name is scanned; Compare is an
    // unconstrained template parameter (top type: the table decides).
    const HPP: &str = "file:///usr/include/c++/algorithm";
    let c = call("void f() { std::sort(b, e, less); }\n", "std::sort", "less", Some(2), None, 3);
    let mut s = Fake::default();
    s.file(HPP, ALGORITHM);
    s.answer("textDocument/definition", "file:///repo/main.cpp", 0, loc(HPP, ALGORITHM, "sort(RandomIt"));
    let p = resolve(
        &query(&c, Language::Cpp, "main.cpp", FnTypeRoute::Declaration),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::TopType);
}

#[test]
fn rule_signature_help_is_requested_at_argument_end() {
    let source = "package main\nfunc f() { sort.Slice(xs, less) }\n";
    let c = call(source, "sort.Slice", "less", Some(1), None, 2);
    let mut s = Fake {
        record_only: true,
        ..Fake::default()
    };
    // Dry run (pipelining): the request is recorded, nothing is claimed.
    assert!(resolve(
        &query(&c, Language::Go, "main.go", FnTypeRoute::Label),
        &mut s,
        &mut FnTypeCache::default()
    )
    .is_none());
    let (method, params) = s.log.first().expect("one request");
    assert_eq!(method, "textDocument/signatureHelp");
    let end = c.arg.arg_span.end as usize;
    let line_start = source[..end].rfind('\n').map_or(0, |i| i + 1);
    assert_eq!(params["position"]["line"], 1);
    assert_eq!(params["position"]["character"], (end - line_start) as u64);
    assert_eq!(s.log.len(), 1, "one request per query in the recording pass");
}

/// Rule (hover fallback): a first-class callable `f(...)` passed to a library call gets the
/// parameter's declared type; when `signatureHelp` gives no parameter, the callee's hover
/// declaration is parsed with the PHP grammar. A usable signature answer needs no hover.
#[test]
fn rule_label_route_falls_back_to_the_hover_declaration() {
    let source = "<?php\n$r = array_map(trim_name(...), $xs);\n";
    let c = call(source, "array_map", "trim_name(...)", Some(0), None, 2);
    let mut s = Fake::default();
    s.answer("textDocument/signatureHelp", "file:///repo/main.php", 1, Value::Null);
    s.answer(
            "textDocument/hover",
            "file:///repo/main.php",
            1,
            json!({"contents": {"kind": "markdown", "value": "```php\n<?php\nfunction array_map(?callable $callback, array $array, array ...$arrays): array { }\n```\nApplies the callback."}}),
        );
    let p = resolve(
        &query(&c, Language::Php, "main.php", FnTypeRoute::Label),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::FunctionType);
    assert_eq!(p.param_name.as_deref(), Some("callback"));
    assert_eq!(p.route, "label");
    assert_eq!(s.log.iter().filter(|(m, _)| m == "textDocument/hover").count(), 1);

    // A signature answer with the parameter: no hover request.
    let mut s = Fake::default();
    s.answer(
        "textDocument/signatureHelp",
        "file:///repo/main.php",
        1,
        json!({"signatures": [{"label": "?callable $callback", "parameters": [{"label": [0, 19]}]}]}),
    );
    let p = resolve(
        &query(&c, Language::Php, "main.php", FnTypeRoute::Label),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::FunctionType);
    assert!(s.log.iter().all(|(m, _)| m != "textDocument/hover"));

    // The recording pass of the pipelining never falls back (one request per query).
    let mut s = Fake {
        record_only: true,
        ..Fake::default()
    };
    assert!(resolve(
        &query(&c, Language::Php, "main.php", FnTypeRoute::Label),
        &mut s,
        &mut FnTypeCache::default()
    )
    .is_none());
    assert_eq!(s.log.len(), 1);

    // A hover without a declaration claims nothing.
    let mut s = Fake::default();
    s.answer("textDocument/signatureHelp", "file:///repo/main.php", 1, json!({"signatures": []}));
    s.answer("textDocument/hover", "file:///repo/main.php", 1, json!({"contents": "no documentation"}));
    let p = resolve(
        &query(&c, Language::Php, "main.php", FnTypeRoute::Label),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::Unknown);
    // Languages without a hover form never ask for it.
    let go =
        call("package main\nfunc f() { sort.Slice(xs, less) }\n", "sort.Slice", "less", Some(1), None, 2);
    let mut s = Fake::default();
    s.answer("textDocument/signatureHelp", "file:///repo/main.go", 1, Value::Null);
    let _ = resolve(
        &query(&go, Language::Go, "main.go", FnTypeRoute::Label),
        &mut s,
        &mut FnTypeCache::default(),
    );
    assert!(s.log.iter().all(|(m, _)| m != "textDocument/hover"));
}

/// Rule (PHP and every label language): an anonymous function passed to a library call
/// is a callback argument of the function-type rule (its declaration span is the
/// argument); `signatureHelp` is asked at its START, and a `callable` parameter gives a
/// function type. A closure bound to a variable first is not an argument.
#[test]
fn rule_php_closure_argument_gets_the_declared_parameter_type() {
    let source = "<?php\n$ys = array_map(function ($x) { return strlen($x); }, $xs);\n$f = function ($y) { return $y; };\n";
    let facts = trace_syntax::extract(trace_syntax::SourceInput {
        path: "a.php",
        language: Language::Php,
        source: source.as_bytes(),
    })
    .expect("facts");
    let args = anonymous_arguments(&facts, source.as_bytes());
    assert_eq!(args.len(), 1, "only the closure passed as an argument: {args:?}");
    let arg = &args[0];
    assert_eq!(arg.index, Some(0));
    assert!(arg.argument.starts_with("function"), "{}", arg.argument);
    let call_site = facts
        .calls
        .iter()
        .find(|c| c.callee_span == arg.call_callee_span)
        .expect("receiving call");
    assert_eq!(call_site.callee, "array_map");
    let mut s = Fake::default();
    s.answer(
            "textDocument/signatureHelp",
            "file:///repo/a.php",
            1,
            json!({"signatures": [{"label": "array_map(?callable $callback, array $array, array ...$arrays): array",
                "parameters": [{"label": "?callable $callback"}, {"label": "array $array"}, {"label": "array ...$arrays"}]}]}),
        );
    let q = FnTypeQuery {
        language: Language::Php,
        path: "a.php",
        source: source.as_bytes(),
        facts: &facts,
        call: call_site,
        arg,
        route: FnTypeRoute::Label,
    };
    let p = resolve(&q, &mut s, &mut FnTypeCache::default()).expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::FunctionType);
    assert_eq!(p.param_name.as_deref(), Some("callback"));
    assert_eq!(p.arg, arg.arg_span, "the answer names the closure's span");
    let (_, params) = s.log.first().expect("signatureHelp");
    let line_start = source[..arg.arg_span.start as usize].rfind('\n').map_or(0, |i| i + 1);
    assert_eq!(
        params["position"]["character"],
        (arg.arg_span.start as usize - line_start) as u64,
        "asked at the start"
    );
}

#[test]
fn rule_java_method_reference_targets_functional_interface() {
    let c =
        call("class A { void f() { new Thread(Main::work); } }\n", "Thread", "Main::work", Some(0), None, 1);
    let mut s = Fake::default();
    let p = resolve(
        &query(&c, Language::Java, "A.java", FnTypeRoute::LanguageRule),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::FunctionType);
    assert_eq!(p.route, "language_rule");
    assert!(s.log.is_empty(), "no request");
    let c = call(
        "class A { void f() { xs.forEach(x -> work(x)); } }\n",
        "xs.forEach",
        "x -> work(x)",
        Some(0),
        None,
        1,
    );
    let p = resolve(
        &query(&c, Language::Java, "A.java", FnTypeRoute::LanguageRule),
        &mut s,
        &mut FnTypeCache::default(),
    );
    assert_eq!(p.map(|p| p.verdict), Some(FnTypeVerdict::FunctionType));
    // A plain variable is not a method reference: no claim.
    let c = call("class A { void f() { run(task); } }\n", "run", "task", Some(0), None, 1);
    assert!(resolve(
        &query(&c, Language::Java, "A.java", FnTypeRoute::LanguageRule),
        &mut s,
        &mut FnTypeCache::default()
    )
    .is_none());
}

#[test]
fn rule_table_only_and_checker_routes_make_no_request() {
    let c = call("trap cleanup EXIT\n", "trap", "cleanup", Some(0), None, 2);
    let mut s = Fake::default();
    assert!(resolve(
        &query(&c, Language::Bash, "a.sh", FnTypeRoute::TableOnly),
        &mut s,
        &mut FnTypeCache::default()
    )
    .is_none());
    assert!(resolve(
        &query(&c, Language::TypeScript, "a.ts", FnTypeRoute::Checker),
        &mut s,
        &mut FnTypeCache::default()
    )
    .is_none());
    assert!(s.log.is_empty());
}

#[test]
fn rule_label_types_classify_per_language() {
    let t = tables();
    let cases: &[(Language, &str, FnTypeVerdict)] = &[
        (Language::Go, "func(int) bool", FnTypeVerdict::FunctionType),
        (Language::Go, "filepath.WalkFunc", FnTypeVerdict::FunctionType),
        (Language::Go, "any", FnTypeVerdict::TopType),
        (Language::Php, "?callable", FnTypeVerdict::FunctionType),
        (Language::Php, "callable|null", FnTypeVerdict::FunctionType),
        (Language::Php, "\\Closure", FnTypeVerdict::FunctionType),
        (Language::Php, "string", FnTypeVerdict::NotFunctionType),
        (Language::CSharp, "Action", FnTypeVerdict::FunctionType),
        (Language::CSharp, "Func<int, bool>", FnTypeVerdict::FunctionType),
        (Language::CSharp, "Expression<Func<int, bool>>", FnTypeVerdict::NotFunctionType),
        (Language::Scala, "A => B", FnTypeVerdict::FunctionType),
        (Language::Scala, "=> Unit", FnTypeVerdict::FunctionType),
        (Language::Python, "Callable[[int], int] | None", FnTypeVerdict::FunctionType),
        (Language::Python, "typing.Optional[Callable[..., Any]]", FnTypeVerdict::FunctionType),
        (Language::Python, "\"Callable[[], None]\"", FnTypeVerdict::FunctionType),
        (Language::Python, "int", FnTypeVerdict::Unknown),
        (Language::Rust, "impl FnOnce() -> T + Send", FnTypeVerdict::FunctionType),
        (Language::Rust, "Box<dyn Fn(i32) -> i32>", FnTypeVerdict::FunctionType),
        (Language::Rust, "&dyn std::fmt::Debug", FnTypeVerdict::TopType),
        (Language::C, "void (*)(int)", FnTypeVerdict::FunctionType),
        (Language::C, "void *p", FnTypeVerdict::TopType),
        (Language::C, "int n", FnTypeVerdict::NotFunctionType),
        (Language::Cpp, "const std::function<void()> &f", FnTypeVerdict::FunctionType),
        (Language::Haskell, "IO ()", FnTypeVerdict::FunctionType),
        (Language::Haskell, "a -> b", FnTypeVerdict::FunctionType),
    ];
    for (language, text, verdict) in cases {
        assert_eq!(classify(*language, text, t), *verdict, "{language:?} {text}");
    }
}

#[test]
fn rule_callable_concept_template_parameter_is_a_function_type() {
    const HPP: &str = "file:///usr/include/c++/algorithm";
    let t = tables();
    let c = call("void f() { std::run_now(work); }\n", "std::run_now", "work", Some(0), None, 1);
    let mut s = Fake::default();
    s.file(HPP, ALGORITHM);
    s.answer("textDocument/definition", "file:///repo/main.cpp", 0, loc(HPP, ALGORITHM, "run_now(F"));
    let p = resolve(
        &query(&c, Language::Cpp, "main.cpp", FnTypeRoute::Declaration),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::FunctionType);
    // std::function parameter.
    let c = call("void f() { std::at_quick_exit(work); }\n", "std::at_quick_exit", "work", Some(0), None, 1);
    s.answer("textDocument/definition", "file:///repo/exit.cpp", 0, loc(HPP, ALGORITHM, "at_quick_exit("));
    let p = resolve(
        &query(&c, Language::Cpp, "exit.cpp", FnTypeRoute::Declaration),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::FunctionType);
    assert_eq!(classify(Language::Cpp, "auto f", t), FnTypeVerdict::TopType);
}

#[test]
fn rule_haskell_action_parameter_is_run() {
    let c = call("main = forM_ xs work\n", "forM_", "work", Some(1), None, 2);
    let mut s = Fake::default();
    s.answer(
            "textDocument/hover",
            "file:///repo/Main.hs",
            0,
            json!({"contents": {"kind": "markdown", "value": "```haskell\nforM_ :: (Foldable t, Monad m) => t a -> (a -> m b) -> m ()\n```\n*Defined in base*"}}),
        );
    let p = resolve(
        &query(&c, Language::Haskell, "Main.hs", FnTypeRoute::Declaration),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::FunctionType);
    // forkIO :: IO () -> IO ThreadId: the IO action argument runs.
    let c = call("main = forkIO work\n", "forkIO", "work", Some(0), None, 1);
    let mut s = Fake::default();
    s.answer(
        "textDocument/hover",
        "file:///repo/Main.hs",
        0,
        json!({"contents": {"kind": "markdown", "value": "```haskell\nforkIO :: IO () -> IO ThreadId\n```"}}),
    );
    let p = resolve(
        &query(&c, Language::Haskell, "Main.hs", FnTypeRoute::Declaration),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::FunctionType);
    // print :: Show a => a -> IO (): a value, not run.
    let c = call("main = print work\n", "print", "work", Some(0), None, 1);
    let mut s = Fake::default();
    s.answer(
        "textDocument/hover",
        "file:///repo/Main.hs",
        0,
        json!({"contents": {"kind": "markdown", "value": "```haskell\nprint :: Show a => a -> IO ()\n```"}}),
    );
    let p = resolve(
        &query(&c, Language::Haskell, "Main.hs", FnTypeRoute::Declaration),
        &mut s,
        &mut FnTypeCache::default(),
    )
    .expect("answer");
    assert_eq!(p.verdict, FnTypeVerdict::TopType);
}

#[test]
fn rule_declaration_answers_are_cached_per_file_line_and_parameter() {
    let c = call(
        "import threading\nthreading.Thread(target=work)\n",
        "threading.Thread",
        "work",
        None,
        Some("target"),
        0,
    );
    let mut s = python_session();
    s.answer("textDocument/definition", "file:///repo/app.py", 1, loc(STUB, TYPESHED, "Thread:"));
    let mut cache = FnTypeCache::default();
    let first = resolve(&query(&c, Language::Python, "app.py", FnTypeRoute::Declaration), &mut s, &mut cache);
    s.files.clear();
    let second =
        resolve(&query(&c, Language::Python, "app.py", FnTypeRoute::Declaration), &mut s, &mut cache);
    assert_eq!(first, second, "the parsed declaration file and the verdict are reused");
    assert_eq!(second.map(|p| p.verdict), Some(FnTypeVerdict::FunctionType));
}
