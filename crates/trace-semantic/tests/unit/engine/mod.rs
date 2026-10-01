use super::*;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use trace_core::facts::{Activation, CallSite, CallbackArg, FileFacts, RefKind, Reference};
use trace_core::model::{
    ByteSpan, EdgeKind, ExecutionModel, Provider, Resolution, SymbolKind, UnresolvedKind,
};
use trace_core::semantics::{FileSemantics, SemImplementation, SemLibraryDispatch};
use trace_core::Language;

use crate::backend::SemanticFile;
use crate::cache::FileReuse;
use crate::languages::{Prepared, Server};
use crate::mapping::{DeclRef, DeclTable};
use crate::test_support::facts::{decl_at, find};
use crate::test_support::session::{call, empty_prepared, prepared, range, FakeSession, FakeUris, Handler};
use trace_core::facts::{AnonymousKind, AnonymousScope, Consumer};
use trace_core::Hash32;

fn py_options() -> Options<'static> {
    Options {
        provider: Provider::Pyright,
        tool_fingerprint: "fp",
        python: true,
        syntax_answers: true,
        hooks: &crate::languages::DefaultServer,
        prepared: empty_prepared(),
        calls_by_definition: false,
        reuse: None,
    }
}

fn py_caps() -> Value {
    json!({"callHierarchyProvider": true, "definitionProvider": true, "documentSymbolProvider": true})
}

/// Rule: a bodiless declaration owning no syntax call (abstract / native method) is
/// prepared like every callable, but its outgoing calls are never asked: it calls nothing.
#[test]
fn rule_bodiless_declarations_are_not_asked_for_outgoing_calls() {
    let src: &[u8] = b"abstract class Db {\n  abstract void close();\n  void open() { close(); }\n}\n";
    let close_at = find(src, "close");
    let open_at = find(src, "open");
    let call_at = find(src, "close(); }");
    let mut close =
        decl_at(src, "close", "Db.close", SymbolKind::Method, (close_at - 14, close_at + 7), close_at);
    close.parent = Some(0);
    close.is_stub = true;
    let mut open = decl_at(src, "open", "Db.open", SymbolKind::Method, (open_at - 5, call_at + 11), open_at);
    open.parent = Some(0);
    let facts = FileFacts {
        declarations: vec![
            decl_at(src, "Db", "Db", SymbolKind::Class, (0, src.len() as u32 - 1), 15),
            close,
            open,
        ],
        calls: vec![call(call_at, "close", Some(2), 3)],
        ..FileFacts::default()
    };
    let file = SemanticFile {
        path: "Db.java",
        language: Language::Java,
        hash: Hash32::of(src),
        source: src,
        facts: &facts,
    };
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = asked.clone();
    let handler: Handler = Box::new(move |method, params| {
        Ok(match method {
            "textDocument/prepareCallHierarchy" => prepared(params),
            "callHierarchy/outgoingCalls" => {
                seen.lock()
                    .unwrap()
                    .push(params["item"]["selectionRange"]["start"]["line"].clone());
                json!([])
            }
            _ => Value::Null,
        })
    });
    let mut session = FakeSession::new(json!({"callHierarchyProvider": true}), handler);
    let opts = Options {
        provider: Provider::Lsp("x".into()),
        tool_fingerprint: "fp",
        python: false,
        syntax_answers: true,
        hooks: &crate::languages::DefaultServer,
        prepared: empty_prepared(),
        calls_by_definition: false,
        reuse: None,
    };
    analyze(&mut session, &[&file], &decls, &FakeUris, &opts).unwrap();
    let prepares = session
        .requests
        .iter()
        .filter(|m| *m == "textDocument/prepareCallHierarchy")
        .count();
    assert_eq!(prepares, 2, "both methods are prepared");
    assert_eq!(*asked.lock().unwrap(), vec![json!(2)], "only the method with a body is asked");
}

#[test]
fn python_mode_edges_blind_sites_callbacks_and_value_refs() {
    let src: &[u8] = b"def helper():\n    return 1\n\ndef main():\n    helper()\n    unknown(helper)\n";
    let main_def = find(src, "def main");
    let helper_call = find(src, "    helper()") + 4;
    let unknown_call = find(src, "unknown");
    let arg = unknown_call + "unknown(".len() as u32;
    let mut facts = FileFacts {
        declarations: vec![
            decl_at(src, "helper", "helper", SymbolKind::Function, (0, main_def - 1), 4),
            decl_at(
                src,
                "main",
                "main",
                SymbolKind::Function,
                (main_def, src.len() as u32 - 1),
                main_def + 4,
            ),
        ],
        calls: vec![
            call(helper_call, "helper", Some(1), 5),
            call(unknown_call, "unknown", Some(1), 6),
        ],
        ..FileFacts::default()
    };
    facts.callbacks.push(CallbackArg {
        call_callee_span: facts.calls[1].callee_span,
        callee: "unknown".into(),
        arg_span: ByteSpan::new(arg, arg + 6),
        argument: "helper".into(),
        name: "helper".into(),
        owner: Some(1),
        index: None,
        keyword: None,
    });
    facts.references.push(Reference {
        span: ByteSpan::new(arg, arg + 6),
        name: "helper".into(),
        owner: Some(1),
        in_decorator: false,
        local: false,
        kind: trace_core::facts::RefKind::Argument,
    });
    let file = SemanticFile {
        path: "a.py",
        language: Language::Python,
        hash: Hash32::of(src),
        source: src,
        facts: &facts,
    };
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let handler: Handler = Box::new(|method, params| {
        let helper_item = json!({"name": "helper", "kind": 12, "uri": "file:///ws/a.py",
                                     "range": range(0, 0, 13), "selectionRange": range(0, 4, 10)});
        Ok(match method {
            "textDocument/prepareCallHierarchy" => prepared(params),
            "callHierarchy/outgoingCalls" => {
                if params["item"]["selectionRange"]["start"]["line"] == 3 {
                    json!([{"to": helper_item, "fromRanges": [range(4, 4, 10)]}])
                } else {
                    json!([])
                }
            }
            "textDocument/definition" => {
                if params["position"]["line"] == 5 {
                    json!([{"targetUri": "file:///ws/a.py", "targetRange": range(0, 0, 13),
                                "targetSelectionRange": range(0, 4, 10)}])
                } else {
                    Value::Null
                }
            }
            other => panic!("unexpected request {other}"),
        })
    });
    let mut session = FakeSession::new(py_caps(), handler);
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &py_options()).unwrap();
    // Syntax declarations suffice: no documentSymbol; two pipelined round trips.
    assert!(!session.requests.iter().any(|m| m == "textDocument/documentSymbol"));
    assert_eq!(session.batches, 2);
    let sem = &analysis.files["a.py"];
    assert_eq!(sem.tool_fingerprint, "fp");
    let calls: Vec<_> = sem.edges.iter().filter(|e| e.kind == EdgeKind::Calls).collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].owner, 1);
    assert_eq!(calls[0].target, "a.py:helper");
    assert_eq!(calls[0].at, ByteSpan::new(helper_call, helper_call + 6));
    assert_eq!(calls[0].line, 5);
    assert_eq!(calls[0].resolution, Resolution::CallHierarchy);
    let callback: Vec<_> = sem
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::PassesCallback)
        .collect();
    assert_eq!(callback.len(), 1);
    assert_eq!(callback[0].at, ByteSpan::new(arg, arg + 6));
    // Unresolved-site recording: `unknown(...)` had no analyzer result.
    assert_eq!(sem.unresolved.len(), 1);
    assert_eq!(sem.unresolved[0].kind, UnresolvedKind::NoSemanticTarget);
    assert_eq!(sem.unresolved[0].callee, "unknown");
    assert_eq!(sem.value_refs.len(), 1);
    assert_eq!(sem.value_refs[0].target, "a.py:helper");
    assert!(analysis.incomplete.is_empty());
}

/// A parameter shadowing a declaration name is a local variable: its definition is the
/// parameter itself, so neither the callback nor the value reference is requested.
#[test]
fn local_variables_are_not_sent_to_definition() {
    let src: &[u8] = b"def helper():\n    return 1\n\ndef main(helper):\n    unknown(helper)\n";
    let main_def = find(src, "def main");
    let unknown_call = find(src, "unknown");
    let arg = unknown_call + "unknown(".len() as u32;
    let mut facts = FileFacts {
        declarations: vec![
            decl_at(src, "helper", "helper", SymbolKind::Function, (0, main_def - 1), 4),
            decl_at(
                src,
                "main",
                "main",
                SymbolKind::Function,
                (main_def, src.len() as u32 - 1),
                main_def + 4,
            ),
        ],
        calls: vec![call(unknown_call, "unknown", Some(1), 5)],
        ..FileFacts::default()
    };
    facts.callbacks.push(CallbackArg {
        call_callee_span: facts.calls[0].callee_span,
        callee: "unknown".into(),
        arg_span: ByteSpan::new(arg, arg + 6),
        argument: "helper".into(),
        name: "helper".into(),
        owner: Some(1),
        index: None,
        keyword: None,
    });
    facts.references.push(Reference {
        span: ByteSpan::new(arg, arg + 6),
        name: "helper".into(),
        owner: Some(1),
        in_decorator: false,
        local: true,
        kind: trace_core::facts::RefKind::Argument,
    });
    let file = SemanticFile {
        path: "a.py",
        language: Language::Python,
        hash: Hash32::of(src),
        source: src,
        facts: &facts,
    };
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let handler: Handler = Box::new(|method, params| {
        Ok(match method {
            "textDocument/prepareCallHierarchy" => prepared(params),
            "callHierarchy/outgoingCalls" => json!([]),
            other => panic!("unexpected request {other}"),
        })
    });
    let mut session = FakeSession::new(py_caps(), handler);
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &py_options()).unwrap();
    assert!(!session.requests.iter().any(|m| m == "textDocument/definition"));
    let sem = &analysis.files["a.py"];
    assert!(sem.value_refs.is_empty());
    assert!(!sem.edges.iter().any(|e| e.kind == EdgeKind::PassesCallback));
}

#[test]
fn document_symbols_only_for_files_with_syntax_errors() {
    let src: &[u8] = b"def main():\n    pass\ndef (broken\n";
    let mut facts = FileFacts {
        declarations: vec![decl_at(src, "main", "main", SymbolKind::Function, (0, 20), 4)],
        ..FileFacts::default()
    };
    facts.error_count = 1;
    let file = SemanticFile {
        path: "e.py",
        language: Language::Python,
        hash: Hash32::of(src),
        source: src,
        facts: &facts,
    };
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let handler: Handler = Box::new(|method, params| {
        Ok(match method {
            "textDocument/documentSymbol" => json!([
                {"name": "main", "kind": 12, "range": range(0, 0, 11), "selectionRange": range(0, 4, 8)},
                {"name": "lost", "kind": 12, "range": range(2, 0, 11), "selectionRange": range(2, 4, 8)}
            ]),
            "textDocument/prepareCallHierarchy" => prepared(params),
            _ => json!([]),
        })
    });
    let mut session = FakeSession::new(py_caps(), handler);
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &py_options()).unwrap();
    assert_eq!(
        session
            .requests
            .iter()
            .filter(|m| *m == "textDocument/documentSymbol")
            .count(),
        1
    );
    let sem = &analysis.files["e.py"];
    assert!(sem
        .diagnostics
        .iter()
        .any(|d| d.kind == "unmapped_declaration" && d.message.starts_with("1 ")));
}

#[test]
fn external_and_ambiguous_targets_are_explicit() {
    let src: &[u8] = b"def main():\n    lib()\n";
    let lib_call = find(src, "lib");
    let facts = FileFacts {
        declarations: vec![decl_at(src, "main", "main", SymbolKind::Function, (0, src.len() as u32 - 1), 4)],
        calls: vec![call(lib_call, "lib", Some(0), 2)],
        ..FileFacts::default()
    };
    let file = SemanticFile {
        path: "m.py",
        language: Language::Python,
        hash: Hash32::of(src),
        source: src,
        facts: &facts,
    };
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let handler: Handler = Box::new(|method, params| {
        Ok(match method {
            "textDocument/prepareCallHierarchy" => prepared(params),
            "callHierarchy/outgoingCalls" => json!([{"to": {"name": "lib", "kind": 12,
                    "uri": "file:///elsewhere/lib.pyi", "range": range(0, 0, 3), "selectionRange": range(0, 4, 7)},
                    "fromRanges": [range(1, 4, 7)]}]),
            _ => Value::Null,
        })
    });
    let mut session = FakeSession::new(json!({"callHierarchyProvider": true}), handler);
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &py_options()).unwrap();
    let sem = &analysis.files["m.py"];
    assert!(sem.edges.is_empty());
    assert_eq!(sem.unresolved.len(), 1);
    assert_eq!(sem.unresolved[0].kind, UnresolvedKind::ExternalOrAmbiguous);
    assert_eq!(sem.unresolved[0].at, ByteSpan::new(lib_call, lib_call + 3));
    assert!(analysis.incomplete.contains(&("m.py".to_string(), lib_call)));
}

/// IMPROVEMENTS "10-compaction": `settle(lambda: record_usage())`. With a synthetic
/// `<lambda>` declaration the call inside the lambda body becomes a proven edge of the
/// lambda (attributed from the enclosing function's outgoing calls); the lambda itself is
/// never sent to prepareCallHierarchy. Without a synthetic scope (owner `None`) the
/// compiler's resolution is kept as a value reference at the callee.
#[test]
fn lambda_body_calls_are_attributed_to_the_executing_scope() {
    let src: &[u8] = b"def record_usage():\n    pass\n\ndef compact():\n    settle(lambda: record_usage())\n";
    let compact_def = find(src, "def compact");
    let settle_at = find(src, "settle");
    let lambda_at = find(src, "lambda");
    // The call in the lambda body (not the `def` line).
    let usage_call = lambda_at + "lambda: ".len() as u32;
    let usage_line0 = 4;
    let usage_col = usage_call - find(src, "    settle");
    let build = |synthetic: bool| {
        let mut declarations = vec![
            decl_at(src, "record_usage", "record_usage", SymbolKind::Function, (0, compact_def - 1), 4),
            decl_at(
                src,
                "compact",
                "compact",
                SymbolKind::Function,
                (compact_def, src.len() as u32 - 1),
                compact_def + 4,
            ),
        ];
        let mut anonymous = Vec::new();
        let lambda_owner = if synthetic {
            let mut lambda = decl_at(
                src,
                "lambda",
                "compact.<lambda>",
                SymbolKind::Function,
                (lambda_at, src.len() as u32 - 2),
                lambda_at,
            );
            lambda.name = "<lambda>".into();
            lambda.parent = Some(1);
            declarations.push(lambda);
            anonymous.push(AnonymousScope {
                decl: 2,
                kind: AnonymousKind::Lambda,
                created_in: Some(1),
                consumer: Consumer::Argument {
                    call: 0,
                    slot: trace_core::facts::ArgSlot::Positional {
                        index: 0,
                        exact: true,
                    },
                },
                eager: None,
            });
            Some(2)
        } else {
            None
        };
        FileFacts {
            declarations,
            calls: vec![
                call(settle_at, "settle", Some(1), 5),
                call(usage_call, "record_usage", lambda_owner, 5),
            ],
            anonymous,
            ..FileFacts::default()
        }
    };
    for synthetic in [true, false] {
        let facts = build(synthetic);
        let file = SemanticFile {
            path: "c.py",
            language: Language::Python,
            hash: Hash32::of(src),
            source: src,
            facts: &facts,
        };
        let decls = DeclTable::new([(file.path, file.source, file.facts)]);
        let handler: Handler = Box::new(move |method, params| {
            let usage_item = json!({"name": "record_usage", "kind": 12, "uri": "file:///ws/c.py",
                                        "range": range(0, 0, 19), "selectionRange": range(0, 4, 16)});
            Ok(match method {
                "textDocument/prepareCallHierarchy" => prepared(params),
                "callHierarchy/outgoingCalls" => {
                    if params["item"]["selectionRange"]["start"]["line"] == 3 {
                        json!([{"to": usage_item,
                                    "fromRanges": [range(usage_line0, usage_col, usage_col + 12)]}])
                    } else {
                        json!([])
                    }
                }
                _ => Value::Null,
            })
        });
        let mut session = FakeSession::new(py_caps(), handler);
        let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &py_options()).unwrap();
        let prepares = session
            .requests
            .iter()
            .filter(|m| *m == "textDocument/prepareCallHierarchy")
            .count();
        assert_eq!(prepares, 2, "synthetic scopes are never prepared");
        let sem = &analysis.files["c.py"];
        if synthetic {
            assert_eq!(sem.edges.len(), 1);
            assert_eq!(sem.edges[0].owner, 2, "edge of the <lambda> scope");
            assert_eq!(sem.edges[0].target, "c.py:record_usage");
            assert_eq!(sem.edges[0].kind, EdgeKind::Calls);
            // The lambda's call is covered; `settle` stays an explicit unknown.
            assert_eq!(sem.unresolved.len(), 1);
            assert_eq!(sem.unresolved[0].callee, "settle");
        } else {
            assert!(sem.edges.is_empty());
            assert_eq!(sem.value_refs.len(), 1);
            assert_eq!(sem.value_refs[0].at, ByteSpan::new(usage_call, usage_call + 12));
            assert_eq!(sem.value_refs[0].target, "c.py:record_usage");
            assert!(sem.diagnostics.iter().any(|d| d.kind == "lazy_scope_call"));
        }
    }
}

/// IMPROVEMENTS "01-nth-prime": `all(_probable_prime(n, b) for b in bases)`. The call in
/// the generator expression executes in the `<genexpr>` scope (when consumed), so it is an
/// edge of that scope, not of the enclosing function.
#[test]
fn generator_expression_calls_belong_to_the_genexpr_scope() {
    let src: &[u8] = b"def _probable_prime(n, b):\n    return True\n\ndef is_prime(n):\n    return all(_probable_prime(n, b) for b in bases())\n";
    let is_prime_def = find(src, "def is_prime");
    let all_at = find(src, "all(");
    // The call inside the generator expression (not the `def` line).
    let probable_call = all_at + 4;
    let bases_call = find(src, "bases()");
    let gen_start = probable_call;
    let line_start = find(src, "    return all");
    let mut genexpr = decl_at(
        src,
        "_probable_prime",
        "is_prime.<genexpr>",
        SymbolKind::Function,
        (gen_start, src.len() as u32 - 2),
        gen_start,
    );
    genexpr.name = "<genexpr>".into();
    genexpr.parent = Some(1);
    genexpr.execution = ExecutionModel::Generator;
    let facts = FileFacts {
        declarations: vec![
            decl_at(
                src,
                "_probable_prime",
                "_probable_prime",
                SymbolKind::Function,
                (0, is_prime_def - 1),
                4,
            ),
            decl_at(
                src,
                "is_prime",
                "is_prime",
                SymbolKind::Function,
                (is_prime_def, src.len() as u32 - 1),
                is_prime_def + 4,
            ),
            genexpr,
        ],
        calls: vec![
            call(all_at, "all", Some(1), 5),
            call(probable_call, "_probable_prime", Some(2), 5),
            // The first iterable is evaluated eagerly by the enclosing function.
            call(bases_call, "bases", Some(1), 5),
        ],
        anonymous: vec![AnonymousScope {
            decl: 2,
            kind: AnonymousKind::GeneratorExpression,
            created_in: Some(1),
            consumer: Consumer::Argument {
                call: 0,
                slot: trace_core::facts::ArgSlot::Positional {
                    index: 0,
                    exact: true,
                },
            },
            eager: Some(ByteSpan::new(bases_call, bases_call + 7)),
        }],
        ..FileFacts::default()
    };
    let file = SemanticFile {
        path: "p.py",
        language: Language::Python,
        hash: Hash32::of(src),
        source: src,
        facts: &facts,
    };
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let col = probable_call - line_start;
    let handler: Handler = Box::new(move |method, params| {
        let target = json!({"name": "_probable_prime", "kind": 12, "uri": "file:///ws/p.py",
                                "range": range(0, 0, 26), "selectionRange": range(0, 4, 19)});
        Ok(match method {
            "textDocument/prepareCallHierarchy" => prepared(params),
            "callHierarchy/outgoingCalls" if params["item"]["selectionRange"]["start"]["line"] == 3 => {
                json!([{"to": target, "fromRanges": [range(4, col, col + 15)]}])
            }
            _ => json!([]),
        })
    });
    let mut session = FakeSession::new(py_caps(), handler);
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &py_options()).unwrap();
    let sem = &analysis.files["p.py"];
    assert_eq!(sem.edges.len(), 1);
    assert_eq!((sem.edges[0].owner, sem.edges[0].target.as_str()), (2, "p.py:_probable_prime"));
    let blind: Vec<&str> = sem.unresolved.iter().map(|u| u.callee.as_str()).collect();
    assert_eq!(blind, vec!["all", "bases"], "unresolved sites stay recorded");
}

/// IMPROVEMENTS "08-webhook": `partial(payments.parse_webhook, ...)`. The attribute
/// callback resolves by definition to the declaration the compiler binds (here the
/// abstract method); the edge is recorded so inference can compose dispatch.
#[test]
fn attribute_callbacks_resolve_to_the_bound_declaration() {
    let src: &[u8] = b"class PaymentProvider:\n    def parse_webhook(self, body):\n        raise NotImplementedError\n\ndef handle(payments):\n    return partial(payments.parse_webhook, 1)\n";
    let class_end = find(src, "\ndef handle");
    let method_at = find(src, "parse_webhook(self");
    let handle_def = find(src, "def handle");
    let partial_at = find(src, "partial(");
    let attr_at = find(src, "parse_webhook, 1");
    let mut method = decl_at(
        src,
        "parse_webhook",
        "PaymentProvider.parse_webhook",
        SymbolKind::Method,
        (method_at - 8, class_end),
        method_at,
    );
    method.parent = Some(0);
    let mut facts = FileFacts {
        declarations: vec![
            decl_at(src, "PaymentProvider", "PaymentProvider", SymbolKind::Class, (0, class_end), 6),
            method,
            decl_at(
                src,
                "handle",
                "handle",
                SymbolKind::Function,
                (handle_def, src.len() as u32 - 1),
                handle_def + 4,
            ),
        ],
        calls: vec![call(partial_at, "partial", Some(2), 6)],
        ..FileFacts::default()
    };
    facts.callbacks.push(CallbackArg {
        call_callee_span: facts.calls[0].callee_span,
        callee: "partial".into(),
        arg_span: ByteSpan::new(attr_at, attr_at + 13),
        argument: "payments.parse_webhook".into(),
        name: "parse_webhook".into(),
        owner: Some(2),
        index: None,
        keyword: None,
    });
    let file = SemanticFile {
        path: "pay.py",
        language: Language::Python,
        hash: Hash32::of(src),
        source: src,
        facts: &facts,
    };
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let handler: Handler = Box::new(|method, params| {
        Ok(match method {
            "textDocument/prepareCallHierarchy" => prepared(params),
            "textDocument/definition" if params["position"]["line"] == 5 => {
                json!([{"uri": "file:///ws/pay.py", "range": range(1, 8, 21)}])
            }
            _ => json!([]),
        })
    });
    let mut session = FakeSession::new(py_caps(), handler);
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &py_options()).unwrap();
    let sem = &analysis.files["pay.py"];
    let callbacks: Vec<_> = sem
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::PassesCallback)
        .collect();
    assert_eq!(callbacks.len(), 1);
    assert_eq!(callbacks[0].owner, 2);
    assert_eq!(callbacks[0].target, "pay.py:PaymentProvider.parse_webhook");
}

#[test]
fn nested_scopes_outside_the_owner_are_ignored() {
    let facts = FileFacts {
        declarations: vec![
            decl_at(b"x", "a", "a", SymbolKind::Function, (0, 1), 0),
            decl_at(b"x", "b", "b", SymbolKind::Function, (0, 1), 0),
        ],
        ..FileFacts::default()
    };
    assert!(nested_in(&facts, 0, 0));
    assert!(!nested_in(&facts, 1, 0));
}

#[test]
fn activation_kinds_follow_execution_models() {
    use trace_core::model::ExecutionModel::*;
    assert_eq!(activation_kind(Activation::Plain, Ordinary), EdgeKind::Calls);
    assert_eq!(activation_kind(Activation::Await, Coroutine), EdgeKind::Awaits);
    assert_eq!(activation_kind(Activation::Plain, Coroutine), EdgeKind::CreatesCoroutine);
    assert_eq!(activation_kind(Activation::Iterate, Generator), EdgeKind::Iterates);
    assert_eq!(activation_kind(Activation::Plain, AsyncGenerator), EdgeKind::CreatesGenerator);
    assert_eq!(edge_kind(None, Ordinary, Some(7)), EdgeKind::PropertyGet);
    assert_eq!(edge_kind(None, Ordinary, Some(12)), EdgeKind::References);
}

/// (line, UTF-16 column) of a byte offset.
fn pos(src: &[u8], byte: u32) -> (u32, u32) {
    trace_core::text::LineIndex::new(src).utf16_of_byte(src, byte)
}

fn lsp_range(src: &[u8], start: u32, end: u32) -> Value {
    let (l0, c0) = pos(src, start);
    let (l1, c1) = pos(src, end);
    json!({"start": {"line": l0, "character": c0}, "end": {"line": l1, "character": c1}})
}

/// `<module>` declaration appended last (trace-syntax, extractor 5).
fn with_module(mut facts: FileFacts, src: &[u8]) -> FileFacts {
    let mut module = decl_at(src, "", "<module>", SymbolKind::Module, (0, src.len() as u32), 0);
    module.name = "<module>".into();
    facts.declarations.push(module);
    facts.module_decl = Some(facts.declarations.len() as u32 - 1);
    facts
}

fn reference(src: &[u8], needle: &str, name: &str, kind: RefKind) -> Reference {
    let at = find(src, needle) + needle.find(name).unwrap() as u32;
    Reference {
        span: ByteSpan::new(at, at + name.len() as u32),
        name: name.into(),
        owner: None,
        in_decorator: false,
        local: false,
        kind,
    }
}

/// NEXT.md items 1 and 4 (flask `app.redirect = redirect`): module-level code has no
/// preparable owner; its call gets `definition` at the member identifier (an edge from
/// `<module>`), and non-call uses become `writes` / `references` / `imports` edges owned
/// by `<module>` (the read stays a value reference).
#[test]
fn module_level_calls_and_uses_become_edges_of_the_module() {
    let src: &[u8] = b"class App:\n    def redirect(self):\n        pass\n\ndef helper():\n    pass\n\nhelper()\napp.redirect = helper\nfrom .m import helper\n";
    let class_end = find(src, "\ndef helper");
    let redirect_at = find(src, "redirect(self)");
    let helper_def = find(src, "def helper");
    let helper_call = find(src, "helper()\napp");
    let mut method = decl_at(
        src,
        "redirect",
        "App.redirect",
        SymbolKind::Method,
        (redirect_at - 4, class_end),
        redirect_at,
    );
    method.parent = Some(0);
    let mut facts = FileFacts {
        declarations: vec![
            decl_at(src, "App", "App", SymbolKind::Class, (0, class_end), 6),
            method,
            decl_at(
                src,
                "helper",
                "helper",
                SymbolKind::Function,
                (helper_def, helper_call - 1),
                helper_def + 4,
            ),
        ],
        calls: vec![call(helper_call, "helper", None, 8)],
        ..FileFacts::default()
    };
    facts.references = vec![
        reference(src, "app.redirect =", "redirect", RefKind::Write),
        reference(src, "= helper", "helper", RefKind::Read),
        reference(src, "import helper", "helper", RefKind::Import),
    ];
    let facts = with_module(facts, src);
    let module = facts.module_decl.unwrap();
    let file = SemanticFile {
        path: "t.py",
        language: Language::Python,
        hash: Hash32::of(src),
        source: src,
        facts: &facts,
    };
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let redirect_line = pos(src, redirect_at).0;
    let helper_name = pos(src, helper_def + 4);
    let write_line = pos(src, find(src, "app.redirect")).0;
    let handler: Handler = Box::new(move |method, params| {
        // `callHierarchy/outgoingCalls` carries an item, not a position.
        let line = params["position"]["line"].as_u64().unwrap_or(u64::MAX) as u32;
        let ch = params["position"]["character"].as_u64().unwrap_or(u64::MAX) as u32;
        let helper = json!([{"uri": "file:///ws/t.py", "range": range(helper_name.0, helper_name.1, helper_name.1 + 6)}]);
        Ok(match method {
            "textDocument/prepareCallHierarchy" => prepared(params),
            "callHierarchy/outgoingCalls" => json!([]),
            "textDocument/definition" if line == write_line && ch == 4 => {
                json!([{"uri": "file:///ws/t.py", "range": range(redirect_line, 8, 16)}])
            }
            "textDocument/definition" => helper,
            other => panic!("unexpected request {other}"),
        })
    });
    let mut session = FakeSession::new(py_caps(), handler);
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &py_options()).unwrap();
    // <module> and the class are never prepared.
    let prepares = session
        .requests
        .iter()
        .filter(|m| *m == "textDocument/prepareCallHierarchy")
        .count();
    assert_eq!(prepares, 2);
    let sem = &analysis.files["t.py"];
    let edges: Vec<(u32, &str, EdgeKind, Resolution)> = sem
        .edges
        .iter()
        .map(|e| (e.owner, e.target.as_str(), e.kind, e.resolution))
        .collect();
    assert_eq!(
        edges,
        vec![
            (module, "t.py:helper", EdgeKind::Calls, Resolution::Definition),
            (module, "t.py:App.redirect", EdgeKind::Writes, Resolution::Definition),
            (module, "t.py:helper", EdgeKind::References, Resolution::Definition),
            (module, "t.py:helper", EdgeKind::Imports, Resolution::Definition),
        ]
    );
    assert_eq!(sem.edges[0].at, ByteSpan::new(helper_call, helper_call + 6));
    assert!(sem.unresolved.is_empty(), "the module call is covered");
    assert_eq!(sem.value_refs.len(), 1, "only the read is a flow input");
}

/// Request economy without changing answers (fastapi docs apps): a decorator call's
/// member identifier asked as a call and as a decorator reference is sent once; a bare
/// name neither declared in the file nor imported (`app`, a module variable) is never
/// asked (Python scoping); `from pkg.api import FastAPI` in two files of one top-level
/// directory is asked once and answers both.
#[test]
fn identical_and_impossible_value_requests_are_not_sent() {
    let a: &[u8] =
        b"from pkg.api import FastAPI\napp = FastAPI()\n@app.get('/')\ndef index():\n    return app\n";
    let b: &[u8] = b"from pkg.api import FastAPI\n";
    let index_def = find(a, "def index");
    let get_at = find(a, "get('/')");
    let mut fa = FileFacts {
        declarations: vec![decl_at(
            a,
            "index",
            "index",
            SymbolKind::Function,
            (index_def, a.len() as u32 - 1),
            index_def + 4,
        )],
        calls: vec![
            call(find(a, "FastAPI()"), "FastAPI", None, 2),
            call(find(a, "app.get"), "app.get", None, 3),
        ],
        ..FileFacts::default()
    };
    fa.references = vec![
        reference(a, "FastAPI\n", "FastAPI", RefKind::Import),
        reference(a, "app.get", "app", RefKind::Decorator),
        reference(a, "get('/')", "get", RefKind::Decorator),
        reference(a, "app\n", "app", RefKind::Read),
    ];
    fa.imports = vec![trace_core::facts::Import {
        local: "FastAPI".into(),
        target: "pkg.api.FastAPI".into(),
        kind: trace_core::facts::ImportKind::Member,
        scope: trace_core::facts::Scope::Module,
        span: ByteSpan::new(0, find(a, "\napp")),
        line: 1,
    }];
    let fa = with_module(fa, a);
    let mut fb = FileFacts {
        references: vec![reference(b, "FastAPI\n", "FastAPI", RefKind::Import)],
        ..FileFacts::default()
    };
    fb.imports = fa.imports.clone();
    let fb = with_module(fb, b);
    // `app` is also a declared name elsewhere (a function in pkg/api.py): only Python
    // scoping rules out the requests for a.py's module variable `app`.
    let api: &[u8] = b"class FastAPI:\n    def get(self, p):\n        pass\ndef app():\n    pass\n";
    let get_def = find(api, "get(self");
    let app_def = find(api, "def app");
    let mut get = decl_at(api, "get", "FastAPI.get", SymbolKind::Method, (get_def - 4, app_def - 1), get_def);
    get.parent = Some(0);
    let fapi = with_module(
        FileFacts {
            declarations: vec![
                decl_at(api, "FastAPI", "FastAPI", SymbolKind::Class, (0, app_def - 1), 6),
                get,
                decl_at(
                    api,
                    "app",
                    "app",
                    SymbolKind::Function,
                    (app_def, api.len() as u32 - 1),
                    app_def + 4,
                ),
            ],
            ..FileFacts::default()
        },
        api,
    );
    let files = [
        SemanticFile {
            path: "src/a.py",
            language: Language::Python,
            hash: Hash32::of(a),
            source: a,
            facts: &fa,
        },
        SemanticFile {
            path: "src/b.py",
            language: Language::Python,
            hash: Hash32::of(b),
            source: b,
            facts: &fb,
        },
        SemanticFile {
            path: "pkg/api.py",
            language: Language::Python,
            hash: Hash32::of(api),
            source: api,
            facts: &fapi,
        },
    ];
    let decls = DeclTable::new(files.iter().map(|f| (f.path, f.source, f.facts)));
    let class_at = pos(api, 6);
    let get_pos = pos(api, get_def);
    let get_line = pos(a, get_at).0;
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, u32, u32)>::new()));
    let log = asked.clone();
    let handler: Handler = Box::new(move |method, params| {
        let line = params["position"]["line"].as_u64().unwrap_or(u64::MAX) as u32;
        let ch = params["position"]["character"].as_u64().unwrap_or(u64::MAX) as u32;
        let uri = params["textDocument"]["uri"].as_str().unwrap_or("").to_string();
        log.lock().unwrap().push((format!("{method} {uri}"), line, ch));
        Ok(match method {
            "textDocument/prepareCallHierarchy" => prepared(params),
            "callHierarchy/outgoingCalls" => json!([]),
            "textDocument/definition" if line == get_line => {
                json!([{"uri": "file:///ws/pkg/api.py", "range": range(get_pos.0, get_pos.1, get_pos.1 + 3)}])
            }
            "textDocument/definition" => {
                json!([{"uri": "file:///ws/pkg/api.py", "range": range(class_at.0, class_at.1, class_at.1 + 7)}])
            }
            other => panic!("unexpected request {other}"),
        })
    });
    let mut session = FakeSession::new(py_caps(), handler);
    let refs: Vec<&SemanticFile<'_>> = files.iter().collect();
    let analysis = analyze(&mut session, &refs, &decls, &FakeUris, &py_options()).unwrap();
    let asked = asked.lock().unwrap().clone();
    let definitions: Vec<&(String, u32, u32)> = asked
        .iter()
        .filter(|(m, _, _)| m.starts_with("textDocument/definition"))
        .collect();
    let at_get = definitions.iter().filter(|(_, l, _)| *l == get_line).count();
    assert_eq!(at_get, 1, "call member and decorator reference share one request: {definitions:?}");
    let imports = definitions
        .iter()
        .filter(|(m, l, _)| *l == 0 && m.contains("/src/"))
        .count();
    assert_eq!(imports, 1, "one absolute import request for src/: {definitions:?}");
    let app_line = pos(a, find(a, "app\n")).0;
    assert!(!definitions.iter().any(|(_, l, _)| *l == app_line), "bare module variable never asked");
    let import_edges = |p: &str| {
        analysis.files[p]
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Imports && e.target == "pkg/api.py:FastAPI")
            .count()
    };
    assert_eq!((import_edges("src/a.py"), import_edges("src/b.py")), (1, 1));
    let decorator_ref = analysis.files["src/a.py"]
        .edges
        .iter()
        .any(|e| e.kind == EdgeKind::References && e.target == "pkg/api.py:FastAPI.get");
    assert!(decorator_ref, "the shared answer still yields the decorator reference");
}

/// NEXT.md item 3 (ripgrep): `builder.max_depth(1).current_dir(&p)` and
/// `Data::from_bytes(x)` inside a prepared method. Outgoing ranges are matched by the
/// member identifier's END (the outer chain call's callee contains every step), a range
/// starting at the path (`Data`) still designates its call, and the chain step the call
/// hierarchy missed is resolved by `definition` in batch C.
#[test]
fn chain_steps_match_by_member_end_and_missing_calls_fall_back_to_definition() {
    let src: &[u8] = b"impl HiArgs {\n    fn walk_builder(&self) {\n        builder\n            .max_depth(1)\n            .current_dir(&self.cwd);\n        let d = Data::from_bytes(x);\n    }\n}\nimpl WalkBuilder {\n    fn max_depth(&self) {}\n    fn current_dir(&self) {}\n}\nimpl Data {\n    fn from_bytes(b: u8) {}\n}\n";
    let walk = find(src, "walk_builder");
    let walk_end = find(src, "    }\n}\nimpl Walk") + 5;
    let builder = find(src, "builder\n");
    let max_depth = find(src, "max_depth(1)");
    let current = find(src, "current_dir(&self");
    let data = find(src, "Data::from_bytes");
    let from_bytes = data + 6;
    let max_decl = find(src, "max_depth(&self");
    let cur_decl = find(src, "current_dir(&self) {}");
    let fb_decl = find(src, "from_bytes(b");
    let method = |name: &str, qualified: &str, at: u32| {
        let end = at + find(&src[at as usize..], "}") + 1;
        decl_at(src, name, qualified, SymbolKind::Method, (at - 3, end), at)
    };
    let mut walker =
        decl_at(src, "walk_builder", "HiArgs.walk_builder", SymbolKind::Method, (walk - 3, walk_end), walk);
    walker.container = Some("HiArgs".into());
    let chain = |end: u32, member: &str| CallSite {
        owner: Some(0),
        lexical_owner: Some(0),
        span: ByteSpan::new(builder, end + 3),
        callee_span: ByteSpan::new(builder, end),
        callee: slice_text(src, ByteSpan::new(builder, end)),
        member: Some(member.into()),
        receiver: None,
        line: 3,
        activation: Activation::Plain,
        is_new: false,
        arg_count: 1,
    };
    let facts = FileFacts {
        declarations: vec![
            walker,
            method("max_depth", "WalkBuilder.max_depth", max_decl),
            method("current_dir", "WalkBuilder.current_dir", cur_decl),
            method("from_bytes", "Data.from_bytes", fb_decl),
        ],
        calls: vec![
            chain(current + 11, "current_dir"),
            chain(max_depth + 9, "max_depth"),
            call(data, "Data::from_bytes", Some(0), 6),
        ],
        ..FileFacts::default()
    };
    // The syntax member of a path call is its last segment.
    let mut facts = facts;
    facts.calls[2].member = Some("from_bytes".into());
    let file = SemanticFile {
        path: "r.rs",
        language: Language::Rust,
        hash: Hash32::of(src),
        source: src,
        facts: &facts,
    };
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let item = |at: u32, name: &str| {
        json!({"name": name, "kind": 6, "uri": "file:///ws/r.rs",
                   "range": lsp_range(src, at - 3, at + 10), "selectionRange": lsp_range(src, at, at + name.len() as u32)})
    };
    let walk_line = pos(src, walk).0;
    let current_pos = pos(src, current);
    let outgoing = json!([
        {"to": item(max_decl, "max_depth"), "fromRanges": [lsp_range(src, max_depth, max_depth + 9)]},
        {"to": item(fb_decl, "from_bytes"), "fromRanges": [lsp_range(src, data, from_bytes + 10)]}
    ]);
    let cur_target = lsp_range(src, cur_decl, cur_decl + 11);
    let handler: Handler = Box::new(move |method, params| {
        Ok(match method {
            "textDocument/prepareCallHierarchy" => prepared(params),
            "callHierarchy/outgoingCalls" => {
                if params["item"]["selectionRange"]["start"]["line"] == walk_line {
                    outgoing.clone()
                } else {
                    json!([])
                }
            }
            "textDocument/definition" => {
                assert_eq!(
                    (
                        params["position"]["line"].as_u64().unwrap() as u32,
                        params["position"]["character"].as_u64().unwrap() as u32
                    ),
                    current_pos,
                    "only the uncovered chain step"
                );
                json!([{"targetUri": "file:///ws/r.rs", "targetRange": cur_target, "targetSelectionRange": cur_target}])
            }
            other => panic!("unexpected request {other}"),
        })
    });
    let caps = json!({"callHierarchyProvider": true, "definitionProvider": true});
    let mut session = FakeSession::new(caps, handler);
    let opts = Options {
        provider: Provider::RustAnalyzer,
        tool_fingerprint: "fp",
        python: false,
        syntax_answers: true,
        hooks: &crate::languages::DefaultServer,
        prepared: empty_prepared(),
        calls_by_definition: false,
        reuse: None,
    };
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &opts).unwrap();
    assert_eq!(session.batches, 3, "prepare, outgoing, definition fallback");
    let sem = &analysis.files["r.rs"];
    let edges: Vec<(&str, Resolution)> =
        sem.edges.iter().map(|e| (e.target.as_str(), e.resolution)).collect();
    assert!(edges.contains(&("r.rs:WalkBuilder.max_depth", Resolution::CallHierarchy)));
    assert!(edges.contains(&("r.rs:Data.from_bytes", Resolution::CallHierarchy)));
    assert!(edges.contains(&("r.rs:WalkBuilder.current_dir", Resolution::Definition)));
    assert!(sem.edges.iter().all(|e| e.owner == 0));
    assert!(sem.unresolved.is_empty(), "{:?}", sem.unresolved);
    assert!(sem.diagnostics.iter().any(|d| d.kind == "definition_fallback"));
}

fn generic_options() -> Options<'static> {
    Options {
        provider: Provider::Lsp("test".into()),
        tool_fingerprint: "fp",
        python: false,
        syntax_answers: true,
        hooks: &crate::languages::DefaultServer,
        prepared: empty_prepared(),
        calls_by_definition: false,
        reuse: None,
    }
}

fn sem_file<'a>(path: &'a str, language: Language, src: &'a [u8], facts: &'a FileFacts) -> SemanticFile<'a> {
    SemanticFile {
        path,
        language,
        hash: Hash32::of(src),
        source: src,
        facts,
    }
}

fn count(session: &FakeSession, method: &str) -> usize {
    session.requests.iter().filter(|m| *m == method).count()
}

/// Rule 6 (server side): `textDocument/implementation` at a trait member's name; the
/// in-index location becomes a `SemImplementation` of the BASE file (the base itself
/// and locations outside the index are ignored); the trait name in the implementing
/// file's `impl` header is asked with `definition` (a compiler fact for the family rule).
#[test]
fn rule_implementation_results_are_recorded_on_the_base_file() {
    let a: &[u8] = b"pub trait Sink {\n    fn matched(&self);\n}\n";
    let b: &[u8] = b"impl Sink for JsonSink {\n    fn matched(&self) {}\n}\n";
    let sink_at = find(a, "Sink");
    let base_at = find(a, "matched");
    let mut base =
        decl_at(a, "matched", "Sink.matched", SymbolKind::Method, (base_at - 3, base_at + 14), base_at);
    base.parent = Some(0);
    base.is_stub = true;
    let fa = with_module(
        FileFacts {
            declarations: vec![
                decl_at(a, "Sink", "Sink", SymbolKind::Interface, (0, a.len() as u32 - 1), sink_at),
                base,
            ],
            ..FileFacts::default()
        },
        a,
    );
    let imp_at = find(b, "matched");
    let mut imp =
        decl_at(b, "matched", "JsonSink.matched", SymbolKind::Method, (imp_at - 3, imp_at + 17), imp_at);
    imp.container = Some("JsonSink".into());
    let mut fb = FileFacts {
        declarations: vec![imp],
        ..FileFacts::default()
    };
    fb.references.push(reference(b, "impl Sink", "Sink", RefKind::Type));
    let fb = with_module(fb, b);
    let files = [
        sem_file("a.rs", Language::Rust, a, &fa),
        sem_file("b.rs", Language::Rust, b, &fb),
    ];
    let decls = DeclTable::new(files.iter().map(|f| (f.path, f.source, f.facts)));
    let base_pos = pos(a, base_at);
    let imp_range = lsp_range(b, imp_at, imp_at + 7);
    let base_range = lsp_range(a, base_at, base_at + 7);
    let sink_range = lsp_range(a, sink_at, sink_at + 4);
    let header_pos = pos(b, find(b, "Sink"));
    let handler: Handler = Box::new(move |method, params| {
        let at = (
            params["position"]["line"].as_u64().unwrap_or(u64::MAX) as u32,
            params["position"]["character"].as_u64().unwrap_or(u64::MAX) as u32,
        );
        Ok(match method {
            "textDocument/implementation" => {
                assert_eq!(at, base_pos, "asked at the base member's name");
                json!([
                    {"targetUri": "file:///ws/b.rs", "targetRange": imp_range, "targetSelectionRange": imp_range},
                    {"uri": "file:///elsewhere/std.rs", "range": range(3, 4, 11)},
                    {"uri": "file:///ws/a.rs", "range": base_range}
                ])
            }
            "textDocument/definition" => {
                assert_eq!(at, header_pos, "only the header type reference");
                json!([{"uri": "file:///ws/a.rs", "range": sink_range}])
            }
            other => panic!("unexpected request {other}"),
        })
    });
    let caps = json!({"implementationProvider": true, "definitionProvider": true});
    let mut session = FakeSession::new(caps, handler);
    let refs: Vec<&SemanticFile<'_>> = files.iter().collect();
    let analysis = analyze(&mut session, &refs, &decls, &FakeUris, &generic_options()).unwrap();
    assert_eq!(count(&session, "textDocument/implementation"), 1, "only implementable members");
    assert_eq!(session.batches, 1, "pipelined with batch A");
    assert_eq!(
        analysis.files["a.rs"].implementations,
        vec![SemImplementation {
            base: 1,
            implementor: "b.rs:JsonSink.matched".into(),
            kind: EdgeKind::Implements,
        }]
    );
    assert!(analysis.files["b.rs"].implementations.is_empty(), "recorded on the base side only");
    assert!(analysis.files["b.rs"]
        .edges
        .iter()
        .any(|e| e.kind == EdgeKind::References && e.target == "a.rs:Sink"));
    // The predicate: interface members and stubs; never constructors or free functions.
    assert!(implementable(&fa, 1));
    assert!(!implementable(&fa, 0));
    assert!(!implementable(&fb, 0), "a member of an impl block is an implementor");
    assert!(overridable_modifier("@abc.abstractmethod"));
    assert!(overridable_modifier("open"));
    assert!(!overridable_modifier("@opener"));
}

/// Rule 6: `prepareTypeHierarchy` + `typeHierarchy/subtypes` for types with members; a
/// subtype member with the base member's name is an override recorded on the base
/// file; a member the direct subtype does not redeclare is searched one level deeper.
#[test]
fn rule_type_hierarchy_subtypes_map_to_members() {
    let base: &[u8] = b"class Base {\n    void run() {}\n    void stop() {}\n}\n";
    let mid: &[u8] = b"class Mid extends Base {\n    void run() {}\n}\n";
    let leaf: &[u8] = b"class Leaf extends Mid {\n    void stop() {}\n}\n";
    let class = |src: &[u8], name: &str, members: &[&str]| {
        let mut declarations = vec![decl_at(
            src,
            name,
            name,
            SymbolKind::Class,
            (0, src.len() as u32 - 1),
            find(src, name),
        )];
        for m in members {
            let at = find(src, &format!("{m}()"));
            let mut d = decl_at(src, m, &format!("{name}.{m}"), SymbolKind::Method, (at - 5, at + 7), at);
            d.parent = Some(0);
            declarations.push(d);
        }
        with_module(
            FileFacts {
                declarations,
                ..FileFacts::default()
            },
            src,
        )
    };
    let (fbase, fmid, fleaf) =
        (class(base, "Base", &["run", "stop"]), class(mid, "Mid", &["run"]), class(leaf, "Leaf", &["stop"]));
    let files = [
        sem_file("Base.java", Language::Java, base, &fbase),
        sem_file("Mid.java", Language::Java, mid, &fmid),
        sem_file("Leaf.java", Language::Java, leaf, &fleaf),
    ];
    let decls = DeclTable::new(files.iter().map(|f| (f.path, f.source, f.facts)));
    let item = |uri: &str, src: &[u8], name: &str| {
        let at = find(src, name);
        json!({"name": name, "kind": 5, "uri": uri, "range": lsp_range(src, 0, src.len() as u32 - 1),
                   "selectionRange": lsp_range(src, at, at + name.len() as u32)})
    };
    let (mid_item, leaf_item) =
        (item("file:///ws/Mid.java", mid, "Mid"), item("file:///ws/Leaf.java", leaf, "Leaf"));
    let handler: Handler = Box::new(move |method, params| {
        Ok(match method {
            "textDocument/prepareTypeHierarchy" => prepared(params),
            "typeHierarchy/subtypes" => match params["item"]["uri"].as_str().unwrap() {
                "file:///ws/Base.java" => json!([mid_item.clone()]),
                "file:///ws/Mid.java" => json!([leaf_item.clone()]),
                _ => json!([]),
            },
            other => panic!("unexpected request {other}"),
        })
    });
    let mut session = FakeSession::new(json!({"typeHierarchyProvider": true}), handler);
    let refs: Vec<&SemanticFile<'_>> = files.iter().collect();
    let analysis = analyze(&mut session, &refs, &decls, &FakeUris, &generic_options()).unwrap();
    assert_eq!(count(&session, "textDocument/prepareTypeHierarchy"), 3);
    assert_eq!(session.batches, 3, "prepare, first subtypes round (batch B), one deeper round");
    let overrides = |base: u32, implementor: &str| SemImplementation {
        base,
        implementor: implementor.into(),
        kind: EdgeKind::Overrides,
    };
    assert_eq!(
        analysis.files["Base.java"].implementations,
        vec![overrides(1, "Mid.java:Mid.run"), overrides(2, "Leaf.java:Leaf.stop")]
    );
    assert!(analysis.files["Mid.java"].implementations.is_empty(), "Leaf does not override run");
    assert!(analysis.files["Leaf.java"].implementations.is_empty());
}

/// Rule 2 (server side): non-call uses whose definition lands outside the index or on a
/// local binding (no declaration of that name) are recorded as resolved elsewhere; a use
/// resolved to its declaration is an edge and never resolved elsewhere.
#[test]
fn rule_definitions_outside_the_index_are_resolved_elsewhere() {
    let a: &[u8] = b"func helper() {}\nfunc run() {\n    x := helper\n    y := lib\n    helper := 1\n    z := helper\n}\n";
    let b: &[u8] = b"func lib() {}\n";
    let run_at = find(a, "run");
    let mut fa = FileFacts {
        declarations: vec![
            decl_at(a, "helper", "helper", SymbolKind::Function, (0, 16), 5),
            decl_at(a, "run", "run", SymbolKind::Function, (run_at - 5, a.len() as u32 - 1), run_at),
        ],
        ..FileFacts::default()
    };
    let mut refs = vec![
        reference(a, "x := helper", "helper", RefKind::Read),
        reference(a, "y := lib", "lib", RefKind::Read),
        reference(a, "z := helper", "helper", RefKind::Read),
    ];
    for r in &mut refs {
        r.owner = Some(1);
    }
    fa.references = refs.clone();
    let fa = with_module(fa, a);
    let fb = with_module(
        FileFacts {
            declarations: vec![decl_at(b, "lib", "lib", SymbolKind::Function, (0, 13), 5)],
            ..FileFacts::default()
        },
        b,
    );
    let files = [sem_file("a.go", Language::Go, a, &fa), sem_file("b.go", Language::Go, b, &fb)];
    let decls = DeclTable::new(files.iter().map(|f| (f.path, f.source, f.facts)));
    let (x_line, y_line, z_line) =
        (pos(a, refs[0].span.start).0, pos(a, refs[1].span.start).0, pos(a, refs[2].span.start).0);
    let local = lsp_range(a, find(a, "helper := 1"), find(a, "helper := 1") + 6);
    let handler: Handler = Box::new(move |method, params| {
        let line = params["position"]["line"].as_u64().unwrap() as u32;
        Ok(match method {
            "textDocument/definition" if line == x_line => {
                json!([{"uri": "file:///ws/a.go", "range": range(0, 5, 11)}])
            }
            "textDocument/definition" if line == y_line => {
                json!([{"uri": "file:///goroot/src/lib.go", "range": range(9, 5, 8)}])
            }
            "textDocument/definition" if line == z_line => {
                json!([{"uri": "file:///ws/a.go", "range": local}])
            }
            other => panic!("unexpected request {other}"),
        })
    });
    let mut session = FakeSession::new(json!({"definitionProvider": true}), handler);
    let shard: Vec<&SemanticFile<'_>> = files.iter().collect();
    let analysis = analyze(&mut session, &shard, &decls, &FakeUris, &generic_options()).unwrap();
    let sem = &analysis.files["a.go"];
    assert_eq!(sem.resolved_elsewhere, vec![refs[1].span, refs[2].span]);
    let to_helper: Vec<ByteSpan> = sem
        .edges
        .iter()
        .filter(|e| e.target == "a.go:helper" && e.kind == EdgeKind::References)
        .map(|e| e.at)
        .collect();
    assert_eq!(to_helper, vec![refs[0].span]);
    assert!(!sem.resolved_elsewhere.contains(&refs[0].span));
}

/// Rule 15 (1): names syntax proves are local bindings (`FileFacts::local_spans`) are
/// never sent to `definition`: references and callbacks are skipped, a call of a local is
/// recorded exactly like a definition answer outside the index (`external_or_ambiguous`).
#[test]
fn rule_no_definition_requests_for_local_bindings() {
    let src: &[u8] = b"helper <- function() NULL\nrun <- function(helper) {\n  helper()\n  helper\n}\n";
    let run_at = find(src, "run <-");
    let param = find(src, "helper) {");
    let call_at = find(src, "helper()\n");
    let read = reference(src, "  helper\n}", "helper", RefKind::Read);
    let mut facts = FileFacts {
        declarations: vec![
            decl_at(src, "helper", "helper", SymbolKind::Function, (0, 25), 0),
            decl_at(src, "run", "run", SymbolKind::Function, (run_at, src.len() as u32 - 1), run_at),
        ],
        calls: vec![call(call_at, "helper", Some(1), 3)],
        ..FileFacts::default()
    };
    facts.references.push(Reference {
        owner: Some(1),
        ..read.clone()
    });
    facts.local_spans = vec![ByteSpan::new(param, param + 6), ByteSpan::new(call_at, call_at + 6), read.span];
    let facts = with_module(facts, src);
    let file = sem_file("m.R", Language::R, src, &facts);
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let handler: Handler = Box::new(|method, _| panic!("no request expected, got {method}"));
    let mut session = FakeSession::new(json!({"definitionProvider": true}), handler);
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &generic_options()).unwrap();
    assert!(session.requests.is_empty(), "{:?}", session.requests);
    let sem = &analysis.files["m.R"];
    assert!(sem.edges.is_empty(), "a local never links to the same-named function");
    assert_eq!(sem.unresolved.len(), 1);
    assert_eq!(sem.unresolved[0].kind, UnresolvedKind::ExternalOrAmbiguous);
    assert!(sem.unresolved[0].candidates.is_empty());
    assert!(sem.diagnostics.iter().any(|d| d.kind == "local_call"));
}

/// Rule 3 with lexical scoping (`crate::engine::rules::scoping`): a Haskell bare call whose only
/// declarations are `where` bindings of another function is answered `external_by_name`
/// without a request; the call inside the binding's equation is still asked.
#[test]
fn rule_bare_call_of_out_of_scope_where_binding_is_external_by_name() {
    let src: &[u8] =
        b"module M where\n\ngetChecker xs = map xs\n  where\n    map ys = ys\n\nrender xs = map xs\n";
    let facts = trace_syntax::extract(trace_syntax::SourceInput {
        path: "M.hs",
        language: Language::Haskell,
        source: src,
    })
    .unwrap();
    let file = sem_file("M.hs", Language::Haskell, src, &facts);
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let inside = find(src, "map xs\n  where");
    let outside = src.len() as u32 - "map xs\n".len() as u32;
    let asked: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&asked);
    let handler: Handler = Box::new(move |method, params| {
        assert_eq!(method, "textDocument/definition");
        let at = (
            params["position"]["line"].as_u64().unwrap() as u32,
            params["position"]["character"].as_u64().unwrap() as u32,
        );
        log.lock().unwrap().push(at);
        Ok(Value::Null)
    });
    let mut session = FakeSession::new(json!({"definitionProvider": true}), handler);
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &generic_options()).unwrap();
    let asked = asked.lock().unwrap().clone();
    assert!(asked.contains(&pos(src, inside)), "{asked:?}");
    assert!(!asked.contains(&pos(src, outside)), "{asked:?}");
    let sem = &analysis.files["M.hs"];
    let outside_call = facts.calls.iter().find(|c| c.callee_span.start == outside).unwrap();
    let answer = sem
        .unresolved
        .iter()
        .find(|u| u.at == outside_call.callee_span)
        .unwrap();
    assert_eq!(answer.kind, UnresolvedKind::ExternalOrAmbiguous);
    assert!(answer.candidates.is_empty());
    assert!(sem.edges.iter().all(|e| e.at != outside_call.callee_span));
}

/// Scala application rule on definition answers (`crate::engine::rules::scala_apply`): `Wub(0L)` answered
/// by the case class and its companion object (no own or inherited `apply`) is a proven
/// call of the class.
#[test]
fn rule_scala_definition_answer_of_case_class_application_constructs_the_class() {
    let src: &[u8] = b"object Use {\n  def zero = Wub(0L)\n}\n\ncase class Wub(x: Long)\n\nobject Wub\n";
    let facts = trace_syntax::extract(trace_syntax::SourceInput {
        path: "A.scala",
        language: Language::Scala,
        source: src,
    })
    .unwrap();
    let file = sem_file("A.scala", Language::Scala, src, &facts);
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let class_at = find(src, "Wub(x");
    let object_at = find(src, "Wub\n");
    let (class_pos, object_pos) = (pos(src, class_at), pos(src, object_at));
    let handler: Handler = Box::new(move |method, _params| {
        assert_eq!(method, "textDocument/definition");
        Ok(json!([
            {"uri": "file:///ws/A.scala", "range": range(class_pos.0, class_pos.1, class_pos.1 + 3)},
            {"uri": "file:///ws/A.scala", "range": range(object_pos.0, object_pos.1, object_pos.1 + 3)}
        ]))
    });
    let mut session = FakeSession::new(json!({"definitionProvider": true}), handler);
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &generic_options()).unwrap();
    let sem = &analysis.files["A.scala"];
    let call_at = find(src, "Wub(0L)");
    let class = facts
        .declarations
        .iter()
        .position(|d| d.name_span.start == class_at)
        .unwrap();
    let class_uid = decls.uid(DeclRef {
        path: "A.scala",
        decl: class as u32,
    });
    let edge = sem
        .edges
        .iter()
        .find(|e| e.at.start == call_at)
        .expect("a proven call");
    assert_eq!(edge.target, class_uid);
    assert!(sem.unresolved.iter().all(|u| u.at.start != call_at));
}

/// A shell script with its definition answers: every call's member position maps to the
/// name of the declaration it calls (any file), or `null` for commands no file declares.
struct Scripts {
    lib: &'static [u8],
    main: &'static [u8],
    flib: FileFacts,
    fmain: FileFacts,
}

fn scripts() -> Scripts {
    let lib: &'static [u8] = b"greet() {\n  echo hi\n}\nshout() {\n  greet\n}\n";
    let main: &'static [u8] =
        b"run() {\n  greet\n  shout\n  helper\n  echo done\n}\nhelper() {\n  true\n}\nrun\n";
    let function = |src: &[u8], name: &str, end: &str| {
        let at = find(src, &format!("{name}() {{"));
        let end = find(src, end) + end.len() as u32;
        decl_at(src, name, name, SymbolKind::Function, (at, end), at)
    };
    let flib = with_module(
        FileFacts {
            declarations: vec![function(lib, "greet", "hi\n}"), function(lib, "shout", "greet\n}")],
            calls: vec![
                call(find(lib, "echo"), "echo", Some(0), 2),
                call(find(lib, "greet\n}"), "greet", Some(1), 5),
            ],
            ..FileFacts::default()
        },
        lib,
    );
    let fmain = with_module(
        FileFacts {
            declarations: vec![function(main, "run", "done\n}"), function(main, "helper", "true\n}")],
            calls: vec![
                call(find(main, "greet"), "greet", Some(0), 2),
                call(find(main, "shout"), "shout", Some(0), 3),
                call(find(main, "helper\n"), "helper", Some(0), 4),
                call(find(main, "echo"), "echo", Some(0), 5),
                call(find(main, "true"), "true", Some(1), 8),
                call(find(main, "run\n"), "run", None, 10),
            ],
            // main.sh sources lib.sh (the server sees lib.sh's functions from main.sh).
            imports: vec![source_import("./lib.sh")],
            ..FileFacts::default()
        },
        main,
    );
    Scripts {
        lib,
        main,
        flib,
        fmain,
    }
}

/// A Bash `source <target>` import fact.
fn source_import(target: &str) -> trace_core::facts::Import {
    trace_core::facts::Import {
        local: "*".into(),
        target: target.into(),
        kind: trace_core::facts::ImportKind::Wildcard,
        scope: trace_core::facts::Scope::Module,
        span: ByteSpan::new(0, 1),
        line: 1,
    }
}

/// Edges (path, owner, target, kind, at), resolutions, and the definition requests of one
/// run over [`scripts`].
type ScriptRun = (Vec<(String, u32, String, EdgeKind, ByteSpan)>, Vec<Resolution>, usize);

fn run_scripts(s: &Scripts, syntax_answers: bool) -> ScriptRun {
    let files = [
        sem_file("lib.sh", Language::Bash, s.lib, &s.flib),
        sem_file("main.sh", Language::Bash, s.main, &s.fmain),
    ];
    let decls = DeclTable::new(files.iter().map(|f| (f.path, f.source, f.facts)));
    // (uri, line, character) of each call member -> the called declaration's name range.
    let mut answers: HashMap<(String, u32, u32), Value> = HashMap::new();
    for f in &files {
        for c in &f.facts.calls {
            let (line, ch) = pos(f.source, member_point(c));
            let target = files.iter().find_map(|g| {
                    g.facts
                        .declarations
                        .iter()
                        .find(|d| Some(d.name.as_str()) == c.member.as_deref() && d.kind.is_callable())
                        .map(|d| json!([{"uri": format!("file:///ws/{}", g.path), "range": lsp_range(g.source, d.name_span.start, d.name_span.end)}]))
                });
            answers.insert((format!("file:///ws/{}", f.path), line, ch), target.unwrap_or(Value::Null));
        }
    }
    let handler: Handler = Box::new(move |method, params| {
        assert_eq!(method, "textDocument/definition");
        let key = (
            params["textDocument"]["uri"].as_str().unwrap().to_string(),
            params["position"]["line"].as_u64().unwrap() as u32,
            params["position"]["character"].as_u64().unwrap() as u32,
        );
        Ok(answers.get(&key).cloned().unwrap_or(Value::Null))
    });
    let mut session = FakeSession::new(json!({"definitionProvider": true}), handler);
    let opts = Options {
        syntax_answers,
        ..generic_options()
    };
    let refs: Vec<&SemanticFile<'_>> = files.iter().collect();
    let analysis = analyze(&mut session, &refs, &decls, &FakeUris, &opts).unwrap();
    let mut edges = Vec::new();
    let mut resolutions = Vec::new();
    for (path, sem) in &analysis.files {
        for e in &sem.edges {
            edges.push((path.clone(), e.owner, e.target.clone(), e.kind, e.at));
            resolutions.push(e.resolution);
        }
    }
    edges.sort_by(|a, b| (&a.0, a.4.start).cmp(&(&b.0, b.4.start)));
    (edges, resolutions, count(&session, "textDocument/definition"))
}

/// Rule 15 (2, 3): Bash calls of the only same-file function are answered from syntax
/// (`syntax_definition`) and commands no file declares are not asked; the edges are
/// identical in (owner, target, kind, at) with and without the shortcut, with far fewer
/// requests.
#[test]
fn rule_same_file_definitions_come_from_syntax() {
    let s = scripts();
    let (with_edges, with_resolutions, with_requests) = run_scripts(&s, true);
    let (without_edges, without_resolutions, without_requests) = run_scripts(&s, false);
    assert_eq!(with_edges, without_edges, "identical answers");
    // lib.sh: shout -> greet; main.sh: run -> greet, shout, helper; <module> -> run.
    assert_eq!(with_edges.len(), 5, "{with_edges:?}");
    assert_eq!(without_requests, 8, "every call is asked without the shortcut");
    assert_eq!(with_requests, 2, "only the cross-file calls greet and shout");
    assert_eq!(
        with_resolutions
            .iter()
            .filter(|r| **r == Resolution::SyntaxDefinition)
            .count(),
        3
    );
    assert!(without_resolutions.iter().all(|r| *r == Resolution::Definition));
    // Never across files, never for member accesses or other languages.
    let files = [sem_file("main.sh", Language::Bash, s.main, &s.fmain)];
    let decls = DeclTable::new(files.iter().map(|f| (f.path, f.source, f.facts)));
    let greet = &s.fmain.calls[0];
    assert_eq!(syntax_definition(&files[0], greet, &decls, None), None, "greet is declared in another file");
    let helper = &s.fmain.calls[2];
    assert!(syntax_definition(&files[0], helper, &decls, None).is_some());
    let as_go = sem_file("main.sh", Language::Go, s.main, &s.fmain);
    assert_eq!(syntax_definition(&as_go, helper, &decls, None), None);
    let mut dotted = helper.clone();
    dotted.callee = format!("obj.{}", dotted.callee);
    assert_eq!(syntax_definition(&files[0], &dotted, &decls, None), None, "member accesses never");
}

/// Rule 15: every request of a generic (definition-only) server is sent in one pipelined
/// batch, implementation requests included.
#[test]
fn rule_requests_are_pipelined_for_generic_servers() {
    let sources: Vec<(String, Vec<u8>)> = (0..3)
        .map(|i| {
            let next = (i + 1) % 3;
            let body: String = (0..5).map(|_| format!("    f{next}()\n")).collect();
            (format!("m{i}.go"), format!("func f{i}() {{\n{body}}}\n").into_bytes())
        })
        .collect();
    let facts: Vec<FileFacts> = sources
        .iter()
        .enumerate()
        .map(|(i, (_, src))| {
            let next = (i + 1) % 3;
            let callee = format!("f{next}");
            let mut calls = Vec::new();
            let mut from = 10usize;
            while let Some(at) = src[from..]
                .windows(callee.len() + 2)
                .position(|w| w == format!("{callee}()").as_bytes())
            {
                let at = (from + at) as u32;
                calls.push(call(at, &callee, Some(0), pos(src, at).0 + 1));
                from = at as usize + 1;
            }
            with_module(
                FileFacts {
                    declarations: vec![decl_at(
                        src,
                        &format!("f{i}"),
                        &format!("f{i}"),
                        SymbolKind::Function,
                        (0, src.len() as u32 - 1),
                        5,
                    )],
                    calls,
                    ..FileFacts::default()
                },
                src,
            )
        })
        .collect();
    let files: Vec<SemanticFile<'_>> = sources
        .iter()
        .zip(&facts)
        .map(|((path, src), facts)| sem_file(path, Language::Go, src, facts))
        .collect();
    let decls = DeclTable::new(files.iter().map(|f| (f.path, f.source, f.facts)));
    let handler: Handler = Box::new(|method, params| {
        assert_eq!(method, "textDocument/definition");
        let uri = params["textDocument"]["uri"].as_str().unwrap();
        let i: usize = uri
            .trim_start_matches("file:///ws/m")
            .trim_end_matches(".go")
            .parse()
            .unwrap();
        Ok(json!([{"uri": format!("file:///ws/m{}.go", (i + 1) % 3), "range": range(0, 5, 7)}]))
    });
    let mut session =
        FakeSession::new(json!({"definitionProvider": true, "implementationProvider": true}), handler);
    let refs: Vec<&SemanticFile<'_>> = files.iter().collect();
    let analysis = analyze(&mut session, &refs, &decls, &FakeUris, &generic_options()).unwrap();
    assert_eq!(session.batches, 1, "one pipelined batch");
    assert_eq!(count(&session, "textDocument/definition"), 15);
    for (i, f) in files.iter().enumerate() {
        let sem = &analysis.files[f.path];
        assert_eq!(sem.edges.len(), 5, "{}", f.path);
        assert!(sem
            .edges
            .iter()
            .all(|e| e.target == format!("m{}.go:f{}", (i + 1) % 3, (i + 1) % 3)));
    }
}

/// Rule 15 (2, shell scoping): a bare call's candidates are the declarations of the
/// script and the files it sources, transitively. A function also declared in a script
/// that is NOT sourced is still answered from syntax; one also declared in a sourced
/// file is asked (the server sees both).
#[test]
fn rule_bash_bare_call_candidates_follow_sourced_files() {
    let main: &[u8] =
        b"source ./lib.sh\nhelper() {\n  true\n}\nlog() {\n  true\n}\nrun() {\n  helper\n  log\n}\n";
    let lib: &[u8] = b"log() {\n  echo lib\n}\n";
    let other: &[u8] = b"helper() {\n  echo other\n}\n";
    let function = |src: &[u8], name: &str| {
        let at = find(src, &format!("{name}() {{"));
        let end = at + src[at as usize..].windows(2).position(|w| w == b"}\n").unwrap() as u32 + 2;
        decl_at(src, name, name, SymbolKind::Function, (at, end), at)
    };
    let fmain = with_module(
        FileFacts {
            declarations: vec![function(main, "helper"), function(main, "log"), function(main, "run")],
            calls: vec![
                call(find(main, "helper\n  log"), "helper", Some(2), 9),
                call(find(main, "  log\n") + 2, "log", Some(2), 10),
            ],
            imports: vec![source_import("./lib.sh")],
            ..FileFacts::default()
        },
        main,
    );
    let flib = with_module(
        FileFacts {
            declarations: vec![function(lib, "log")],
            ..FileFacts::default()
        },
        lib,
    );
    let fother = with_module(
        FileFacts {
            declarations: vec![function(other, "helper")],
            ..FileFacts::default()
        },
        other,
    );
    let files = [
        sem_file("main.sh", Language::Bash, main, &fmain),
        sem_file("lib.sh", Language::Bash, lib, &flib),
        sem_file("tests/other.sh", Language::Bash, other, &fother),
    ];
    let decls = DeclTable::new(files.iter().map(|f| (f.path, f.source, f.facts)));
    let log_range = lsp_range(lib, 0, 3);
    let handler: Handler = Box::new(move |method, params| {
        assert_eq!(method, "textDocument/definition");
        assert_eq!(params["textDocument"]["uri"], "file:///ws/main.sh");
        // The server sees main.sh's and lib.sh's `log`.
        Ok(
            json!([{"uri": "file:///ws/main.sh", "range": range(4, 0, 3)}, {"uri": "file:///ws/lib.sh", "range": log_range}]),
        )
    });
    let mut session = FakeSession::new(json!({"definitionProvider": true}), handler);
    let analysis = analyze(&mut session, &[&files[0]], &decls, &FakeUris, &generic_options()).unwrap();
    assert_eq!(count(&session, "textDocument/definition"), 1, "only `log` is asked");
    let sem = &analysis.files["main.sh"];
    let helper = sem
        .edges
        .iter()
        .find(|e| e.target == "main.sh:helper")
        .expect("helper edge");
    assert_eq!(helper.resolution, Resolution::SyntaxDefinition, "tests/other.sh is not sourced");
    let log = sem
        .unresolved
        .iter()
        .find(|u| u.callee == "log")
        .expect("log is ambiguous");
    assert_eq!(log.candidates, vec!["lib.sh:log".to_string(), "main.sh:log".to_string()]);
    // Partition-wide (no scope), `helper` has two declarations: never a syntax answer.
    assert_eq!(syntax_definition(&files[0], &fmain.calls[0], &decls, None), None);
    let scope: BTreeSet<&str> = ["main.sh", "lib.sh"].into_iter().collect();
    assert!(syntax_definition(&files[0], &fmain.calls[0], &decls, Some(&scope)).is_some());
}

/// Rule (declaration reuse): after an edit inside one function, the unchanged function's
/// answers are reused (shifted to its new position) and only the edited function is
/// asked; the result equals a full analysis of the edited file.
#[test]
fn rule_unchanged_declarations_reuse_answers() {
    use crate::cache::{CacheContext, SemanticCache};
    let v1: &[u8] = b"func f1() {\n    g()\n}\nfunc f2() {\n    g()\n}\n";
    let v2: &[u8] = b"func f1() {\n    g()\n    g()\n}\nfunc f2() {\n    g()\n}\n";
    let b: &[u8] = b"func g() {}\n";
    let facts_of = |src: &[u8]| {
        let f2 = find(src, "func f2");
        let mut calls = Vec::new();
        let mut from = 0usize;
        while let Some(at) = src[from..].windows(3).position(|w| w == b"g()") {
            let at = (from + at) as u32;
            calls.push(call(at, "g", Some(u32::from(at > f2)), pos(src, at).0 + 1));
            from = at as usize + 1;
        }
        let mut facts = with_module(
            FileFacts {
                declarations: vec![
                    decl_at(src, "f1", "f1", SymbolKind::Function, (0, f2), 5),
                    decl_at(src, "f2", "f2", SymbolKind::Function, (f2, src.len() as u32), f2 + 5),
                ],
                calls,
                ..FileFacts::default()
            },
            src,
        );
        facts.interface = trace_core::Hash32::of(b"f1() f2()");
        facts
    };
    let (fa1, fa2) = (facts_of(v1), facts_of(v2));
    let fb = with_module(
        FileFacts {
            declarations: vec![decl_at(b, "g", "g", SymbolKind::Function, (0, 11), 5)],
            ..FileFacts::default()
        },
        b,
    );
    let g_range = lsp_range(b, 5, 6);
    let handler = move || -> Handler {
        let g_range = g_range.clone();
        Box::new(move |method, _| {
            assert_eq!(method, "textDocument/definition");
            Ok(json!([{"uri": "file:///ws/b.go", "range": g_range}]))
        })
    };
    let caps = json!({"definitionProvider": true});
    let run = |a: &SemanticFile<'_>, opts: &Options<'_>| {
        let fbfile = sem_file("b.go", Language::Go, b, &fb);
        let decls = DeclTable::new([(a.path, a.source, a.facts), (fbfile.path, fbfile.source, fbfile.facts)]);
        let mut session = FakeSession::new(caps.clone(), handler());
        let analysis = analyze(&mut session, &[a], &decls, &FakeUris, opts).unwrap();
        (analysis, count(&session, "textDocument/definition"))
    };
    let a1 = sem_file("a.go", Language::Go, v1, &fa1);
    let a2 = sem_file("a.go", Language::Go, v2, &fa2);
    let bfile = sem_file("b.go", Language::Go, b, &fb);
    let (first, asked) = run(&a1, &generic_options());
    assert_eq!(asked, 2);
    let answers = first.answers["a.go"].clone();
    assert_eq!(answers.units.len(), 2, "f1 and f2 are reuse units");
    let env = trace_core::Hash32::of(b"env");
    let mut cache = SemanticCache::in_memory("lsp:test");
    {
        let refs = [&a1, &bfile];
        let mut ctx = CacheContext::new(env, &refs);
        cache.store(&mut ctx, "a.go", &first.files["a.go"]);
        cache.store_answers(&mut ctx, "a.go", answers, &first.files["a.go"]);
    }
    let refs = [&a2, &bfile];
    let mut ctx = CacheContext::new(env, &refs);
    assert!(cache.lookup(&mut ctx, "a.go").is_none(), "the file changed");
    let reuse = cache.reuse(&mut ctx, "a.go").expect("f2 is unchanged");
    assert_eq!(reuse.spans.len(), 1);
    let map: HashMap<String, FileReuse> = [("a.go".to_string(), reuse)].into_iter().collect();
    let opts = Options {
        reuse: Some(&map),
        ..generic_options()
    };
    let (partial, asked) = run(&a2, &opts);
    assert_eq!(asked, 2, "only the two calls of the edited f1");
    assert_eq!(partial.units["a.go"], (1, 1));
    let (full, asked_full) = run(&a2, &generic_options());
    assert_eq!(asked_full, 3);
    assert_eq!(partial.files["a.go"], full.files["a.go"], "reuse equals a full analysis");
    assert_eq!(partial.answers["a.go"].units, full.answers["a.go"].units);
    // A changed interface disables reuse.
    let mut other_interface = facts_of(v2);
    other_interface.interface = trace_core::Hash32::of(b"f1(x) f2()");
    let a3 = sem_file("a.go", Language::Go, v2, &other_interface);
    let refs = [&a3, &bfile];
    let mut ctx = CacheContext::new(env, &refs);
    assert!(cache.reuse(&mut ctx, "a.go").is_none());
}

#[test]
fn bom_is_stripped_for_servers() {
    assert_eq!(lsp_text(b"\xEF\xBB\xBFx = 1"), Some("x = 1"));
    assert_eq!(lsp_text(b"\xFF"), None);
}

/// Hooks of the answer-mapping rule tests: inactive regions, one file outside the build,
/// external programs by name.
struct RuleHooks {
    inactive_regions: bool,
    outside: Option<&'static str>,
    programs: &'static [&'static str],
}

impl Server for RuleHooks {
    fn preflight(
        &self,
        _cx: &crate::languages::SetupContext<'_>,
    ) -> Result<Prepared, trace_core::setup_error::SetupError> {
        Ok(Prepared::default())
    }
    fn fn_type_route(&self, _language: Language) -> crate::backends::fntype::FnTypeRoute {
        crate::backends::fntype::FnTypeRoute::TableOnly
    }
    fn answer_policy(&self) -> crate::languages::AnswerPolicy {
        crate::languages::AnswerPolicy {
            inactive_regions: self.inactive_regions,
            ..crate::languages::AnswerPolicy::default()
        }
    }
    fn outside_build_file(&self, path: &str, _prepared: &Prepared) -> Option<String> {
        (self.outside == Some(path)).then(|| "not in the compile database".to_string())
    }
    fn external_program(&self, command: &str, _prepared: &Prepared) -> bool {
        self.programs.contains(&command)
    }
}

/// A Go module-cache file of `module@version` under a temporary directory (an absolute
/// path, so its URI parses on every OS; `external::locate` finds the `pkg/mod` layout).
fn modcache_uri(dir: &std::path::Path, module: &str, file: &str) -> String {
    let path = dir
        .join("gopath")
        .join("pkg")
        .join("mod")
        .join("example.com")
        .join(module)
        .join(file);
    crate::lsp::path_to_uri(&path).expect("uri")
}

/// Rule (I-04): an outgoing-call `to` item outside the index is classified like a
/// definition location and recorded as a library call of the matched syntax call (its
/// declaration position kept); the call stays `external_or_ambiguous`.
#[test]
fn rule_call_hierarchy_external_targets_are_library_calls() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lib_uri = modcache_uri(tmp.path(), "kit@v1.2.0", "kit.go");
    let src: &[u8] = b"func run() {\n    kit.Do()\n}\n";
    let run_at = find(src, "run");
    let call_at = find(src, "kit.Do");
    let facts = with_module(
        FileFacts {
            declarations: vec![decl_at(
                src,
                "run",
                "run",
                SymbolKind::Function,
                (0, src.len() as u32 - 1),
                run_at,
            )],
            calls: vec![call(call_at, "kit.Do", Some(0), 2)],
            ..FileFacts::default()
        },
        src,
    );
    let file = sem_file("a.go", Language::Go, src, &facts);
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let do_range = lsp_range(src, call_at + 4, call_at + 6);
    let handler: Handler = Box::new(move |method, params| {
        Ok(match method {
            "textDocument/prepareCallHierarchy" => prepared(params),
            "callHierarchy/outgoingCalls" => json!([{
                "to": {"name": "Do", "kind": 12, "uri": lib_uri.clone(), "range": range(3, 0, 20),
                       "selectionRange": range(3, 5, 7)},
                "fromRanges": [do_range.clone()]
            }]),
            other => panic!("unexpected request {other}"),
        })
    });
    let caps = json!({"callHierarchyProvider": true, "definitionProvider": true});
    let mut session = FakeSession::new(caps, handler);
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &generic_options()).unwrap();
    assert_eq!(count(&session, "textDocument/definition"), 0, "the call hierarchy answered the call");
    let sem = &analysis.files["a.go"];
    let callee = facts.calls[0].callee_span;
    assert_eq!(sem.library_calls.len(), 1, "{:?}", sem.library_calls);
    let lc = &sem.library_calls[0];
    assert_eq!(lc.at, callee);
    assert_eq!((lc.decl_line, lc.decl_column), (3, 5));
    let lib = &sem.library_files[lc.file as usize];
    assert_eq!(lib.package, "example.com/kit");
    assert_eq!(lib.version.as_deref(), Some("v1.2.0"));
    assert!(sem
        .unresolved
        .iter()
        .any(|u| u.at == callee && u.kind == UnresolvedKind::ExternalOrAmbiguous));
    assert!(sem.edges.is_empty());
}

/// Rule (I-06): a `fromRanges` range over a whole invocation (jdtls, Roslyn) designates
/// the call whose expression ends where the range ends (never the receiver's inner call
/// sharing its start), and the edge is keyed on that call's callee span.
#[test]
fn rule_whole_invocation_ranges_are_keyed_on_the_callee() {
    let src: &[u8] =
            b"class A {\n  void run() {\n    make().helper(1);\n  }\n  void helper(int x) {}\n  A make() { return this; }\n}\n";
    let class_at = find(src, "A {");
    let run_at = find(src, "run(");
    let helper_at = find(src, "helper(int");
    let make_at = find(src, "make() {");
    let chain = find(src, "make().helper");
    let method = |name: &str, at: u32, span: (u32, u32)| {
        let mut d = decl_at(src, name, &format!("A.{name}"), SymbolKind::Method, span, at);
        d.parent = Some(0);
        d
    };
    let run_end = find(src, "1);\n  }") + "1);\n  }".len() as u32;
    let helper_end = helper_at + "helper(int x) {}".len() as u32;
    let make_end = make_at + "make() { return this; }".len() as u32;
    let make_call = call(chain, "make", Some(1), 3);
    let mut helper_call = call(chain, "make().helper", Some(1), 3);
    helper_call.span = ByteSpan::new(chain, chain + "make().helper(1)".len() as u32);
    helper_call.arg_count = 1;
    let facts = with_module(
        FileFacts {
            declarations: vec![
                decl_at(src, "A", "A", SymbolKind::Class, (0, src.len() as u32 - 1), class_at),
                method("run", run_at, (run_at - 5, run_end)),
                method("helper", helper_at, (helper_at - 5, helper_end)),
                method("make", make_at, (make_at - 2, make_end)),
            ],
            calls: vec![make_call.clone(), helper_call.clone()],
            ..FileFacts::default()
        },
        src,
    );
    let file = sem_file("A.java", Language::Java, src, &facts);
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let item = |name: &str, at: u32| {
        let r = lsp_range(src, at, at + name.len() as u32);
        json!({"name": name, "kind": 6, "uri": "file:///ws/A.java", "range": r, "selectionRange": r})
    };
    let (helper_item, make_item) = (item("helper", helper_at), item("make", make_at));
    let whole = lsp_range(src, chain, helper_call.span.end);
    let inner = lsp_range(src, chain, make_call.span.end);
    let run_line = u64::from(pos(src, run_at).0);
    let handler: Handler = Box::new(move |method, params| {
        Ok(match method {
            "textDocument/prepareCallHierarchy" => prepared(params),
            "callHierarchy/outgoingCalls" => {
                if params["item"]["selectionRange"]["start"]["line"] == run_line {
                    json!([
                        {"to": helper_item.clone(), "fromRanges": [whole.clone()]},
                        {"to": make_item.clone(), "fromRanges": [inner.clone()]}
                    ])
                } else {
                    json!([])
                }
            }
            other => panic!("unexpected request {other}"),
        })
    });
    let caps = json!({"callHierarchyProvider": true, "definitionProvider": true});
    let mut session = FakeSession::new(caps, handler);
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &generic_options()).unwrap();
    assert_eq!(count(&session, "textDocument/definition"), 0, "both calls covered");
    let sem = &analysis.files["A.java"];
    let at_of = |target: &str| {
        sem.edges
            .iter()
            .find(|e| e.target == target && e.kind == EdgeKind::Calls)
            .map(|e| (e.at, e.line, e.owner))
    };
    assert_eq!(at_of("A.java:A.helper"), Some((helper_call.callee_span, 3, 1)));
    assert_eq!(at_of("A.java:A.make"), Some((make_call.callee_span, 3, 1)));
    assert!(sem.unresolved.is_empty(), "{:?}", sem.unresolved);
}

/// Rule (I-07): a call carrying a callback argument is asked even when no declaration of
/// the partition carries its name (library behaviour needs the library target); without a
/// server answer it keeps the rule-3 answer. Calls without a callback keep rule 3 (no
/// request).
#[test]
fn rule_callback_calls_are_always_asked() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lib_uri = modcache_uri(tmp.path(), "kit@v1.2.0", "kit.go");
    let src: &[u8] =
            b"func handle() {}\nfunc run() {\n    kit.Each(items, handle)\n    kit.Map(items, handle)\n    kit.Other(items)\n}\n";
    let run_at = find(src, "run(");
    let each = find(src, "kit.Each");
    let map = find(src, "kit.Map");
    let other = find(src, "kit.Other");
    let mut facts = FileFacts {
        declarations: vec![
            decl_at(src, "handle", "handle", SymbolKind::Function, (0, 16), 5),
            decl_at(src, "run", "run", SymbolKind::Function, (run_at - 5, src.len() as u32 - 1), run_at),
        ],
        calls: vec![
            call(each, "kit.Each", Some(1), 3),
            call(map, "kit.Map", Some(1), 4),
            call(other, "kit.Other", Some(1), 5),
        ],
        ..FileFacts::default()
    };
    for (i, at) in [(0usize, each), (1usize, map)] {
        let arg = at + find(&src[at as usize..], "handle");
        facts.callbacks.push(CallbackArg {
            call_callee_span: facts.calls[i].callee_span,
            callee: facts.calls[i].callee.clone(),
            arg_span: ByteSpan::new(arg, arg + 6),
            argument: "handle".into(),
            name: "handle".into(),
            owner: Some(1),
            index: Some(1),
            keyword: None,
        });
    }
    let facts = with_module(facts, src);
    let file = sem_file("a.go", Language::Go, src, &facts);
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let each_member = pos(src, each + 4);
    let map_member = pos(src, map + 4);
    let handler: Handler = Box::new(move |method, params| {
        assert_eq!(method, "textDocument/definition");
        let at = (
            params["position"]["line"].as_u64().unwrap() as u32,
            params["position"]["character"].as_u64().unwrap() as u32,
        );
        Ok(if at == each_member {
            json!([{"uri": lib_uri.clone(), "range": range(7, 5, 9)}])
        } else if at == map_member {
            Value::Null
        } else {
            // The callback arguments: the in-index function.
            json!([{"uri": "file:///ws/a.go", "range": range(0, 5, 11)}])
        })
    });
    let mut session = FakeSession::new(json!({"definitionProvider": true}), handler);
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &generic_options()).unwrap();
    // Each and Map (callbacks) and the two callback arguments; never Other.
    assert_eq!(count(&session, "textDocument/definition"), 4);
    let sem = &analysis.files["a.go"];
    let (each_span, map_span, other_span) =
        (facts.calls[0].callee_span, facts.calls[1].callee_span, facts.calls[2].callee_span);
    assert!(sem.library_calls.iter().any(|c| c.at == each_span), "{:?}", sem.library_calls);
    let kind_at = |span: ByteSpan| {
        sem.unresolved
            .iter()
            .find(|u| u.at == span)
            .map(|u| (u.kind, u.candidates.len()))
    };
    assert_eq!(kind_at(each_span), Some((UnresolvedKind::ExternalOrAmbiguous, 0)));
    assert_eq!(kind_at(map_span), Some((UnresolvedKind::ExternalOrAmbiguous, 0)), "rule-3 answer kept");
    assert_eq!(kind_at(other_span), Some((UnresolvedKind::ExternalOrAmbiguous, 0)));
    assert!(sem.diagnostics.iter().any(|d| d.kind == "external_by_name"));
    assert_eq!(
        sem.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::PassesCallback)
            .count(),
        2
    );
}

/// Rule (I-02): calls through a receiver answered only by one library declaration ask
/// `textDocument/implementation` ONCE for that declaration; the concrete in-index members
/// named like the member become library dispatch entries of every such call (interface
/// members and library locations are not implementations). A module-qualified function
/// (`kit.ServeHTTP` after `import kit`) is never asked.
#[test]
fn rule_library_abstract_call_asks_implementations_once() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let web_uri = modcache_uri(tmp.path(), "web@v1.0.0", "server.go");
    let kit_uri = modcache_uri(tmp.path(), "kit@v1.2.0", "kit.go");
    let a: &[u8] = b"func run(h Handler, g Handler) {\n    h.ServeHTTP(w, r)\n    g.ServeHTTP(w, r)\n    kit.ServeHTTP(w)\n}\n";
    let b: &[u8] = b"type Engine struct{}\nfunc (e *Engine) ServeHTTP(w, r) {}\n";
    let c: &[u8] = b"type Handler interface {\n    ServeHTTP(w, r)\n}\n";
    let run_at = find(a, "run(");
    let (h_call, g_call, kit_call) = (find(a, "h.Serve"), find(a, "g.Serve"), find(a, "kit.Serve"));
    let receiver_call = |at: u32, callee: &str, receiver: &str, line: u32| {
        let mut c = call(at, callee, Some(0), line);
        c.receiver = Some(receiver.to_string());
        c.arg_count = 2;
        c
    };
    let access = |at: u32, root: &str| trace_core::facts::MemberAccess {
        span: ByteSpan::new(at, at + "ServeHTTP".len() as u32),
        receiver_root: Some(root.to_string()),
        self_receiver: false,
    };
    let fa = with_module(
        FileFacts {
            declarations: vec![decl_at(
                a,
                "run",
                "run",
                SymbolKind::Function,
                (0, a.len() as u32 - 1),
                run_at,
            )],
            calls: vec![
                receiver_call(h_call, "h.ServeHTTP", "h", 2),
                receiver_call(g_call, "g.ServeHTTP", "g", 3),
                receiver_call(kit_call, "kit.ServeHTTP", "kit", 4),
            ],
            member_accesses: vec![
                access(h_call + 2, "h"),
                access(g_call + 2, "g"),
                access(kit_call + 4, "kit"),
            ],
            imports: vec![trace_core::facts::Import {
                local: "kit".into(),
                target: "example.com/kit".into(),
                kind: trace_core::facts::ImportKind::Module,
                scope: trace_core::facts::Scope::Module,
                span: ByteSpan::new(0, 1),
                line: 1,
            }],
            ..FileFacts::default()
        },
        a,
    );
    let serve_b = find(b, "ServeHTTP");
    let mut imp = decl_at(
        b,
        "ServeHTTP",
        "Engine.ServeHTTP",
        SymbolKind::Method,
        (serve_b - 17, b.len() as u32 - 1),
        serve_b,
    );
    imp.container = Some("Engine".into());
    let fb = with_module(
        FileFacts {
            declarations: vec![decl_at(b, "Engine", "Engine", SymbolKind::Class, (0, 20), 5), imp],
            ..FileFacts::default()
        },
        b,
    );
    let serve_c = find(c, "ServeHTTP");
    let mut abstract_member =
        decl_at(c, "ServeHTTP", "Handler.ServeHTTP", SymbolKind::Method, (serve_c, serve_c + 15), serve_c);
    abstract_member.parent = Some(0);
    abstract_member.is_stub = true;
    let fc = with_module(
        FileFacts {
            declarations: vec![
                decl_at(c, "Handler", "Handler", SymbolKind::Interface, (0, c.len() as u32 - 1), 5),
                abstract_member,
            ],
            ..FileFacts::default()
        },
        c,
    );
    let files = [
        sem_file("a.go", Language::Go, a, &fa),
        sem_file("b.go", Language::Go, b, &fb),
        sem_file("c.go", Language::Go, c, &fc),
    ];
    let decls = DeclTable::new(files.iter().map(|f| (f.path, f.source, f.facts)));
    let (h_member, g_member, kit_member) = (pos(a, h_call + 2), pos(a, g_call + 2), pos(a, kit_call + 4));
    let serve_b_range = lsp_range(b, serve_b, serve_b + 9);
    let serve_c_range = lsp_range(c, serve_c, serve_c + 9);
    let handler: Handler = Box::new(move |method, params| {
        let at = (
            params["position"]["line"].as_u64().unwrap() as u32,
            params["position"]["character"].as_u64().unwrap() as u32,
        );
        Ok(match method {
            "textDocument/definition" if at == h_member || at == g_member => {
                json!([{"uri": web_uri.clone(), "range": range(10, 5, 14)}])
            }
            "textDocument/definition" if at == kit_member => {
                json!([{"uri": kit_uri.clone(), "range": range(3, 5, 14)}])
            }
            "textDocument/implementation" => {
                assert_eq!(at, h_member, "asked at the first call of the declaration");
                json!([
                    {"uri": "file:///ws/b.go", "range": serve_b_range.clone()},
                    {"uri": "file:///ws/c.go", "range": serve_c_range.clone()},
                    {"uri": web_uri.clone(), "range": range(40, 5, 14)}
                ])
            }
            other => panic!("unexpected request {other} at {at:?}"),
        })
    });
    let caps = json!({"definitionProvider": true, "implementationProvider": true});
    let mut session = FakeSession::new(caps, handler);
    let analysis = analyze(&mut session, &[&files[0]], &decls, &FakeUris, &generic_options()).unwrap();
    assert_eq!(count(&session, "textDocument/implementation"), 1, "one request per library declaration");
    let sem = &analysis.files["a.go"];
    assert_eq!(sem.library_calls.len(), 3, "{:?}", sem.library_calls);
    let expected: Vec<SemLibraryDispatch> = [(h_call, 2u32), (g_call, 3u32)]
        .into_iter()
        .map(|(at, line)| SemLibraryDispatch {
            owner: 0,
            at: ByteSpan::new(at, at + "h.ServeHTTP".len() as u32),
            line,
            library_symbol: Some("example.com/web.ServeHTTP".into()),
            implementations: vec!["b.go:Engine.ServeHTTP".into()],
        })
        .collect();
    assert_eq!(sem.library_dispatch, expected);
    // The calls themselves stay library calls (external), never edges.
    assert!(sem.edges.is_empty());
}

/// Rule (I-17): in a language where calling a type converts a value (Go `T(x)`), a call
/// answered by a repository type is a type use (`references`), and one answered only by a
/// type declaration outside the index (`string(b)`) is resolved elsewhere, never an
/// unresolved call or a library call; a builtin function (`len(b)`) stays a library /
/// external call. Elsewhere calling a type constructs.
#[test]
fn rule_conversion_to_repository_type_is_a_type_reference() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let builtin = tmp
        .path()
        .join("goroot")
        .join("src")
        .join("builtin")
        .join("builtin.go");
    std::fs::create_dir_all(builtin.parent().expect("parent")).expect("dirs");
    std::fs::write(&builtin, "package builtin\n\ntype string string\n\nfunc len(v Type) int\n")
        .expect("write");
    let builtin_uri = crate::lsp::path_to_uri(&builtin).expect("uri");
    let src: &[u8] =
        b"type Celsius float64\nfunc run() {\n    c := Celsius(x)\n    s := string(b)\n    n := len(b)\n}\n";
    let run_at = find(src, "run(");
    let (celsius, string, len) = (find(src, "Celsius(x"), find(src, "string(b"), find(src, "len(b"));
    let facts = with_module(
        FileFacts {
            declarations: vec![
                decl_at(src, "Celsius", "Celsius", SymbolKind::Class, (0, 20), 5),
                decl_at(src, "run", "run", SymbolKind::Function, (run_at - 5, src.len() as u32 - 1), run_at),
            ],
            calls: vec![
                call(celsius, "Celsius", Some(1), 3),
                call(string, "string", Some(1), 4),
                call(len, "len", Some(1), 5),
            ],
            ..FileFacts::default()
        },
        src,
    );
    let file = sem_file("a.go", Language::Go, src, &facts);
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let (celsius_at, string_at, len_at) = (pos(src, celsius), pos(src, string), pos(src, len));
    let handler: Handler = Box::new(move |method, params| {
        assert_eq!(method, "textDocument/definition");
        let at = (
            params["position"]["line"].as_u64().unwrap() as u32,
            params["position"]["character"].as_u64().unwrap() as u32,
        );
        Ok(if at == celsius_at {
            json!([{"uri": "file:///ws/a.go", "range": range(0, 5, 12)}])
        } else if at == string_at {
            json!([{"uri": builtin_uri.clone(), "range": range(2, 5, 11)}])
        } else if at == len_at {
            json!([{"uri": builtin_uri.clone(), "range": range(4, 5, 8)}])
        } else {
            panic!("unexpected definition at {at:?}")
        })
    });
    let mut session = FakeSession::new(json!({"definitionProvider": true}), handler);
    // Every call is asked (no syntax answers), as for names a repository also declares.
    let opts = Options {
        syntax_answers: false,
        ..generic_options()
    };
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &opts).unwrap();
    let sem = &analysis.files["a.go"];
    let spans: Vec<ByteSpan> = facts.calls.iter().map(|c| c.callee_span).collect();
    let to_type: Vec<_> = sem.edges.iter().filter(|e| e.target == "a.go:Celsius").collect();
    assert_eq!(to_type.len(), 1);
    assert_eq!(to_type[0].kind, EdgeKind::References, "a conversion is a type use");
    assert_eq!(to_type[0].at, spans[0]);
    assert!(sem.resolved_elsewhere.contains(&spans[1]), "{:?}", sem.resolved_elsewhere);
    assert!(!sem.unresolved.iter().any(|u| u.at == spans[1]));
    assert!(!sem.library_calls.iter().any(|c| c.at == spans[1]));
    assert!(!sem.resolved_elsewhere.contains(&spans[2]), "len is a function");
    assert!(sem
        .unresolved
        .iter()
        .any(|u| u.at == spans[2] && u.kind == UnresolvedKind::ExternalOrAmbiguous));
    // Negative: without the conversion rule a called type is a constructor.
    let target = decls.by_uid("a.go:Celsius").expect("type");
    let edge = call_edge(&facts.calls[0], 1, target, &decls, Resolution::Definition, false);
    assert_eq!(edge.kind, EdgeKind::Constructor);
}

/// Rule (I-41): a definition location inside an indexed file that maps to no declaration
/// (a using-declaration line) does not make the answer external when another location
/// maps to a declaration named like the member: that declaration is the proven target.
/// Answered only by such a line, the call stays `external_or_ambiguous`.
#[test]
fn rule_using_declaration_location_defers_to_the_declaration() {
    let a: &[u8] = b"using fmt::runtime;\nusing fmt::other;\nvoid run() {\n  runtime(1);\n  other(2);\n}\n";
    let h: &[u8] = b"namespace fmt {\nvoid runtime(int x) {}\nvoid other(int x) {}\n}\n";
    let run_at = find(a, "run(");
    let (runtime_call, other_call) = (find(a, "runtime(1"), find(a, "other(2"));
    let fa = with_module(
        FileFacts {
            declarations: vec![decl_at(
                a,
                "run",
                "run",
                SymbolKind::Function,
                (run_at - 5, a.len() as u32 - 1),
                run_at,
            )],
            calls: vec![call(runtime_call, "runtime", Some(0), 4), call(other_call, "other", Some(0), 5)],
            ..FileFacts::default()
        },
        a,
    );
    let (rt, ot) = (find(h, "runtime"), find(h, "other"));
    let fh = with_module(
        FileFacts {
            declarations: vec![
                decl_at(h, "runtime", "fmt.runtime", SymbolKind::Function, (rt - 5, rt + 17), rt),
                decl_at(h, "other", "fmt.other", SymbolKind::Function, (ot - 5, ot + 15), ot),
            ],
            ..FileFacts::default()
        },
        h,
    );
    let files = [
        sem_file("a.cc", Language::Cpp, a, &fa),
        sem_file("fmt.h", Language::Cpp, h, &fh),
    ];
    let decls = DeclTable::new(files.iter().map(|f| (f.path, f.source, f.facts)));
    let using_runtime = lsp_range(a, find(a, "runtime;"), find(a, "runtime;") + 7);
    let using_other = lsp_range(a, find(a, "other;"), find(a, "other;") + 5);
    let runtime_decl = lsp_range(h, rt, rt + 7);
    let (runtime_at, other_at) = (pos(a, runtime_call), pos(a, other_call));
    let handler: Handler = Box::new(move |method, params| {
        assert_eq!(method, "textDocument/definition");
        let at = (
            params["position"]["line"].as_u64().unwrap() as u32,
            params["position"]["character"].as_u64().unwrap() as u32,
        );
        Ok(if at == runtime_at {
            json!([{"uri": "file:///ws/a.cc", "range": using_runtime.clone()},
                       {"uri": "file:///ws/fmt.h", "range": runtime_decl.clone()}])
        } else if at == other_at {
            json!([{"uri": "file:///ws/a.cc", "range": using_other.clone()}])
        } else {
            panic!("unexpected definition at {at:?}")
        })
    });
    let mut session = FakeSession::new(json!({"definitionProvider": true}), handler);
    let analysis = analyze(&mut session, &[&files[0]], &decls, &FakeUris, &generic_options()).unwrap();
    let sem = &analysis.files["a.cc"];
    let edge = sem
        .edges
        .iter()
        .find(|e| e.target == "fmt.h:fmt.runtime")
        .expect("proven edge");
    assert_eq!(
        (edge.kind, edge.resolution, edge.at),
        (EdgeKind::Calls, Resolution::Definition, fa.calls[0].callee_span)
    );
    assert!(!sem.unresolved.iter().any(|u| u.at == fa.calls[0].callee_span));
    assert!(!analysis.incomplete.contains(&("a.cc".to_string(), runtime_call)));
    // Negative: only the using line -> unresolved, candidates incomplete.
    let other = sem
        .unresolved
        .iter()
        .find(|u| u.at == fa.calls[1].callee_span)
        .expect("other unresolved");
    assert_eq!(other.kind, UnresolvedKind::ExternalOrAmbiguous);
    assert!(other.candidates.is_empty());
    assert!(analysis.incomplete.contains(&("a.cc".to_string(), other_call)));
}

/// Rule (I-42): a call the server leaves unanswered whose callee depends on a template
/// parameter of an enclosing template (a receiver declared `T&`, a qualified call
/// `T::make()`) is `template_dependent`; the same call through a concrete type stays
/// `no_semantic_target`.
#[test]
fn rule_call_on_template_parameter_is_template_dependent() {
    let src: &[u8] = b"struct Widget {\n  void run() {}\n  static void make() {}\n};\ntemplate <typename T>\nvoid apply(T& t) {\n  t.run();\n  T::make();\n}\nvoid plain(Widget& w) {\n  w.run();\n}\n";
    let widget_end = find(src, "};") + 2;
    let run_at = find(src, "run() {}");
    let make_at = find(src, "make() {}");
    let apply_at = find(src, "apply(");
    let apply_start = find(src, "template <");
    let apply_end = find(src, "T::make();\n}") + "T::make();\n}".len() as u32;
    let plain_at = find(src, "plain(");
    let member = |name: &str, at: u32, len: u32| {
        let mut d = decl_at(src, name, &format!("Widget.{name}"), SymbolKind::Method, (at, at + len), at);
        d.parent = Some(0);
        d
    };
    let (t_run, t_make, w_run) = (find(src, "t.run"), find(src, "T::make"), find(src, "w.run"));
    let mut make_call = call(t_make, "T::make", Some(3), 8);
    make_call.member = Some("make".into());
    let facts = with_module(
        FileFacts {
            declarations: vec![
                decl_at(src, "Widget", "Widget", SymbolKind::Class, (0, widget_end), 7),
                member("run", run_at, 8),
                member("make", make_at, 9),
                decl_at(src, "apply", "apply", SymbolKind::Function, (apply_start, apply_end), apply_at),
                decl_at(
                    src,
                    "plain",
                    "plain",
                    SymbolKind::Function,
                    (plain_at - 5, src.len() as u32 - 1),
                    plain_at,
                ),
            ],
            calls: vec![call(t_run, "t.run", Some(3), 7), make_call, call(w_run, "w.run", Some(4), 11)],
            ..FileFacts::default()
        },
        src,
    );
    let file = sem_file("t.cpp", Language::Cpp, src, &facts);
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let handler: Handler = Box::new(|method, _| {
        assert_eq!(method, "textDocument/definition");
        Ok(Value::Null)
    });
    let mut session = FakeSession::new(json!({"definitionProvider": true}), handler);
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &generic_options()).unwrap();
    assert_eq!(count(&session, "textDocument/definition"), 3);
    let sem = &analysis.files["t.cpp"];
    let kind_at = |span: ByteSpan| sem.unresolved.iter().find(|u| u.at == span).map(|u| u.kind);
    assert_eq!(kind_at(facts.calls[0].callee_span), Some(UnresolvedKind::TemplateDependent));
    assert_eq!(kind_at(facts.calls[1].callee_span), Some(UnresolvedKind::TemplateDependent));
    assert_eq!(kind_at(facts.calls[2].callee_span), Some(UnresolvedKind::NoSemanticTarget));
    assert!(sem.diagnostics.iter().any(|d| d.kind == "template_dependent"));
}

/// Rule (function-type rule): an anonymous function passed as an argument (a Java lambda
/// here) is asked like a named callback argument when its receiving call has no in-index
/// target; its answer is keyed on the anonymous function's declaration span.
#[test]
fn rule_anonymous_function_arguments_get_function_type_answers() {
    struct LambdaHooks;
    impl Server for LambdaHooks {
        fn preflight(
            &self,
            _cx: &crate::languages::SetupContext<'_>,
        ) -> Result<Prepared, trace_core::setup_error::SetupError> {
            Ok(Prepared::default())
        }
        fn fn_type_route(&self, _language: Language) -> crate::backends::fntype::FnTypeRoute {
            crate::backends::fntype::FnTypeRoute::LanguageRule
        }
    }
    let src: &[u8] = b"class A {\n  void m() {\n    go(x -> x);\n  }\n}\n";
    let m_at = find(src, "m()");
    let method_end = find(src, "  }\n}") + 3;
    let go_at = find(src, "go(");
    let lambda_at = find(src, "x -> x");
    let lambda_end = lambda_at + "x -> x".len() as u32;
    let mut method = decl_at(src, "m", "A.m", SymbolKind::Method, (m_at - 5, method_end), m_at);
    method.parent = Some(0);
    let mut lambda =
        decl_at(src, "x", "A.m.<lambda>", SymbolKind::Function, (lambda_at, lambda_end), lambda_at);
    lambda.name = "<lambda>".into();
    lambda.parent = Some(1);
    let mut go = call(go_at, "go", Some(1), 3);
    go.arg_count = 1;
    let facts = with_module(
        FileFacts {
            declarations: vec![
                decl_at(src, "A", "A", SymbolKind::Class, (0, src.len() as u32 - 1), 6),
                method,
                lambda,
            ],
            calls: vec![go],
            anonymous: vec![AnonymousScope {
                decl: 2,
                kind: AnonymousKind::Lambda,
                created_in: Some(1),
                consumer: Consumer::Argument {
                    call: 0,
                    slot: trace_core::facts::ArgSlot::Positional {
                        index: 0,
                        exact: true,
                    },
                },
                eager: None,
            }],
            ..FileFacts::default()
        },
        src,
    );
    let file = sem_file("A.java", Language::Java, src, &facts);
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let handler: Handler = Box::new(|_, _| Ok(Value::Null));
    let mut session = FakeSession::new(json!({"definitionProvider": true}), handler);
    let options = Options {
        hooks: &LambdaHooks,
        ..generic_options()
    };
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &options).unwrap();
    let sem = &analysis.files["A.java"];
    assert_eq!(sem.callback_params.len(), 1, "{:?}", sem.callback_params);
    let p = &sem.callback_params[0];
    assert_eq!(p.arg, ByteSpan::new(lambda_at, lambda_end));
    assert_eq!(p.call, facts.calls[0].callee_span);
    assert_eq!(p.verdict, trace_core::semantics::FnTypeVerdict::FunctionType);
    assert_eq!(p.route, "language_rule");
}

/// A C file with an active call of `helper` and a call of `legacy` inside `#if 0`.
fn inactive_fixture() -> (&'static [u8], FileFacts) {
    let src: &'static [u8] =
            b"void helper(void) {}\nvoid legacy(void) {}\nvoid run(void) {\n  helper();\n#if 0\n  legacy();\n#endif\n}\n";
    let run_at = find(src, "run(");
    let facts = with_module(
        FileFacts {
            declarations: vec![
                decl_at(src, "helper", "helper", SymbolKind::Function, (0, 20), 5),
                decl_at(src, "legacy", "legacy", SymbolKind::Function, (21, 41), 26),
                decl_at(src, "run", "run", SymbolKind::Function, (run_at - 5, src.len() as u32 - 1), run_at),
            ],
            calls: vec![
                call(find(src, "helper();"), "helper", Some(2), 4),
                call(find(src, "legacy();"), "legacy", Some(2), 6),
            ],
            ..FileFacts::default()
        },
        src,
    );
    (src, facts)
}

/// Rule (I-43): with a server that reports inactive preprocessor regions, a call inside
/// one is `inactive_code` and never asked; active calls are asked as usual.
#[test]
fn rule_calls_in_inactive_regions_are_inactive_code() {
    let (src, facts) = inactive_fixture();
    let file = sem_file("a.c", Language::C, src, &facts);
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let helper_at = pos(src, facts.calls[0].callee_span.start);
    let legacy_line = pos(src, facts.calls[1].callee_span.start).0;
    let handler: Handler = Box::new(move |method, params| {
        assert_eq!(method, "textDocument/definition");
        assert_eq!(params["position"]["line"].as_u64().unwrap() as u32, helper_at.0, "only the active call");
        Ok(json!([{"uri": "file:///ws/a.c", "range": range(0, 5, 11)}]))
    });
    let mut session = FakeSession::new(json!({"definitionProvider": true}), handler);
    session.notifications.push((
        "textDocument/inactiveRegions".into(),
        json!({"textDocument": {"uri": "file:///ws/a.c"}, "regions": [range(legacy_line, 0, 11)]}),
    ));
    let hooks = RuleHooks {
        inactive_regions: true,
        outside: None,
        programs: &[],
    };
    let opts = Options {
        hooks: &hooks,
        ..generic_options()
    };
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &opts).unwrap();
    assert_eq!(count(&session, "textDocument/definition"), 1);
    let sem = &analysis.files["a.c"];
    assert!(sem.edges.iter().any(|e| e.target == "a.c:helper"));
    let legacy = sem
        .unresolved
        .iter()
        .find(|u| u.at == facts.calls[1].callee_span)
        .expect("legacy");
    assert_eq!(legacy.kind, UnresolvedKind::InactiveCode);
    assert!(sem.diagnostics.iter().any(|d| d.kind == "inactive_code"));
    // Negative: a server without the capability flag asks every call.
    let handler: Handler = Box::new(|_, _| Ok(Value::Null));
    let mut session = FakeSession::new(json!({"definitionProvider": true}), handler);
    session.notifications.push((
        "textDocument/inactiveRegions".into(),
        json!({"textDocument": {"uri": "file:///ws/a.c"}, "regions": [range(legacy_line, 0, 11)]}),
    ));
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &generic_options()).unwrap();
    assert_eq!(count(&session, "textDocument/definition"), 2);
    assert!(!analysis.files["a.c"]
        .unresolved
        .iter()
        .any(|u| u.kind == UnresolvedKind::InactiveCode));
}

/// Rule (I-43 / I-52): a file the hooks say is not part of the build on this machine
/// (`outside_build_file`) is never asked; it carries the reason and its calls are unknown.
#[test]
fn rule_files_outside_the_build_are_never_asked() {
    let (src, facts) = inactive_fixture();
    let files = [
        sem_file("a.c", Language::C, src, &facts),
        sem_file("fuzz/f.c", Language::C, src, &facts),
    ];
    let decls = DeclTable::new(files.iter().map(|f| (f.path, f.source, f.facts)));
    let handler: Handler = Box::new(|method, params| {
        assert_eq!(method, "textDocument/definition");
        assert_eq!(params["textDocument"]["uri"], "file:///ws/a.c", "never the file outside the build");
        Ok(Value::Null)
    });
    let mut session = FakeSession::new(json!({"definitionProvider": true}), handler);
    let hooks = RuleHooks {
        inactive_regions: false,
        outside: Some("fuzz/f.c"),
        programs: &[],
    };
    let opts = Options {
        hooks: &hooks,
        ..generic_options()
    };
    let refs: Vec<&SemanticFile<'_>> = files.iter().collect();
    let analysis = analyze(&mut session, &refs, &decls, &FakeUris, &opts).unwrap();
    assert_eq!(count(&session, "textDocument/definition"), 2, "the two calls of a.c");
    let outside = &analysis.files["fuzz/f.c"];
    assert_eq!(outside.outside_build.as_deref(), Some("not in the compile database"));
    assert_eq!(outside.unresolved.len(), 2);
    assert!(outside
        .unresolved
        .iter()
        .all(|u| u.kind == UnresolvedKind::NoSemanticTarget));
    assert!(analysis.files["a.c"].outside_build.is_none());
}

/// A shell script calling `curl` and `nosuch` (no function of either name) and its run
/// with `programs` found on this machine.
fn run_commands(
    script: &'static [u8],
    programs: &'static [&'static str],
) -> (FileFacts, FileSemantics, usize) {
    let run_at = find(script, "run()");
    let run_end = find(script, "\n}") + 2;
    let mut declarations =
        vec![decl_at(script, "run", "run", SymbolKind::Function, (run_at, run_end), run_at)];
    if let Some(at) = script.windows(9).position(|w| w == b"curl() {\n") {
        let at = at as u32;
        declarations.push(decl_at(script, "curl", "curl", SymbolKind::Function, (at, at + 18), at));
    }
    let facts = with_module(
        FileFacts {
            declarations,
            calls: vec![
                call(find(script, "curl -s"), "curl", Some(0), 2),
                call(find(script, "nosuch"), "nosuch", Some(0), 3),
            ],
            ..FileFacts::default()
        },
        script,
    );
    let sem = {
        let file = sem_file("main.sh", Language::Bash, script, &facts);
        let decls = DeclTable::new([(file.path, file.source, file.facts)]);
        let handler: Handler = Box::new(|method, _| panic!("no request expected, got {method}"));
        let mut session = FakeSession::new(json!({"definitionProvider": true}), handler);
        let hooks = RuleHooks {
            inactive_regions: false,
            outside: None,
            programs,
        };
        let opts = Options {
            hooks: &hooks,
            ..generic_options()
        };
        let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &opts).unwrap();
        (analysis.files["main.sh"].clone(), session.requests.len())
    };
    (facts, sem.0, sem.1)
}

/// Rule (I-44): a bare command no function carries that the hooks find on this machine
/// (`external_program`) is resolved elsewhere at its name, without a request.
#[test]
fn rule_external_program_calls_are_resolved_elsewhere_without_a_request() {
    let script: &'static [u8] = b"run() {\n  curl -s x\n  nosuch y\n}\n";
    let (facts, sem, requests) = run_commands(script, &["curl"]);
    assert_eq!(requests, 0);
    let curl = facts.calls[0].callee_span;
    assert_eq!(sem.resolved_elsewhere, vec![curl]);
    assert!(!sem.unresolved.iter().any(|u| u.at == curl));
    assert!(sem.diagnostics.iter().any(|d| d.kind == "external_program"));
    let nosuch = sem
        .unresolved
        .iter()
        .find(|u| u.at == facts.calls[1].callee_span)
        .expect("nosuch");
    assert_eq!(nosuch.kind, UnresolvedKind::ExternalOrAmbiguous);
}

/// Rule (I-44, negative): a command the hooks do not find stays an unresolved external
/// command, and a script function named like a program is the target, never the program.
#[test]
fn rule_unknown_commands_stay_external_or_ambiguous() {
    let script: &'static [u8] = b"run() {\n  curl -s x\n  nosuch y\n}\n";
    let (facts, sem, _) = run_commands(script, &[]);
    assert!(sem.resolved_elsewhere.is_empty());
    for c in &facts.calls {
        let u = sem
            .unresolved
            .iter()
            .find(|u| u.at == c.callee_span)
            .expect("unresolved");
        assert_eq!(u.kind, UnresolvedKind::ExternalOrAmbiguous);
    }
    let script: &'static [u8] = b"run() {\n  curl -s x\n  nosuch y\n}\ncurl() {\n  true\n}\n";
    let (facts, sem, _) = run_commands(script, &["curl"]);
    assert!(sem.resolved_elsewhere.is_empty());
    assert!(sem
        .edges
        .iter()
        .any(|e| e.target == "main.sh:curl" && e.at == facts.calls[0].callee_span));
}

/// Rule (I-47): several in-index targets of one call are narrowed by the call's argument
/// count (`arity_accepts`); exactly one left is the proven target. Negative: a variadic
/// overload accepts the count too, so both stay candidates.
#[test]
fn rule_overloads_are_narrowed_by_argument_count() {
    use trace_core::facts::{Param, ParamKind};
    let src: &[u8] = b"class A {\n  void helper(int a) {}\n  void helper(int a, int b) {}\n  void many(int... xs) {}\n  void many(int a, int b) {}\n  void run() {\n    helper(1);\n    many(1, 2);\n  }\n}\n";
    let param = |name: &str, kind: ParamKind| Param {
        name: name.into(),
        kind,
        has_default: false,
    };
    let positional = |names: &[&str]| {
        names
            .iter()
            .map(|n| param(n, ParamKind::Positional))
            .collect::<Vec<_>>()
    };
    let method = |needle: &str, name: &str, params: Vec<Param>| {
        let at = find(src, needle);
        let end = at + find(&src[at as usize..], "}") + 1;
        let mut d = decl_at(src, name, &format!("A.{name}"), SymbolKind::Method, (at - 5, end), at);
        d.parent = Some(0);
        d.parameters = params;
        d
    };
    let run_at = find(src, "run()");
    let mut run =
        decl_at(src, "run", "A.run", SymbolKind::Method, (run_at - 5, find(src, "2);\n  }") + 7), run_at);
    run.parent = Some(0);
    let (helper_call, many_call) = (find(src, "helper(1)"), find(src, "many(1, 2)"));
    let mut c_helper = call(helper_call, "helper", Some(5), 7);
    c_helper.arg_count = 1;
    let mut c_many = call(many_call, "many", Some(5), 8);
    c_many.arg_count = 2;
    let facts = with_module(
        FileFacts {
            declarations: vec![
                decl_at(src, "A", "A", SymbolKind::Class, (0, src.len() as u32 - 1), 6),
                method("helper(int a) ", "helper", positional(&["a"])),
                method("helper(int a, ", "helper", positional(&["a", "b"])),
                method("many(int...", "many", vec![param("xs", ParamKind::VarPositional)]),
                method("many(int a", "many", positional(&["a", "b"])),
                run,
            ],
            calls: vec![c_helper, c_many],
            ..FileFacts::default()
        },
        src,
    );
    let file = sem_file("A.java", Language::Java, src, &facts);
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let name_range = |needle: &str, len: u32| {
        let at = find(src, needle);
        json!({"uri": "file:///ws/A.java", "range": lsp_range(src, at, at + len)})
    };
    let helpers = json!([name_range("helper(int a) ", 6), name_range("helper(int a, ", 6)]);
    let manys = json!([name_range("many(int...", 4), name_range("many(int a", 4)]);
    let helper_line = pos(src, helper_call).0;
    let handler: Handler = Box::new(move |method, params| {
        assert_eq!(method, "textDocument/definition");
        Ok(if params["position"]["line"].as_u64().unwrap() as u32 == helper_line {
            helpers.clone()
        } else {
            manys.clone()
        })
    });
    let mut session = FakeSession::new(json!({"definitionProvider": true}), handler);
    let analysis = analyze(&mut session, &[&file], &decls, &FakeUris, &generic_options()).unwrap();
    let sem = &analysis.files["A.java"];
    let edge = sem
        .edges
        .iter()
        .find(|e| e.at == facts.calls[0].callee_span)
        .expect("helper edge");
    assert_eq!((edge.target.as_str(), edge.resolution), ("A.java:A.helper", Resolution::Definition));
    assert!(sem.diagnostics.iter().any(|d| d.kind == "arity_narrowed"));
    let many = sem
        .unresolved
        .iter()
        .find(|u| u.at == facts.calls[1].callee_span)
        .expect("many ambiguous");
    assert_eq!(many.candidates, vec!["A.java:A.many".to_string(), "A.java:A.many#2".to_string()]);
}
