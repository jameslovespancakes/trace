use super::*;
use crate::facts::{CallSite, Declaration, FileFacts};
use crate::languages::{Language, SupportLevel};
use crate::model::{EdgeKind, ExecutionModel, Provider, Resolution, Span, SymbolKind, UnresolvedKind};
use crate::semantics::{FileSemantics, SemEdge, SemValueRef};

fn decl(name: &str, qualified: &str, parent: Option<u32>) -> Declaration {
    Declaration {
        name: name.into(),
        qualified_name: qualified.into(),
        kind: SymbolKind::Function,
        span: Span {
            bytes: ByteSpan::new(0, 10),
            start_line: 1,
            end_line: 2,
        },
        name_span: ByteSpan::new(4, 5),
        body_start: 8,
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

fn record(path: &str, facts: Option<FileFacts>, semantic: Option<FileSemantics>) -> FileRecord {
    FileRecord {
        path: path.into(),
        language: Language::Python,
        hash: Hash32::of(path.as_bytes()),
        size: 1,
        mtime_ns: 0,
        support: SupportLevel::Semantic,
        facts,
        semantic,
        first_symbol: 0,
        symbol_count: 0,
        diagnostics: Vec::new(),
        pending: None,
    }
}

fn call(owner: Option<u32>, start: u32) -> CallSite {
    CallSite {
        owner,
        lexical_owner: owner,
        span: ByteSpan::new(start, start + 3),
        callee_span: ByteSpan::new(start, start + 1),
        callee: "g".into(),
        member: Some("g".into()),
        receiver: None,
        line: 1,
        activation: Default::default(),
        is_new: false,
        arg_count: 0,
    }
}

fn header() -> IndexHeader {
    crate::test_support::index::index_with(&[], &[]).header
}

#[test]
fn symbols_edges_and_unknowns() {
    let a_facts = FileFacts {
        declarations: vec![decl("f", "f", None), decl("f", "f", None), decl("inner", "f.inner", Some(1))],
        ..FileFacts::default()
    };
    let sem = FileSemantics {
        provider: Provider::Pyright,
        tool_fingerprint: "t".into(),
        edges: vec![
            SemEdge {
                owner: 0,
                target: "b.py:g".into(),
                kind: EdgeKind::Calls,
                at: ByteSpan::new(5, 6),
                line: 1,
                resolution: Resolution::CallHierarchy,
            },
            // Duplicate fact.
            SemEdge {
                owner: 0,
                target: "b.py:g".into(),
                kind: EdgeKind::Calls,
                at: ByteSpan::new(5, 6),
                line: 1,
                resolution: Resolution::CallHierarchy,
            },
            // Dangling target.
            SemEdge {
                owner: 0,
                target: "gone.py:x".into(),
                kind: EdgeKind::Calls,
                at: ByteSpan::new(5, 6),
                line: 1,
                resolution: Resolution::CallHierarchy,
            },
            // Backends may not report inferred kinds.
            SemEdge {
                owner: 0,
                target: "b.py:g".into(),
                kind: EdgeKind::InferredCall,
                at: ByteSpan::new(5, 6),
                line: 1,
                resolution: Resolution::DeterministicUnique,
            },
        ],
        unresolved: Vec::new(),
        value_refs: vec![SemValueRef {
            at: ByteSpan::new(7, 8),
            line: 1,
            target: "b.py:g".into(),
        }],
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
    };
    let b_facts = FileFacts {
        declarations: vec![decl("g", "g", None)],
        calls: vec![call(Some(0), 2), call(None, 20)],
        ..FileFacts::default()
    };
    let index = assemble(AssembleInput {
        header: header(),
        // Deliberately unsorted.
        files: vec![
            record("b.py", Some(b_facts), None),
            record("a.py", Some(a_facts), Some(sem)),
            record("c.vb", None, None),
        ],
        configs: Vec::new(),
        omitted: Vec::new(),
        support: Vec::new(),
        backend_runs: Vec::new(),
        diagnostics: Vec::new(),
    });
    index.validate().unwrap();
    let uids: Vec<&str> = index.symbols.iter().map(|s| s.uid.as_str()).collect();
    assert_eq!(uids, vec!["a.py:f", "a.py:f#2", "a.py:f.inner", "b.py:g"]);
    assert_eq!(index.symbols[2].parent, Some(SymbolId(1)));
    assert!(index.symbols[0].semantic);
    assert!(!index.symbols[3].semantic);
    assert_eq!(index.edges.len(), 1);
    assert_eq!(index.edges[0].to, SymbolId(3));
    assert_eq!(index.edges[0].tier, Tier::Proven);
    assert_eq!(index.value_refs.len(), 1);
    // A file without semantics (pending) has symbols but no answers from syntax.
    assert!(index.unresolved.is_empty());
    let kinds: Vec<&str> = index.diagnostics.iter().map(|d| d.kind.as_str()).collect();
    assert!(kinds.contains(&"dangling_semantic_target"));
    assert!(kinds.contains(&"non_proven_semantic_edge"));
    assert_eq!(index.file_by_path("c.vb"), Some(FileId(2)));
    assert_eq!(index.file(FileId(2)).symbol_count, 0);
}

/// SPEC 8.5a: `FileSemantics::implementations` of a base file become proven edges
/// implementor -> base (kind as recorded, provider = the backend, resolution
/// `implementation`, `at` = the implementor's name span in its own file); unknown uids are
/// dropped (dangling), other kinds and self links are ignored, duplicates collapse.
#[test]
fn rule_server_implementations_become_proven_edges() {
    use crate::semantics::SemImplementation;
    let mut base_class = decl("Sink", "Sink", None);
    base_class.kind = SymbolKind::Interface;
    let mut base = decl("matched", "Sink.matched", Some(0));
    base.kind = SymbolKind::Method;
    base.is_stub = true;
    let a_facts = FileFacts {
        declarations: vec![base_class, base],
        ..FileFacts::default()
    };
    let implementation = |implementor: &str, kind: EdgeKind| SemImplementation {
        base: 1,
        implementor: implementor.into(),
        kind,
    };
    let sem = FileSemantics {
        provider: Provider::RustAnalyzer,
        tool_fingerprint: "t".into(),
        edges: Vec::new(),
        unresolved: Vec::new(),
        value_refs: Vec::new(),
        diagnostics: Vec::new(),
        implementations: vec![
            implementation("b.rs:JsonSink.matched", EdgeKind::Implements),
            // Reported twice (implementation + type hierarchy): one edge.
            implementation("b.rs:JsonSink.matched", EdgeKind::Implements),
            // The implementor no longer exists.
            implementation("gone.rs:Old.matched", EdgeKind::Implements),
            // Not a family kind.
            implementation("b.rs:JsonSink.matched", EdgeKind::Calls),
            // The base itself.
            implementation("a.rs:Sink.matched", EdgeKind::Implements),
        ],
        resolved_elsewhere: Vec::new(),
        callback_params: Vec::new(),
        library_files: Vec::new(),
        library_calls: Vec::new(),
        outside_build: None,
        expanded: Vec::new(),
        library_dispatch: Vec::new(),
        library_bases: Vec::new(),
    };
    let mut imp_class = decl("JsonSink", "JsonSink", None);
    imp_class.kind = SymbolKind::Class;
    let mut imp = decl("matched", "JsonSink.matched", None);
    imp.kind = SymbolKind::Method;
    imp.container = Some("JsonSink".into());
    imp.span = Span {
        bytes: ByteSpan::new(40, 90),
        start_line: 7,
        end_line: 9,
    };
    imp.name_span = ByteSpan::new(47, 54);
    let b_facts = FileFacts {
        declarations: vec![imp_class, imp],
        ..FileFacts::default()
    };
    let mut a = record("a.rs", Some(a_facts), Some(sem));
    a.language = Language::Rust;
    let mut b = record("b.rs", Some(b_facts), None);
    b.language = Language::Rust;
    let index = assemble(AssembleInput {
        header: header(),
        files: vec![a, b],
        configs: Vec::new(),
        omitted: Vec::new(),
        support: Vec::new(),
        backend_runs: Vec::new(),
        diagnostics: Vec::new(),
    });
    index.validate().unwrap();
    let family: Vec<&Edge> = index
        .edges
        .iter()
        .filter(|e| matches!(e.kind, EdgeKind::Implements | EdgeKind::Overrides))
        .collect();
    assert_eq!(family.len(), 1, "{family:?}");
    let e = family[0];
    assert_eq!(index.symbols[e.from.idx()].uid, "b.rs:JsonSink.matched");
    assert_eq!(index.symbols[e.to.idx()].uid, "a.rs:Sink.matched");
    assert_eq!(e.kind, EdgeKind::Implements);
    assert_eq!(e.tier, Tier::Proven);
    assert_eq!(e.provider, Provider::RustAnalyzer);
    assert_eq!(e.resolution, Resolution::Implementation);
    assert_eq!(e.at.file, FileId(1));
    assert_eq!(e.at.bytes, ByteSpan::new(47, 54));
    assert_eq!(e.at.line, 7);
    assert!(e.site.is_none() && e.bridge.is_none());
    assert!(index.diagnostics.iter().any(|d| d.kind == "dangling_semantic_target"));

    // A rule edge for the same pair appended later (pipeline phase 4b) collapses onto the
    // server edge, which is kept.
    let mut edges = index.edges.clone();
    let mut rule = e.clone();
    rule.provider = Provider::Rule("rust-trait-impl".into());
    rule.resolution = Resolution::InheritanceRule;
    edges.push(rule);
    sort_edges(&mut edges);
    let kept: Vec<&Edge> = edges.iter().filter(|x| x.kind == EdgeKind::Implements).collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].provider, Provider::RustAnalyzer);
}

fn sem_with(edges: &[(u32, &str, u32)], refs: &[&str], implementations: &[(u32, &str)]) -> FileSemantics {
    use crate::semantics::SemImplementation;
    FileSemantics {
        provider: Provider::Pyright,
        tool_fingerprint: "t".into(),
        edges: edges
            .iter()
            .map(|&(owner, target, at)| SemEdge {
                owner,
                target: target.into(),
                kind: EdgeKind::Calls,
                at: ByteSpan::new(at, at + 1),
                line: 1,
                resolution: Resolution::CallHierarchy,
            })
            .collect(),
        unresolved: vec![crate::semantics::SemUnresolved {
            owner: Some(0),
            kind: UnresolvedKind::NoSemanticTarget,
            at: ByteSpan::new(30, 31),
            line: 2,
            callee: "x.g2".into(),
            candidates: refs.iter().map(|r| r.to_string()).collect(),
        }],
        value_refs: refs
            .iter()
            .enumerate()
            .map(|(i, r)| SemValueRef {
                at: ByteSpan::new(40 + i as u32, 41 + i as u32),
                line: 3,
                target: r.to_string(),
            })
            .collect(),
        diagnostics: Vec::new(),
        implementations: implementations
            .iter()
            .map(|&(base, implementor)| SemImplementation {
                base,
                implementor: implementor.into(),
                kind: EdgeKind::Overrides,
            })
            .collect(),
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

fn facts_of(decls: &[(&str, u32)]) -> FileFacts {
    FileFacts {
        declarations: decls
            .iter()
            .map(|&(q, start)| {
                let mut d = decl(q.rsplit('.').next().unwrap_or(q), q, None);
                d.span.bytes = ByteSpan::new(start, start + 10);
                d.name_span = ByteSpan::new(start + 4, start + 5);
                d
            })
            .collect(),
        calls: vec![call(Some(0), 50)],
        ..FileFacts::default()
    }
}

fn input_of(files: Vec<FileRecord>) -> AssembleInput {
    AssembleInput {
        header: header(),
        files,
        configs: Vec::new(),
        omitted: Vec::new(),
        support: Vec::new(),
        backend_runs: Vec::new(),
        diagnostics: vec![Diagnostic::new("pipeline", None, "kept".to_string())],
    }
}

fn delta_input(files: Vec<FileRecord>, removed: &[&str]) -> AssembleDeltaInput {
    let full = input_of(Vec::new());
    AssembleDeltaInput {
        header: full.header,
        files,
        removed: removed.iter().map(|s| s.to_string()).collect(),
        configs: full.configs,
        omitted: full.omitted,
        support: full.support,
        backend_runs: full.backend_runs,
        diagnostics: full.diagnostics,
    }
}

/// The records of a small repository: a.py calls into b.py and c.py, b.py's methods are
/// overridden in c.py (server implementations), e.py is unrelated, n.py has no semantics.
fn repo() -> BTreeMap<&'static str, FileRecord> {
    let mut out = BTreeMap::new();
    out.insert(
        "a.py",
        record(
            "a.py",
            Some(facts_of(&[("f", 0), ("f2", 20)])),
            Some(sem_with(&[(0, "b.py:g", 5), (1, "c.py:h", 25), (0, "gone.py:x", 6)], &["b.py:g2"], &[])),
        ),
    );
    out.insert(
        "b.py",
        record(
            "b.py",
            Some(facts_of(&[("g", 0), ("g2", 20)])),
            Some(sem_with(&[(0, "c.py:h", 5)], &[], &[(0, "c.py:Impl.g")])),
        ),
    );
    out.insert(
        "c.py",
        record(
            "c.py",
            Some(facts_of(&[("h", 0), ("Impl.g", 30)])),
            Some(sem_with(&[(0, "a.py:f", 5)], &["a.py:f2"], &[])),
        ),
    );
    out.insert(
        "e.py",
        record("e.py", Some(facts_of(&[("e", 0)])), Some(sem_with(&[(0, "e.py:e", 5)], &[], &[]))),
    );
    out.insert("n.py", record("n.py", Some(facts_of(&[("n", 0)])), None));
    out
}

fn full_of(files: &BTreeMap<&'static str, FileRecord>) -> Index {
    // Deliberately unsorted input for the full path.
    assemble(input_of(files.values().rev().cloned().collect()))
}

/// Run one incremental update: `changed` records replace (or add) files, `removed` go.
fn check_delta(
    before: &BTreeMap<&'static str, FileRecord>,
    changed: Vec<FileRecord>,
    removed: &[&'static str],
) -> (Index, IdRemap, Index) {
    let prev = full_of(before);
    let mut after = before.clone();
    for r in removed {
        after.remove(r);
    }
    let mut delta = IndexDelta::default();
    for rec in &changed {
        let path: &'static str = Box::leak(rec.path.clone().into_boxed_str());
        if before.contains_key(path) {
            delta.modified.insert(rec.path.clone());
        } else {
            delta.added.insert(rec.path.clone());
        }
        after.insert(path, rec.clone());
    }
    delta.removed = removed.iter().map(|s| s.to_string()).collect();
    let full = full_of(&after);
    let (incremental, remap) = assemble_delta(prev.clone(), delta_input(changed, removed), &delta);
    (incremental, remap, full)
}

#[test]
fn rule_incremental_link_equals_full_link() {
    let before = repo();

    // 1. Body edit of b.py (same uids, spans move): its dependents are linked again.
    let mut b = before["b.py"].clone();
    b.facts = Some(facts_of(&[("g", 3), ("g2", 23)]));
    let (inc, remap, full) = check_delta(&before, vec![b], &[]);
    inc.validate().unwrap();
    assert_eq!(inc, full);
    let prev = full_of(&before);
    for s in &prev.symbols {
        let new = remap.symbol(s.id).expect("every symbol survives");
        assert_eq!(full.symbol(new).uid, s.uid);
    }

    // 2. Rename in b.py (g2 -> g3), a new file declaring the dangling target, a removed
    //    file and a re-queried file whose semantics changed.
    let mut b = before["b.py"].clone();
    b.facts = Some(facts_of(&[("g", 0), ("g3", 20)]));
    let gone = record("gone.py", Some(facts_of(&[("x", 0)])), None);
    let mut c = before["c.py"].clone();
    c.semantic = Some(sem_with(&[(1, "b.py:g", 35)], &["b.py:g3"], &[]));
    let (inc, remap, full) = check_delta(&before, vec![gone, c, b], &["e.py"]);
    inc.validate().unwrap();
    assert_eq!(inc, full);
    let prev = full_of(&before);
    let e = prev.file_by_path("e.py").unwrap();
    assert_eq!(remap.file(e), None, "removed file");
    let g2 = prev.symbols.iter().find(|s| s.uid == "b.py:g2").unwrap().id;
    assert_eq!(remap.symbol(g2), None, "renamed symbol");
    let n = prev.file_by_path("n.py").unwrap();
    assert_eq!(full.file_path(remap.file(n).unwrap()), "n.py");

    // 3. The implementor file changes: the base file's implementation edges move.
    let mut c = before["c.py"].clone();
    c.facts = Some(facts_of(&[("h", 0), ("Impl.g", 60)]));
    let (inc, _, full) = check_delta(&before, vec![c], &[]);
    assert_eq!(inc, full);

    // 4. A sequence: the output of one delta is the input of the next.
    let mut state = before.clone();
    let mut index = full_of(&state);
    let steps: Vec<(Vec<FileRecord>, Vec<&'static str>)> = vec![
        (
            vec![record(
                "d.py",
                Some(facts_of(&[("d", 0)])),
                Some(sem_with(&[(0, "a.py:f", 5)], &[], &[])),
            )],
            vec![],
        ),
        (vec![], vec!["a.py"]),
        (
            vec![record(
                "a.py",
                Some(facts_of(&[("f", 0)])),
                Some(sem_with(&[(0, "d.py:d", 5)], &[], &[])),
            )],
            vec!["d.py"],
        ),
    ];
    for (changed, removed) in steps {
        let mut delta = IndexDelta::default();
        for r in &removed {
            state.remove(r);
            delta.removed.insert(r.to_string());
        }
        for rec in &changed {
            let path: &'static str = Box::leak(rec.path.clone().into_boxed_str());
            delta.modified.insert(rec.path.clone());
            state.insert(path, rec.clone());
        }
        let (next, _) = assemble_delta(index, delta_input(changed, &removed), &delta);
        next.validate().unwrap();
        assert_eq!(next, full_of(&state));
        index = next;
    }
}

/// C records: `inc/f.h` declares the prototype `f` (and `g`), `src/f.c` defines `f`,
/// `src/main.c` calls `f` (the server answered the header prototype) and takes its
/// address (value reference), `src/g1.c` / `src/g2.c` define `g` twice (static functions:
/// no unique definition).
fn c_repo(main_target: &str) -> BTreeMap<&'static str, FileRecord> {
    let c_record = |path: &str, decls: Vec<Declaration>, sem: Option<FileSemantics>| {
        let mut r = record(
            path,
            Some(FileFacts {
                declarations: decls,
                calls: vec![call(Some(0), 50)],
                ..FileFacts::default()
            }),
            sem,
        );
        r.language = Language::C;
        r
    };
    let stub = |name: &str| {
        let mut d = decl(name, name, None);
        d.is_stub = true;
        d
    };
    let mut out = BTreeMap::new();
    out.insert("inc/f.h", c_record("inc/f.h", vec![stub("f"), stub("g")], None));
    out.insert("src/f.c", c_record("src/f.c", vec![decl("f", "f", None)], None));
    out.insert("src/g1.c", c_record("src/g1.c", vec![decl("g", "g", None)], None));
    out.insert("src/g2.c", c_record("src/g2.c", vec![decl("g", "g", None)], None));
    let mut sem = sem_with(&[(0, main_target, 5), (0, "inc/f.h:g", 7)], &["inc/f.h:f"], &[]);
    sem.unresolved.clear();
    out.insert("src/main.c", c_record("src/main.c", vec![decl("main", "main", None)], Some(sem)));
    out
}

/// I-40 (language rule, unique match): a call and a value reference the server answered
/// with a C prototype target its unique definition; a prototype with two definitions
/// keeps the prototype. An incremental link where the server answered the definition
/// equals the full link where it answered the prototype, and adding a second definition
/// of `f` (no longer unique) is linked exactly like a full link.
#[test]
fn rule_calls_into_prototypes_target_the_unique_definition() {
    let before = c_repo("inc/f.h:f");
    let index = full_of(&before);
    index.validate().unwrap();
    let uid = |id: SymbolId| index.symbol(id).uid.clone();
    let main = index.symbols.iter().find(|s| s.uid == "src/main.c:main").unwrap().id;
    let targets: Vec<String> = index
        .edges
        .iter()
        .filter(|e| e.from == main)
        .map(|e| uid(e.to))
        .collect();
    assert_eq!(targets, vec!["inc/f.h:g".to_string(), "src/f.c:f".to_string()]);
    let refs: Vec<String> = index.value_refs.iter().map(|r| uid(r.target)).collect();
    assert_eq!(refs, vec!["src/f.c:f".to_string()]);
    let map = c_prototype_definitions(&index.symbols);
    assert_eq!(map.len(), 1, "g has two definitions");

    // The server answers the definition in a one-file update: same index.
    let edited = c_repo("src/f.c:f");
    let main_c = edited["src/main.c"].clone();
    let (inc, _, full) = check_delta(&before, vec![main_c], &[]);
    assert_eq!(inc, full);
    assert_eq!(inc.edges, index.edges);

    // A second definition of `f` in a new file: `f` is no longer unique; the delta links
    // like the full link (the prototype stays the target).
    let mut extra = edited["src/f.c"].clone();
    extra.path = "src/f2.c".into();
    let (inc, _, full) = check_delta(&before, vec![extra], &[]);
    inc.validate().unwrap();
    assert_eq!(inc, full);
    let f_proto = full.symbols.iter().find(|s| s.uid == "inc/f.h:f").unwrap().id;
    assert!(full
        .edges
        .iter()
        .any(|e| e.to == f_proto && e.kind == EdgeKind::Calls));
}

#[test]
fn rule_incremental_link_without_state_links_fully() {
    let before = repo();
    let mut prev = full_of(&before);
    prev.phase_state.clear();
    let mut b = before["b.py"].clone();
    b.facts = Some(facts_of(&[("g", 1)]));
    let mut after = before.clone();
    after.insert("b.py", b.clone());
    let mut delta = IndexDelta::default();
    delta.modified.insert("b.py".into());
    let (inc, _) = assemble_delta(prev, delta_input(vec![b], &[]), &delta);
    assert_eq!(inc, full_of(&after));
}
