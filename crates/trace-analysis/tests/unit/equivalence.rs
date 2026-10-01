use super::*;
use trace_core::model::{ByteSpan, Diagnostic, Edge, EdgeKind, Provider, Resolution, Tier};

fn empty_index() -> Index {
    let mut index = trace_core::assemble::assemble(trace_core::assemble::AssembleInput {
        header: trace_core::model::IndexHeader {
            schema: trace_core::SCHEMA_VERSION,
            trace_version: trace_core::TRACE_VERSION.into(),
            root: "/repo".into(),
            built_unix: 1.0,
            syntax_version: 1,
            infer_version: 1,
            bridge_version: 1,
            inventory_fingerprint: trace_core::Hash32::default(),
            full_builds: 1,
            incremental_updates: 0,
        },
        files: Vec::new(),
        configs: Vec::new(),
        omitted: Vec::new(),
        support: Vec::new(),
        backend_runs: Vec::new(),
        diagnostics: Vec::new(),
    });
    index.diagnostics.push(Diagnostic::new("note", None, "x".to_string()));
    index
}

#[test]
fn rule_equivalence_ignores_volatile_fields() {
    let a = empty_index();
    let mut b = a.clone();
    b.header.built_unix = 99.0;
    b.header.incremental_updates = 7;
    assert!(compare(&a, &b).is_empty());
    b.diagnostics
        .push(Diagnostic::new("extra", Some("m.py".into()), "y".to_string()));
    let d = compare(&a, &b);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].area, "diagnostics");
    assert_eq!(d[0].incremental, "x0");
}

#[test]
fn rule_equivalence_compares_ids_by_uid() {
    // Same edge, but the positional ids differ because a file was ordered differently:
    // compared through uids it is one difference only when the uids differ.
    let mut a = empty_index();
    let mut b = a.clone();
    let edge = Edge {
        from: SymbolId(0),
        to: SymbolId(0),
        kind: EdgeKind::Calls,
        tier: Tier::Proven,
        provider: Provider::Pyright,
        resolution: Resolution::CallHierarchy,
        at: Location {
            file: FileId(0),
            bytes: ByteSpan::new(1, 2),
            line: 1,
        },
        site: None,
        bridge: None,
    };
    a.edges.push(edge.clone());
    b.edges.push(edge);
    assert!(compare(&a, &b).is_empty());
    b.edges[0].at.line = 2;
    let d = compare(&a, &b);
    assert_eq!(d.len(), 2, "one edge on each side: {d:?}");
    assert!(d.iter().all(|x| x.area == "edges"));
}
