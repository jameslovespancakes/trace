//! Framework reproduction (bridges gate, DESIGN-bridges §6.2; verify group v-xlang-library).
//!
//! Every framework / library family the deleted framework tables covered (bridges gate step
//! 4 removed them; trace names no package outside the irreducible rows) has a fixture
//! project `tests/fixtures/rule-bridges-<ecosystem>` (sources + `deps.json` naming what the lab
//! installs + `expected.json`). The lab's `setup_deps.py` copies each fixture to
//! `$TRACE_BRIDGE_FIXTURES/<fixture dir>` and installs its packages there (never into the trace
//! repository). A test here:
//! 1. builds the index of the installed copy (syntax facts; dependency folders skipped),
//! 2. stands in for the language server: each listed call gets the library declaration it
//!    resolves to (`target`: a file glob under a library root + the declaration's qualified
//!    name or a text snippet at its start) or its runtime symbol (`symbol`, for primitives
//!    without source),
//! 3. asks trace-library for the behaviour of those calls (derivation from the installed
//!    source - no package knowledge anywhere),
//! 4. checks that the file endpoints contain every expected endpoint (the old tables'
//!    registrations / clients / bindings), and that the expected bridges exist.
//!
//! Without `TRACE_BRIDGE_FIXTURES` or without the installed copy the test says so and passes
//! (nothing to measure); the verify stage runs them with the packages installed:
//! `cargo nextest run --release -E 'test(/rule_bridges_/)'`. A family that cannot be
//! reproduced moves to the irreducible table with a concrete `why_not_derivable` (then its
//! expected endpoints are produced from that row) - never a skipped test.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use trace_core::assemble::{assemble, AssembleInput};
use trace_core::config::BridgeSettings;
use trace_core::facts::{BoundaryRole, CallSite, FileFacts};
use trace_core::model::{BridgeKind, FileRecord, Index, IndexHeader};
use trace_core::semantics::{FileSemantics, LibraryFile, SemLibraryCall};
use trace_core::source::SourceStore;
use trace_core::text::LineIndex;
use trace_core::{Hash32, Language, SupportLevel};
use trace_env::{EcosystemId, LibraryKind, LibraryRoot};
use trace_library::gate::{BridgeGate, Gate};
use trace_library::installed::InstalledPackages;
use trace_library::model::{BehaviourRequest, RequestArg};
use trace_library::{Library, LibraryKnowledge};

use crate::{BridgeInput, BRIDGE_VERSION};

/// Folders of installed dependencies and build output (not repository sources).
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    ".venv",
    "venv",
    "vendor",
    "target",
    "__pycache__",
    "site-packages",
    ".gradle",
    "build",
    "dist",
    "bin",
    "obj",
    "deps",
    "gopath",
    "gomodcache",
    "packages",
];

#[derive(Debug, Deserialize)]
struct Expected {
    #[serde(default)]
    roots: Vec<RootSpec>,
    families: BTreeMap<String, Family>,
}

/// A library root of the installed copy: a folder found by name below the copy, a path
/// relative to it, an absolute folder from an environment variable, the output of a
/// toolchain query (`go env GOMODCACHE`), or a folder below the user's home (`join` is
/// appended to these three). The first that exists is used.
#[derive(Debug, Deserialize)]
struct RootSpec {
    #[serde(default)]
    find: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    env: Option<String>,
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    home: Option<String>,
    #[serde(default)]
    join: Option<String>,
    ecosystem: String,
    layout: String,
    #[serde(default)]
    stdlib: bool,
}

#[derive(Debug, Deserialize)]
struct Family {
    #[serde(default)]
    calls: Vec<CallSpec>,
    #[serde(default)]
    endpoints: Vec<EndpointSpec>,
    #[serde(default)]
    bridges: Vec<BridgeSpec>,
    /// Packages that must be installed for the family's rows (`activated_by`).
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CallSpec {
    file: String,
    callee: String,
    #[serde(default)]
    occurrence: usize,
    #[serde(default)]
    target: Option<TargetSpec>,
    /// Runtime symbol of a primitive without source (`fetch`, `dlsym`).
    #[serde(default)]
    symbol: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TargetSpec {
    #[serde(default)]
    root: usize,
    /// Glob of the declaration file relative to the root.
    file: String,
    #[serde(default)]
    qualified: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EndpointSpec {
    file: String,
    kind: String,
    role: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct BridgeSpec {
    kind: String,
    from: String,
    to: String,
}

fn kind_of(name: &str) -> BridgeKind {
    BridgeKind::ALL
        .into_iter()
        .find(|k| k.to_string() == name)
        .unwrap_or_else(|| panic!("unknown bridge kind {name}"))
}

fn ecosystem_of(name: &str) -> EcosystemId {
    EcosystemId::ALL
        .into_iter()
        .find(|e| e.as_str() == name)
        .unwrap_or_else(|| panic!("unknown ecosystem {name}"))
}

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

/// Repository sources of the installed copy (dependency folders skipped), bounded.
fn sources(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if out.len() > 2_000 {
            return;
        }
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
                sources(root, &path, out);
            }
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push((rel.to_string_lossy().replace('\\', "/"), path));
        }
    }
}

/// Files below `dir` (bounded walk).
fn files_below(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 12 || out.len() > 200_000 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            files_below(&p, depth + 1, out);
        } else {
            out.push(p);
        }
    }
}

/// The first folder named `name` below `dir` (breadth-first, bounded).
fn find_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut queue = std::collections::VecDeque::from([(dir.to_path_buf(), 0usize)]);
    while let Some((d, depth)) = queue.pop_front() {
        if depth > 6 {
            continue;
        }
        let Ok(entries) = fs::read_dir(&d) else { continue };
        let mut entries: Vec<_> = entries.flatten().filter(|e| e.path().is_dir()).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            if e.file_name().to_string_lossy() == name {
                return Some(e.path());
            }
            queue.push_back((e.path(), depth + 1));
        }
    }
    None
}

/// Output of a toolchain query (`python` is `TRACE_TEST_PYTHON` when set). Tests only.
fn query(command: &[String]) -> Option<PathBuf> {
    let (program, args) = command.split_first()?;
    let program = match (program.as_str(), trace_core::env::test::python()) {
        ("python", Some(p)) => p,
        _ => program.into(),
    };
    let out = std::process::Command::new(program).args(args).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !text.is_empty()).then(|| PathBuf::from(text))
}

fn resolve_root(copy: &Path, spec: &RootSpec) -> Option<LibraryRoot> {
    let home = || trace_core::env::test::home().map(PathBuf::from);
    let joined = |p: PathBuf| match &spec.join {
        Some(j) => p.join(j),
        None => p,
    };
    let candidates: Vec<Option<PathBuf>> = vec![
        spec.find.as_ref().and_then(|name| find_dir(copy, name)),
        spec.path.as_ref().map(|rel| copy.join(rel)),
        spec.env
            .as_ref()
            .and_then(|name| trace_core::env::test::var(name))
            .map(PathBuf::from)
            .map(joined),
        query(&spec.command).map(joined),
        spec.home
            .as_ref()
            .and_then(|rel| home().map(|h| h.join(rel)))
            .map(joined),
    ];
    let path = candidates.into_iter().flatten().find(|p| p.is_dir())?;
    Some(LibraryRoot {
        path,
        kind: if spec.stdlib {
            LibraryKind::Stdlib
        } else {
            LibraryKind::Dependency
        },
        ecosystem: ecosystem_of(&spec.ecosystem),
        layout: match spec.layout.as_str() {
            "site_packages" => "site_packages",
            "node_modules" => "node_modules",
            "go_modcache" => "go_modcache",
            "cargo_registry" => "cargo_registry",
            "maven_repo" => "maven_repo",
            "gradle_cache" => "gradle_cache",
            "nuget_packages" => "nuget_packages",
            "php_vendor" => "php_vendor",
            "toolchain_stdlib" => "toolchain_stdlib",
            other => panic!("unknown layout {other}"),
        },
        version: None,
    })
}

/// (library file, 0-based line, byte column) of the target declaration.
fn locate(root: &LibraryRoot, target: &TargetSpec) -> Option<(LibraryFile, u32, u32)> {
    let mut all = Vec::new();
    files_below(&root.path, 0, &mut all);
    all.sort();
    for path in all {
        let Ok(rel) = path.strip_prefix(&root.path) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if !trace_syntax::testing::glob_matches(&target.file, &rel) {
            continue;
        }
        let language = trace_core::languages::from_path(&path)?;
        let Ok(source) = fs::read(&path) else { continue };
        let lines = LineIndex::new(&source);
        let at = match (&target.qualified, &target.text) {
            (Some(q), _) => {
                let facts = trace_syntax::extract(trace_syntax::SourceInput {
                    path: &rel,
                    language,
                    source: &source,
                })
                .ok()?;
                facts
                    .declarations
                    .iter()
                    .find(|d| d.qualified_name == *q || d.qualified_name.ends_with(&format!(".{q}")))
                    .map(|d| d.name_span.start)
            }
            (None, Some(t)) => {
                let text = String::from_utf8_lossy(&source);
                text.find(t.as_str()).map(|i| i as u32)
            }
            (None, None) => None,
        };
        let Some(at) = at else { continue };
        let line = lines.line0(at);
        let start = lines.line_span(&source, line).map(|s| s.start).unwrap_or(0);
        let package = rel.split('/').next().unwrap_or_default().to_string();
        return Some((
            LibraryFile {
                path: path.to_string_lossy().into_owned(),
                package,
                version: None,
                stdlib: root.kind == LibraryKind::Stdlib,
                readable: true,
                language,
            },
            line,
            at - start,
        ));
    }
    None
}

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

fn index_of(copy: &Path) -> Index {
    let mut listed = Vec::new();
    sources(copy, copy, &mut listed);
    let mut files = Vec::new();
    for (rel, path) in listed {
        let Ok(bytes) = fs::read(&path) else { continue };
        let Some(language) = trace_core::languages::from_path(Path::new(&rel)) else { continue };
        let facts = trace_syntax::grammar(language).and_then(|_| {
            trace_syntax::extract(trace_syntax::SourceInput {
                path: &rel,
                language,
                source: &bytes,
            })
            .ok()
        });
        files.push(FileRecord {
            path: rel,
            language,
            hash: Hash32::of(&bytes),
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
        });
    }
    assemble(AssembleInput {
        header: header(&copy.display().to_string()),
        files,
        configs: Vec::new(),
        omitted: Vec::new(),
        support: Vec::new(),
        backend_runs: Vec::new(),
        diagnostics: Vec::new(),
    })
}

/// Endpoint names match exactly, or an HTTP key derived with any method (`* /p`) stands for
/// the method the old table named (`GET /p`).
fn same_key(got: &str, want: &str) -> bool {
    if got == want {
        return true;
    }
    match (got.split_once(' '), want.split_once(' ')) {
        (Some(("*", gp)), Some((_, wp))) => gp == wp,
        _ => false,
    }
}

fn call_of<'f>(facts: &'f FileFacts, callee: &str, occurrence: usize) -> Option<(usize, &'f CallSite)> {
    facts
        .calls
        .iter()
        .enumerate()
        .filter(|(_, c)| c.callee == callee)
        .nth(occurrence)
}

fn empty_semantics() -> FileSemantics {
    FileSemantics {
        provider: trace_core::model::Provider::Deterministic,
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

/// Reproduce one family of one fixture (module docs).
#[allow(clippy::type_complexity)]
pub(crate) fn reproduce(fixture: &str, family: &str) {
    let Some(base) = trace_core::env::test::bridge_fixtures() else {
        eprintln!("{fixture}/{family}: TRACE_BRIDGE_FIXTURES is not set; the lab's setup_deps.py installs the fixture packages (nothing measured)");
        return;
    };
    let copy = base.join(fixture);
    if !copy.is_dir() {
        eprintln!("{fixture}/{family}: no installed copy at {} (nothing measured)", copy.display());
        return;
    }
    let text = fs::read_to_string(fixture_dir(fixture).join("expected.json")).expect("expected.json");
    let expected: Expected = serde_json::from_str(&text).expect("expected.json parses");
    let spec = expected
        .families
        .get(family)
        .unwrap_or_else(|| panic!("{fixture}: no family {family} in expected.json"));
    let roots: Vec<Option<LibraryRoot>> = expected.roots.iter().map(|r| resolve_root(&copy, r)).collect();
    let mut index = index_of(&copy);
    // Stand in for the server: library files and calls per repository file.
    let mut per_file: BTreeMap<String, FileSemantics> = BTreeMap::new();
    let mut targets: Vec<(String, u32, Option<(LibraryFile, u32, u32)>, Option<String>)> = Vec::new();
    let mut problems = Vec::new();
    for c in &spec.calls {
        let Some(file) = index.file_by_path(&c.file) else {
            problems.push(format!("{}: not indexed", c.file));
            continue;
        };
        let Some(facts) = index.files[file.idx()].facts.as_ref() else { continue };
        let Some((_, call)) = call_of(facts, &c.callee, c.occurrence) else {
            problems.push(format!("{}: no call #{} of `{}`", c.file, c.occurrence, c.callee));
            continue;
        };
        let located = match &c.target {
            Some(t) => match roots.get(t.root).and_then(|r| r.as_ref()) {
                Some(root) => {
                    let l = locate(root, t);
                    if l.is_none() {
                        problems.push(format!(
                            "{}: target {:?} not found under {}",
                            c.callee,
                            t,
                            root.path.display()
                        ));
                    }
                    l
                }
                None => {
                    problems.push(format!("{}: library root #{} not found in the copy", c.callee, t.root));
                    None
                }
            },
            None => None,
        };
        let sem = per_file.entry(c.file.clone()).or_insert_with(empty_semantics);
        let lib_index = match &located {
            Some((lf, _, _)) => {
                sem.library_files.push(lf.clone());
                (sem.library_files.len() - 1) as u32
            }
            None => 0,
        };
        if located.is_some() || c.symbol.is_some() {
            if c.symbol.is_some() && sem.library_files.is_empty() {
                sem.library_files.push(LibraryFile {
                    path: String::new(),
                    package: String::new(),
                    version: None,
                    stdlib: true,
                    readable: false,
                    language: index.files[file.idx()].language,
                });
            }
            sem.library_calls.push(SemLibraryCall {
                at: call.callee_span,
                line: call.line,
                file: lib_index,
                decl_line: located.as_ref().map(|l| l.1).unwrap_or(0),
                decl_column: located.as_ref().map(|l| l.2).unwrap_or(0),
                symbol: c.symbol.clone(),
            });
        }
        targets.push((c.file.clone(), call.callee_span.start, located, c.symbol.clone()));
    }
    for (path, sem) in per_file {
        if let Some(f) = index.file_by_path(&path) {
            index.files[f.idx()].semantic = Some(sem);
        }
    }
    // Behaviour of the calls (derived from the installed source).
    let cache = tempfile::Builder::new()
        .prefix("trace-bridge-family-")
        .tempdir_in({
            let d = std::env::temp_dir().join("trace-tests");
            fs::create_dir_all(&d).expect("temp root");
            d
        })
        .expect("temp dir");
    let present: Vec<LibraryRoot> = roots.iter().flatten().cloned().collect();
    let library = Library::open(cache.path())
        .expect("library")
        .with_roots(present.clone());
    let mut knowledge = LibraryKnowledge::default();
    {
        let mut requests: Vec<BehaviourRequest<'_>> = Vec::new();
        for (path, start, located, symbol) in &targets {
            let Some(file) = index.file_by_path(path) else { continue };
            let rec = &index.files[file.idx()];
            let Some(facts) = rec.facts.as_ref() else { continue };
            let Some((ci, call)) = facts
                .calls
                .iter()
                .enumerate()
                .find(|(_, c)| c.callee_span.start == *start)
            else {
                continue;
            };
            let detail = facts.call_detail(ci);
            let mut args = Vec::new();
            let mut positional = 0u32;
            let mut keywords = Vec::new();
            if let Some(d) = detail {
                for a in &d.arguments {
                    match &a.slot {
                        trace_core::facts::ArgSlot::Positional { index, .. } => {
                            positional += 1;
                            args.push(RequestArg {
                                span: a.span,
                                index: Some(*index),
                                keyword: None,
                            });
                        }
                        trace_core::facts::ArgSlot::Keyword(k) => {
                            keywords.push(k.as_str());
                            args.push(RequestArg {
                                span: a.span,
                                index: None,
                                keyword: Some(k.clone()),
                            });
                        }
                        _ => {}
                    }
                }
            }
            requests.push(BehaviourRequest {
                file: rec.path.as_str(),
                language: rec.language,
                callee: call.callee_span,
                spelling: call.member.as_deref().unwrap_or(call.callee.as_str()),
                qualifier: call.receiver.as_deref(),
                positional_args: positional,
                keywords,
                target: located.as_ref().map(|(f, l, c)| (f, *l, *c)),
                symbol: symbol.as_deref(),
                callback_params: Vec::new(),
                args,
            });
        }
        let k = library.knowledge(&requests);
        knowledge.by_call = k.by_call;
    }
    let installed = InstalledPackages::from_roots(&present);
    let store = SourceStore::with_root(&index, copy.clone());
    let tables = trace_library::table::Tables::builtin();
    let config = BridgeSettings::default();
    let input = BridgeInput {
        index: &index,
        sources: &store,
        config: &config,
        knowledge: &knowledge,
        tables: &tables,
        installed: &installed,
    };
    let mut gate = Gate::default();
    for l in Language::ALL {
        gate.bridges.insert(l, BridgeGate { passed: true });
    }
    let out = crate::run(&input, &gate, None);
    let mut missing = Vec::new();
    for e in &spec.endpoints {
        let Some(file) = index.file_by_path(&e.file) else {
            missing.push(format!("{}: not indexed", e.file));
            continue;
        };
        let role = if e.role == "provides" {
            BoundaryRole::Provides
        } else {
            BoundaryRole::Uses
        };
        let kind = kind_of(&e.kind);
        let ends = crate::test_support::file_endpoints(&input, file);
        if !ends
            .iter()
            .any(|x| x.fact.kind == kind && x.fact.role == role && same_key(&x.fact.name, &e.name))
        {
            let got: Vec<String> = ends
                .iter()
                .map(|x| format!("{} {:?} {}", x.fact.kind, x.fact.role, x.fact.name))
                .collect();
            missing.push(format!("{} {} {} {} (endpoints: {got:?})", e.file, e.kind, e.role, e.name));
        }
    }
    for b in &spec.bridges {
        let kind = kind_of(&b.kind);
        let found = out
            .bridges
            .iter()
            .any(|x| x.kind == kind && index.symbol(x.from).uid == b.from && index.symbol(x.to).uid == b.to);
        if !found {
            missing.push(format!("bridge {} {} -> {}", b.kind, b.from, b.to));
        }
    }
    if !missing.is_empty() || !problems.is_empty() {
        let behaviours: Vec<String> = knowledge
            .by_call
            .iter()
            .map(|((f, s), b)| format!("{f}@{s}: {:?} ({})", b.effects, b.reason))
            .collect();
        panic!(
            "{fixture}/{family} not reproduced{}\nproblems: {problems:#?}\nmissing: {missing:#?}\nderived behaviours: {behaviours:#?}",
            spec.note.as_deref().map(|n| format!(" ({n})")).unwrap_or_default()
        );
    }
}

macro_rules! family_tests {
    ($($name:ident => ($fixture:literal, $family:literal)),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                reproduce($fixture, $family);
            }
        )*
    };
}

// One test per family of the old tables (routes, clients, messages, processes, FFI) and the
// new-coverage frameworks (DESIGN-bridges §6.3).
family_tests! {
    rule_bridges_flask_reproduced => ("rule-bridges-python", "flask"),
    rule_bridges_fastapi_reproduced => ("rule-bridges-python", "fastapi"),
    rule_bridges_django_reproduced => ("rule-bridges-python", "django"),
    rule_bridges_starlette_reproduced => ("rule-bridges-python", "starlette"),
    rule_bridges_requests_reproduced => ("rule-bridges-python", "requests"),
    rule_bridges_httpx_reproduced => ("rule-bridges-python", "httpx"),
    rule_bridges_aiohttp_reproduced => ("rule-bridges-python", "aiohttp"),
    rule_bridges_urllib_reproduced => ("rule-bridges-python", "urllib"),
    rule_bridges_python_socketio_reproduced => ("rule-bridges-python", "python-socketio"),
    rule_bridges_redis_pubsub_reproduced => ("rule-bridges-python", "redis"),
    rule_bridges_kafka_reproduced => ("rule-bridges-python", "kafka"),
    rule_bridges_celery_reproduced => ("rule-bridges-python", "celery"),
    rule_bridges_amqp_reproduced => ("rule-bridges-python", "amqp"),
    rule_bridges_python_subprocess_reproduced => ("rule-bridges-python", "subprocess"),
    rule_bridges_ctypes_reproduced => ("rule-bridges-python", "ctypes"),
    rule_bridges_cffi_reproduced => ("rule-bridges-python", "cffi"),
    rule_bridges_express_reproduced => ("rule-bridges-node", "express"),
    rule_bridges_fastify_reproduced => ("rule-bridges-node", "fastify"),
    rule_bridges_koa_reproduced => ("rule-bridges-node", "koa"),
    rule_bridges_hono_reproduced => ("rule-bridges-node", "hono"),
    rule_bridges_restify_reproduced => ("rule-bridges-node", "restify"),
    rule_bridges_fetch_reproduced => ("rule-bridges-node", "fetch"),
    rule_bridges_axios_reproduced => ("rule-bridges-node", "axios"),
    rule_bridges_ky_got_superagent_reproduced => ("rule-bridges-node", "ky-got-superagent"),
    rule_bridges_socketio_reproduced => ("rule-bridges-node", "socket.io"),
    rule_bridges_node_messages_reproduced => ("rule-bridges-node", "node-messages"),
    rule_bridges_node_subprocess_reproduced => ("rule-bridges-node", "subprocess"),
    rule_bridges_node_ffi_reproduced => ("rule-bridges-node", "ffi"),
    rule_bridges_nextjs_reproduced => ("rule-bridges-node", "next"),
    rule_bridges_nestjs_reproduced => ("rule-bridges-ts", "nestjs"),
    rule_bridges_angular_httpclient_reproduced => ("rule-bridges-ts", "angular-httpclient"),
    rule_bridges_gin_reproduced => ("rule-bridges-go", "gin"),
    rule_bridges_chi_reproduced => ("rule-bridges-go", "chi"),
    rule_bridges_echo_reproduced => ("rule-bridges-go", "echo"),
    rule_bridges_go_net_http_reproduced => ("rule-bridges-go", "net-http"),
    rule_bridges_go_subprocess_reproduced => ("rule-bridges-go", "subprocess"),
    rule_bridges_axum_reproduced => ("rule-bridges-rust", "axum"),
    rule_bridges_reqwest_reproduced => ("rule-bridges-rust", "reqwest"),
    rule_bridges_rust_subprocess_reproduced => ("rule-bridges-rust", "subprocess"),
    rule_bridges_spring_reproduced => ("rule-bridges-java", "spring"),
    rule_bridges_jaxrs_reproduced => ("rule-bridges-java", "jaxrs"),
    rule_bridges_java_clients_reproduced => ("rule-bridges-java", "clients"),
    rule_bridges_java_subprocess_reproduced => ("rule-bridges-java", "subprocess"),
    rule_bridges_aspnet_reproduced => ("rule-bridges-csharp", "aspnet"),
    rule_bridges_aspnet_minimal_reproduced => ("rule-bridges-csharp", "aspnet-minimal"),
    rule_bridges_dotnet_httpclient_reproduced => ("rule-bridges-csharp", "httpclient"),
    rule_bridges_csharp_subprocess_ffi_reproduced => ("rule-bridges-csharp", "subprocess-ffi"),
    rule_bridges_laravel_reproduced => ("rule-bridges-php", "laravel"),
    rule_bridges_php_subprocess_reproduced => ("rule-bridges-php", "subprocess"),
}

#[test]
fn rule_bridges_expected_files_parse() {
    for fixture in [
        "rule-bridges-python",
        "rule-bridges-node",
        "rule-bridges-ts",
        "rule-bridges-go",
        "rule-bridges-rust",
        "rule-bridges-java",
        "rule-bridges-csharp",
        "rule-bridges-php",
    ] {
        let dir = fixture_dir(fixture);
        let text = fs::read_to_string(dir.join("expected.json")).unwrap_or_else(|e| panic!("{fixture}: {e}"));
        let expected: Expected = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{fixture}: {e}"));
        let deps = fs::read_to_string(dir.join("deps.json")).unwrap_or_else(|e| panic!("{fixture}: {e}"));
        let _: serde_json::Value =
            serde_json::from_str(&deps).unwrap_or_else(|e| panic!("{fixture} deps.json: {e}"));
        for r in &expected.roots {
            let _ = ecosystem_of(&r.ecosystem);
        }
        for (name, f) in &expected.families {
            assert!(!f.endpoints.is_empty() || !f.bridges.is_empty(), "{fixture}/{name} expects something");
            for e in &f.endpoints {
                let _ = kind_of(&e.kind);
                assert!(dir.join(&e.file).is_file(), "{fixture}/{name}: {} exists", e.file);
            }
            for c in &f.calls {
                assert!(dir.join(&c.file).is_file(), "{fixture}/{name}: {} exists", c.file);
                assert!(
                    c.target.is_some() || c.symbol.is_some(),
                    "{fixture}/{name}: {} has a target or symbol",
                    c.callee
                );
                if let Some(t) = &c.target {
                    assert!(t.root < expected.roots.len(), "{fixture}/{name}: root index");
                    assert!(
                        t.qualified.is_some() || t.text.is_some(),
                        "{fixture}/{name}: target names its declaration"
                    );
                }
            }
        }
    }
}
