//! Small in-memory indexes for unit tests (no files, no analyzers).

use trace_core::model::{
    ByteSpan, Decision, DecisionStatus, Edge, EdgeKind, ExecutionModel, FileId, FileRecord, IndexHeader,
    Location, Provider, Resolution, Site, SiteCategory, SiteId, Span, Symbol, SymbolId, SymbolKind,
};
use trace_core::{Hash32, Index, Language, SupportLevel};

/// Symbol `qualified` in file `file`; spans are synthetic (one line per symbol id).
pub fn sym(
    id: u32,
    file: u32,
    path: &str,
    qualified: &str,
    kind: SymbolKind,
    doc: Option<&str>,
    parent: Option<u32>,
) -> Symbol {
    let name = qualified.rsplit('.').next().unwrap_or(qualified).to_string();
    let start = id * 100;
    Symbol {
        id: SymbolId(id),
        uid: format!("{path}:{qualified}"),
        file: FileId(file),
        decl: 0,
        name,
        qualified_name: qualified.to_string(),
        kind,
        language: Language::Python,
        span: Span {
            bytes: ByteSpan::new(start, start + 50),
            start_line: id + 1,
            end_line: id + 2,
        },
        name_span: ByteSpan::new(start + 4, start + 8),
        body_start: start + 10,
        parent: parent.map(SymbolId),
        container: None,
        doc: doc.map(str::to_string),
        decorators: Vec::new(),
        bases: Vec::new(),
        parameters: Vec::new(),
        execution: ExecutionModel::Ordinary,
        is_stub: false,
        is_test: false,
        declaration_lines: Vec::new(),
        semantic: true,
    }
}

/// Index over `paths` (sorted) and `symbols` (grouped by file, ids = positions) with
/// proven `calls` edges. Paths under `tests/` are marked as test files (their symbols
/// get `is_test`).
pub fn index(paths: &[&str], mut symbols: Vec<Symbol>, calls: &[(u32, u32)]) -> Index {
    for s in &mut symbols {
        s.is_test = paths[s.file.idx()].starts_with("tests/");
    }
    let files = paths
        .iter()
        .enumerate()
        .map(|(fi, p)| {
            let ids: Vec<u32> = symbols
                .iter()
                .filter(|s| s.file.0 == fi as u32)
                .map(|s| s.id.0)
                .collect();
            FileRecord {
                path: p.to_string(),
                language: Language::Python,
                hash: Hash32::of(p.as_bytes()),
                size: 0,
                mtime_ns: 0,
                support: SupportLevel::Semantic,
                facts: None,
                semantic: None,
                first_symbol: ids.first().copied().unwrap_or(0),
                symbol_count: ids.len() as u32,
                diagnostics: Vec::new(),
                pending: None,
            }
        })
        .collect();
    let edges = calls
        .iter()
        .map(|&(a, b)| Edge {
            from: SymbolId(a),
            to: SymbolId(b),
            kind: EdgeKind::Calls,
            tier: EdgeKind::Calls.tier(),
            provider: Provider::Pyright,
            resolution: Resolution::CallHierarchy,
            at: Location {
                file: symbols[a as usize].file,
                bytes: ByteSpan::new(a * 100 + 20, a * 100 + 25),
                line: a + 1,
            },
            site: None,
            bridge: None,
        })
        .collect();
    Index {
        header: IndexHeader {
            schema: trace_core::SCHEMA_VERSION,
            trace_version: trace_core::TRACE_VERSION.to_string(),
            root: "fixture".into(),
            built_unix: 0.0,
            syntax_version: trace_syntax::EXTRACTOR_VERSION,
            infer_version: trace_infer::INFER_VERSION,
            bridge_version: trace_bridge::BRIDGE_VERSION,
            inventory_fingerprint: Hash32::default(),
            full_builds: 1,
            incremental_updates: 0,
        },
        files,
        configs: Vec::new(),
        omitted: Vec::new(),
        symbols,
        edges,
        unresolved: Vec::new(),
        value_refs: Vec::new(),
        sites: Vec::new(),
        decisions: Vec::new(),
        bridges: Vec::new(),
        support: Vec::new(),
        backend_runs: Vec::new(),
        diagnostics: Vec::new(),
        phase_state: Vec::new(),
        stale: Default::default(),
        library_receivers: Vec::new(),
    }
}

/// Append an undecided site (deterministic abstention) owned by `owner` with
/// `candidates`; returns its index.
pub fn push_site(
    index: &mut Index,
    name: &str,
    category: SiteCategory,
    owner: u32,
    candidates: &[u32],
) -> u32 {
    let start = owner * 100 + 30 + index.sites.len() as u32;
    let at = Location {
        file: index.symbols[owner as usize].file,
        bytes: ByteSpan::new(start, start + 1),
        line: owner + 1,
    };
    let site = index.sites.len() as u32;
    index.sites.push(Site {
        id: SiteId(name.to_string()),
        category,
        owner: SymbolId(owner),
        declared_target: None,
        activation: EdgeKind::Calls,
        at,
        callee: name.to_string(),
        candidates: candidates.iter().copied().map(SymbolId).collect(),
        flow_candidates: Vec::new(),
        field_only: Vec::new(),
        truncated_candidates: false,
        operation: None,
        argument: None,
        via: None,
        test_only: Vec::new(),
        receiver_exact: false,
        library: None,
        declared_library: None,
    });
    index.decisions.push(Decision {
        site,
        status: DecisionStatus::Unknown,
        targets: Vec::new(),
        reason: Some("abstain: not unique".into()),
    });
    site
}

/// A decision choosing `targets` for `site`.
pub fn decided(site: u32, targets: &[u32]) -> Decision {
    Decision {
        site,
        status: DecisionStatus::Decided,
        targets: targets.iter().copied().map(SymbolId).collect(),
        reason: None,
    }
}

/// A small auth/store project. Symbols: 0 `api.py:login_view`, 1 `auth.py:Session`
/// (class), 2 `auth.py:Session.login`, 3 `auth.py:Session._check`, 4 `store.py:Store`
/// (class), 5 `store.py:Store.load`. Calls: 0 -> 2, 2 -> 3, 2 -> 5.
pub fn project() -> Index {
    use SymbolKind::*;
    let symbols = vec![
        sym(0, 0, "api.py", "login_view", Function, None, None),
        sym(1, 1, "auth.py", "Session", Class, Some("User sessions."), None),
        sym(2, 1, "auth.py", "Session.login", Method, Some("Log a user in."), Some(1)),
        sym(3, 1, "auth.py", "Session._check", Method, None, Some(1)),
        sym(4, 2, "store.py", "Store", Class, None, None),
        sym(5, 2, "store.py", "Store.load", Method, Some("Load a record."), Some(4)),
    ];
    index(&["api.py", "auth.py", "store.py"], symbols, &[(0, 2), (2, 3), (2, 5)])
}

/// A fresh root and cache home under the temp directory for a pipeline test (`name` keeps
/// parallel tests apart).
pub fn fixture_paths(name: &str) -> (std::path::PathBuf, trace_core::paths::RepoPaths) {
    let base = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("pipeline-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let paths = trace_core::paths::RepoPaths::resolve_in(&root, &base.join("home")).unwrap();
    (base, paths)
}
