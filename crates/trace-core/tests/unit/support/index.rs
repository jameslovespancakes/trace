//! Index builders for unit tests: a small index of `a.py` symbols with proven calls, sites
//! and decisions.

use crate::fingerprint::Hash32;
use crate::languages::{Language, SupportLevel};
use crate::model::*;

/// Build a small index: files `a.py` (symbols given by name) with proven `calls` edges.
pub(crate) fn index_with(names: &[&str], calls: &[(u32, u32)]) -> Index {
    let symbols: Vec<Symbol> = names
        .iter()
        .enumerate()
        .map(|(i, name)| Symbol {
            id: SymbolId(i as u32),
            uid: format!("a.py:{name}"),
            file: FileId(0),
            decl: i as u32,
            name: name.rsplit('.').next().unwrap_or(*name).to_string(),
            qualified_name: name.to_string(),
            kind: SymbolKind::Function,
            language: Language::Python,
            span: Span {
                bytes: ByteSpan::new(i as u32 * 10, i as u32 * 10 + 9),
                start_line: i as u32 + 1,
                end_line: i as u32 + 1,
            },
            name_span: ByteSpan::new(i as u32 * 10 + 4, i as u32 * 10 + 5),
            body_start: i as u32 * 10 + 8,
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
            semantic: true,
        })
        .collect();
    let edges = calls
        .iter()
        .map(|&(f, t)| Edge {
            from: SymbolId(f),
            to: SymbolId(t),
            kind: EdgeKind::Calls,
            tier: Tier::Proven,
            provider: Provider::Pyright,
            resolution: Resolution::CallHierarchy,
            at: Location {
                file: FileId(0),
                bytes: ByteSpan::new(f * 10 + 8, f * 10 + 9),
                line: f + 1,
            },
            site: None,
            bridge: None,
        })
        .collect();
    Index {
        header: IndexHeader {
            schema: crate::SCHEMA_VERSION,
            trace_version: crate::TRACE_VERSION.into(),
            root: "C:/repo".into(),
            built_unix: 0.0,
            syntax_version: 1,
            infer_version: 1,
            bridge_version: 1,
            inventory_fingerprint: Hash32::default(),
            full_builds: 1,
            incremental_updates: 0,
        },
        files: vec![FileRecord {
            path: "a.py".into(),
            language: Language::Python,
            hash: Hash32::default(),
            size: 0,
            mtime_ns: 0,
            support: SupportLevel::Semantic,
            facts: None,
            semantic: None,
            first_symbol: 0,
            symbol_count: names.len() as u32,
            diagnostics: Vec::new(),
            pending: None,
        }],
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

pub(crate) fn site(owner: u32, candidates: &[u32]) -> Site {
    Site {
        id: SiteId(format!("site{owner}")),
        category: SiteCategory::NoTarget,
        owner: SymbolId(owner),
        declared_target: None,
        activation: EdgeKind::Calls,
        at: Location {
            file: FileId(0),
            bytes: ByteSpan::new(owner * 10 + 8, owner * 10 + 9),
            line: owner + 1,
        },
        callee: "x.run".into(),
        candidates: candidates.iter().map(|&c| SymbolId(c)).collect(),
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
    }
}

pub(crate) fn decision(site: u32, targets: &[u32]) -> Decision {
    Decision {
        site,
        status: if targets.is_empty() {
            DecisionStatus::Unknown
        } else {
            DecisionStatus::Decided
        },
        targets: targets.iter().map(|&t| SymbolId(t)).collect(),
        reason: None,
    }
}
