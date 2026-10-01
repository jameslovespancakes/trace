//! Bridge tests: hand-made facts assembled with `trace_core::assemble`, and one fixture
//! repository per bridge kind (`tests/fixtures/xlang-<kind>/`) whose facts
//! are extracted with trace-syntax. Every fixture has a unique case (proven / inferred), an
//! ambiguous case (possible rows) and negative controls (no bridge). Proven rows are
//! compared as exact sets: a proven row that is not expected fails the test (gate: 0 wrong
//! proven crossings).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use trace_core::assemble::{assemble, AssembleInput};
use trace_core::config::BridgeSettings;
use trace_core::facts::{BoundaryFact, BoundaryRole, Declaration, FileFacts};
use trace_core::model::{
    Bridge, BridgeKind, ByteSpan, ExecutionModel, FileRecord, Index, IndexHeader, Span, SymbolKind, Tier,
};
use trace_core::source::SourceStore;
use trace_core::{Hash32, Language, SupportLevel};

use trace_library::gate::{BridgeGate, Gate};
use trace_library::installed::InstalledPackages;
use trace_library::model::{ArgSel, BehaviourSource, CallBehaviour, Channel, Effect, VerbSel};
use trace_library::LibraryKnowledge;

use crate::{BridgeInput, BridgeOutput, BRIDGE_VERSION};

// ---------------------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------------------

fn header(root: &str) -> IndexHeader {
    IndexHeader {
        schema: trace_core::SCHEMA_VERSION,
        trace_version: trace_core::TRACE_VERSION.to_string(),
        root: root.to_string(),
        built_unix: 0.0,
        syntax_version: trace_syntax::EXTRACTOR_VERSION,
        infer_version: 0,
        bridge_version: BRIDGE_VERSION,
        inventory_fingerprint: Hash32::default(),
        full_builds: 1,
        incremental_updates: 0,
    }
}

fn record(path: &str, language: Language, bytes: &[u8], facts: Option<FileFacts>) -> FileRecord {
    FileRecord {
        path: path.to_string(),
        language,
        hash: Hash32::of(bytes),
        size: bytes.len() as u64,
        mtime_ns: 0,
        support: if facts.is_some() {
            SupportLevel::Semantic
        } else {
            SupportLevel::Inventoried
        },
        facts,
        semantic: None,
        first_symbol: 0,
        symbol_count: 0,
        diagnostics: Vec::new(),
        pending: None,
    }
}

fn fixture_root(name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name);
    trace_core::inventory::canonical_root(&p).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let mut entries: Vec<_> = fs::read_dir(dir).expect("read fixture dir").flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        if path.is_dir() {
            walk(root, &path, out);
        } else {
            let rel = path
                .strip_prefix(root)
                .expect("inside root")
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, path));
        }
    }
}

/// Index of a fixture repository: syntax facts from trace-syntax, no semantics.
fn build(name: &str) -> (Index, PathBuf) {
    let root = fixture_root(name);
    let mut listed = Vec::new();
    walk(&root, &root, &mut listed);
    let mut files = Vec::new();
    let mut configs = Vec::new();
    for (rel, path) in listed {
        let bytes = fs::read(&path).expect("read fixture file");
        if rel.ends_with("Cargo.toml") {
            configs.push((rel, Hash32::of(&bytes)));
            continue;
        }
        let Some(language) = trace_core::languages::from_path(Path::new(&rel)) else { continue };
        let facts = trace_syntax::grammar(language).and_then(|_| {
            trace_syntax::extract(trace_syntax::SourceInput {
                path: &rel,
                language,
                source: &bytes,
            })
            .ok()
        });
        files.push(record(&rel, language, &bytes, facts));
    }
    let index = assemble(AssembleInput {
        header: header(&root.display().to_string()),
        files,
        configs,
        omitted: Vec::new(),
        support: Vec::new(),
        backend_runs: Vec::new(),
        diagnostics: Vec::new(),
    });
    index.validate().expect("fixture index is valid");
    (index, root)
}

/// A gate where every language's bridge gate passed (derived crossings may be inferred).
fn open_gate() -> Gate {
    let mut gate = Gate::default();
    for l in Language::ALL {
        gate.bridges.insert(l, BridgeGate { passed: true });
    }
    gate
}

fn run_with(index: &Index, root: &Path, config: &BridgeSettings) -> BridgeOutput {
    run_full(index, root, config, &LibraryKnowledge::default(), &InstalledPackages::default(), &open_gate())
}

/// Full detection with the embedded tables and the given knowledge, installed packages and gate.
fn run_full(
    index: &Index,
    root: &Path,
    config: &BridgeSettings,
    knowledge: &LibraryKnowledge,
    installed: &InstalledPackages,
    gate: &Gate,
) -> BridgeOutput {
    let store = SourceStore::with_root(index, root.to_path_buf());
    let tables = trace_library::table::Tables::builtin();
    let input = BridgeInput {
        index,
        sources: &store,
        config,
        knowledge,
        tables: &tables,
        installed,
    };
    let out = crate::run(&input, gate, None);
    check(index, &out);
    out
}

/// Records are valid for the index (ids, locations, tier never above the kind's max), sorted,
/// deduplicated, and carry assumptions.
fn check(index: &Index, out: &BridgeOutput) {
    let mut checked = index.clone();
    checked.bridges = out.bridges.clone();
    checked.validate().expect("bridges are consistent with the index");
    let keys: Vec<_> = out
        .bridges
        .iter()
        .map(|b| (b.kind, b.from, b.to, b.from_at, b.to_at))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(keys, sorted, "bridges are sorted and deduplicated");
    for b in &out.bridges {
        assert!(b.tier >= b.kind.max_tier(), "{} {} exceeds its max tier", b.kind, b.label);
        assert!(b.candidates >= 1);
        if b.candidates > 1 {
            assert_eq!(b.tier, Tier::Possible, "non-unique rows are possible: {}", b.label);
        }
        assert!(!b.assumptions.is_empty(), "{} has assumptions", b.label);
    }
}

// ---------------------------------------------------------------------------------------
// Injected library knowledge (what trace-library derives from installed package source)
// ---------------------------------------------------------------------------------------

fn derived(symbol: &str, effects: Vec<Effect>) -> CallBehaviour {
    CallBehaviour {
        symbol: Some(symbol.to_string()),
        effects,
        source: BehaviourSource::Derived,
        inferred: false,
        reason: "derived from the installed source (test)".to_string(),
    }
}

/// Knowledge key of the `occurrence`-th call whose callee text is `callee` in `path`.
fn call_key(index: &Index, path: &str, callee: &str, occurrence: usize) -> (String, u32) {
    let file = index.file_by_path(path).unwrap_or_else(|| panic!("{path} indexed"));
    let facts = index.files[file.idx()].facts.as_ref().expect("facts");
    let call = facts
        .calls
        .iter()
        .filter(|c| c.callee == callee)
        .nth(occurrence)
        .unwrap_or_else(|| {
            let all: Vec<&str> = facts.calls.iter().map(|c| c.callee.as_str()).collect();
            panic!("call #{occurrence} of `{callee}` in {path}; calls: {all:?}")
        });
    (path.to_string(), call.callee_span.start)
}

fn knowledge(index: &Index, entries: &[(&str, &str, usize, CallBehaviour)]) -> LibraryKnowledge {
    let mut k = LibraryKnowledge::default();
    for (path, callee, occurrence, b) in entries {
        k.by_call
            .insert(call_key(index, path, callee, *occurrence), b.clone());
    }
    k
}

fn sends(channel: Channel, key: ArgSel, verb: VerbSel) -> Effect {
    Effect::Sends { channel, key, verb }
}

fn registers(channel: Channel, key: ArgSel, handler: ArgSel, verb: VerbSel) -> Effect {
    Effect::Registers {
        channel,
        key,
        handler,
        verb,
    }
}

fn decorates(inner: Effect) -> Effect {
    Effect::Decorates {
        inner: Box::new(inner),
    }
}

fn get() -> VerbSel {
    VerbSel::Const("GET".into())
}

fn run(name: &str) -> (Index, BridgeOutput) {
    let (index, root) = build(name);
    let out = run_with(&index, &root, &BridgeSettings::default());
    (index, out)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Row {
    kind: BridgeKind,
    from: String,
    to: String,
    tier: Tier,
    candidates: u32,
    label: String,
}

fn rows(index: &Index, bridges: &[Bridge]) -> Vec<Row> {
    bridges
        .iter()
        .map(|b| Row {
            kind: b.kind,
            from: index.symbol(b.from).uid.clone(),
            to: index.symbol(b.to).uid.clone(),
            tier: b.tier,
            candidates: b.candidates,
            label: b.label.clone(),
        })
        .collect()
}

fn find<'r>(rows: &'r [Row], kind: BridgeKind, from: &str, to: &str) -> Option<&'r Row> {
    rows.iter().find(|r| r.kind == kind && r.from == from && r.to == to)
}

fn assert_row(rows: &[Row], kind: BridgeKind, from: &str, to: &str, tier: Tier) {
    let r =
        find(rows, kind, from, to).unwrap_or_else(|| panic!("missing {kind} {from} -> {to} in {rows:#?}"));
    assert_eq!(r.tier, tier, "{kind} {from} -> {to}");
}

/// Exactly these proven rows (0 wrong proven crossings).
fn assert_proven(rows: &[Row], expected: &[(&str, &str)]) {
    let got: BTreeSet<(String, String)> = rows
        .iter()
        .filter(|r| r.tier == Tier::Proven)
        .map(|r| (r.from.clone(), r.to.clone()))
        .collect();
    let want: BTreeSet<(String, String)> =
        expected.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    assert_eq!(got, want, "proven rows: {rows:#?}");
}

fn ambiguous(rows: &[Row], kind: BridgeKind, from: &str, n: usize) {
    let hits: Vec<&Row> = rows
        .iter()
        .filter(|r| r.kind == kind && r.from == from && r.candidates as usize == n)
        .collect();
    assert!(
        hits.len() >= n && hits.iter().all(|r| r.tier == Tier::Possible),
        "{n} possible {kind} rows from {from}: {rows:#?}"
    );
}

fn none_mentions(rows: &[Row], needle: &str) {
    assert!(
        !rows
            .iter()
            .any(|r| r.from.contains(needle) || r.to.contains(needle) || r.label.contains(needle)),
        "no bridge may mention {needle}: {rows:#?}"
    );
}

// ---------------------------------------------------------------------------------------
// Hand-made facts
// ---------------------------------------------------------------------------------------

fn decl(
    name: &str,
    qualified: &str,
    kind: SymbolKind,
    start: u32,
    end: u32,
    parent: Option<u32>,
) -> Declaration {
    Declaration {
        name: name.into(),
        qualified_name: qualified.into(),
        kind,
        span: Span {
            bytes: ByteSpan::new(start, end),
            start_line: 1,
            end_line: 1,
        },
        name_span: ByteSpan::new(start, start + 1),
        body_start: end,
        parent,
        container: None,
        doc: None,
        decorators: Vec::new(),
        bases: Vec::new(),
        parameters: Vec::new(),
        execution: ExecutionModel::Ordinary,
        is_stub: false,
        is_test: false,
        declaration_lines: Vec::new(),
        identifiers: Vec::new(),
    }
}

fn fact(kind: BridgeKind, role: BoundaryRole, name: &str, decl: Option<u32>, start: u32) -> BoundaryFact {
    BoundaryFact {
        kind,
        role,
        name: name.into(),
        owner: None,
        decl,
        span: ByteSpan::new(start, start + 4),
        line: 1,
        detail: vec![("definition".into(), "true".into()), ("linkage".into(), "c".into())],
    }
}

#[test]
fn hand_made_facts_link_by_abi_names() {
    let dir = tempfile::Builder::new()
        .prefix("trace-fixtures-bridge-")
        .tempdir_in(std::env::temp_dir().join("trace-tests"))
        .expect("temp dir");
    let root = trace_core::inventory::canonical_root(dir.path()).unwrap();
    let java = FileFacts {
        language: Some(Language::Java),
        declarations: vec![
            decl("Native", "Native", SymbolKind::Class, 0, 60, None),
            decl("compute", "Native.compute", SymbolKind::Method, 10, 40, Some(0)),
        ],
        boundaries: vec![fact(BridgeKind::Jni, BoundaryRole::Uses, "Java_Native_compute", Some(1), 10)],
        ..FileFacts::default()
    };
    let c = FileFacts {
        language: Some(Language::C),
        declarations: vec![
            decl("Java_Native_compute", "Java_Native_compute", SymbolKind::Function, 0, 30, None),
            decl("dup", "dup", SymbolKind::Function, 40, 60, None),
        ],
        boundaries: vec![
            fact(BridgeKind::Jni, BoundaryRole::Provides, "Java_Native_compute", Some(0), 0),
            fact(BridgeKind::CAbi, BoundaryRole::Provides, "dup", Some(1), 40),
        ],
        ..FileFacts::default()
    };
    let c2 = FileFacts {
        language: Some(Language::C),
        declarations: vec![decl("dup", "dup", SymbolKind::Function, 0, 20, None)],
        boundaries: vec![fact(BridgeKind::CAbi, BoundaryRole::Provides, "dup", Some(0), 0)],
        ..FileFacts::default()
    };
    let rust = FileFacts {
        language: Some(Language::Rust),
        declarations: vec![decl("dup", "dup", SymbolKind::Function, 0, 20, None)],
        boundaries: vec![{
            let mut f = fact(BridgeKind::CAbi, BoundaryRole::Uses, "dup", Some(0), 0);
            f.detail = vec![("definition".into(), "false".into()), ("linkage".into(), "rust".into())];
            f
        }],
        ..FileFacts::default()
    };
    let files = vec![
        record("Native.java", Language::Java, b"j", Some(java)),
        record("native.c", Language::C, b"c", Some(c)),
        record("other.c", Language::C, b"o", Some(c2)),
        record("lib.rs", Language::Rust, b"r", Some(rust)),
    ];
    let index = assemble(AssembleInput {
        header: header(&root.display().to_string()),
        files,
        configs: Vec::new(),
        omitted: Vec::new(),
        support: Vec::new(),
        backend_runs: Vec::new(),
        diagnostics: Vec::new(),
    });
    let out = run_with(&index, &root, &BridgeSettings::default());
    let r = rows(&index, &out.bridges);
    assert_row(
        &r,
        BridgeKind::Jni,
        "Native.java:Native.compute",
        "native.c:Java_Native_compute",
        Tier::Proven,
    );
    ambiguous(&r, BridgeKind::CAbi, "lib.rs:dup", 2);
    assert_proven(&r, &[("Native.java:Native.compute", "native.c:Java_Native_compute")]);

    // Disabled detection produces nothing.
    let off = BridgeSettings {
        enabled: false,
        ..BridgeSettings::default()
    };
    assert!(run_with(&index, &root, &off).bridges.is_empty());
}

// ---------------------------------------------------------------------------------------
// Fixtures, one per kind
// ---------------------------------------------------------------------------------------

#[test]
fn fixture_c_abi() {
    let (index, out) = run("xlang-c_abi");
    let r = rows(&index, &out.bridges);
    assert_row(&r, BridgeKind::CAbi, "rust/src/lib.rs:c_compress", "c/compress.c:c_compress", Tier::Proven);
    assert_row(&r, BridgeKind::CAbi, "c/api.h:rs_add", "rust/src/lib.rs:rs_add", Tier::Proven);
    ambiguous(&r, BridgeKind::CAbi, "rust/src/lib.rs:c_dup", 2);
    assert_proven(
        &r,
        &[
            ("rust/src/lib.rs:c_compress", "c/compress.c:c_compress"),
            ("c/api.h:rs_add", "rust/src/lib.rs:rs_add"),
        ],
    );
    none_mentions(&r, "c_missing");
    none_mentions(&r, "nobody");
    none_mentions(&r, "local_only");
}

#[test]
fn fixture_c_cpp() {
    let (index, out) = run("xlang-c_cpp");
    let r = rows(&index, &out.bridges);
    assert_row(&r, BridgeKind::CCpp, "c/api.h:cpp_impl", "cpp/impl.cpp:cpp_impl", Tier::Proven);
    ambiguous(&r, BridgeKind::CCpp, "c/api.h:twice", 2);
    assert_proven(&r, &[("c/api.h:cpp_impl", "cpp/impl.cpp:cpp_impl")]);
    none_mentions(&r, "mangled_only");
}

#[test]
fn fixture_jni() {
    let (index, out) = run("xlang-jni");
    let r = rows(&index, &out.bridges);
    let java = "java/com/example/Native.java";
    assert_row(
        &r,
        BridgeKind::Jni,
        &format!("{java}:Native.compute"),
        "c/native.c:Java_com_example_Native_compute",
        Tier::Proven,
    );
    assert_row(
        &r,
        BridgeKind::Jni,
        &format!("{java}:Native.over_load"),
        "c/native.c:Java_com_example_Native_over_1load__I",
        Tier::Proven,
    );
    ambiguous(&r, BridgeKind::Jni, &format!("{java}:Native.Inner.greet"), 2);
    assert_proven(
        &r,
        &[
            (&format!("{java}:Native.compute"), "c/native.c:Java_com_example_Native_compute"),
            (&format!("{java}:Native.over_load"), "c/native.c:Java_com_example_Native_over_1load__I"),
        ],
    );
    none_mentions(&r, "Other_compute");
    none_mentions(&r, "notNative");
}

#[test]
fn fixture_cgo() {
    let (index, out) = run("xlang-cgo");
    let r = rows(&index, &out.bridges);
    let proven: Vec<&Row> = r.iter().filter(|x| x.tier == Tier::Proven).collect();
    assert!(
        proven
            .iter()
            .any(|x| x.label == "C.inline_add" && x.from == "go/main.go:compute"),
        "{r:#?}"
    );
    assert_row(&r, BridgeKind::Cgo, "go/main.go:compute", "c/lib.c:lib_mul", Tier::Proven);
    assert_eq!(proven.len(), 2, "only inline_add and lib_mul are proven: {r:#?}");
    let dup: Vec<&Row> = r.iter().filter(|x| x.label == "C.dup").collect();
    assert_eq!(dup.len(), 2);
    assert!(dup.iter().all(|x| x.tier == Tier::Possible && x.candidates == 2));
    none_mentions(&r, "not_defined");
    none_mentions(&r, "CString");
}

/// cgo `//export name`: the C prototype links to the exported Go function (proven when it
/// is the only definition of the symbol, possible next to a same-named C definition); a
/// comment that is not the directive exports nothing.
#[test]
fn fixture_cgo_export() {
    let (index, out) = run("xlang-cgo-export");
    let r = rows(&index, &out.bridges);
    assert_row(&r, BridgeKind::Cgo, "c/unlock.c:wait_for_unlock", "go/notify.go:waitForUnlock", Tier::Proven);
    assert_proven(&r, &[("c/unlock.c:wait_for_unlock", "go/notify.go:waitForUnlock")]);
    ambiguous(&r, BridgeKind::Cgo, "c/unlock.c:dup_symbol", 2);
    none_mentions(&r, "not_exported");
    none_mentions(&r, "notExported");
}

#[test]
fn fixture_pyo3() {
    let (index, out) = run("xlang-pyo3");
    let r = rows(&index, &out.bridges);
    let from = "py/pkg/app.py:run";
    assert_row(&r, BridgeKind::Pyo3, from, "rust/src/lib.rs:fast_sum", Tier::Proven);
    assert_row(&r, BridgeKind::Pyo3, from, "rust/src/lib.rs:internal_name", Tier::Proven);
    assert_row(&r, BridgeKind::Pyo3, from, "rust/src/lib.rs:Counter.new", Tier::Proven);
    ambiguous(&r, BridgeKind::Pyo3, from, 2);
    assert_proven(
        &r,
        &[
            (from, "rust/src/lib.rs:fast_sum"),
            (from, "rust/src/lib.rs:internal_name"),
            (from, "rust/src/lib.rs:Counter.new"),
        ],
    );
    none_mentions(&r, "missing");
    none_mentions(&r, "py/pkg/util.py");
}

#[test]
fn fixture_python_stub() {
    let (index, out) = run("xlang-python_stub");
    let r = rows(&index, &out.bridges);
    let stub = "py/pkg/_engine.pyi";
    assert_row(&r, BridgeKind::PythonStub, &format!("{stub}:Engine"), "rust/src/lib.rs:Engine", Tier::Proven);
    assert_row(
        &r,
        BridgeKind::PythonStub,
        &format!("{stub}:Engine.__init__"),
        "rust/src/lib.rs:Engine.create",
        Tier::Proven,
    );
    assert_row(
        &r,
        BridgeKind::PythonStub,
        &format!("{stub}:Engine.start"),
        "rust/src/lib.rs:Engine.start",
        Tier::Proven,
    );
    ambiguous(&r, BridgeKind::PythonStub, &format!("{stub}:version"), 2);
    assert_proven(
        &r,
        &[
            (&format!("{stub}:Engine"), "rust/src/lib.rs:Engine"),
            (&format!("{stub}:Engine.__init__"), "rust/src/lib.rs:Engine.create"),
            (&format!("{stub}:Engine.start"), "rust/src/lib.rs:Engine.start"),
        ],
    );
    none_mentions(&r, "Engine.stop");
    none_mentions(&r, "plain");
}

#[test]
fn fixture_wasm_bindgen() {
    let (index, out) = run("xlang-wasm_bindgen");
    let r = rows(&index, &out.bridges);
    let from = "www/index.js:main";
    let module = "www/index.js:<module>";
    assert_row(&r, BridgeKind::WasmBindgen, from, "src/lib.rs:Universe.new", Tier::Proven);
    assert_row(&r, BridgeKind::WasmBindgen, from, "src/lib.rs:greet_user", Tier::Proven);
    // `const universe = Universe.new()` (new() returns the exported class): its method calls
    // are the class's exports; an enum variant read is the exported enum.
    assert_row(&r, BridgeKind::WasmBindgen, from, "src/lib.rs:Universe.tick", Tier::Proven);
    assert_row(&r, BridgeKind::WasmBindgen, from, "src/lib.rs:Cell", Tier::Proven);
    // Every imported export is bound by the import itself.
    assert_row(&r, BridgeKind::WasmBindgen, module, "src/lib.rs:Universe", Tier::Proven);
    let render: Vec<&Row> = r.iter().filter(|x| x.label == "render").collect();
    assert_eq!(render.len(), 4, "call + import, two crates each: {r:#?}");
    assert!(render.iter().all(|x| x.tier == Tier::Possible));
    assert_proven(
        &r,
        &[
            (from, "src/lib.rs:Universe.new"),
            (from, "src/lib.rs:greet_user"),
            (from, "src/lib.rs:Universe.tick"),
            (from, "src/lib.rs:Cell"),
            ("www/index.js:rebound", "src/lib.rs:Universe.new"),
            (module, "src/lib.rs:Universe"),
            (module, "src/lib.rs:Cell"),
            (module, "src/lib.rs:greet_user"),
        ],
    );
    // A parameter rebinding the name, and a name bound twice, carry no instance type.
    none_mentions(&r, "shadowed");
    assert!(
        !r.iter()
            .any(|x| x.from == "www/index.js:rebound" && x.to.ends_with("Universe.tick")),
        "{r:#?}"
    );
    none_mentions(&r, "local.js");
    none_mentions(&r, "private_helper");
    none_mentions(&r, "alive");
    none_mentions(&r, "helper");
}

#[test]
fn fixture_napi() {
    let (index, out) = run("xlang-napi");
    let r = rows(&index, &out.bridges);
    let from = "js/index.js:main";
    assert_row(&r, BridgeKind::Napi, from, "addon/src/lib.rs:sum_values", Tier::Proven);
    assert_row(&r, BridgeKind::Napi, from, "addon/native.c:Hello", Tier::Possible);
    assert_row(&r, BridgeKind::Napi, from, "addon/native2.c:HelloAgain", Tier::Possible);
    assert_proven(&r, &[(from, "addon/src/lib.rs:sum_values")]);
    none_mentions(&r, "unknownFn");
    none_mentions(&r, "util.js");
}

#[test]
fn fixture_cpython() {
    let (index, out) = run("xlang-cpython");
    let r = rows(&index, &out.bridges);
    let from = "py/app.py:main";
    assert_row(&r, BridgeKind::Cpython, from, "ext/spam.c:spam_system", Tier::Proven);
    assert_row(&r, BridgeKind::Cpython, from, "ext/ham.c:ham_shared", Tier::Possible);
    assert_row(&r, BridgeKind::Cpython, from, "ext/jam.c:jam_shared", Tier::Possible);
    assert_proven(&r, &[(from, "ext/spam.c:spam_system")]);
    none_mentions(&r, "eggs");
    none_mentions(&r, "nothing");
}

#[test]
fn fixture_grpc() {
    let (index, out) = run("xlang-grpc");
    let r = rows(&index, &out.bridges);
    let from = "client/client.py:run";
    assert_row(&r, BridgeKind::Grpc, from, "server/server.go:server.SayHello", Tier::Inferred);
    assert_row(&r, BridgeKind::Grpc, from, "server/server.go:server.SayGoodbye", Tier::Possible);
    assert_row(&r, BridgeKind::Grpc, from, "server/legacy.go:legacy.SayGoodbye", Tier::Possible);
    // A stub created inline by the generated Go factory; the contract is declared by two
    // identical .proto copies (one wire method), so the match stays inferred.
    assert_row(
        &r,
        BridgeKind::Grpc,
        "goclient/client.go:Hello",
        "server/server.go:server.SayHello",
        Tier::Inferred,
    );
    none_mentions(&r, "goclient/client.go:Local");
    // Node handler table entry `sayHi: GreeterServer.sayHiHandler.bind(this)`.
    assert_row(
        &r,
        BridgeKind::Grpc,
        "goclient/client.go:Hi",
        "server/node/server.js:GreeterServer.sayHiHandler",
        Tier::Inferred,
    );
    none_mentions(&r, "GreeterServer.register");
    // Unqualified C# client (`using static`), servicer subclasses of an abstract base.
    let wave = "csclient/WaveClient.cs:WaveClient.Run";
    assert_row(&r, BridgeKind::Grpc, wave, "server/py/wave_server.py:LoudWaver.Wave", Tier::Possible);
    assert_row(&r, BridgeKind::Grpc, wave, "server/py/wave_server.py:QuietWaver.Wave", Tier::Possible);
    none_mentions(&r, "Unrelated");
    let hello = out
        .bridges
        .iter()
        .find(|b| b.label.ends_with("Greeter/SayHello"))
        .expect("SayHello row");
    assert_eq!(hello.label, "helloworld.Greeter/SayHello");
    let proto = index.file_by_path("protos/greeter.proto").expect("proto indexed");
    assert_eq!(hello.contract, Some(proto));
    assert!(hello.assumptions.iter().any(|a| a.contains("2 .proto copies")), "{:?}", hello.assumptions);
    assert_proven(&r, &[]);
    none_mentions(&r, "NotAnRpc");
    none_mentions(&r, "say_hello_locally");
}

#[test]
fn fixture_graphql() {
    let (index, out) = run("xlang-graphql");
    let r = rows(&index, &out.bridges);
    let from = "web/queries.ts:load";
    let user: Vec<&Row> = r.iter().filter(|x| x.label == "Query.user").collect();
    assert_eq!(user.len(), 1, "{r:#?}");
    assert_eq!(user[0].tier, Tier::Inferred);
    assert!(user[0].to.starts_with("server/resolvers.js:"));
    assert_row(&r, BridgeKind::Graphql, from, "server/resolvers.js:addPostImpl", Tier::Inferred);
    let posts: Vec<&Row> = r.iter().filter(|x| x.label == "Query.posts").collect();
    assert_eq!(posts.len(), 2, "{r:#?}");
    assert!(posts.iter().all(|x| x.tier == Tier::Possible));
    assert!(posts.iter().any(|x| x.to == "server/schema.py:Query.posts"));
    assert_proven(&r, &[]);
    none_mentions(&r, "stats");
    none_mentions(&r, "helper");
}

#[test]
fn fixture_openapi() {
    let (index, root) = build("xlang-openapi");
    let route = |verb: &str| {
        decorates(registers(Channel::Http, ArgSel::Pos(0), ArgSel::Pos(0), VerbSel::Const(verb.into())))
    };
    let fetch = || {
        derived(
            "fetch",
            vec![sends(
                Channel::Http,
                ArgSel::Pos(0),
                VerbSel::Arg(ArgSel::Field {
                    arg: 1,
                    field: "method".into(),
                }),
            )],
        )
    };
    let own = Effect::Mounts {
        key: ArgSel::Kw("prefix".into()),
        target: ArgSel::Receiver,
    };
    let mount = Effect::Mounts {
        key: ArgSel::Kw("prefix".into()),
        target: ArgSel::Pos(0),
    };
    let k = knowledge(
        &index,
        &[
            ("server/items.py", "APIRouter", 0, derived("APIRouter", vec![own])),
            ("server/items.py", "router.get", 0, derived("APIRouter.get", vec![route("GET")])),
            ("server/items.py", "router.post", 0, derived("APIRouter.post", vec![route("POST")])),
            ("server/legacy.py", "legacy_app.post", 0, derived("FastAPI.post", vec![route("POST")])),
            ("server/main.py", "app.include_router", 0, derived("FastAPI.include_router", vec![mount])),
            ("web/client.ts", "fetch", 0, fetch()),
            ("web/client.ts", "fetch", 1, fetch()),
            ("web/client.ts", "fetch", 2, fetch()),
        ],
    );
    let out =
        run_full(&index, &root, &BridgeSettings::default(), &k, &InstalledPackages::default(), &open_gate());
    let r = rows(&index, &out.bridges);
    assert_row(
        &r,
        BridgeKind::Openapi,
        "web/client.ts:readItem",
        "server/items.py:read_item",
        Tier::Inferred,
    );
    let contract = index.file_by_path("openapi.yaml").expect("contract indexed");
    let read = out
        .bridges
        .iter()
        .find(|b| b.kind == BridgeKind::Openapi && b.tier == Tier::Inferred)
        .unwrap();
    assert_eq!(read.contract, Some(contract));
    assert_row(
        &r,
        BridgeKind::Openapi,
        "web/client.ts:createItem",
        "server/items.py:create_item",
        Tier::Possible,
    );
    assert_row(
        &r,
        BridgeKind::Openapi,
        "web/client.ts:createItem",
        "server/legacy.py:create_item",
        Tier::Possible,
    );
    assert_proven(&r, &[]);
    none_mentions(&r, "health");
    assert!(
        !out.diagnostics.iter().any(|d| d.kind == "contract_unparsed"),
        "the YAML contract parses: {:?}",
        out.diagnostics
    );
}

#[test]
fn fixture_js_ts() {
    let (index, out) = run("xlang-js_ts");
    let r = rows(&index, &out.bridges);
    assert_row(&r, BridgeKind::JsTs, "lib/math.d.ts:add", "lib/math.js:add", Tier::Proven);
    assert_row(&r, BridgeKind::JsTs, "lib/math.d.ts:Calc", "lib/math.js:Calc", Tier::Proven);
    assert_row(&r, BridgeKind::JsTs, "lib/math.d.ts:Calc.mul", "lib/math.js:Calc.mul", Tier::Proven);
    ambiguous(&r, BridgeKind::JsTs, "lib/math.d.ts:sub", 2);
    assert_proven(
        &r,
        &[
            ("lib/math.d.ts:add", "lib/math.js:add"),
            ("lib/math.d.ts:Calc", "lib/math.js:Calc"),
            ("lib/math.d.ts:Calc.mul", "lib/math.js:Calc.mul"),
        ],
    );
    none_mentions(&r, "missing");
    none_mentions(&r, "orphan");
}

#[test]
fn fixture_user_contract() {
    let (index, root) = build("xlang-user_contract");
    let sym = |uid: &str| {
        index
            .symbols
            .iter()
            .find(|s| s.uid == uid)
            .unwrap_or_else(|| panic!("{uid}"))
    };
    let from = sym("client/submit.js:submit_job");
    let to = sym("service/handler.py:handle_job");
    let sha = |path: &str| crate::manifest::sha256_hex(&fs::read(root.join(path)).unwrap());
    let endpoint = |path: &str, s: &trace_core::model::Symbol| serde_json::json!({"file": path, "sha256": sha(path), "start_byte": s.span.bytes.start, "end_byte": s.span.bytes.end});
    let good = serde_json::json!({
        "kind": "contract_link",
        "from": endpoint("client/submit.js", from),
        "to": endpoint("service/handler.py", to),
        "evidence": [endpoint("client/submit.js", from)],
        "assumptions": ["jobs pushed to QUEUE are consumed by handle_job"],
        "label": "queue:jobs"
    });
    let mut stale = good.clone();
    stale["to"]["sha256"] = serde_json::Value::String("0".repeat(64));
    let mut invalid = good.clone();
    invalid["kind"] = serde_json::Value::String("guess".into());
    let dir = tempfile::Builder::new()
        .prefix("trace-fixtures-bridge-manifest-")
        .tempdir_in(std::env::temp_dir().join("trace-tests"))
        .unwrap();
    let manifest = dir.path().join("bridges.json");
    fs::write(
        &manifest,
        serde_json::to_vec(&serde_json::json!({"schema": 1, "bridges": [good, stale, invalid]})).unwrap(),
    )
    .unwrap();
    let config = BridgeSettings {
        manifests: vec![manifest],
        ..BridgeSettings::default()
    };
    let out = run_with(&index, &root, &config);
    let r = rows(&index, &out.bridges);
    assert_eq!(r.len(), 1, "{r:#?}");
    assert_row(
        &r,
        BridgeKind::UserContract,
        "client/submit.js:submit_job",
        "service/handler.py:handle_job",
        Tier::Inferred,
    );
    assert_eq!(r[0].label, "queue:jobs");
    let kinds: Vec<&str> = out.diagnostics.iter().map(|d| d.kind.as_str()).collect();
    assert!(kinds.contains(&"bridge_manifest_stale"), "{kinds:?}");
    assert!(kinds.contains(&"bridge_manifest_invalid"), "{kinds:?}");
    none_mentions(&r, "unrelated");
}

/// Every fixture: bridges survive a cache round trip of the index validation and no
/// fixture produces a proven row for a weak or contract kind.
#[test]
fn every_fixture_respects_tier_limits() {
    for name in [
        "xlang-c_abi",
        "xlang-c_cpp",
        "xlang-jni",
        "xlang-cgo",
        "xlang-cgo-export",
        "xlang-pyo3",
        "xlang-python_stub",
        "xlang-wasm_bindgen",
        "xlang-napi",
        "xlang-cpython",
        "xlang-grpc",
        "xlang-graphql",
        "xlang-openapi",
        "xlang-http",
        "xlang-js_ts",
        "xlang-subprocess",
        "xlang-ffi",
        "xlang-message",
        "xlang-user_contract",
    ] {
        let (_, out) = run(name);
        for b in &out.bridges {
            match b.kind {
                BridgeKind::Subprocess | BridgeKind::Ffi | BridgeKind::Message => {
                    assert_eq!(b.tier, Tier::Possible)
                }
                BridgeKind::Grpc
                | BridgeKind::Openapi
                | BridgeKind::Graphql
                | BridgeKind::Http
                | BridgeKind::UserContract => {
                    assert_ne!(b.tier, Tier::Proven)
                }
                _ => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Derived channel effects (PLAN decision 14): endpoints from library knowledge
// ---------------------------------------------------------------------------------------

/// Knowledge of the xlang-http fixture as trace-library derives it from the installed
/// packages (route registration calls, a decorator factory, a mount, client calls).
fn http_knowledge(index: &Index) -> LibraryKnowledge {
    let reg = |verb: VerbSel| registers(Channel::Http, ArgSel::Pos(0), ArgSel::Last, verb);
    let fetch = || {
        derived(
            "fetch",
            vec![sends(
                Channel::Http,
                ArgSel::Pos(0),
                VerbSel::Arg(ArgSel::Field {
                    arg: 1,
                    field: "method".into(),
                }),
            )],
        )
    };
    let route = decorates(registers(
        Channel::Http,
        ArgSel::Pos(0),
        ArgSel::Pos(0),
        VerbSel::Arg(ArgSel::Kw("methods".into())),
    ));
    let mount = Effect::Mounts {
        key: ArgSel::Pos(0),
        target: ArgSel::Last,
    };
    knowledge(
        index,
        &[
            ("server/app.js", "app.get", 0, derived("Router.get", vec![reg(get())])),
            (
                "server/app.js",
                "app.post",
                0,
                derived("Router.post", vec![reg(VerbSel::Const("POST".into()))]),
            ),
            ("server/app.js", "router.get", 0, derived("Router.get", vec![reg(get())])),
            ("server/app.js", "app.use", 0, derived("Router.use", vec![mount])),
            ("server/api.py", "app.route", 0, derived("Scaffold.route", vec![route.clone()])),
            ("server/api.py", "app.route", 1, derived("Scaffold.route", vec![route])),
            ("web/client.ts", "fetch", 0, fetch()),
            ("web/client.ts", "fetch", 1, fetch()),
            ("web/client.ts", "fetch", 2, fetch()),
            ("web/client.ts", "fetch", 3, fetch()),
            (
                "web/client.ts",
                "axios.post",
                0,
                derived(
                    "Axios.post",
                    vec![sends(Channel::Http, ArgSel::Pos(0), VerbSel::Const("POST".into()))],
                ),
            ),
        ],
    )
}

fn endpoints_of(index: &Index, root: &Path, k: &LibraryKnowledge, path: &str) -> Vec<BoundaryFact> {
    let store = SourceStore::with_root(index, root.to_path_buf());
    let tables = trace_library::table::Tables::builtin();
    let installed = InstalledPackages::default();
    let config = BridgeSettings::default();
    let input = BridgeInput {
        index,
        sources: &store,
        config: &config,
        knowledge: k,
        tables: &tables,
        installed: &installed,
    };
    let file = index.file_by_path(path).expect("file");
    crate::test_support::file_endpoints(&input, file)
        .into_iter()
        .map(|e| e.fact)
        .collect()
}

fn detail_of<'f>(f: &'f BoundaryFact, key: &str) -> Option<&'f str> {
    f.detail.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn temp_root() -> PathBuf {
    let dir = std::env::temp_dir().join("trace-tests");
    fs::create_dir_all(&dir).expect("temp root");
    dir
}

fn node_packages(dir: &Path, names: &[&str]) -> InstalledPackages {
    let nm = dir.join("node_modules");
    for n in names {
        fs::create_dir_all(nm.join(n)).expect("package dir");
    }
    InstalledPackages::from_roots(&[trace_env::LibraryRoot {
        path: nm,
        kind: trace_env::LibraryKind::Dependency,
        ecosystem: trace_env::EcosystemId::Node,
        layout: "node_modules",
        version: None,
    }])
}

#[test]
fn rule_endpoint_key_from_argument_template() {
    let (index, root) = build("xlang-http");
    let k = http_knowledge(&index);
    let ends = endpoints_of(&index, &root, &k, "web/client.ts");
    let names: BTreeSet<String> = ends
        .iter()
        .filter(|f| f.kind == BridgeKind::Http && f.role == BoundaryRole::Uses)
        .map(|f| f.name.clone())
        .collect();
    // Template literal with a placeholder, a literal method constant, an origin stripped,
    // the Fetch default method; a dynamic URL gives no endpoint.
    assert!(names.contains("GET /users/{}"), "{names:?}");
    assert!(names.contains("POST /users"), "{names:?}");
    assert!(names.contains("GET /api/status"), "{names:?}");
    assert!(names.contains("GET /nothing/here"), "{names:?}");
    assert_eq!(names.len(), 4, "the dynamic `fetch(url)` has no key: {names:?}");
    assert!(ends
        .iter()
        .all(|f| detail_of(f, crate::recognize::DERIVED) == Some("true")));
    // Without knowledge, no package call is recognized (trace names no framework).
    let none = endpoints_of(&index, &root, &LibraryKnowledge::default(), "web/client.ts");
    assert!(none.iter().all(|f| f.kind != BridgeKind::Http), "{none:?}");
}

#[test]
fn rule_decorated_handler_gets_decorator_key() {
    let (index, root) = build("xlang-http");
    let k = http_knowledge(&index);
    let ends = endpoints_of(&index, &root, &k, "server/api.py");
    let facts = index.files[index.file_by_path("server/api.py").expect("api.py").idx()]
        .facts
        .as_ref()
        .expect("facts");
    let decl_named = |name: &str| {
        facts
            .declarations
            .iter()
            .position(|d| d.name == name)
            .map(|i| i as u32)
    };
    let user = ends
        .iter()
        .find(|f| f.name == "GET /users/{}")
        .unwrap_or_else(|| panic!("{ends:#?}"));
    assert_eq!(user.decl, decl_named("get_user_py"), "the decorated function is the handler");
    let reports = ends
        .iter()
        .find(|f| f.name == "* /reports")
        .unwrap_or_else(|| panic!("{ends:#?}"));
    assert_eq!(reports.decl, decl_named("reports"), "no methods argument: any method");
}

#[test]
fn rule_derived_routes_and_clients_link() {
    let (index, root) = build("xlang-http");
    let k = http_knowledge(&index);
    let none = InstalledPackages::default();
    let out = run_full(&index, &root, &BridgeSettings::default(), &k, &none, &open_gate());
    let r = rows(&index, &out.bridges);
    assert_row(&r, BridgeKind::Http, "web/client.ts:loadUser", "server/app.js:getUser", Tier::Possible);
    assert_row(&r, BridgeKind::Http, "web/client.ts:loadUser", "server/api.py:get_user_py", Tier::Possible);
    let create: Vec<&Row> = r.iter().filter(|x| x.from == "web/client.ts:createUser").collect();
    assert_eq!(create.len(), 1, "{r:#?}");
    assert_eq!(create[0].tier, Tier::Inferred);
    assert!(create[0].to.starts_with("server/app.js:"));
    assert_eq!(create[0].label, "POST /users");
    // `router.get("/status")` mounted with `app.use("/api", router)`.
    assert_row(&r, BridgeKind::Http, "web/client.ts:status", "server/app.js:status", Tier::Inferred);
    assert_proven(&r, &[]);
    for negative in [
        "web/client.ts:dynamic",
        "web/client.ts:missing",
        "web/client.ts:notHttp",
        "reports",
    ] {
        none_mentions(&r, negative);
    }
    // `http: false` turns route matching off; `enabled: false` turns every bridge off.
    let no_http = BridgeSettings {
        http: false,
        ..BridgeSettings::default()
    };
    let out = run_full(&index, &root, &no_http, &k, &none, &open_gate());
    assert!(out.bridges.iter().all(|b| b.kind != BridgeKind::Http));
    let off = BridgeSettings {
        enabled: false,
        ..BridgeSettings::default()
    };
    assert!(run_full(&index, &root, &off, &k, &none, &open_gate())
        .bridges
        .is_empty());
}

#[test]
fn rule_derived_crossing_below_gate_is_possible() {
    let (index, root) = build("xlang-http");
    let k = http_knowledge(&index);
    let none = InstalledPackages::default();
    let out = run_full(&index, &root, &BridgeSettings::default(), &k, &none, &Gate::default());
    let create: Vec<&Bridge> = out
        .bridges
        .iter()
        .filter(|b| index.symbol(b.from).uid == "web/client.ts:createUser")
        .collect();
    assert_eq!(create.len(), 1, "{:#?}", out.bridges);
    assert_eq!(create[0].tier, Tier::Possible);
    assert!(create[0].assumptions.iter().any(|a| a.contains("bridge gate")), "{:?}", create[0].assumptions);
    // Every derived end's language must have passed its gate.
    let mut half = Gate::default();
    half.bridges.insert(Language::JavaScript, BridgeGate { passed: true });
    let out = run_full(&index, &root, &BridgeSettings::default(), &k, &none, &half);
    let r = rows(&index, &out.bridges);
    assert_row(&r, BridgeKind::Http, "web/client.ts:status", "server/app.js:status", Tier::Inferred);
    let mut other = Gate::default();
    other.bridges.insert(Language::Python, BridgeGate { passed: true });
    let out = run_full(&index, &root, &BridgeSettings::default(), &k, &none, &other);
    let r = rows(&index, &out.bridges);
    assert_row(&r, BridgeKind::Http, "web/client.ts:status", "server/app.js:status", Tier::Possible);
}

#[test]
fn rule_contract_crossing_stays_proven() {
    let (index, root) = build("xlang-c_abi");
    let out = run_full(
        &index,
        &root,
        &BridgeSettings::default(),
        &LibraryKnowledge::default(),
        &InstalledPackages::default(),
        &Gate::default(),
    );
    let r = rows(&index, &out.bridges);
    assert_row(&r, BridgeKind::CAbi, "rust/src/lib.rs:c_compress", "c/compress.c:c_compress", Tier::Proven);
    assert_row(&r, BridgeKind::CAbi, "c/api.h:rs_add", "rust/src/lib.rs:rs_add", Tier::Proven);
}

/// fastapi docs apps include one router several times under the same prefix: mount chains
/// are deduplicated per distinct prefix, so the client matches one route (inferred) instead
/// of identical copies (possible).
#[test]
fn rule_mounted_prefix_joins_route_key() {
    let (index, root) = build("xlang-http-mounts");
    let mount = || {
        derived(
            "FastAPI.include_router",
            vec![Effect::Mounts {
                key: ArgSel::Kw("prefix".into()),
                target: ArgSel::Pos(0),
            }],
        )
    };
    let route = decorates(registers(Channel::Http, ArgSel::Pos(0), ArgSel::Pos(0), get()));
    let k = knowledge(
        &index,
        &[
            ("server/main.py", "app.include_router", 0, mount()),
            ("server/main.py", "app.include_router", 1, mount()),
            ("server/routers/items.py", "router.get", 0, derived("APIRouter.get", vec![route])),
            (
                "web/client.ts",
                "fetch",
                0,
                derived("fetch", vec![sends(Channel::Http, ArgSel::Pos(0), VerbSel::Any)]),
            ),
        ],
    );
    let out =
        run_full(&index, &root, &BridgeSettings::default(), &k, &InstalledPackages::default(), &open_gate());
    let r = rows(&index, &out.bridges);
    let hits: Vec<&Row> = r.iter().filter(|x| x.from == "web/client.ts:loadItem").collect();
    assert_eq!(hits.len(), 1, "{r:#?}");
    assert_eq!(hits[0].to, "server/routers/items.py:read_item");
    assert_eq!(hits[0].tier, Tier::Inferred);
}

/// full-stack-fastapi-template: `include_router(api_router, prefix=settings.API_V1_STR)`.
/// The prefix is the class attribute default of the module-level `settings = Settings()`
/// in the imported config module: resolved (inferred, with the assumption stated). A prefix
/// whose attribute is bound twice stays unknown (suffix match, possible). The routers' own
/// prefixes (`APIRouter(prefix="/items")`) are constructor arguments the derivation reports
/// as a mount of the constructed object.
#[test]
fn rule_constant_mount_prefix_resolves_through_imports() {
    let (index, root) = build("xlang-http-const-prefix");
    let mount = || {
        derived(
            "FastAPI.include_router",
            vec![Effect::Mounts {
                key: ArgSel::Kw("prefix".into()),
                target: ArgSel::Pos(0),
            }],
        )
    };
    let own = || {
        derived(
            "APIRouter",
            vec![Effect::Mounts {
                key: ArgSel::Kw("prefix".into()),
                target: ArgSel::Receiver,
            }],
        )
    };
    let route = || {
        derived(
            "APIRouter.get",
            vec![decorates(registers(Channel::Http, ArgSel::Pos(0), ArgSel::Pos(0), get()))],
        )
    };
    let fetch = || derived("fetch", vec![sends(Channel::Http, ArgSel::Pos(0), VerbSel::Any)]);
    let k = knowledge(
        &index,
        &[
            ("server/main.py", "app.include_router", 0, mount()),
            ("server/main.py", "app.include_router", 1, mount()),
            ("server/routers/items.py", "APIRouter", 0, own()),
            ("server/routers/items.py", "router.get", 0, route()),
            ("server/routers/users.py", "APIRouter", 0, own()),
            ("server/routers/users.py", "router.get", 0, route()),
            ("web/client.ts", "fetch", 0, fetch()),
            ("web/client.ts", "fetch", 1, fetch()),
        ],
    );
    let out =
        run_full(&index, &root, &BridgeSettings::default(), &k, &InstalledPackages::default(), &open_gate());
    let items: Vec<&Bridge> = out
        .bridges
        .iter()
        .filter(|b| index.symbol(b.from).uid == "web/client.ts:loadItem")
        .collect();
    assert_eq!(items.len(), 1, "{:#?}", out.bridges);
    assert_eq!(index.symbol(items[0].to).uid, "server/routers/items.py:read_item");
    assert_eq!(items[0].tier, Tier::Inferred);
    assert!(
        items[0]
            .assumptions
            .iter()
            .any(|a| a.contains("settings.API_V1_STR") && a.contains("/api/v1")),
        "{:?}",
        items[0].assumptions
    );
    let users: Vec<&Bridge> = out
        .bridges
        .iter()
        .filter(|b| index.symbol(b.from).uid == "web/client.ts:loadUser")
        .collect();
    assert_eq!(users.len(), 1, "{:#?}", out.bridges);
    assert_eq!(index.symbol(users[0].to).uid, "server/routers/users.py:read_user");
    assert_eq!(users[0].tier, Tier::Possible);
}

#[test]
fn rule_process_send_resolves_repository_script() {
    let (index, root) = build("xlang-subprocess");
    let spawn = |symbol: &str| derived(symbol, vec![sends(Channel::Process, ArgSel::Pos(0), VerbSel::Any)]);
    let k = knowledge(
        &index,
        &[
            ("tools/run.js", "spawn", 0, spawn("child_process.spawn")),
            ("tools/run.js", "spawn", 1, spawn("child_process.spawn")),
            ("tools/run.js", "execSync", 0, spawn("child_process.execSync")),
            ("scripts/build.py", "subprocess.run", 0, spawn("subprocess.run")),
        ],
    );
    let out =
        run_full(&index, &root, &BridgeSettings::default(), &k, &InstalledPackages::default(), &open_gate());
    let r = rows(&index, &out.bridges);
    let build_rows: Vec<&Row> = r.iter().filter(|x| x.from == "tools/run.js:build").collect();
    assert_eq!(build_rows.len(), 1, "{r:#?}");
    assert!(build_rows[0].to.starts_with("scripts/build.py:"));
    assert_eq!(build_rows[0].tier, Tier::Possible);
    let serve: Vec<&Row> = r.iter().filter(|x| x.from == "scripts/build.py:main").collect();
    assert_eq!(serve.len(), 1, "{r:#?}");
    assert!(serve[0].to.starts_with("tools/serve.js:"));
    assert_proven(&r, &[]);
    // `helper.sh` names two repository files: an ambiguous path, no bridge.
    none_mentions(&r, "helper.sh");
}

#[test]
fn rule_message_topic_links_across_languages() {
    let (index, root) = build("xlang-message");
    let publish = || derived("Redis.publish", vec![sends(Channel::Message, ArgSel::Pos(0), VerbSel::Any)]);
    let subscribe = || {
        derived(
            "PubSub.subscribe",
            vec![registers(Channel::Message, ArgSel::Pos(0), ArgSel::Pos(1), VerbSel::Any)],
        )
    };
    let k = knowledge(
        &index,
        &[
            ("js/api.js", "client.publish", 0, publish()),
            ("js/api.js", "client.publish", 1, publish()),
            ("py/worker.py", "pubsub.subscribe", 0, subscribe()),
            ("py/worker.py", "pubsub.subscribe", 1, subscribe()),
            ("py/worker.py", "r.publish", 0, publish()),
            ("py/audit.py", "redis.Redis().pubsub().subscribe", 0, subscribe()),
        ],
    );
    let none = InstalledPackages::default();
    let out = run_full(&index, &root, &BridgeSettings::default(), &k, &none, &open_gate());
    let r = rows(&index, &out.bridges);
    assert_row(&r, BridgeKind::Message, "js/api.js:createOrder", "py/worker.py:handle_order", Tier::Possible);
    assert_row(&r, BridgeKind::Message, "js/api.js:createOrder", "py/audit.py:audit", Tier::Possible);
    assert_proven(&r, &[]);
    none_mentions(&r, "local.only");
    none_mentions(&r, "nobody.listens");
    // Weak kinds are off with `weak: false`.
    let strict = BridgeSettings {
        weak: false,
        ..BridgeSettings::default()
    };
    let out = run_full(&index, &root, &strict, &k, &none, &open_gate());
    assert!(out.bridges.iter().all(|b| b.kind != BridgeKind::Message));
}

fn semantics(provider: trace_core::model::Provider) -> trace_core::semantics::FileSemantics {
    trace_core::semantics::FileSemantics {
        provider,
        tool_fingerprint: String::new(),
        edges: Vec::new(),
        unresolved: Vec::new(),
        value_refs: Vec::new(),
        diagnostics: Vec::new(),
        implementations: Vec::new(),
        resolved_elsewhere: Vec::new(),
        callback_params: Vec::new(),
        library_files: Vec::new(),
        library_calls: Vec::new(),
        outside_build: None,
        expanded: Vec::new(),
        library_dispatch: Vec::new(),
        library_bases: Vec::new(),
    }
}

/// A library call whose server-given symbol is an irreducible `ffi_conventions` row
/// (`dlsym(handle, "name")`) looks the name up at run time: a possible link to the C
/// definition.
#[test]
fn rule_ffi_lookup_from_primitive_row_matches_c_definition() {
    let (mut index, root) = build("rule-bridges-ffi-lookup");
    let file = index.file_by_path("loader/load.c").expect("loader");
    let facts = index.files[file.idx()].facts.clone().expect("facts");
    let calls: Vec<trace_core::semantics::SemLibraryCall> = facts
        .calls
        .iter()
        .filter(|c| c.callee == "dlsym")
        .map(|c| trace_core::semantics::SemLibraryCall {
            at: c.callee_span,
            line: c.line,
            file: 0,
            decl_line: 0,
            decl_column: 0,
            symbol: Some("dlsym".into()),
        })
        .collect();
    assert_eq!(calls.len(), 2);
    let mut sem = semantics(trace_core::model::Provider::Lsp("clangd".into()));
    sem.library_files = vec![trace_core::semantics::LibraryFile {
        path: "dlfcn.h".into(),
        package: "libc".into(),
        version: None,
        stdlib: true,
        readable: false,
        language: Language::C,
    }];
    sem.library_calls = calls;
    index.files[file.idx()].semantic = Some(sem);
    let out = run_with(&index, &root, &BridgeSettings::default());
    let r = rows(&index, &out.bridges);
    assert_row(&r, BridgeKind::Ffi, "loader/load.c:run", "native/lib.c:compress_buf", Tier::Possible);
    none_mentions(&r, "not_there");
    assert_proven(&r, &[]);
}

/// Rust attribute macros are expanded by the language server under `--allow-build`; the
/// expansion's language-level export (`#[no_mangle] extern "C"`) is an ABI fact of the
/// annotated function: the C declaration links to it (proven, independent of any gate).
#[test]
fn rule_ffi_export_from_expanded_macro_matches_import() {
    let (mut index, root) = build("rule-bridges-expanded-export");
    let file = index.file_by_path("rust/src/lib.rs").expect("rust file");
    let text = fs::read_to_string(root.join("rust/src/lib.rs")).expect("read");
    let start = text.find("#[export_fn]").expect("attribute") as u32;
    let span = ByteSpan::new(start, start + "#[export_fn]".len() as u32);
    let mut sem = semantics(trace_core::model::Provider::RustAnalyzer);
    sem.expanded = vec![trace_core::semantics::ExpandedMacro {
        span,
        text: "#[no_mangle]\npub extern \"C\" fn add_numbers(a: i32, b: i32) -> i32 { a + b }\n".into(),
    }];
    index.files[file.idx()].semantic = Some(sem);
    let mut k = LibraryKnowledge::default();
    k.by_call.insert(
        ("rust/src/lib.rs".into(), start),
        CallBehaviour {
            symbol: None,
            effects: vec![Effect::Exports {
                channel: Channel::Ffi,
                name: ArgSel::Kw("add_numbers".into()),
            }],
            source: BehaviourSource::Derived,
            inferred: false,
            reason: "exported by the expanded macro code".into(),
        },
    );
    let out = run_full(
        &index,
        &root,
        &BridgeSettings::default(),
        &k,
        &InstalledPackages::default(),
        &Gate::default(),
    );
    let r = rows(&index, &out.bridges);
    assert_row(&r, BridgeKind::CAbi, "c/api.h:add_numbers", "rust/src/lib.rs:add_numbers", Tier::Proven);
    none_mentions(&r, "private_helper");
}

/// Filesystem routing rows apply only when their activating package is installed.
#[test]
fn rule_fs_route_row_needs_installed_package() {
    let (index, root) = build("rule-bridges-fs-routes");
    let fetch = derived("fetch", vec![sends(Channel::Http, ArgSel::Pos(0), VerbSel::Any)]);
    let k = knowledge(&index, &[("web/client.ts", "fetch", 0, fetch)]);
    let config = BridgeSettings::default();
    let without = run_full(&index, &root, &config, &k, &InstalledPackages::default(), &open_gate());
    assert!(without.bridges.iter().all(|b| b.kind != BridgeKind::Http), "{:#?}", without.bridges);
    let dir = tempfile::Builder::new()
        .prefix("trace-fixtures-bridge-fs-")
        .tempdir_in(temp_root())
        .expect("temp dir");
    let installed = node_packages(dir.path(), &["next"]);
    let with = run_full(&index, &root, &config, &k, &installed, &open_gate());
    let r = rows(&index, &with.bridges);
    assert_row(
        &r,
        BridgeKind::Http,
        "web/client.ts:loadUser",
        "pages/api/users/[id].ts:handler",
        Tier::Inferred,
    );
    none_mentions(&r, "lib/helpers.ts");
}

/// An annotation whose meta-annotation chain (read from the annotation's declaration)
/// reaches a `reflection_roots` row registers the method under the joined class + method
/// paths with the chain's method.
#[test]
fn rule_reflection_root_chain_gives_route() {
    let (index, root) = build("rule-bridges-reflection");
    let fetch = derived("fetch", vec![sends(Channel::Http, ArgSel::Pos(0), VerbSel::Any)]);
    let k = knowledge(&index, &[("web/client.ts", "fetch", 0, fetch)]);
    let dir = tempfile::Builder::new()
        .prefix("trace-fixtures-bridge-refl-")
        .tempdir_in(temp_root())
        .expect("temp dir");
    fs::create_dir_all(
        dir.path()
            .join("files-2.1")
            .join("org.springframework")
            .join("spring-web"),
    )
    .expect("package dir");
    let installed = InstalledPackages::from_roots(&[trace_env::LibraryRoot {
        path: dir.path().to_path_buf(),
        kind: trace_env::LibraryKind::Dependency,
        ecosystem: trace_env::EcosystemId::Jvm,
        layout: "gradle_cache",
        version: None,
    }]);
    let out = run_full(&index, &root, &BridgeSettings::default(), &k, &installed, &open_gate());
    let r = rows(&index, &out.bridges);
    assert_row(
        &r,
        BridgeKind::Http,
        "web/client.ts:loadUser",
        "server/src/main/java/demo/web/UserController.java:UserController.get",
        Tier::Inferred,
    );
    none_mentions(&r, "notARoute");
    // The root row is inactive without its package: no route.
    let none =
        run_full(&index, &root, &BridgeSettings::default(), &k, &InstalledPackages::default(), &open_gate());
    assert!(none.bridges.iter().all(|b| b.kind != BridgeKind::Http), "{:#?}", none.bridges);
}

#[test]
fn rule_route_placeholders_from_route_patterns_rows() {
    use trace_syntax::boundary::Tpl;
    let ph = crate::http::Placeholders::from_tables(&trace_library::table::Tables::builtin());
    for (key, want) in [
        ("/users/<int:user_id>", "/users/{}"),
        ("/users/:id", "/users/{}"),
        ("/users/{id}", "/users/{}"),
        ("/users/[id]", "/users/{}"),
        ("/files/*", "/files/{}"),
        ("/users/{id:[0-9]+}", "/users/{}"),
        ("/static/app.js", "/static/app.js"),
    ] {
        let n = crate::http::normalize(&Tpl::literal(key), false, &ph).expect("normalizes");
        assert_eq!(n.path, want, "{key}");
    }
    // `METHOD /path` keys (a route_patterns row) carry their method.
    let n = crate::http::normalize(&Tpl::literal("GET /users/{id}"), false, &ph).expect("normalizes");
    assert_eq!((n.verb.as_deref(), n.path.as_str()), (Some("GET"), "/users/{}"));
    // Without rows, a family is not a placeholder: the rows are the only source.
    let none = crate::http::Placeholders::default();
    let n = crate::http::normalize(&Tpl::literal("/users/:id"), false, &none).expect("normalizes");
    assert_eq!(n.path, "/users/:id");
}

/// Incremental detection (cached endpoint blocks + full matching) equals the full detection
/// after a knowledge change of one file, with a stale delta, and after a global input change.
#[test]
fn rule_incremental_bridges_equal_full() {
    use trace_core::delta::IndexDelta;
    let (index, root) = build("xlang-http");
    let store = SourceStore::with_root(&index, root.clone());
    let tables = trace_library::table::Tables::builtin();
    let config = BridgeSettings::default();
    let gate = open_gate();
    let none = InstalledPackages::default();
    let k1 = http_knowledge(&index);
    let render = |o: BridgeOutput| (format!("{:?}", o.bridges), format!("{:?}", o.diagnostics));
    let detect = |k: &LibraryKnowledge,
                  installed: &InstalledPackages,
                  inc: Option<(&IndexDelta, &mut crate::BridgeState)>| {
        let i = BridgeInput {
            index: &index,
            sources: &store,
            config: &config,
            knowledge: k,
            tables: &tables,
            installed,
        };
        render(crate::run(&i, &gate, inc))
    };
    let mut state = crate::BridgeState::default();
    let full = detect(&k1, &none, None);
    let seeded = detect(&k1, &none, Some((&IndexDelta::full(), &mut state)));
    assert_eq!(seeded, full, "a full delta is the full detection");
    assert!(!state.files.is_empty(), "the state is seeded");
    // Knowledge of one file changes (a route registration loses its behaviour; a client call
    // passing a URL literal stays a client without knowledge, so the change is on the server).
    let mut k2 = k1.clone();
    k2.by_call.remove(&call_key(&index, "server/app.js", "app.post", 0));
    let mut delta = IndexDelta::default();
    delta.requeried.insert("server/app.js".into());
    let incremental = detect(&k2, &none, Some((&delta, &mut state)));
    assert_eq!(incremental, detect(&k2, &none, None), "incremental == full after a knowledge change");
    assert_ne!(incremental, full, "the change is visible");
    // A delta that names nothing: the fingerprints still find the changed file.
    let back = detect(&k1, &none, Some((&IndexDelta::default(), &mut state)));
    assert_eq!(back, full, "incremental == full after the change is reverted");
    // Another global input (installed packages) invalidates every block.
    let before = state.inputs;
    let dir = tempfile::Builder::new()
        .prefix("trace-fixtures-bridge-inc-")
        .tempdir_in(temp_root())
        .expect("temp dir");
    let installed = node_packages(dir.path(), &["next"]);
    let after = detect(&k1, &installed, Some((&IndexDelta::default(), &mut state)));
    assert_ne!(state.inputs, before);
    assert_eq!(after, detect(&k1, &installed, None));
}

/// Frozen list of the framework / library names the deleted bridge tables keyed their rules
/// on ("requests" is left out: it is also an ordinary English word and identifier across
/// trace). After the bridges gate (DESIGN §5.7) none may appear in trace's code or assets
/// except in `assets/library/*.json` irreducible rows (and in tests / fixtures).
const OLD_FRAMEWORK_NAMES: &[&str] = &[
    "express",
    "fastify",
    "fastapi",
    "FastAPI",
    "flask",
    "Flask",
    "django",
    "Django",
    "starlette",
    "Starlette",
    "Sanic",
    "Quart",
    "laravel",
    "Laravel",
    "rails",
    "Rails",
    "sinatra",
    "Sinatra",
    "spring",
    "Spring",
    "jaxrs",
    "nestjs",
    "NestJS",
    "Koa",
    "koa",
    "Hono",
    "hono",
    "restify",
    "gin",
    "httprouter",
    "axios",
    "superagent",
    "ofetch",
    "undici",
    "httpx",
    "aiohttp",
    "urllib3",
    "reqwest",
    "okhttp",
    "OkHttpClient",
    "RestTemplate",
    "TestRestTemplate",
    "WebClient",
    "RestClient",
    "APIRouter",
    "Blueprint",
    "include_router",
    "register_blueprint",
    "add_url_rule",
    "GetMapping",
    "PostMapping",
    "RequestMapping",
    "HttpGet",
    "HttpPost",
    "MapGet",
    "MapPost",
    "kafka",
    "kafkajs",
    "celery",
    "amqplib",
    "pika",
    "socketio",
    "strawberry",
    "ariadne",
    "graphene",
    "koffi",
    "cffi",
    "execa",
];

/// Package-specific names that moved into `syntax_conventions` rows of `assets/library/*.json`
/// (binding attributes, generated RPC names, GraphQL resolver conventions, addon loaders):
/// the code reads them only from the rows.
const NAMES_MOVED_TO_ROWS: &[&str] = &[
    "pyfunction",
    "pyclass",
    "pymethods",
    "pymodule",
    "wrap_pyfunction",
    "add_wrapped",
    "DECLARE_NAPI_METHOD",
    "node-addon-api",
    "node-gyp-build",
    "QueryMapping",
    "MutationMapping",
    "SubscriptionMapping",
    "SchemaMapping",
    "newBlockingStub",
    "newFutureStub",
    "ImplBase",
    "Servicer",
    "addService",
    "createInsecure",
    "ChannelCredentials",
    "makeExecutableSchema",
];

/// Rust source without comments and without its `#[cfg(test)]` part (test code may name
/// packages as data).
fn code_of(text: &str) -> String {
    let text = match text.find("#[cfg(test)]") {
        Some(i) => &text[..i],
        None => text,
    };
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_str = false;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Identifier-like words; `-` joins (package spellings such as `undici-types`, a type package
/// the TypeScript server installs, are one word and never the bare framework name).
fn words(text: &str) -> BTreeSet<&str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .map(|w| w.trim_matches('-'))
        .filter(|w| !w.is_empty())
        .collect()
}

/// The bridges gate deleted the old tables (DESIGN §5.7: "then the old
/// assets/bridges/*.json, loader and framework lists are deleted ... and
/// `rule_no_framework_names_outside_tables` passes"): no framework or package name remains in
/// trace's code or assets outside the irreducible rows of `assets/library/*.json` (and tests
/// / fixtures).
#[test]
fn rule_no_framework_names_outside_tables() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut hits = Vec::new();
    let mut files = Vec::new();
    walk(&repo.join("crates"), &repo.join("crates"), &mut files);
    for (rel, path) in files {
        let test_code = rel.contains("/tests/")
            || rel.contains("/examples/")
            || rel.contains("/benches/")
            || rel.ends_with("tests.rs");
        if !rel.ends_with(".rs") || test_code {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap_or_default();
        let code = code_of(&text);
        let w = words(&code);
        hits.extend(
            OLD_FRAMEWORK_NAMES
                .iter()
                .chain(NAMES_MOVED_TO_ROWS)
                .filter(|n| w.contains(**n))
                .map(|n| format!("crates/{rel}: {n}")),
        );
        if code.contains("assets/bridges") {
            hits.push(format!("crates/{rel}: reads assets/bridges"));
        }
    }
    assert!(!repo.join("assets").join("bridges").exists(), "assets/bridges is deleted");
    let mut assets = Vec::new();
    walk(&repo.join("assets"), &repo.join("assets"), &mut assets);
    for (rel, path) in assets {
        if rel.starts_with("library/") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap_or_default();
        let w = words(&text);
        hits.extend(
            OLD_FRAMEWORK_NAMES
                .iter()
                .chain(NAMES_MOVED_TO_ROWS)
                .filter(|n| w.contains(**n))
                .map(|n| format!("assets/{rel}: {n}")),
        );
    }
    assert!(hits.is_empty(), "framework names outside the irreducible tables:\n{}", hits.join("\n"));
}

/// A registry of the file (`router`, receiver of HTTP registrations) registered as the handler
/// of another registry (`app.use("/ops", router)`) is mounted under the key: its routes are
/// served below the prefix.
#[test]
fn rule_registry_registered_as_handler_is_mounted() {
    let (index, root) = build("xlang-http-registry-mount");
    let register = |symbol: &str| {
        derived(symbol, vec![registers(Channel::Http, ArgSel::Pos(0), ArgSel::Rest(1), VerbSel::Any)])
    };
    let k = knowledge(
        &index,
        &[
            ("server/app.js", "router.get", 0, register("lib/router.verb")),
            ("server/app.js", "app.use", 0, register("lib/app.use")),
            (
                "web/client.js",
                "fetch",
                0,
                derived("fetch", vec![sends(Channel::Http, ArgSel::Pos(0), VerbSel::Any)]),
            ),
        ],
    );
    let out =
        run_full(&index, &root, &BridgeSettings::default(), &k, &InstalledPackages::default(), &open_gate());
    let r = rows(&index, &out.bridges);
    assert!(
        find(&r, BridgeKind::Http, "web/client.js:loadHealth", "server/app.js:health").is_some(),
        "{r:#?}"
    );
    none_mentions(&r, "server/app.js:router");
}

/// A group of a group (`admin := v1.Group("/admin")` with `v1 := r.Group("/api/v1")`): the
/// group's own prefix is under its receiver's own prefix.
#[test]
fn rule_group_of_a_group_is_under_both_prefixes() {
    let (index, root) = build("xlang-http-group-prefix");
    let group = || {
        derived(
            "Engine.Group",
            vec![Effect::Mounts {
                key: ArgSel::Pos(0),
                target: ArgSel::Receiver,
            }],
        )
    };
    let k = knowledge(
        &index,
        &[
            ("server/main.go", "r.Group", 0, group()),
            ("server/main.go", "v1.Group", 0, group()),
            (
                "server/main.go",
                "admin.GET",
                0,
                derived("Engine.GET", vec![registers(Channel::Http, ArgSel::Pos(0), ArgSel::Last, get())]),
            ),
            (
                "web/client.ts",
                "fetch",
                0,
                derived("fetch", vec![sends(Channel::Http, ArgSel::Pos(0), VerbSel::Any)]),
            ),
        ],
    );
    let out =
        run_full(&index, &root, &BridgeSettings::default(), &k, &InstalledPackages::default(), &open_gate());
    let r = rows(&index, &out.bridges);
    let hits: Vec<&Row> = r.iter().filter(|x| x.from == "web/client.ts:loadUser").collect();
    assert_eq!(hits.len(), 1, "{r:#?}");
    assert_eq!(hits[0].to, "server/main.go:userHandler");
}

// ---------------------------------------------------------------------------------------
// Chained calls and member lookups (fixture `tests/fixtures/rule-bridges-chain-member`)
// ---------------------------------------------------------------------------------------

/// `exec.Command("python", "procs/build.py").Run()`: both calls share the callee start the
/// knowledge is keyed by; the process key belongs to the call passing the program.
#[test]
fn rule_chained_call_effect_belongs_to_the_call_passing_its_key() {
    let (index, root) = build("rule-bridges-chain-member");
    let k = knowledge(
        &index,
        &[(
            "procs/run.go",
            "exec.Command",
            0,
            derived("os/exec.Command", vec![sends(Channel::Process, ArgSel::Pos(0), VerbSel::Any)]),
        )],
    );
    let ends = endpoints_of(&index, &root, &k, "procs/run.go");
    assert!(
        ends.iter().any(|e| e.kind == BridgeKind::Subprocess
            && e.role == BoundaryRole::Uses
            && e.name == "procs/build.py"),
        "{ends:#?}"
    );
    let out =
        run_full(&index, &root, &BridgeSettings::default(), &k, &InstalledPackages::default(), &open_gate());
    let r = rows(&index, &out.bridges);
    assert_row(&r, BridgeKind::Subprocess, "procs/run.go:Build", "procs/build.py:<module>", Tier::Possible);
    assert_proven(&r, &[]);
}

/// `lib.compress_buf(10)` resolved to the member-lookup hook looks up `compress_buf` (the
/// spelled member), never the call's argument.
#[test]
fn rule_member_lookup_hook_uses_the_spelled_member() {
    let (index, root) = build("rule-bridges-chain-member");
    let k = knowledge(
        &index,
        &[(
            "native/use.py",
            "lib.compress_buf",
            0,
            derived("ctypes.CDLL.__getattr__", vec![sends(Channel::Ffi, ArgSel::Member, VerbSel::Any)]),
        )],
    );
    let ends = endpoints_of(&index, &root, &k, "native/use.py");
    let ffi: Vec<&BoundaryFact> = ends.iter().filter(|e| e.kind == BridgeKind::Ffi).collect();
    assert_eq!(ffi.len(), 1, "{ends:#?}");
    assert_eq!(ffi[0].name, "compress_buf");
    assert_eq!(ffi[0].role, BoundaryRole::Uses);
    let out =
        run_full(&index, &root, &BridgeSettings::default(), &k, &InstalledPackages::default(), &open_gate());
    let r = rows(&index, &out.bridges);
    assert_row(&r, BridgeKind::Ffi, "native/use.py:compress", "native/lib.c:compress_buf", Tier::Possible);
    assert_proven(&r, &[]);
}
