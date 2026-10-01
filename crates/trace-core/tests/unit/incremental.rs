use super::*;
use crate::facts::CallSite;
use crate::inventory::InventoryEntry;
use crate::model::{ByteSpan, EdgeKind, Provider, Resolution, UnresolvedKind};
use crate::semantics::{FileSemantics, SemEdge, SemUnresolved};
use crate::test_support::index::index_with;

fn entry(path: &str, content: &str) -> HashedEntry {
    HashedEntry {
        entry: InventoryEntry {
            path: path.into(),
            abs: path.into(),
            language: crate::languages::from_path(std::path::Path::new(path)),
            is_config: false,
            size: content.len() as u64,
            mtime_ns: 0,
        },
        hash: Hash32::of(content.as_bytes()),
    }
}

/// Previous index with files `a.py` (calls b.py:g, unresolved `x.run`) and `b.py`.
fn previous() -> Index {
    let mut index = index_with(&["f"], &[]);
    let mut b = index.files[0].clone();
    b.path = "b.py".into();
    b.symbol_count = 0;
    b.first_symbol = 1;
    b.hash = Hash32::of(b"b");
    index.files[0].hash = Hash32::of(b"a");
    let span = ByteSpan::new(5, 10);
    index.files[0].facts = Some(FileFacts {
        calls: vec![CallSite {
            owner: Some(0),
            lexical_owner: Some(0),
            span,
            callee_span: span,
            callee: "x.run".into(),
            member: Some("run".into()),
            receiver: Some("x".into()),
            line: 1,
            activation: Default::default(),
            is_new: false,
            arg_count: 0,
        }],
        ..FileFacts::default()
    });
    index.files[0].semantic = Some(FileSemantics {
        provider: Provider::Pyright,
        tool_fingerprint: "tool1".into(),
        edges: vec![SemEdge {
            owner: 0,
            target: "b.py:g".into(),
            kind: EdgeKind::Calls,
            at: span,
            line: 1,
            resolution: Resolution::CallHierarchy,
        }],
        unresolved: vec![SemUnresolved {
            owner: Some(0),
            kind: UnresolvedKind::NoSemanticTarget,
            at: span,
            line: 1,
            callee: "x.run".into(),
            candidates: Vec::new(),
        }],
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
    });
    index.files.push(b);
    index
}

fn files() -> Vec<(String, Language)> {
    vec![
        ("a.py".into(), Language::Python),
        ("b.py".into(), Language::Python),
        ("c.ts".into(), Language::TypeScript),
    ]
}

#[test]
fn plan_classifies_files_and_configs() {
    let prev = previous();
    let sources = [entry("a.py", "a"), entry("b.py", "changed"), entry("c.py", "new")];
    let p = plan(Some(&prev), &sources, &[]);
    assert_eq!(p.added, vec!["c.py"]);
    assert_eq!(p.changed, vec!["b.py"]);
    assert_eq!(p.unchanged, vec!["a.py"]);
    assert!(p.removed.is_empty());
    assert!(!p.configs_changed);
    assert!(!p.is_noop());

    let p = plan(Some(&prev), &[entry("a.py", "a")], &[entry("pyproject.toml", "x")]);
    assert_eq!(p.removed, vec!["b.py"]);
    assert!(p.configs_changed);

    let fresh = plan(None, &sources, &[]);
    assert_eq!(fresh.added.len(), 3);
    assert!(fresh.configs_changed);
}

#[test]
fn reuse_requires_same_hash_and_extractor() {
    let prev = previous();
    let h = Hash32::of(b"a");
    assert!(reusable_record(Some(&prev), "a.py", &h, 1).is_some());
    assert!(reusable_record(Some(&prev), "a.py", &h, 2).is_none());
    assert!(reusable_record(Some(&prev), "a.py", &Hash32::of(b"z"), 1).is_none());
    assert!(reusable_record(None, "a.py", &h, 1).is_none());
}

/// One call of [`semantic_requery`] over the Python partition of [`files`].
fn requery(
    prev: Option<&Index>,
    plan: &UpdatePlan,
    tool: &str,
    names: &HashSet<String>,
    interface_changed: &HashSet<String>,
    policy: &StalePolicy,
) -> Requery {
    let files = files();
    semantic_requery(RequeryInput {
        prev,
        plan,
        partition: &[Language::Python],
        current_files: &files,
        tool_fingerprint: tool,
        declared_names: names,
        interface_changed,
        policy,
    })
}

fn set(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn requery_rules() {
    let prev = previous();
    let none = HashSet::new();
    let all = StalePolicy::ResolveAll;
    let noop = UpdatePlan {
        unchanged: vec!["a.py".into(), "b.py".into()],
        ..UpdatePlan::default()
    };
    // Nothing changed: only b.py (no cached semantics) is queried; c.ts is not Python.
    let q = requery(Some(&prev), &noop, "tool1", &none, &none, &all);
    assert_eq!(q.now, set(&["b.py"]));
    assert!(q.stale.is_empty());
    // Rule 1: tool fingerprint changed.
    let q = requery(Some(&prev), &noop, "tool2", &none, &none, &all);
    assert!(q.now.contains("a.py"));
    // Rule 2: configs changed -> whole partition.
    let cfg = UpdatePlan {
        configs_changed: true,
        ..noop.clone()
    };
    let q = requery(Some(&prev), &cfg, "tool1", &none, &none, &all);
    assert_eq!(q.now.len(), 2);
    // Rule 3: a.py's edge targets b.py, whose interface changed.
    let changed_b = UpdatePlan {
        changed: vec!["b.py".into()],
        unchanged: vec!["a.py".into()],
        ..UpdatePlan::default()
    };
    let q = requery(Some(&prev), &changed_b, "tool1", &none, &set(&["b.py"]), &all);
    assert_eq!(q.now, set(&["a.py", "b.py"]));
    // Rule 3 for a removed file (always an interface change).
    let removed_b = UpdatePlan {
        removed: vec!["b.py".into()],
        unchanged: vec!["a.py".into()],
        ..UpdatePlan::default()
    };
    let q = requery(Some(&prev), &removed_b, "tool1", &none, &set(&["b.py"]), &all);
    assert!(q.now.contains("a.py"));
    // Rule 4: an added file declares `run`, the member of a.py's unresolved call.
    let added = UpdatePlan {
        added: vec!["d.py".into()],
        unchanged: vec!["a.py".into(), "b.py".into()],
        ..UpdatePlan::default()
    };
    let d = set(&["d.py"]);
    let q = requery(Some(&prev), &added, "tool1", &set(&["run"]), &d, &all);
    assert!(q.now.contains("a.py"));
    let q = requery(Some(&prev), &added, "tool1", &set(&["walk"]), &d, &all);
    assert!(!q.now.contains("a.py"));
    // No previous index: everything in the partition.
    let q = requery(None, &noop, "tool1", &none, &none, &all);
    assert_eq!(q.now.len(), 2);
}

#[test]
fn rule_interface_stable_edit_requeries_only_the_file() {
    let prev = previous();
    let none = HashSet::new();
    let changed_b = UpdatePlan {
        changed: vec!["b.py".into()],
        unchanged: vec!["a.py".into()],
        ..UpdatePlan::default()
    };
    // b.py's interface is unchanged: a.py (which calls b.py:g) keeps its answers.
    for policy in [
        StalePolicy::ResolveAll,
        StalePolicy::Defer {
            resolve: BTreeSet::new(),
        },
    ] {
        let q = requery(Some(&prev), &changed_b, "tool1", &none, &none, &policy);
        assert_eq!(q.now, set(&["b.py"]));
        assert!(q.stale.is_empty());
    }
}

#[test]
fn rule_interface_change_leaves_dependents_stale() {
    let prev = previous();
    let none = HashSet::new();
    let changed_b = UpdatePlan {
        changed: vec!["b.py".into()],
        unchanged: vec!["a.py".into()],
        ..UpdatePlan::default()
    };
    let defer = StalePolicy::Defer {
        resolve: BTreeSet::new(),
    };
    let q = requery(Some(&prev), &changed_b, "tool1", &none, &set(&["b.py"]), &defer);
    assert_eq!(q.now, set(&["b.py"]), "the edit itself is re-queried at once");
    assert_eq!(q.stale, set(&["a.py"]), "its dependent is left stale");
    // Negative: nothing depends on an interface change of a file nobody targets.
    let changed_a = UpdatePlan {
        changed: vec!["a.py".into()],
        unchanged: vec!["b.py".into()],
        ..UpdatePlan::default()
    };
    let q = requery(Some(&prev), &changed_a, "tool1", &none, &set(&["a.py"]), &defer);
    assert!(q.stale.is_empty());
}

#[test]
fn rule_stale_dependent_is_resolved_before_a_query_reads_it() {
    // a.py was left stale by an earlier update; nothing changed since.
    let mut prev = previous();
    prev.files[1].semantic = prev.files[0].semantic.clone();
    prev.stale.insert("a.py".into());
    let none = HashSet::new();
    let noop = UpdatePlan {
        unchanged: vec!["a.py".into(), "b.py".into()],
        ..UpdatePlan::default()
    };
    // The watcher keeps it stale until one of its batches resolves it.
    let keep = StalePolicy::Defer {
        resolve: BTreeSet::new(),
    };
    let q = requery(Some(&prev), &noop, "tool1", &none, &none, &keep);
    assert!(q.now.is_empty());
    assert_eq!(q.stale, set(&["a.py"]));
    // The batch names it: re-queried now, no longer stale.
    let frontier = StalePolicy::Defer {
        resolve: BTreeSet::from(["a.py".to_string()]),
    };
    let q = requery(Some(&prev), &noop, "tool1", &none, &none, &frontier);
    assert_eq!(q.now, set(&["a.py"]));
    assert!(q.stale.is_empty());
    // In-process commands resolve everything before answering.
    let q = requery(Some(&prev), &noop, "tool1", &none, &none, &StalePolicy::ResolveAll);
    assert_eq!(q.now, set(&["a.py"]));
    assert!(q.stale.is_empty());
}

#[test]
fn rule_changed_declaration_names_are_the_symmetric_difference() {
    use crate::facts::Declaration;
    use crate::model::{ExecutionModel, Span, SymbolKind};
    let decl = |n: &str| Declaration {
        name: n.into(),
        qualified_name: n.into(),
        kind: SymbolKind::Function,
        span: Span {
            bytes: ByteSpan::new(0, 1),
            start_line: 1,
            end_line: 1,
        },
        name_span: ByteSpan::new(0, 1),
        body_start: 1,
        parent: None,
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
    };
    let mut prev = previous();
    prev.files[1].facts = Some(FileFacts {
        declarations: vec![decl("g"), decl("old")],
        ..FileFacts::default()
    });
    let plan = UpdatePlan {
        changed: vec!["b.py".into()],
        removed: vec!["a.py".into()],
        ..UpdatePlan::default()
    };
    let new_b = FileFacts {
        declarations: vec![decl("g"), decl("run")],
        ..FileFacts::default()
    };
    let names = changed_declaration_names(Some(&prev), &plan, [("b.py", Some(&new_b))]);
    // b.py: `old` disappeared, `run` appeared (`g` kept); a.py declared nothing.
    assert_eq!(names, set(&["old", "run"]));
    // Rule 4: a.py calls `x.run`: a new `run` may be its target now.
    let changed_b = UpdatePlan {
        changed: vec!["b.py".into()],
        unchanged: vec!["a.py".into()],
        ..UpdatePlan::default()
    };
    // (Rule 3 is kept out of this check: no interface change is passed.)
    let q =
        requery(Some(&previous()), &changed_b, "tool1", &names, &HashSet::new(), &StalePolicy::ResolveAll);
    assert!(q.now.contains("a.py"));
    let q = requery(
        Some(&previous()),
        &changed_b,
        "tool1",
        &set(&["other"]),
        &HashSet::new(),
        &StalePolicy::ResolveAll,
    );
    assert!(!q.now.contains("a.py"));
}

#[test]
fn rule_interface_changed_compares_fingerprints() {
    let mut prev = previous();
    prev.files[0].facts.as_mut().unwrap().interface = Hash32::of(b"i1");
    let plan = UpdatePlan {
        changed: vec!["a.py".into()],
        added: vec!["n.py".into()],
        removed: vec!["b.py".into()],
        ..UpdatePlan::default()
    };
    let same = FileFacts {
        interface: Hash32::of(b"i1"),
        ..FileFacts::default()
    };
    let other = FileFacts {
        interface: Hash32::of(b"i2"),
        ..FileFacts::default()
    };
    let changed = interface_changed(Some(&prev), &plan, [("a.py", Some(&same)), ("n.py", Some(&same))]);
    assert_eq!(changed, set(&["n.py", "b.py"]));
    let changed = interface_changed(Some(&prev), &plan, [("a.py", Some(&other))]);
    assert!(changed.contains("a.py"));
    let changed = interface_changed(Some(&prev), &plan, [("a.py", None)]);
    assert!(changed.contains("a.py"), "missing facts count as a change");
}

#[test]
fn rule_index_delta_lists_symbol_changes_of_interface_changes() {
    use crate::facts::Declaration;
    use crate::model::{ExecutionModel, Span, SymbolKind};
    let prev = previous();
    let decl = |q: &str| Declaration {
        name: q.into(),
        qualified_name: q.into(),
        kind: SymbolKind::Function,
        span: Span {
            bytes: ByteSpan::new(0, 1),
            start_line: 1,
            end_line: 1,
        },
        name_span: ByteSpan::new(0, 1),
        body_start: 1,
        parent: None,
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
    };
    let mut a = prev.files[0].clone();
    a.facts = Some(FileFacts {
        declarations: vec![decl("f"), decl("h"), decl("h")],
        ..FileFacts::default()
    });
    let plan = UpdatePlan {
        changed: vec!["a.py".into()],
        unchanged: vec!["b.py".into()],
        ..UpdatePlan::default()
    };
    let requeried = set(&["a.py"]);
    let stale = set(&["b.py"]);
    let d = index_delta(DeltaInput {
        prev: Some(&prev),
        plan: &plan,
        full: false,
        records: std::slice::from_ref(&a),
        requeried: &requeried,
        interface_changed: &set(&["a.py"]),
        stale: &stale,
    });
    assert!(!d.full);
    assert_eq!(d.modified, BTreeSet::from(["a.py".to_string()]));
    assert_eq!(d.symbols_changed, BTreeSet::from(["a.py:f".to_string()]));
    assert_eq!(d.symbols_added, BTreeSet::from(["a.py:h".to_string(), "a.py:h#2".to_string()]));
    assert!(d.symbols_removed.is_empty());
    assert_eq!(d.stale, BTreeSet::from(["b.py".to_string()]));
    // An interface-stable edit touches no symbol.
    let d = index_delta(DeltaInput {
        prev: Some(&prev),
        plan: &plan,
        full: false,
        records: std::slice::from_ref(&a),
        requeried: &requeried,
        interface_changed: &HashSet::new(),
        stale: &HashSet::new(),
    });
    assert!(d.symbols_added.is_empty() && d.symbols_changed.is_empty());
    // Without a previous index the delta is full.
    let d = index_delta(DeltaInput {
        prev: None,
        plan: &plan,
        full: false,
        records: &[],
        requeried: &requeried,
        interface_changed: &HashSet::new(),
        stale: &HashSet::new(),
    });
    assert!(d.full);
}
