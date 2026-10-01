//! Scale test (SPEC §5.1a, NEXT.md item 0): synthetic Python repositories of 200 and
//! 1 000 files written under `<temp>/trace-tests/trace-fixtures-scale-*` (removed
//! afterwards), extracted with trace-syntax and given by-name semantics (every name of the
//! corpus is unique, so the synthetic resolver is exact). Site generation must do
//! near-linear work: evaluations(1k) <= 6 x evaluations(200). Work counters only, never
//! wall-clock time.

use std::collections::HashMap;

use trace_core::assemble::{assemble, AssembleInput};
use trace_core::semantics::{FileSemantics, SemEdge, SemUnresolved, SemValueRef};
use trace_core::source::SourceStore;
use trace_core::{
    EdgeKind, FileRecord, Hash32, IndexHeader, Language, Provider, Resolution, SupportLevel, UnresolvedKind,
};

use crate::hierarchy::Hierarchy;
use crate::sites::{generate_report, SiteStats};
use crate::test_support::canonical;

fn path_of(k: usize) -> String {
    format!("pkg{}/mod{k}.py", k / 100)
}

fn source_of(k: usize) -> String {
    let prev = k.saturating_sub(1);
    let import = if k > 0 {
        format!("from pkg{}.mod{prev} import Handler{prev}\n\n", prev / 100)
    } else {
        String::new()
    };
    format!(
        "{import}class Handler{k}:\n    def __init__(self, cb):\n        self.cb = cb\n        self.next = None\n\n    def run(self, x):\n        return self.cb(x)\n\n    def chain(self, other):\n        self.next = other\n        return self.next.run(1)\n\n\ndef work{k}(x):\n    return x\n\n\ndef build{k}():\n    h = Handler{k}(work{k})\n    h.chain(Handler{prev}(work{k}))\n    return h.run(2)\n"
    )
}

/// Exact by-name semantics for the synthetic corpus.
fn semantics(
    facts: &trace_core::facts::FileFacts,
    k: usize,
    globals: &HashMap<String, String>,
) -> FileSemantics {
    let mut edges = Vec::new();
    let mut unresolved = Vec::new();
    for c in &facts.calls {
        let Some(owner) = c.owner else { continue };
        let target = if let Some(uid) = globals.get(&c.callee) {
            Some(uid.clone())
        } else {
            c.callee
                .strip_prefix("h.")
                .map(|m| format!("{}:Handler{k}.{m}", path_of(k)))
        };
        match target {
            Some(target) => edges.push(SemEdge {
                owner,
                target,
                kind: EdgeKind::Calls,
                at: c.callee_span,
                line: c.line,
                resolution: Resolution::CallHierarchy,
            }),
            None => unresolved.push(SemUnresolved {
                owner: Some(owner),
                kind: UnresolvedKind::NoSemanticTarget,
                at: c.callee_span,
                line: c.line,
                callee: c.callee.clone(),
                candidates: Vec::new(),
            }),
        }
    }
    let value_refs = facts
        .references
        .iter()
        .filter_map(|r| {
            globals.get(&r.name).map(|uid| SemValueRef {
                at: r.span,
                line: 1,
                target: uid.clone(),
            })
        })
        .collect();
    FileSemantics {
        provider: Provider::Pyright,
        tool_fingerprint: "scale-fixture".into(),
        edges,
        unresolved,
        value_refs,
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

/// Write, extract, resolve and generate sites for an `n`-file corpus.
fn run(n: usize) -> (SiteStats, Vec<String>) {
    let base = std::env::temp_dir().join("trace-tests");
    std::fs::create_dir_all(&base).expect("artifacts dir");
    let dir = tempfile::Builder::new()
        .prefix("trace-fixtures-scale-")
        .tempdir_in(&base)
        .expect("temp dir");
    let root = canonical(dir.path());
    let sources: Vec<(String, String)> = (0..n).map(|k| (path_of(k), source_of(k))).collect();
    for (p, s) in &sources {
        let full = dir.path().join(p);
        std::fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
        std::fs::write(&full, s.as_bytes()).expect("write");
    }
    let inputs: Vec<trace_syntax::SourceInput<'_>> = sources
        .iter()
        .map(|(p, s)| trace_syntax::SourceInput {
            path: p,
            language: Language::Python,
            source: s.as_bytes(),
        })
        .collect();
    let facts: Vec<trace_core::facts::FileFacts> = trace_syntax::extract_many(&inputs)
        .into_iter()
        .map(|r| r.expect("synthetic python parses"))
        .collect();
    let mut globals: HashMap<String, String> = HashMap::new();
    for k in 0..n {
        for name in [format!("Handler{k}"), format!("work{k}"), format!("build{k}")] {
            globals.insert(name.clone(), format!("{}:{name}", path_of(k)));
        }
    }
    let files: Vec<FileRecord> = facts
        .into_iter()
        .enumerate()
        .map(|(k, f)| {
            let sem = semantics(&f, k, &globals);
            let bytes = sources[k].1.as_bytes();
            FileRecord {
                path: sources[k].0.clone(),
                language: Language::Python,
                hash: Hash32::of(bytes),
                size: bytes.len() as u64,
                mtime_ns: 0,
                support: SupportLevel::Semantic,
                facts: Some(f),
                semantic: Some(sem),
                first_symbol: 0,
                symbol_count: 0,
                diagnostics: Vec::new(),
                pending: None,
            }
        })
        .collect();
    let index = assemble(AssembleInput {
        header: IndexHeader {
            schema: trace_core::SCHEMA_VERSION,
            trace_version: trace_core::TRACE_VERSION.into(),
            root,
            built_unix: 0.0,
            syntax_version: trace_syntax::EXTRACTOR_VERSION,
            infer_version: crate::INFER_VERSION,
            bridge_version: 0,
            inventory_fingerprint: Hash32::default(),
            full_builds: 1,
            incremental_updates: 0,
        },
        files,
        configs: Vec::new(),
        omitted: Vec::new(),
        support: Vec::new(),
        backend_runs: Vec::new(),
        diagnostics: Vec::new(),
    });
    let hierarchy = Hierarchy::build(&index);
    let store = SourceStore::new(&index);
    let generated = generate_report(&index, &store, &hierarchy, crate::LibraryInputs::none()).expect("sites");
    let kinds = generated.diagnostics.iter().map(|d| d.kind.clone()).collect();
    (generated.stats, kinds)
}

#[test]
fn flow_work_grows_near_linearly() {
    let (small, small_diags) = run(200);
    let (large, large_diags) = run(1_000);
    eprintln!("scale 200: {small:?}");
    eprintln!("scale 1k: {large:?}");
    assert!(small.flow.evals > 0 && small.sites > 0, "the corpus produces flow work");
    assert!(
        large.flow.evals <= 6 * small.flow.evals,
        "evaluations 1k {} > 6 x 200 {}",
        large.flow.evals,
        small.flow.evals
    );
    assert!(large.flow.items <= 6 * small.flow.items);
    assert!(large.flow.values <= 6 * small.flow.values);
    assert!(large.sites <= 6 * small.sites);
    assert!(large.flow.rounds_product < trace_core::config::current().flow.max_iterations as u64);
    assert!(!large.flow.budget_exhausted);
    // The shared field-name slot (`.cb` of every handler) is capped and reported.
    assert!(large.flow.saturated_slots > 0);
    assert!(large_diags.iter().any(|k| k == "flow_bound"));
    let _ = small_diags;
}
