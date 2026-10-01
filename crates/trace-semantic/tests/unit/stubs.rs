use super::*;
use crate::test_support::facts::{decl_at, find};
use trace_core::facts::FileFacts;
use trace_core::model::{ByteSpan, Provider, SymbolKind};

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn collapse_keeps_implementations_only_when_every_stub_pairs() {
    assert_eq!(collapse_stub_pairs(set(&["pkg/m.pyi:f", "pkg/m.py:f"])), set(&["pkg/m.py:f"]));
    assert_eq!(
        collapse_stub_pairs(set(&["pkg/m.pyi:f#2", "pkg/m.pyi:f", "pkg/m.py:f"])),
        set(&["pkg/m.py:f"])
    );
    // A stub without a sibling implementation in the result keeps the ambiguity.
    let mixed = set(&["pkg/m.pyi:f", "pkg/m.py:f", "pkg/other.pyi:g"]);
    assert_eq!(collapse_stub_pairs(mixed.clone()), mixed);
    let different = set(&["a.pyi:f", "b.py:f"]);
    assert_eq!(collapse_stub_pairs(different.clone()), different);
    assert_eq!(collapse_stub_pairs(set(&["a.py:f"])), set(&["a.py:f"]));
}

fn sem(unresolved: Vec<SemUnresolved>) -> FileSemantics {
    FileSemantics {
        provider: Provider::Pyright,
        tool_fingerprint: "fp".into(),
        edges: Vec::new(),
        unresolved,
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

#[test]
fn stub_rules_link_and_resolve_overloads() {
    let stub_src: &[u8] = b"@overload\ndef load(x: int) -> int: ...\n@overload\ndef load(x: str) -> str: ...\ndef only_stub() -> None: ...\n";
    let first = find(stub_src, "load");
    let second = find(stub_src, "load(x: str");
    let only = find(stub_src, "only_stub");
    let stub_facts = FileFacts {
        declarations: vec![
            decl_at(stub_src, "load", "load", SymbolKind::Function, (0, second - 14), first),
            decl_at(stub_src, "load", "load", SymbolKind::Function, (second - 14, only - 4), second),
            decl_at(
                stub_src,
                "only_stub",
                "only_stub",
                SymbolKind::Function,
                (only - 4, stub_src.len() as u32),
                only,
            ),
        ],
        ..FileFacts::default()
    };
    let impl_src: &[u8] = b"def load(x):\n    yield x\n";
    let mut implementation =
        decl_at(impl_src, "load", "load", SymbolKind::Function, (0, impl_src.len() as u32), 4);
    implementation.execution = ExecutionModel::Generator;
    let impl_facts = FileFacts {
        declarations: vec![implementation],
        ..FileFacts::default()
    };
    let caller_src: &[u8] = b"def main():\n    load(1)\n    other(2)\n";
    let load_call = find(caller_src, "load");
    let other_call = find(caller_src, "other");
    let caller_facts = FileFacts {
        declarations: vec![decl_at(
            caller_src,
            "main",
            "main",
            SymbolKind::Function,
            (0, caller_src.len() as u32),
            4,
        )],
        ..FileFacts::default()
    };
    let decls = DeclTable::new([
        ("pkg/m.pyi", stub_src, &stub_facts),
        ("pkg/m.py", impl_src, &impl_facts),
        ("app.py", caller_src, &caller_facts),
    ]);
    let unresolved = |start: u32, candidates: &[&str]| SemUnresolved {
        owner: Some(0),
        kind: UnresolvedKind::ExternalOrAmbiguous,
        at: ByteSpan::new(start, start + 4),
        line: 2,
        callee: "x".into(),
        candidates: candidates.iter().map(|c| c.to_string()).collect(),
    };
    let mut files: HashMap<String, FileSemantics> = HashMap::new();
    files.insert("pkg/m.pyi".into(), sem(Vec::new()));
    files.insert(
        "app.py".into(),
        sem(vec![
            unresolved(load_call, &["pkg/m.pyi:load", "pkg/m.pyi:load#2"]),
            unresolved(other_call, &["pkg/m.pyi:load", "pkg/m.pyi:only_stub"]),
        ]),
    );
    apply_rules(&mut files, &decls, &HashSet::new());

    let stub = &files["pkg/m.pyi"];
    assert_eq!(stub.edges.len(), 2, "both overload stubs link to the implementation");
    assert!(stub.edges.iter().all(|e| e.kind == EdgeKind::StubImplementation
        && e.target == "pkg/m.py:load"
        && e.resolution == Resolution::StubPackagingRule));

    let app = &files["app.py"];
    assert_eq!(app.edges.len(), 1);
    assert_eq!(app.edges[0].target, "pkg/m.py:load");
    assert_eq!(app.edges[0].kind, EdgeKind::CreatesGenerator);
    assert_eq!(app.edges[0].resolution, Resolution::OverloadedStubImplementation);
    // `only_stub` has no implementation: the site stays ambiguous.
    assert_eq!(app.unresolved.len(), 1);
    assert_eq!(app.unresolved[0].at.start, other_call);

    // Incomplete candidate lists are never rewritten.
    let mut files: HashMap<String, FileSemantics> = HashMap::new();
    files.insert("app.py".into(), sem(vec![unresolved(load_call, &["pkg/m.pyi:load"])]));
    let incomplete: HashSet<(String, u32)> = [("app.py".to_string(), load_call)].into_iter().collect();
    apply_rules(&mut files, &decls, &incomplete);
    assert!(files["app.py"].edges.is_empty());
    assert_eq!(files["app.py"].unresolved.len(), 1);
}
