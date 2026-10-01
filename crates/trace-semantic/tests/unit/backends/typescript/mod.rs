use super::*;
use std::collections::HashSet;
use std::fs::{self};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::time::Duration;

use trace_core::facts::{Activation, CallSite, FileFacts, RefKind};
use trace_core::model::{ByteSpan, EdgeKind, ExecutionModel, Provider, Resolution, UnresolvedKind};
use trace_core::{Hash32, Language};

use crate::backend::{SemanticFile, SemanticRequest};
use crate::mapping::DeclTable;
use crate::references::ReferenceQuery;
use crate::test_support::facts::{decl_at, find};
use trace_core::facts::{AnonymousKind, AnonymousScope, Consumer, Reference};
use trace_core::model::SymbolKind;

fn call(at: u32, callee: &str, span_end: u32, owner: Option<u32>, line: u32) -> CallSite {
    CallSite {
        owner,
        lexical_owner: owner,
        span: ByteSpan::new(at, span_end),
        callee_span: ByteSpan::new(at, at + callee.len() as u32),
        callee: callee.into(),
        member: Some(callee.rsplit('.').next().unwrap_or(callee).into()),
        receiver: None,
        line,
        activation: Activation::Plain,
        is_new: false,
        arg_count: 0,
    }
}

/// `<module>` declaration appended last (trace-syntax, extractor 5).
fn with_module(mut facts: FileFacts, src: &[u8]) -> FileFacts {
    let mut module = decl_at(src, "", "<module>", SymbolKind::Module, (0, src.len() as u32), 0);
    module.name = "<module>".into();
    facts.declarations.push(module);
    facts.module_decl = Some(facts.declarations.len() as u32 - 1);
    facts
}

#[test]
fn worker_output_maps_onto_syntax_declarations() {
    let src: &[u8] =
            b"export function helper() { return 1; }\nexport const main = () => { helper(); later(); };\nasync function job() {}\n";
    let helper_at = find(src, "helper");
    let helper_fn = find(src, "function helper");
    let helper_end = find(src, "}\n") + 1;
    let main_at = find(src, "main");
    let main_line_start = find(src, "export const");
    let arrow = find(src, "() =>");
    let main_end = find(src, "};") + 1;
    let call_helper = find(src, "helper();");
    let call_later = find(src, "later");
    let job_at = find(src, "job");
    let mut job =
        decl_at(src, "job", "job", SymbolKind::Function, (job_at - 15, src.len() as u32 - 1), job_at);
    job.execution = ExecutionModel::Coroutine;
    let facts = FileFacts {
        declarations: vec![
            decl_at(src, "helper", "helper", SymbolKind::Function, (0, helper_end), helper_at),
            decl_at(src, "main", "main", SymbolKind::Function, (main_line_start, main_end + 1), main_at),
            job,
        ],
        calls: vec![
            call(call_helper, "helper", call_helper + 8, Some(1), 2),
            call(call_later, "later", call_later + 7, Some(1), 2),
        ],
        ..FileFacts::default()
    };
    let file = SemanticFile {
        path: "src/a.ts",
        language: Language::TypeScript,
        hash: Hash32::of(src),
        source: src,
        facts: &facts,
    };
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let worker = serde_json::json!({
        "symbols": {
            format!("ts:src/a.ts:{helper_fn}:{helper_end}"): {"id": "x", "file": "src/a.ts", "name": "helper",
                "start_byte": helper_fn, "end_byte": helper_end, "kind": "function"},
            format!("ts:src/a.ts:{arrow}:{main_end}"): {"file": "src/a.ts", "name": "main",
                "start_byte": arrow, "end_byte": main_end},
            "ts:src/a.ts:0:1": {"file": "src/a.ts", "name": "<anonymous@0>", "start_byte": 0, "end_byte": 1}
        },
        "edges": [{"from": format!("ts:src/a.ts:{arrow}:{main_end}"), "to": format!("ts:src/a.ts:{helper_fn}:{helper_end}"),
                   "kind": "calls", "resolution": "typescript_resolved_signature",
                   "evidence": {"file": "src/a.ts", "start_byte": call_helper, "end_byte": call_helper + 8, "line": 2}}],
        "unresolved": [{"owner": "ts:src/a.ts:0:1", "kind": "unresolved_or_external_signature",
                        "evidence": {"file": "src/a.ts", "start_byte": 5, "end_byte": 6, "line": 1}}],
        "diagnostics": [], "metrics": {}
    });
    let output: WorkerOutput = serde_json::from_value(worker).unwrap();
    let results = map_worker_output(&output, &[&file], &decls, &HashSet::new(), "fp");
    let sem = &results["src/a.ts"];
    assert_eq!(sem.provider, Provider::TypeScript);
    assert_eq!(sem.edges.len(), 1);
    let edge = &sem.edges[0];
    assert_eq!((edge.owner, edge.target.as_str()), (1, "src/a.ts:helper"));
    assert_eq!(edge.at, ByteSpan::new(call_helper, call_helper + 6));
    assert_eq!(edge.resolution, Resolution::ResolvedSignature);
    // `later()` had no worker entry: explicit unknown.
    assert_eq!(sem.unresolved.len(), 1);
    assert_eq!(sem.unresolved[0].kind, UnresolvedKind::NoSemanticTarget);
    assert_eq!(sem.unresolved[0].callee, "later");
    // No syntax call at byte 5 and the worker owner is unmapped (no <module> facts).
    assert!(sem.diagnostics.iter().any(|d| d.kind == "unmapped_owner"));
}

/// hono-shaped: `class Context { notFound = () => {...} }` (a class-field arrow the
/// worker names from its binding, and older workers called `<anonymous@...>`), called
/// from a module-level callback `app.get('/x', (c) => c.notFound())` (a syntax `<lambda>`
/// the worker reports as `<anonymous@...>`) and from module level directly. Every owner
/// maps (no `unmapped_owner`), module-level calls are edges of `<module>`, and uses
/// become `references` / `writes` / `imports` / `reexports` edges.
#[test]
fn module_owner_class_field_arrows_and_lambdas_map() {
    let src: &[u8] = b"import { helper } from './h';\nexport { helper } from './h';\nclass Context {\n  notFound = () => { return 1; }\n}\nconst c = new Context();\napp.get('/x', (c) => c.notFound());\nc.notFound();\nc.notFound = helper;\n";
    let class_at = find(src, "class Context");
    let class_end = find(src, "}\n}") + 3;
    let field_at = find(src, "notFound = ");
    let arrow = find(src, "() => { return");
    let arrow_end = find(src, "1; }") + 4;
    let lambda = find(src, "(c) => c.notFound");
    // Just past `c.notFound()` inside `app.get(..)`.
    let lambda_end = find(src, "());\nc.") + 2;
    let call_in_lambda = find(src, "c.notFound());");
    let get_call = find(src, "app.get");
    let module_call = find(src, "c.notFound();\nc.notFound =");
    let write_at = find(src, "notFound = helper");
    let helper_read = find(src, "helper;\n");
    let import_at = find(src, "helper }");
    let export_at = find(src, "helper } from './h';\nclass");
    let new_at = find(src, "new Context");
    let mut field =
        decl_at(src, "notFound", "Context.notFound", SymbolKind::Method, (field_at, arrow_end), field_at);
    field.parent = Some(0);
    field.body_start = find(src, "{ return");
    let mut lam = decl_at(src, "(", "<lambda>", SymbolKind::Function, (lambda, lambda_end), lambda);
    lam.name = "<lambda>".into();
    let mut facts = FileFacts {
        declarations: vec![
            decl_at(src, "Context", "Context", SymbolKind::Class, (class_at, class_end), class_at + 6),
            field,
            lam,
        ],
        calls: vec![
            {
                let mut c = call(new_at, "Context", new_at + 13, None, 6);
                c.callee_span = ByteSpan::new(new_at + 4, new_at + 11);
                c.is_new = true;
                c
            },
            call(get_call, "app.get", lambda_end + 1, None, 7),
            call(call_in_lambda, "c.notFound", call_in_lambda + 12, Some(2), 7),
            call(module_call, "c.notFound", module_call + 12, None, 8),
        ],
        anonymous: vec![AnonymousScope {
            decl: 2,
            kind: AnonymousKind::Lambda,
            created_in: None,
            consumer: Consumer::Argument {
                call: 1,
                slot: trace_core::facts::ArgSlot::Positional {
                    index: 1,
                    exact: true,
                },
            },
            eager: None,
        }],
        ..FileFacts::default()
    };
    facts.references.push(Reference {
        span: ByteSpan::new(write_at, write_at + 8),
        name: "notFound".into(),
        owner: None,
        in_decorator: false,
        local: false,
        kind: RefKind::Write,
    });
    let facts = with_module(facts, src);
    let module = facts.module_decl.unwrap();
    let file = SemanticFile {
        path: "src/context.ts",
        language: Language::TypeScript,
        hash: Hash32::of(src),
        source: src,
        facts: &facts,
    };
    // A second file declaring `helper` (import / re-export / read target).
    let hsrc: &[u8] = b"export function helper() {}\n";
    let hfacts = with_module(
        FileFacts {
            declarations: vec![decl_at(
                hsrc,
                "helper",
                "helper",
                SymbolKind::Function,
                (0, hsrc.len() as u32 - 1),
                16,
            )],
            ..FileFacts::default()
        },
        hsrc,
    );
    let hfile = SemanticFile {
        path: "src/h.ts",
        language: Language::TypeScript,
        hash: Hash32::of(hsrc),
        source: hsrc,
        facts: &hfacts,
    };
    let decls =
        DeclTable::new([(file.path, file.source, file.facts), (hfile.path, hfile.source, hfile.facts)]);
    let module_id = "ts:src/context.ts:module";
    let field_id = format!("ts:src/context.ts:{arrow}:{arrow_end}");
    let lambda_id = format!("ts:src/context.ts:{lambda}:{lambda_end}");
    let class_id = format!("ts:src/context.ts:{class_at}:{class_end}");
    let helper_id = "ts:src/h.ts:7:27";
    let ev = |start: u32, end: u32, line: u32| serde_json::json!({"file": "src/context.ts", "start_byte": start, "end_byte": end, "line": line});
    let worker = serde_json::json!({
        "symbols": {
            module_id: {"file": "src/context.ts", "name": "<module>", "start_byte": 0, "end_byte": src.len()},
            field_id.clone(): {"file": "src/context.ts", "name": "notFound", "start_byte": arrow, "end_byte": arrow_end},
            lambda_id.clone(): {"file": "src/context.ts", "name": format!("<anonymous@{lambda}>"), "start_byte": lambda, "end_byte": lambda_end},
            class_id.clone(): {"file": "src/context.ts", "name": "Context", "start_byte": class_at, "end_byte": class_end},
            helper_id: {"file": "src/h.ts", "name": "helper", "start_byte": 7, "end_byte": 27}
        },
        "edges": [
            {"from": module_id, "to": class_id, "kind": "constructor", "evidence": ev(new_at, new_at + 13, 6)},
            {"from": lambda_id, "to": field_id, "kind": "calls", "evidence": ev(call_in_lambda, call_in_lambda + 12, 7)},
            {"from": module_id, "to": field_id, "kind": "calls", "evidence": ev(module_call, module_call + 12, 8)}
        ],
        "unresolved": [{"owner": module_id, "kind": "unresolved_or_external_signature", "evidence": ev(get_call, lambda_end + 1, 7)}],
        "uses": [
            {"from": module_id, "to": helper_id, "kind": "import", "evidence": ev(import_at, import_at + 6, 1)},
            {"from": module_id, "to": helper_id, "kind": "reexport", "evidence": ev(export_at, export_at + 6, 2)},
            {"from": module_id, "to": field_id, "kind": "write", "evidence": ev(write_at, write_at + 8, 9)},
            {"from": module_id, "to": helper_id, "kind": "read", "evidence": ev(helper_read, helper_read + 6, 9)}
        ]
    });
    let output: WorkerOutput = serde_json::from_value(worker).unwrap();
    let results = map_worker_output(&output, &[&file, &hfile], &decls, &HashSet::new(), "fp");
    let sem = &results["src/context.ts"];
    assert!(
        !sem.diagnostics
            .iter()
            .any(|d| d.kind == "unmapped_owner" || d.kind == "unmapped_target"),
        "{:?}",
        sem.diagnostics
    );
    let calls: Vec<(u32, &str, EdgeKind)> = sem
        .edges
        .iter()
        .filter(|e| matches!(e.kind, EdgeKind::Calls | EdgeKind::Constructor))
        .map(|e| (e.owner, e.target.as_str(), e.kind))
        .collect();
    assert_eq!(
        calls,
        vec![
            (module, "src/context.ts:Context", EdgeKind::Constructor),
            (2, "src/context.ts:Context.notFound", EdgeKind::Calls),
            (module, "src/context.ts:Context.notFound", EdgeKind::Calls),
        ]
    );
    let uses: Vec<(u32, &str, EdgeKind)> = sem
        .edges
        .iter()
        .filter(|e| !matches!(e.kind, EdgeKind::Calls | EdgeKind::Constructor))
        .map(|e| (e.owner, e.target.as_str(), e.kind))
        .collect();
    assert_eq!(
        uses,
        vec![
            (module, "src/h.ts:helper", EdgeKind::Imports),
            (module, "src/h.ts:helper", EdgeKind::Reexports),
            (module, "src/context.ts:Context.notFound", EdgeKind::Writes),
            (module, "src/h.ts:helper", EdgeKind::References),
        ]
    );
    assert_eq!(sem.value_refs.len(), 1, "reads stay flow inputs");
    // `app.get(...)` is an explicit unknown owned by <module>.
    assert_eq!(sem.unresolved.len(), 1);
    assert_eq!(sem.unresolved[0].owner, Some(module));
    assert_eq!(sem.unresolved[0].kind, UnresolvedKind::UnresolvedSignature);
}

#[test]
fn references_mode_output_maps_and_reports_completeness() {
    let src: &[u8] = b"class C { m() {} }\nnew C().m();\n";
    let facts = FileFacts::default();
    let file = SemanticFile {
        path: "a.ts",
        language: Language::TypeScript,
        hash: Hash32::of(src),
        source: src,
        facts: &facts,
    };
    let m_decl = find(src, "m()");
    let m_use = find(src, "m();");
    let output: WorkerOutput = serde_json::from_value(serde_json::json!({
        "references": [
            {"file": "a.ts", "start_byte": m_use, "end_byte": m_use + 1, "line": 2, "is_declaration": false},
            {"file": "a.ts", "start_byte": m_decl, "end_byte": m_decl + 1, "line": 1, "is_declaration": true},
            {"file": "gone.ts", "start_byte": 0, "end_byte": 1, "line": 1}
        ],
        "references_complete": true
    }))
    .unwrap();
    let query = ReferenceQuery {
        path: "a.ts".into(),
        byte: m_decl,
        include_declaration: false,
    };
    let found = map_worker_references(&output, &[&file], &query, "typescript");
    assert_eq!(found.references.len(), 1);
    assert_eq!(found.references[0].line, 2);
    assert_eq!(found.dropped, 1);
    assert!(!found.complete);
}

#[test]
fn async_targets_use_syntax_activation() {
    let mut c = call(0, "job", 5, Some(0), 1);
    assert_eq!(refine(EdgeKind::Calls, Some(&c), ExecutionModel::Coroutine), EdgeKind::CreatesCoroutine);
    c.activation = Activation::Await;
    assert_eq!(refine(EdgeKind::Calls, Some(&c), ExecutionModel::Coroutine), EdgeKind::Awaits);
    assert_eq!(refine(EdgeKind::Constructor, Some(&c), ExecutionModel::Coroutine), EdgeKind::Constructor);
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("trace-ts-{tag}-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The worker gets the project's configuration texts, the node_modules mappings and the
/// repository root (for realpath), and only the queried files are visited.
#[test]
fn rule_worker_input_carries_project_configs() {
    let base = temp_dir("input");
    let root = base.join("repo");
    let home = base.join("home");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&home).unwrap();
    let repo = trace_core::paths::RepoPaths::resolve_in(&root, &home).unwrap();
    let tools = crate::test_support::setup::tool_env(None);
    let setup = TsSetup {
        node_modules: vec![trace_env::NodeModules {
            dir: String::new(),
            path: root.join("node_modules"),
        }],
        configs: vec![("tsconfig.json".into(), "{\"compilerOptions\":{}}".into())],
        bundle_node_types: false,
        bundle_undici_types: false,
    };
    let prepared = crate::languages::Prepared {
        backend: "typescript".into(),
        data: Some(std::sync::Arc::new(setup)),
        ..Default::default()
    };
    let facts = FileFacts::default();
    let files = [
        SemanticFile {
            path: "src/a.ts",
            language: Language::TypeScript,
            hash: Hash32::of(b"let a = 1;"),
            source: b"let a = 1;",
            facts: &facts,
        },
        SemanticFile {
            path: "bad.js",
            language: Language::JavaScript,
            hash: Hash32::of(b"\xff"),
            source: b"\xff",
            facts: &facts,
        },
    ];
    let query: HashSet<String> = ["src/a.ts".to_string()].into_iter().collect();
    let request = SemanticRequest {
        repo: &repo,
        files: &files,
        configs: &[],
        query: &query,
        tools: &tools,
        prepared: &prepared,
    };
    let refs = worker_files(&request);
    let mut invalid = HashSet::new();
    let input = worker_input(Path::new("/ws"), &request, &refs, &mut invalid);
    let json = serde_json::to_value(&input).unwrap();
    assert_eq!(json["configs"][0]["path"], "tsconfig.json");
    assert!(json["modules"][0]["real"].as_str().unwrap().ends_with("node_modules"));
    assert_eq!(json["repo_root"], repo.root.display().to_string());
    assert_eq!(json["files"].as_array().unwrap().len(), 1, "non-UTF-8 files are left out");
    assert!(invalid.contains("bad.js"));
    assert!(json.get("bundled_types").is_none(), "the project's own types win");
    assert!(json.get("query").is_none(), "the session sends the query with analyze");
    assert_eq!(TsSetup::of(&prepared).configs.len(), 1);
    assert_eq!(partition_language(&refs), Language::TypeScript);
    let _ = fs::remove_dir_all(&base);
}

/// A getter read is a `property_get` edge that covers no syntax call.
#[test]
fn rule_worker_getter_read_is_property_get() {
    let src = b"class C { get v() { return 1; } }\nconst c = new C();\nc.v;\n";
    let facts = trace_syntax::extract(trace_syntax::SourceInput {
        path: "a.ts",
        language: Language::TypeScript,
        source: src,
    })
    .unwrap();
    let file = SemanticFile {
        path: "a.ts",
        language: Language::TypeScript,
        hash: Hash32::of(src),
        source: src,
        facts: &facts,
    };
    let getter = find(src, "get v");
    let getter_end = find(src, "} }") + 1;
    let read = find(src, "c.v;");
    let worker = serde_json::json!({
        "symbols": {
            "g": {"file": "a.ts", "name": "v", "start_byte": getter, "end_byte": getter_end},
            "m": {"file": "a.ts", "name": "<module>", "start_byte": 0, "end_byte": src.len()}
        },
        "edges": [{"from": "m", "to": "g", "kind": "property_get",
            "evidence": {"file": "a.ts", "start_byte": read, "end_byte": read + 3, "line": 3}}]
    });
    let output: WorkerOutput = serde_json::from_value(worker).unwrap();
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let results = map_worker_output(&output, &[&file], &decls, &HashSet::new(), "fp");
    let sem = &results["a.ts"];
    if let Some(edge) = sem.edges.iter().find(|e| e.kind == EdgeKind::PropertyGet) {
        assert_eq!(edge.at, ByteSpan::new(read, read + 3));
    } else {
        // The getter must map onto a syntax declaration; if the grammar names it
        // differently the edge is counted, never invented.
        assert!(sem
            .diagnostics
            .iter()
            .any(|d| d.kind == "unmapped_target" || d.kind == "unmapped_owner"));
    }
}

/// Node + the TypeScript SDK for the live protocol test: node on PATH, the SDK under
/// `TRACE_SEMANTIC_TOOLS` (`typescript/<version>/node_modules/typescript`).
fn live_worker() -> Option<(PathBuf, PathBuf)> {
    let platform = trace_env::os::Platform::current();
    let vars = trace_env::os::EnvVars::from_process();
    let node = trace_env::os::find_executable(&["node"], &trace_env::os::path_dirs(&vars), &platform)?;
    let tools = vars.path("TRACE_SEMANTIC_TOOLS")?;
    let sdk = fs::read_dir(tools.join("typescript"))
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path().join("node_modules").join("typescript"))
        .find(|p| p.join("dist/api/sync/api.js").is_file())?;
    Some((node, sdk))
}

/// `--serve`: open, analyze, update one file, analyze again (the edit is seen), shutdown.
/// Skips without node and an installed TypeScript tool.
#[test]
fn rule_worker_serve_protocol_round_trip() {
    let Some((node, sdk)) = live_worker() else {
        eprintln!("skipped: node or the TypeScript tool is not installed");
        return;
    };
    let home = temp_dir("serve");
    let assets = crate::assets::materialize(&home).unwrap();
    let workspace = home.join("ws");
    fs::create_dir_all(&workspace).unwrap();
    let mut child = Command::new(&node)
        .arg(&assets.ts_worker)
        .arg("--serve")
        .arg(&sdk)
        .current_dir(&workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut call = |message: serde_json::Value| -> serde_json::Value {
        let mut line = serde_json::to_vec(&message).unwrap();
        line.push(b'\n');
        stdin.write_all(&line).unwrap();
        stdin.flush().unwrap();
        let mut answer = String::new();
        stdout.read_line(&mut answer).unwrap();
        serde_json::from_str(&answer).unwrap()
    };
    let src = "export function helper() { return 1; }\nhelper();\n";
    let opened = call(serde_json::json!({"op": "open", "input": {
        "workspace": workspace.display().to_string(),
        "files": [{"path": "a.ts", "source": src}],
        "names": ["helper"]
    }}));
    assert_eq!(opened["ok"], true, "{opened}");
    let calls = |v: &serde_json::Value| {
        v["edges"]
            .as_array()
            .map(|e| e.iter().filter(|x| x["kind"] == "calls").count())
            .unwrap_or(0)
    };
    let first = call(serde_json::json!({"op": "analyze", "query": ["a.ts"], "names": ["helper"]}));
    assert_eq!(calls(&first), 1, "{first}");
    let updated = call(serde_json::json!({"op": "update",
            "changed": [{"path": "a.ts", "text": format!("{src}helper();\n")}], "deleted": []}));
    assert_eq!(updated["ok"], true);
    let second = call(serde_json::json!({"op": "analyze", "query": ["a.ts"], "names": ["helper"]}));
    assert_eq!(calls(&second), 2, "the edited text is analysed: {second}");
    let none = call(serde_json::json!({"op": "analyze", "query": [], "names": ["helper"]}));
    assert_eq!(calls(&none), 0, "only queried files are visited");
    let bye = call(serde_json::json!({"op": "shutdown"}));
    assert_eq!(bye["ok"], true);
    assert!(child.wait().unwrap().success());
    let _ = fs::remove_dir_all(&home);
}

/// A `--serve` worker process driven line by line (tests only).
struct ServeWorker {
    child: std::process::Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl ServeWorker {
    fn start(node: &Path, worker: &Path, sdk: &Path, cwd: &Path) -> ServeWorker {
        let mut child = Command::new(node)
            .arg(worker)
            .arg("--serve")
            .arg(sdk)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        ServeWorker { child, stdin, stdout }
    }

    fn call(&mut self, message: serde_json::Value) -> serde_json::Value {
        let mut line = serde_json::to_vec(&message).unwrap();
        line.push(b'\n');
        self.stdin.write_all(&line).unwrap();
        self.stdin.flush().unwrap();
        let mut answer = String::new();
        self.stdout.read_line(&mut answer).unwrap();
        serde_json::from_str(&answer).unwrap()
    }

    fn stop(mut self) {
        let _ = self.call(serde_json::json!({"op": "shutdown"}));
        let _ = self.child.wait();
    }
}

/// Edges and unresolved entries of an analysis, without volatile fields.
fn analysis_facts(v: &serde_json::Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for key in ["edges", "unresolved", "uses"] {
        for e in v[key].as_array().into_iter().flatten() {
            out.push(format!(
                "{key} {} {} {} {}",
                e["kind"], e["to"], e["evidence"]["file"], e["evidence"]["start_byte"]
            ));
        }
    }
    out.sort();
    out
}

/// Incremental == full: after a sequence of edits through `update`, the session's analysis
/// equals a fresh worker opened on the final texts. Skips without node / TypeScript.
#[test]
fn rule_incremental_ts_worker_equal_full() {
    let Some((node, sdk)) = live_worker() else {
        eprintln!("skipped: node or the TypeScript tool is not installed");
        return;
    };
    let home = temp_dir("equal");
    let assets = crate::assets::materialize(&home).unwrap();
    let workspace = home.join("ws");
    fs::create_dir_all(&workspace).unwrap();
    let ws = workspace.display().to_string();
    let a0 = "export function helper() { return 1; }\nexport class C { get v() { return helper(); } }\n";
    let b0 = "import {helper} from './a';\nhelper();\n";
    let a1 = "export function helper() { return 2; }\nexport function other() { helper(); }\nexport class C { get v() { return other(); } }\n";
    let b1 = "import {helper, C} from './a';\nhelper(); new C().v;\n";
    let names = ["helper", "other", "C", "v"];
    let query = ["a.ts", "b.ts", "c.ts"];
    let mut live = ServeWorker::start(&node, &assets.ts_worker, &sdk, &workspace);
    let open = live.call(serde_json::json!({"op": "open", "input": {"workspace": ws,
            "files": [{"path": "a.ts", "source": a0}, {"path": "b.ts", "source": b0}], "names": names}}));
    assert_eq!(open["ok"], true, "{open}");
    let _ = live.call(serde_json::json!({"op": "analyze", "query": query, "names": names}));
    let _ = live
        .call(serde_json::json!({"op": "update", "changed": [{"path": "a.ts", "text": a1}], "deleted": []}));
    let _ = live.call(serde_json::json!({"op": "analyze", "query": ["a.ts"], "names": names}));
    let _ = live.call(serde_json::json!({"op": "update",
            "changed": [{"path": "b.ts", "text": b1}, {"path": "c.ts", "text": "export const x = 1;\n"}], "deleted": []}));
    let _ = live.call(serde_json::json!({"op": "update", "changed": [], "deleted": ["c.ts"]}));
    let incremental = live.call(serde_json::json!({"op": "analyze", "query": query, "names": names}));
    live.stop();
    let mut fresh = ServeWorker::start(&node, &assets.ts_worker, &sdk, &workspace);
    let _ = fresh.call(serde_json::json!({"op": "open", "input": {"workspace": ws,
            "files": [{"path": "a.ts", "source": a1}, {"path": "b.ts", "source": b1}], "names": names}}));
    let full = fresh.call(serde_json::json!({"op": "analyze", "query": query, "names": names}));
    fresh.stop();
    assert!(!analysis_facts(&full).is_empty(), "{full}");
    assert_eq!(analysis_facts(&incremental), analysis_facts(&full));
    let _ = fs::remove_dir_all(&home);
}

/// Runtime types: a project config without `types` (and no compiler 6+ installed by the
/// project) sees trace's bundled Node types, so a member call into the Node runtime is a
/// library call declared outside the repository; a config whose `types` excludes them
/// keeps its own choice (the call stays unresolved). Skips without node / TypeScript.
#[test]
fn rule_node_types_are_added_when_the_project_has_none() {
    let Some((node, sdk)) = live_worker() else {
        eprintln!("skipped: node or the TypeScript tool is not installed");
        return;
    };
    let home = temp_dir("node-types");
    let assets = crate::assets::materialize(&home).unwrap();
    let types = home.join("bundled").join("@types").join("node");
    fs::create_dir_all(&types).unwrap();
    fs::write(types.join("package.json"), r#"{"name":"@types/node","version":"1.0.0","types":"index.d.ts"}"#)
        .unwrap();
    fs::write(types.join("index.d.ts"), "declare var runtimeProcess: { cwd(): string };\n").unwrap();
    // `cwd` is also a repository name, so only the declared types can decide the call.
    let src = "export function cwd() { return 1; }\nruntimeProcess.cwd();\n";
    let kind_of_call = |config: &str| -> String {
        let workspace = home.join(format!("ws-{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir_all(&workspace).unwrap();
        let ws = workspace.display().to_string();
        let mut live = ServeWorker::start(&node, &assets.ts_worker, &sdk, &workspace);
        let open = live.call(serde_json::json!({"op": "open", "input": {
                "workspace": ws,
                "files": [{"path": "a.ts", "source": src}],
                "configs": [{"path": "tsconfig.json", "text": config}],
                "bundled_types": [{
                    "virtual": workspace.join("node_modules").join("@types").join("node").display().to_string(),
                    "real": types.display().to_string()
                }],
                "names": ["cwd"]}}));
        assert_eq!(open["ok"], true, "{open}");
        let answer = live.call(serde_json::json!({"op": "analyze", "query": ["a.ts"], "names": ["cwd"]}));
        live.stop();
        answer["unresolved"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|u| u["evidence"]["line"] == 2)
            .and_then(|u| u["kind"].as_str())
            .unwrap_or("none")
            .to_string()
    };
    assert_eq!(kind_of_call("{\"compilerOptions\": {\"strict\": true}}"), "external_signature");
    assert_eq!(
        kind_of_call("{\"compilerOptions\": {\"strict\": true, \"types\": []}}"),
        "unresolved_or_external_signature",
        "an explicit types list is the project's choice"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn rule_worker_timeouts_report_whole_minutes() {
    assert_eq!(deadline_minutes(Duration::from_secs(1)), 1);
    assert_eq!(deadline_minutes(Duration::from_secs(600)), 10);
    assert_eq!(deadline_minutes(Duration::from_secs(601)), 11);
    let failure = worker_failure(&serde_json::json!({"ok": false, "error": "Error: boom\n  at x"}));
    assert!(failure.to_string().contains("boom") && !failure.to_string().contains("at x"));
}
