//! trace-bridge: cross-language bridges (NEXT.md P2b items 21-28, SPEC §15a; PLAN decision 14).
//!
//! Input: the assembled [`Index`] (per-file syntax facts, semantics, declarations, imports,
//! proven edges), the library knowledge of trace-library (channel effects derived from the
//! installed package source), the irreducible table rows, the installed packages, verified
//! sources read through [`SourceStore`] (the files' own syntax trees for argument values, and
//! the contract files `Language::{Proto, GraphQl, OpenApi}`), and the `bridges` section of the
//! configuration (hand-written manifests). Output: [`Bridge`] records, sorted and
//! deduplicated, each with its kind, tier (never above `BridgeKind::max_tier`), provider,
//! resolution, evidence locations on both sides, label and assumptions. Nothing is executed;
//! contract files are parsed with structured parsers only.
//!
//! No framework or package is known by name: `recognize` turns every file into endpoints
//! (its module docs list the sources), then the matchers link them.
//!
//! Tier rules (SPEC §15a, DESIGN §1.15):
//! * toolchain/packaging rules (`js_ts`, `c_cpp`, `python_stub`) and ABI/binding rules
//!   (`c_abi`, `jni`, `cgo`, `pyo3`, `wasm_bindgen`, `napi`, `cpython`): proven when the
//!   boundary name matches exactly one providing declaration, else one `possible` row per
//!   candidate;
//! * declared contracts (`grpc`, `openapi`, `graphql`) and HTTP routes: inferred when method
//!   and path template / service method match exactly one handler, else possible; dynamic
//!   keys are possible;
//! * weak boundaries (`subprocess`, `ffi`, `message`): possible only;
//! * a crossing with an end built from DERIVED channel effects is inferred at most when the
//!   bridge gate of that end's language passed (`assets/library/gate.json` `bridges`), else
//!   possible;
//! * manifests: `user_contract`, inferred, both endpoints pinned by file hash and byte span
//!   (stale entries are dropped with a diagnostic).
//!
//! Modules: `recognize` (per-file endpoints), `ctx` (shared lookups, emission), `abi` (C
//! symbol, JNI, cgo, FFI), `bindings` (PyO3, CPython, wasm-bindgen, N-API, `.pyi` of compiled
//! modules, `.d.ts`), `http` (key normalization, routes, clients, mounts, OpenAPI), `rpc`
//! (gRPC, GraphQL), `weak` (subprocess, messages), `manifest`, `contracts` (contract readers;
//! YAML through `trace_core::formats::yaml`).
//!
//! Incremental detection (PLAN decision 13, DESIGN §1.14.5): [`BridgeState`] caches the
//! endpoints of every file with the fingerprint of everything they were computed from; an
//! update re-recognizes only files whose fingerprint changed and re-runs the matching (linear,
//! in memory) over all endpoints, so [`detect_delta`] equals [`detect`] on the same input.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use rayon::prelude::*;

use trace_core::config::BridgeSettings;
use trace_core::facts::BoundaryFact;
use trace_core::model::{Bridge, Diagnostic, FileId, FileRecord, Index, Tier};
use trace_core::source::SourceStore;
use trace_core::{Hash32, Language};
use trace_library::gate::Gate;

mod abi;
mod bindings;
mod contracts;
pub(crate) mod ctx;
mod http;
mod manifest;
mod recognize;
mod rpc;
mod weak;

/// Version of the recognition and matching rules; part of the index header's infer
/// fingerprint via the pipeline (bump on any change of produced bridges for the same input).
/// 2: first detecting implementation (1 was the empty stub).
/// 3: wasm-bindgen / N-API method calls on instances of exported classes, enum variant
/// reads, and import bindings of every imported export; HTTP mount prefixes resolved through
/// Python path constants (`settings.API_V1_STR`); identical .proto copies are one contract;
/// gRPC handler-table entries resolve their named handler.
/// 4: C/C++ prototypes of Go functions exported with cgo `//export name` (`cgo` kind).
/// 5: endpoints from derived channel effects, irreducible primitives, fs-route and
/// reflection-root rows (PLAN decision 14); placeholder families from `route_patterns` rows;
/// derived crossings below the bridge gate are possible.
/// 6 (language fixes): library dispatch facts in the file fingerprint, bridges gate deletion.
pub const BRIDGE_VERSION: u32 = 7;

/// Everything bridge detection reads.
pub struct BridgeInput<'a> {
    pub index: &'a Index,
    pub sources: &'a SourceStore<'a>,
    pub config: &'a BridgeSettings,
    /// Channel effects per library call (trace-library, computed before bridges).
    pub knowledge: &'a trace_library::LibraryKnowledge,
    /// Irreducible table sections.
    pub tables: &'a trace_library::table::Tables,
    /// Installed dependency packages (`activated_by` rows).
    pub installed: &'a trace_library::installed::InstalledPackages,
}

/// Incremental bridge state (PhaseState "bridges"): the endpoints of every file with the
/// fingerprint of their inputs, and the fingerprint of the global inputs.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BridgeState {
    /// Tables, installed packages, gate, configuration, annotation declarations, version.
    pub inputs: Hash32,
    /// Repository path -> endpoint block.
    pub files: BTreeMap<String, FileBlock>,
    /// Java files and whether they declare annotation types (reflection chains).
    pub annotations: BTreeMap<String, recognize::AnnotationFile>,
}

/// The endpoints of one file.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileBlock {
    /// Content hash, language, semantics (library calls / files / expansions) and knowledge
    /// entries of the file.
    pub fingerprint: Hash32,
    pub endpoints: Vec<BoundaryFact>,
}

/// Detected bridges plus diagnostics (`bridge_manifest_stale`, `contract_unparsed`, ...).
#[derive(Debug, Default)]
pub struct BridgeOutput {
    pub bridges: Vec<Bridge>,
    pub diagnostics: Vec<Diagnostic>,
}

/// The embedded bridge gate (an unreadable embedded gate counts as "not passed": derived
/// crossings stay possible; the gate file is validated by trace-library's tests).
fn builtin_gate() -> &'static Gate {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(|| Gate::load_builtin().unwrap_or_default())
}

/// Detect every bridge of an assembled index. Deterministic; never fails (problems become
/// diagnostics).
pub fn detect(input: BridgeInput<'_>) -> BridgeOutput {
    run(&input, builtin_gate(), None)
}

/// Incremental detection (PLAN decision 13): cached endpoints of unchanged files + matching
/// over all endpoints; equals [`detect`] on the same input. A full delta (or a state from
/// other global inputs) recognizes every file and seeds the state. The previous output is
/// not needed: matching is recomputed, and its diagnostics with it.
pub fn detect_delta(
    input: BridgeInput<'_>,
    _prev: BridgeOutput,
    delta: &trace_core::delta::IndexDelta,
    state: &mut BridgeState,
) -> BridgeOutput {
    run(&input, builtin_gate(), Some((delta, state)))
}

/// Detection with an explicit gate, full or incremental.
pub(crate) fn run(
    input: &BridgeInput<'_>,
    gate: &Gate,
    incremental: Option<(&trace_core::delta::IndexDelta, &mut BridgeState)>,
) -> BridgeOutput {
    let (delta, state) = match incremental {
        Some((d, s)) => (Some(d), Some(s)),
        None => (None, None),
    };
    if !input.config.enabled {
        if let Some(s) = state {
            *s = BridgeState::default();
        }
        return BridgeOutput::default();
    }
    // `TRACE_PROFILE=1`: one `profile-bridges: <step> <secs>s` line per detection step.
    let profile = trace_core::env::profile();
    let mut last = std::time::Instant::now();
    let mut lap = |step: &str| {
        if profile {
            let now = std::time::Instant::now();
            eprintln!("profile-bridges: {step} {:.3}s", (now - last).as_secs_f64());
            last = now;
        }
    };
    let annotations = recognize::annotation_files(
        input,
        match (delta, state.as_deref()) {
            (Some(d), Some(s)) if !d.full => Some(&s.annotations),
            _ => None,
        },
    );
    let recognizer = recognize::Recognizer::new(input, &annotations);
    let global = global_fingerprint(input, gate, &annotations);
    let previous: Option<&BridgeState> = match (delta, state.as_deref()) {
        (Some(d), Some(s)) if !d.full && s.inputs == global => Some(s),
        _ => None,
    };
    let mut next = BridgeState {
        inputs: global,
        files: BTreeMap::new(),
        annotations: annotations.clone(),
    };
    let mut endpoints: Vec<recognize::Endpoint> = Vec::new();
    let mut unreadable = 0usize;
    let mut recognized = 0usize;
    let files = &input.index.files;
    let fingerprints: Vec<Hash32> = files
        .iter()
        .map(|rec| file_fingerprint(rec, input.knowledge))
        .collect();
    let cached: Vec<Option<Vec<BoundaryFact>>> = files
        .iter()
        .zip(&fingerprints)
        .map(|(rec, fingerprint)| {
            previous
                .filter(|_| !delta.is_some_and(|d| d.file_changed(&rec.path)))
                .and_then(|p| p.files.get(&rec.path))
                .filter(|b| b.fingerprint == *fingerprint)
                .map(|b| b.endpoints.clone())
        })
        .collect();
    // Cached endpoints, else the file recognized now (in parallel: the endpoints of a file
    // depend only on the file and the global inputs).
    let blocks: Vec<Result<Vec<BoundaryFact>, recognize::FileOut>> = cached
        .into_par_iter()
        .enumerate()
        .map(|(i, c)| c.ok_or_else(|| recognizer.file(FileId(i as u32))))
        .collect();
    for ((i, rec), (block, fingerprint)) in files.iter().enumerate().zip(blocks.into_iter().zip(fingerprints))
    {
        let file = FileId(i as u32);
        let facts = match block {
            Ok(f) => f,
            Err(out) => {
                recognized += 1;
                if out.unreadable {
                    unreadable += 1;
                    endpoints.extend(out.facts.into_iter().map(|fact| recognize::Endpoint {
                        file,
                        language: rec.language,
                        fact,
                    }));
                    continue;
                }
                out.facts
            }
        };
        next.files.insert(
            rec.path.clone(),
            FileBlock {
                fingerprint,
                endpoints: facts.clone(),
            },
        );
        endpoints.extend(facts.into_iter().map(|fact| recognize::Endpoint {
            file,
            language: rec.language,
            fact,
        }));
    }
    if profile && delta.is_some_and(|d| !d.full) {
        eprintln!("profile-bridges: recognized {recognized} of {} files", input.index.files.len());
    }
    if profile {
        for ((path, at), b) in &input.knowledge.by_call {
            eprintln!("profile-bridges: knowledge {path}@{at} {:?} {:?} {:?}", b.symbol, b.source, b.effects);
        }
        for e in &endpoints {
            let path = input.index.files.get(e.file.idx()).map_or("?", |r| r.path.as_str());
            eprintln!(
                "profile-bridges: endpoint {path}:{} {:?} {:?} {} {:?}",
                e.fact.line, e.fact.kind, e.fact.role, e.fact.name, e.fact.detail
            );
        }
    }
    if let Some(s) = state {
        *s = next;
    }
    lap("recognize");
    let mut cx = ctx::Ctx::new(input.index, input.sources, &endpoints);
    lap("context");
    let contracts = contracts::load(&mut cx);
    lap("contracts");
    abi::detect(&mut cx, input.config.weak);
    lap("abi");
    bindings::detect(&mut cx);
    lap("bindings");
    rpc::grpc(&mut cx, &contracts.services);
    rpc::graphql(&mut cx, &contracts.graphql);
    lap("rpc");
    if input.config.http {
        http::detect(&mut cx, &contracts.operations);
    }
    lap("http");
    if input.config.weak {
        weak::subprocess(&mut cx);
        weak::messages(&mut cx);
    }
    manifest::detect(&mut cx, &input.config.manifests);
    lap("weak+manifest");
    if unreadable > 0 {
        cx.diag(
            "bridge_source_unreadable",
            None,
            format!("{unreadable} files could not be read for bridge recognition; their package-derived crossings are missing"),
        );
    }
    let derived = derived_locations(&endpoints);
    finish(cx, &derived, gate)
}

/// Fingerprint of the global inputs of the recognition.
fn global_fingerprint(
    input: &BridgeInput<'_>,
    gate: &Gate,
    annotations: &BTreeMap<String, recognize::AnnotationFile>,
) -> Hash32 {
    let mut h = blake3::Hasher::new();
    h.update(&BRIDGE_VERSION.to_le_bytes());
    h.update(format!("{:?}", input.config).as_bytes());
    h.update(format!("{:?}", input.installed).as_bytes());
    h.update(format!("{:?}", input.tables).as_bytes());
    h.update(format!("{gate:?}").as_bytes());
    for (path, f) in annotations.iter().filter(|(_, f)| f.declares) {
        h.update(path.as_bytes());
        h.update(&f.hash.0);
    }
    Hash32(*h.finalize().as_bytes())
}

/// Fingerprint of everything one file's endpoints are computed from.
fn file_fingerprint(rec: &FileRecord, knowledge: &trace_library::LibraryKnowledge) -> Hash32 {
    let mut h = blake3::Hasher::new();
    h.update(rec.path.as_bytes());
    h.update(rec.language.as_str().as_bytes());
    h.update(&rec.hash.0);
    h.update(&[u8::from(rec.facts.is_some())]);
    if let Some(sem) = &rec.semantic {
        h.update(
            format!(
                "{:?}{:?}{:?}{:?}",
                sem.library_calls, sem.library_files, sem.expanded, sem.library_dispatch
            )
            .as_bytes(),
        );
    }
    for (key, b) in knowledge
        .by_call
        .range((rec.path.clone(), 0)..=(rec.path.clone(), u32::MAX))
    {
        h.update(&key.1.to_le_bytes());
        h.update(format!("{b:?}").as_bytes());
    }
    Hash32(*h.finalize().as_bytes())
}

/// Evidence locations of endpoints built from derived channel effects, with their language.
fn derived_locations(endpoints: &[recognize::Endpoint]) -> HashMap<(FileId, u32, u32), Language> {
    endpoints
        .iter()
        .filter(|e| {
            e.fact
                .detail
                .iter()
                .any(|(k, v)| k == recognize::DERIVED && v == "true")
        })
        .map(|e| ((e.file, e.fact.span.start, e.fact.span.end), e.language))
        .collect()
}

fn finish(cx: ctx::Ctx<'_>, derived: &HashMap<(FileId, u32, u32), Language>, gate: &Gate) -> BridgeOutput {
    let ctx::Ctx {
        mut bridges,
        mut diagnostics,
        unresolved_ends,
        ..
    } = cx;
    if bridges.len() >= ctx::MAX_BRIDGES {
        diagnostics.push(Diagnostic::new(
            "bridge_limit",
            None,
            format!("bridge detection stopped at {} records", ctx::MAX_BRIDGES),
        ));
    }
    if unresolved_ends > 0 {
        diagnostics.push(Diagnostic::new(
            "bridge_endpoint_unresolved",
            None,
            format!("{unresolved_ends} boundary facts had no symbol to attach a bridge to"),
        ));
    }
    // Derived channel effects: inferred only when the language's bridge gate passed.
    for b in bridges.iter_mut() {
        let mut languages: Vec<Language> = [b.from_at, b.to_at]
            .iter()
            .filter_map(|at| derived.get(&(at.file, at.bytes.start, at.bytes.end)).copied())
            .collect();
        if languages.is_empty() {
            continue;
        }
        languages.sort();
        languages.dedup();
        b.assumptions
            .push("the channel behaviour of the package call was derived from its installed source".into());
        let below: Vec<&str> = languages
            .iter()
            .filter(|l| !gate.bridges_passed(**l))
            .map(|l| l.as_str())
            .collect();
        if !below.is_empty() && b.tier != Tier::Possible {
            b.tier = Tier::Possible;
            b.assumptions.push(format!(
                "the bridge gate for {} has not passed: derived crossings stay possible",
                below.join(", ")
            ));
        }
    }
    // Defensive: a record can never be stronger than its kind allows.
    bridges.retain(|b| b.tier >= b.kind.max_tier());
    // Strongest tier first among identical crossings, then deduplicate.
    bridges.sort_by(|a, b| {
        (a.kind, a.from, a.to, a.from_at, a.to_at, a.tier)
            .cmp(&(b.kind, b.from, b.to, b.from_at, b.to_at, b.tier))
    });
    bridges.dedup_by(|later, kept| {
        later.kind == kept.kind
            && later.from == kept.from
            && later.to == kept.to
            && later.from_at == kept.from_at
            && later.to_at == kept.to_at
    });
    BridgeOutput { bridges, diagnostics }
}

#[cfg(test)]
#[path = "../tests/unit/support/mod.rs"]
pub(crate) mod test_support;

#[cfg(test)]
#[path = "../tests/unit/lib.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/unit/families.rs"]
mod families_tests;
