use super::*;
use crate::{extract, SourceInput};
use trace_core::facts::{Activation, ArgSlot};
use trace_core::facts::{Expr, FlowFact};

fn facts(path: &str, language: Language, src: &str) -> FileFacts {
    let f = extract(SourceInput {
        path,
        language,
        source: src.as_bytes(),
    })
    .expect("extract");
    crate::test_support::strip_module(f, src.len())
}

fn names(f: &FileFacts) -> Vec<&str> {
    f.declarations.iter().map(|d| d.qualified_name.as_str()).collect()
}

#[test]
fn javascript_declarations_and_calls() {
    let src = "/** Adds. */\nexport function add(a, b = 1) { return helper(a) + b; }\n\
               const arrow = async (x) => { await fetchIt(x); };\n\
               class Box extends Base {\n  constructor(v) { this.v = v; }\n  *items() { yield* this.v; }\n  get() { return this.store.load(); }\n}\n\
               obj.handler = function () { run(); };\n\
               setTimeout(() => tick(), 10);\n";
    let f = facts("src/a.js", Language::JavaScript, src);
    // Callables bound by `const f = ...` / `obj.f = ...` stay named (a property-assigned
    // function is declared for its receiver path, extractor 10: `obj.handler`); the
    // anonymous `setTimeout` callback is a synthetic `<lambda>` (extractor 5).
    assert_eq!(
        names(&f),
        vec![
            "add",
            "arrow",
            "Box",
            "Box.constructor",
            "Box.items",
            "Box.get",
            "obj.handler",
            "<lambda>"
        ]
    );
    assert_eq!(f.declarations[6].container.as_deref(), Some("obj"));
    let add = &f.declarations[0];
    assert_eq!(add.kind, SymbolKind::Function);
    assert!(add.span.bytes.start < add.name_span.start);
    assert_eq!(&src[add.span.bytes.range()][..6], "export");
    assert_eq!(add.doc.as_deref(), Some("/** Adds. */"));
    assert_eq!(add.parameters.len(), 2);
    assert!(add.parameters[1].has_default);
    assert_eq!(f.declarations[1].execution, ExecutionModel::Coroutine);
    assert_eq!(f.declarations[3].kind, SymbolKind::Constructor);
    assert_eq!(f.declarations[4].execution, ExecutionModel::Generator);
    assert_eq!(f.declarations[2].bases, vec!["Base".to_string()]);

    let helper = f.calls.iter().find(|c| c.callee == "helper").unwrap();
    assert_eq!(helper.owner, Some(0));
    let fetch = f.calls.iter().find(|c| c.callee == "fetchIt").unwrap();
    assert_eq!(fetch.owner, Some(1));
    assert_eq!(fetch.activation, Activation::Await);
    let load = f.calls.iter().find(|c| c.callee == "this.store.load").unwrap();
    assert_eq!(load.member.as_deref(), Some("load"));
    assert_eq!(load.receiver.as_deref(), Some("store"));
    assert_eq!(load.owner, Some(5));
    let run = f.calls.iter().find(|c| c.callee == "run").unwrap();
    assert_eq!(run.owner, Some(6));
    // Anonymous callbacks are synthetic `<lambda>` scopes owning their body.
    let tick = f.calls.iter().find(|c| c.callee == "tick").unwrap();
    assert_eq!(tick.owner, Some(7));
    let timeout_index = f.calls.iter().position(|c| c.callee == "setTimeout").unwrap() as u32;
    let timeout = &f.calls[timeout_index as usize];
    assert_eq!(timeout.owner, None);
    assert_eq!(timeout.arg_count, 2);
    let lambda = f.anonymous_of(7).expect("synthetic callback");
    assert_eq!(lambda.created_in, None);
    assert_eq!(
        lambda.consumer,
        trace_core::facts::Consumer::Argument {
            call: timeout_index,
            slot: ArgSlot::Positional {
                index: 0,
                exact: true
            }
        }
    );
    assert_eq!(f.declarations[7].parent, None);
}

#[test]
fn typescript_interfaces_signatures_and_overloads() {
    let src = "interface Shape { area(): number; }\n\
               abstract class Base { abstract run(): void; }\n\
               function pick(a: string): string;\nfunction pick(a: number): number;\nfunction pick(a: any) { return a; }\n";
    let f = facts("src/s.ts", Language::TypeScript, src);
    // Overload signatures are declarations of their own (`is_stub`, extractor 10); the
    // family rule links them to the implementation.
    assert_eq!(names(&f), vec!["Shape", "Shape.area", "Base", "Base.run", "pick", "pick", "pick"]);
    assert_eq!(f.declarations[0].kind, SymbolKind::Interface);
    assert!(f.declarations[1].is_stub);
    assert!(f.declarations[3].is_stub);
    assert!(f.declarations[4].is_stub && f.declarations[5].is_stub);
    let pick = &f.declarations[6];
    assert!(!pick.is_stub);
    assert!(pick.declaration_lines.is_empty());
}

#[test]
fn rust_impl_methods_have_containers() {
    let src = "/// Server.\n#[derive(Debug)]\npub struct Server;\n\nimpl Display for Server {}\n\n\
               impl Server {\n    pub async fn run(&self, port: u16) { self.bind(port).await; helper(); }\n}\n\n\
               #[test]\nfn works() { Server::new(); }\n";
    let f = facts("src/lib.rs", Language::Rust, src);
    assert_eq!(names(&f), vec!["Server", "Server.run", "works"]);
    let run = &f.declarations[1];
    assert_eq!(run.kind, SymbolKind::Method);
    assert_eq!(run.container.as_deref(), Some("Server"));
    assert_eq!(run.execution, ExecutionModel::Coroutine);
    assert_eq!(run.parameters[0].name, "self");
    assert_eq!(f.declarations[0].doc.as_deref(), Some("/// Server."));
    assert_eq!(f.declarations[0].decorators, vec!["derive(Debug)".to_string()]);
    assert!(f.declarations[2].is_test);
    let bind = f.calls.iter().find(|c| c.member.as_deref() == Some("bind")).unwrap();
    assert_eq!(bind.activation, Activation::Await);
    assert_eq!(bind.owner, Some(1));
    let new = f.calls.iter().find(|c| c.callee == "Server::new").unwrap();
    assert_eq!(new.receiver.as_deref(), Some("Server"));
    assert_eq!(f.impls.len(), 1);
    assert_eq!(f.impls[0].trait_name, "Display");
}

#[test]
fn go_receivers_and_java_methods() {
    let src =
        "package main\n\ntype Server struct{}\n\nfunc (s *Server) Run() { s.listen(); fmt.Println(\"x\") }\n";
    let f = facts("main.go", Language::Go, src);
    assert_eq!(names(&f), vec!["Server", "Server.Run"]);
    assert_eq!(f.declarations[1].container.as_deref(), Some("Server"));
    let listen = f
        .calls
        .iter()
        .find(|c| c.member.as_deref() == Some("listen"))
        .unwrap();
    assert_eq!(listen.owner, Some(1));

    let java = "class A extends B implements C {\n  A() { init(); }\n  @Override\n  void m(int x) { this.store.save(x); }\n}\n";
    let f = facts("A.java", Language::Java, java);
    assert_eq!(names(&f), vec!["A", "A.A", "A.m"]);
    assert_eq!(f.declarations[1].kind, SymbolKind::Constructor);
    assert_eq!(f.declarations[2].decorators, vec!["Override".to_string()]);
    let save = f.calls.iter().find(|c| c.member.as_deref() == Some("save")).unwrap();
    assert_eq!(save.callee, "this.store.save");
    assert_eq!(save.receiver.as_deref(), Some("store"));
    assert_eq!(save.owner, Some(2));
}

/// Bash: the names of `unset -f a b` (a line-continued list, nvm.sh) / `declare -F c` /
/// `export -f d` are read references (the grammar's `unset_command` /
/// `declaration_command` name operands), so completeness lists them.
#[test]
fn bash_function_name_words_are_references() {
    let src = "f() {\n  unset -f nvm_download nvm_ls \\\n    nvm_use\n  unset PATTERN\n}\ndeclare -F helper\nexport -f other\n";
    let f = facts("nvm.sh", Language::Bash, src);
    let refs: Vec<(&str, Option<u32>)> = f
        .references
        .iter()
        .filter(|r| r.kind == trace_core::facts::RefKind::Read)
        .map(|r| (r.name.as_str(), r.owner))
        .collect();
    for (name, owner) in [
        ("nvm_download", Some(0)),
        ("nvm_ls", Some(0)),
        ("nvm_use", Some(0)),
        ("helper", None),
        ("other", None),
    ] {
        assert!(refs.contains(&(name, owner)), "{name}: {refs:?}");
    }
}

/// `new K(..)` lowers its type name (Java `type_identifier`, generics dropped; PHP
/// `qualified_name` without the leading separator) so allocation receivers are typed.
#[test]
fn allocations_lower_their_type_name() {
    let alloc = |f: &FileFacts, var: &str| -> Option<String> {
        f.flow.iter().find_map(|fact| match fact {
            FlowFact::Bind {
                target: trace_core::facts::BindTarget::Var { name, .. },
                value: Expr::Call {
                    func, is_new: true, ..
                },
                ..
            } if name.trim_start_matches('$') == var => match func.as_ref() {
                Expr::Name { name, .. } => Some(name.clone()),
                _ => None,
            },
            _ => None,
        })
    };
    let java = "class T {\n  void t() {\n    JsonReader reader = new JsonReader(r);\n    List<String> xs = new ArrayList<String>();\n    reader.getStrictness();\n  }\n}\n";
    let f = facts("T.java", Language::Java, java);
    assert_eq!(alloc(&f, "reader").as_deref(), Some("JsonReader"), "{:?}", f.flow);
    assert_eq!(alloc(&f, "xs").as_deref(), Some("ArrayList"));
    let php = "<?php\nclass T {\n  function t() {\n    $jar = new \\GuzzleHttp\\Cookie\\CookieJar();\n    return $jar->toArray();\n  }\n}\n";
    let f = facts("T.php", Language::Php, php);
    assert_eq!(alloc(&f, "jar").as_deref(), Some("GuzzleHttp\\Cookie\\CookieJar"), "{:?}", f.flow);
}

#[test]
fn c_prototypes_are_stubs() {
    let src = "int add(int a, int b);\nint add(int a, int b) { return a + b; }\nstatic char *name(void) { return helper(); }\n";
    let f = facts("m.c", Language::C, src);
    assert_eq!(names(&f), vec!["add", "add", "name"]);
    assert!(f.declarations[0].is_stub);
    assert!(!f.declarations[1].is_stub);
    assert_eq!(f.declarations[1].parameters.len(), 2);
    let helper = f.calls.iter().find(|c| c.callee == "helper").unwrap();
    assert_eq!(helper.owner, Some(2));
}

#[test]
fn javascript_test_blocks_in_test_files() {
    let src = "describe('box', () => {\n  it('opens', () => { open(box); });\n  test(\"closes\", () => close());\n});\n";
    let f = facts("src/box.test.js", Language::JavaScript, src);
    let names: Vec<&str> = f.tests.iter().map(|t| t.name.as_str()).collect();
    // Suite registrations are source-retrievable candidates as well as leaf cases.
    assert_eq!(names, vec!["opens", "closes", "box"]);
    assert!(f.tests[0].mentions.contains(&"open".to_string()));
    assert!(f.tests[0].mentions.contains(&"box".to_string()));
    let f = facts("src/box.js", Language::JavaScript, src);
    assert!(f.tests.is_empty());
}

#[test]
fn crlf_and_bom_spans_are_exact() {
    let src = "\u{feff}def f():\r\n    return g()\r\n";
    let f = facts("m.py", Language::Python, src);
    let d = &f.declarations[0];
    assert_eq!(&src.as_bytes()[d.name_span.range()], b"f");
    let call = &f.calls[0];
    assert_eq!(&src.as_bytes()[call.callee_span.range()], b"g");
    assert_eq!(call.line, 2);
}

#[test]
fn syntax_errors_still_yield_facts() {
    let f = facts("m.py", Language::Python, "def ok():\n    run()\n\ndef broken(:\n");
    assert!(f.error_count > 0);
    assert!(f.declarations.iter().any(|d| d.name == "ok"));
}
