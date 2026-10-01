//! Rule tests of the C / C++ answer rules (`cpp_calls`): scripted call hierarchy / definition
//! answers over facts extracted from real sources.

use std::collections::HashMap;

use serde_json::{json, Value};
use trace_core::facts::FileFacts;
use trace_core::model::{ByteSpan, Resolution, UnresolvedKind};
use trace_core::semantics::FileSemantics;
use trace_core::{Hash32, Language};

use crate::backend::SemanticFile;
use crate::engine::{analyze, Options};
use crate::mapping::DeclTable;
use crate::test_support::session::empty_prepared;
use crate::test_support::session::{prepared, FakeSession, FakeUris, Handler};
use trace_core::model::Provider;

const PATH: &str = "t.cpp";

fn options() -> Options<'static> {
    Options {
        provider: Provider::Lsp("test".into()),
        tool_fingerprint: "fp",
        python: false,
        syntax_answers: true,
        hooks: &crate::languages::DefaultServer,
        prepared: empty_prepared(),
        calls_by_definition: false,
        reuse: None,
    }
}

fn facts_of(src: &str) -> FileFacts {
    trace_syntax::extract(trace_syntax::SourceInput {
        path: PATH,
        language: Language::Cpp,
        source: src.as_bytes(),
    })
    .expect("facts")
}

/// (line, UTF-16 column) of a byte offset.
fn pos(src: &str, byte: usize) -> (u32, u32) {
    trace_core::text::LineIndex::new(src.as_bytes()).utf16_of_byte(src.as_bytes(), byte as u32)
}

fn range(src: &str, start: usize, len: usize) -> Value {
    let (l0, c0) = pos(src, start);
    let (l1, c1) = pos(src, start + len);
    json!({"start": {"line": l0, "character": c0}, "end": {"line": l1, "character": c1}})
}

/// Byte offset of the `k`-th (0-based) occurrence of `needle`, plus `skip`.
fn nth(src: &str, needle: &str, k: usize, skip: usize) -> usize {
    src.match_indices(needle)
        .nth(k)
        .map(|(i, _)| i + skip)
        .expect("needle")
}

/// A call-hierarchy item for the declaration named `name` at byte `at`.
fn item(src: &str, at: usize, name: &str) -> Value {
    json!({"name": name, "kind": 12, "uri": format!("file:///ws/{PATH}"),
           "range": range(src, at, name.len()), "selectionRange": range(src, at, name.len())})
}

/// Scripted clangd: `outgoing` per owner name line, `definitions` per (line, character).
fn run(
    src: &str,
    outgoing: HashMap<u32, Value>,
    definitions: HashMap<(u32, u32), Value>,
) -> (FileFacts, FileSemantics) {
    let facts = facts_of(src);
    let file = SemanticFile {
        path: PATH,
        language: Language::Cpp,
        hash: Hash32::of(src.as_bytes()),
        source: src.as_bytes(),
        facts: &facts,
    };
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let handler: Handler = Box::new(move |method, params| {
        let at =
            |p: &Value| (p["line"].as_u64().unwrap_or(0) as u32, p["character"].as_u64().unwrap_or(0) as u32);
        Ok(match method {
            "textDocument/prepareCallHierarchy" => prepared(params),
            "callHierarchy/outgoingCalls" => {
                let line = params["item"]["selectionRange"]["start"]["line"]
                    .as_u64()
                    .unwrap_or(0) as u32;
                outgoing.get(&line).cloned().unwrap_or_else(|| json!([]))
            }
            "textDocument/definition" => definitions
                .get(&at(&params["position"]))
                .cloned()
                .unwrap_or(Value::Null),
            _ => Value::Null,
        })
    });
    let caps = json!({"callHierarchyProvider": true, "definitionProvider": true});
    let mut session = FakeSession::new(caps, handler);
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &options()).expect("analysis");
    let sem = analysis.files[PATH].clone();
    (facts, sem)
}

/// Callee span of the `k`-th call whose callee text is `callee`.
fn callee_span(facts: &FileFacts, callee: &str, k: usize) -> ByteSpan {
    facts
        .calls
        .iter()
        .filter(|c| c.callee == callee)
        .nth(k)
        .map(|c| c.callee_span)
        .expect("call")
}

/// Edges of a call: keyed on its callee, or on the member identifier ending it (call
/// hierarchy ranges).
fn edge_targets(sem: &FileSemantics, at: ByteSpan) -> Vec<(String, Resolution)> {
    sem.edges
        .iter()
        .filter(|e| e.at.end == at.end && e.at.start >= at.start)
        .map(|e| (e.target.clone(), e.resolution))
        .collect()
}

fn candidates(sem: &FileSemantics, at: ByteSpan) -> Option<Vec<String>> {
    sem.unresolved
        .iter()
        .find(|u| u.at == at && u.kind == UnresolvedKind::ExternalOrAmbiguous)
        .map(|u| u.candidates.clone())
}

/// Rule 1: the call-hierarchy targets at a function-like macro invocation are what its
/// expansion calls: the invocation calls each uniquely named one, and `definition` at the
/// macro name answers its own target (the macro). Negatives: targets sharing a name (the
/// overload set of a dependent call in the expansion) are not called edges; a function call
/// answered with its own name stays a single call hierarchy edge.
#[test]
fn rule_cpp_macro_invocation_calls_its_expansion_and_targets_the_macro() {
    let src = "#define CHECK_EQ(a, b) report(equal(a, b))
bool equal(int a, int b) { return a == b; }
bool equal(long a, long b) { return a == b; }
void report(bool ok) {}
void test() {
  CHECK_EQ(1, 2);
  report(true);
}
";
    let macro_at = nth(src, "CHECK_EQ", 0, 0);
    let equal_at = nth(src, "equal(int", 0, 0);
    let equal_long = nth(src, "equal(long", 0, 0);
    let report_at = nth(src, "report(bool", 0, 0);
    let test_at = nth(src, "test()", 0, 0);
    let use_at = nth(src, "CHECK_EQ", 1, 0);
    let report_call = nth(src, "report(true", 0, 0);
    let expansion = range(src, use_at, "CHECK_EQ".len());
    let outgoing = HashMap::from([(
        pos(src, test_at).0,
        json!([
            {"to": item(src, equal_at, "equal"), "fromRanges": [expansion.clone()]},
            {"to": item(src, equal_long, "equal"), "fromRanges": [expansion.clone()]},
            {"to": item(src, report_at, "report"), "fromRanges": [expansion, range(src, report_call, 6)]}
        ]),
    )]);
    let macro_location = json!([{"targetUri": format!("file:///ws/{PATH}"),
        "targetRange": range(src, macro_at, 8), "targetSelectionRange": range(src, macro_at, 8)}]);
    let definitions = HashMap::from([(pos(src, use_at), macro_location)]);
    let (facts, sem) = run(src, outgoing, definitions);
    let at = callee_span(&facts, "CHECK_EQ", 0);
    let mut edges = edge_targets(&sem, at);
    edges.sort_by(|a, b| a.0.cmp(&b.0));
    let expected = vec![
        ("t.cpp:CHECK_EQ".to_string(), Resolution::Definition),
        ("t.cpp:report".to_string(), Resolution::CallHierarchy),
    ];
    assert_eq!(edges, expected);
    assert_eq!(candidates(&sem, at), None, "{:?}", sem.unresolved);
    assert!(sem.diagnostics.iter().any(|d| d.kind == "macro_invocation"));
    let report = callee_span(&facts, "report", 0);
    assert_eq!(edge_targets(&sem, report), vec![("t.cpp:report".to_string(), Resolution::CallHierarchy)]);
}

/// Rule 2: a converting / copying constructor reported at the range of a resolved call is
/// the compiler's implicit call; the target named like the call is the proven target.
#[test]
fn rule_cpp_implicit_calls_at_a_call_are_not_its_target() {
    let src = "struct Result {\n  Result() {}\n  Result(const Result& other) {}\n};\nResult make_from(int x) { return Result(); }\nResult convert(int x) {\n  return make_from(x);\n}\n";
    let copy_at = nth(src, "Result(const", 0, 0);
    let make_at = nth(src, "make_from(int", 0, 0);
    let convert_at = nth(src, "convert(int", 0, 0);
    let call_at = nth(src, "make_from(x)", 0, 0);
    let from = range(src, call_at, "make_from".len());
    let outgoing = HashMap::from([(
        pos(src, convert_at).0,
        json!([
            {"to": item(src, make_at, "make_from"), "fromRanges": [from.clone()]},
            {"to": item(src, copy_at, "Result"), "fromRanges": [from]}
        ]),
    )]);
    let (facts, sem) = run(src, outgoing, HashMap::new());
    let at = callee_span(&facts, "make_from", 0);
    assert_eq!(edge_targets(&sem, at), vec![("t.cpp:make_from".to_string(), Resolution::CallHierarchy)]);
}

/// Rule 3: a dependent call's overload set is narrowed by the argument list (defaults
/// accepted, too many arguments rejected); one candidate left behind a qualified name is
/// proven. Negatives: a pack expansion may bind more arguments (both stay), and an
/// unqualified call with arguments is narrowed but stays a candidate (argument-dependent
/// lookup at instantiation may add functions).
#[test]
fn rule_cpp_overloads_are_narrowed_by_the_argument_list() {
    let src = "namespace ns {\nvoid f(int a) {}\nvoid f(int a, int b) {}\nvoid h(int a, int b = 0) {}\nvoid h() {}\ntemplate <typename T, typename... Ts>\nvoid run(T t, Ts... ts) {\n  ns::f(t);\n  ns::f(t, ts...);\n  f(t);\n  ns::h(t);\n}\n}\n";
    let f1 = nth(src, "f(int a)", 0, 0);
    let f2 = nth(src, "f(int a, int b)", 0, 0);
    let h1 = nth(src, "h(int a", 0, 0);
    let h2 = nth(src, "h() {}", 0, 0);
    let run_at = nth(src, "run(T", 0, 0);
    let f_calls: Vec<usize> = vec![
        nth(src, "ns::f(t)", 0, 4),
        nth(src, "ns::f(t, ts", 0, 4),
        nth(src, "  f(t);", 0, 2),
    ];
    let h_call = nth(src, "ns::h(t)", 0, 4);
    let mut out = Vec::new();
    for at in &f_calls {
        let from = range(src, *at, 1);
        out.push(json!({"to": item(src, f1, "f"), "fromRanges": [from.clone()]}));
        out.push(json!({"to": item(src, f2, "f"), "fromRanges": [from]}));
    }
    let from = range(src, h_call, 1);
    out.push(json!({"to": item(src, h1, "h"), "fromRanges": [from.clone()]}));
    out.push(json!({"to": item(src, h2, "h"), "fromRanges": [from]}));
    let outgoing = HashMap::from([(pos(src, run_at).0, Value::Array(out))]);
    let (facts, sem) = run(src, outgoing, HashMap::new());
    let qualified = callee_span(&facts, "ns::f", 0);
    assert_eq!(edge_targets(&sem, qualified), vec![("t.cpp:ns.f".to_string(), Resolution::CallHierarchy)]);
    let pack = callee_span(&facts, "ns::f", 1);
    assert!(edge_targets(&sem, pack).is_empty());
    assert_eq!(candidates(&sem, pack).map(|c| c.len()), Some(2));
    let unqualified = callee_span(&facts, "f", 0);
    assert!(edge_targets(&sem, unqualified).is_empty());
    assert_eq!(candidates(&sem, unqualified), Some(vec!["t.cpp:ns.f".to_string()]));
    let defaulted = callee_span(&facts, "ns::h", 0);
    assert_eq!(edge_targets(&sem, defaulted), vec![("t.cpp:ns.h".to_string(), Resolution::CallHierarchy)]);
}

/// Rule 3 (redeclarations): a definition whose prototype declares a default argument is
/// never dropped for too few arguments; a C++ `(void)` list takes no argument.
#[test]
fn rule_cpp_default_arguments_of_a_prototype_keep_the_definition() {
    let src = "struct W {\n  void put(int a, int b = 1);\n  void put(int a, int b, int c) {}\n  void put(void) {}\n  void go() {\n    put(1);\n  }\n};\nvoid W::put(int a, int b) {}\n";
    let def = nth(src, "put(int a, int b) {}", 0, 0);
    let three = nth(src, "put(int a, int b, int c)", 0, 0);
    let none = nth(src, "put(void)", 0, 0);
    let go_at = nth(src, "go()", 0, 0);
    let call_at = nth(src, "put(1)", 0, 0);
    let from = range(src, call_at, 3);
    let outgoing = HashMap::from([(
        pos(src, go_at).0,
        json!([
            {"to": item(src, def, "put"), "fromRanges": [from.clone()]},
            {"to": item(src, three, "put"), "fromRanges": [from.clone()]},
            {"to": item(src, none, "put"), "fromRanges": [from]}
        ]),
    )]);
    let (facts, sem) = run(src, outgoing, HashMap::new());
    let at = callee_span(&facts, "put", 0);
    // Declarations in source order: the prototype, the three-parameter and `(void)`
    // overloads, then the out-of-line definition.
    let edges = edge_targets(&sem, at);
    assert_eq!(edges, vec![("t.cpp:W.put#4".to_string(), Resolution::CallHierarchy)], "{:?}", sem.unresolved);
}

/// Call-hierarchy range over the member identifier of a call with template arguments
/// (`detail::get<int>(x)`): the edge is keyed up to the callee's end, so the call it
/// designates is found by its callee end like every other answer.
#[test]
fn rule_member_range_before_template_arguments_is_keyed_to_the_callee_end() {
    let src = "namespace detail {\ntemplate <typename T> T get(T x) { return x; }\n}\nint use(int x) {\n  return detail::get<int>(x);\n}\n";
    let get_at = nth(src, "get(T x)", 0, 0);
    let use_at = nth(src, "use(int", 0, 0);
    let call_at = nth(src, "get<int>", 0, 0);
    let outgoing = HashMap::from([(
        pos(src, use_at).0,
        json!([{"to": item(src, get_at, "get"), "fromRanges": [range(src, call_at, 3)]}]),
    )]);
    let (facts, sem) = run(src, outgoing, HashMap::new());
    let callee = callee_span(&facts, "detail::get<int>", 0);
    let edge = sem
        .edges
        .iter()
        .find(|e| e.target == "t.cpp:detail.get" || e.target == "t.cpp:get")
        .expect("edge");
    assert_eq!(edge.at, ByteSpan::new(call_at as u32, callee.end));
    assert_eq!(edge.resolution, Resolution::CallHierarchy);
}

/// Rule 3 (lookup completeness): a call qualified by a template type parameter (`T::f`) is
/// looked up per instantiation, so the narrowed candidate stays possible; the same call
/// qualified by a class name is proven.
#[test]
fn rule_cpp_call_qualified_by_a_template_parameter_stays_a_candidate() {
    let src = "struct S {
  static void f(int a) {}
  static void f(int a, int b) {}
};
template <typename T>
void run(int x) {
  T::f(x);
  S::f(x);
}
";
    let f1 = nth(src, "f(int a)", 0, 0);
    let f2 = nth(src, "f(int a, int b)", 0, 0);
    let run_at = nth(src, "run(int", 0, 0);
    let mut out = Vec::new();
    for at in [nth(src, "T::f(x)", 0, 3), nth(src, "S::f(x)", 0, 3)] {
        let from = range(src, at, 1);
        out.push(json!({"to": item(src, f1, "f"), "fromRanges": [from.clone()]}));
        out.push(json!({"to": item(src, f2, "f"), "fromRanges": [from]}));
    }
    let outgoing = HashMap::from([(pos(src, run_at).0, Value::Array(out))]);
    let (facts, sem) = run(src, outgoing, HashMap::new());
    let dependent = callee_span(&facts, "T::f", 0);
    assert!(edge_targets(&sem, dependent).is_empty());
    assert_eq!(candidates(&sem, dependent), Some(vec!["t.cpp:S.f".to_string()]));
    let concrete = callee_span(&facts, "S::f", 0);
    assert_eq!(edge_targets(&sem, concrete), vec![("t.cpp:S.f".to_string(), Resolution::CallHierarchy)]);
}

/// Rule 3 (lookup completeness): membership comes from a class body the syntax tree parses
/// without error; a class specifier holding a syntax error may have swallowed declarations
/// after it, so an unqualified call of its narrowed member stays a candidate. Positive: the
/// same members of an error-free class make the call proven.
#[test]
fn rule_cpp_class_with_syntax_error_gives_no_member_lookup_evidence() {
    for (body, proven) in [("  int x = ;\n", false), ("  int x = 0;\n", true)] {
        let src = format!("struct P {{\n{body}  static void w(int a) {{}}\n  static void w(int a, int b) {{}}\n  void run() {{\n    w(1);\n  }}\n}};\n");
        let w1 = nth(&src, "w(int a)", 0, 0);
        let w2 = nth(&src, "w(int a, int b)", 0, 0);
        let run_at = nth(&src, "run()", 0, 0);
        let from = range(&src, nth(&src, "w(1)", 0, 0), 1);
        let outgoing = HashMap::from([(
            pos(&src, run_at).0,
            json!([
                {"to": item(&src, w1, "w"), "fromRanges": [from.clone()]},
                {"to": item(&src, w2, "w"), "fromRanges": [from]}
            ]),
        )]);
        let (facts, sem) = run(&src, outgoing, HashMap::new());
        let at = callee_span(&facts, "w", 0);
        assert_eq!(!edge_targets(&sem, at).is_empty(), proven, "{body:?}: {:?}", sem.unresolved);
        assert_eq!(candidates(&sem, at).is_some(), !proven, "{body:?}");
    }
}
