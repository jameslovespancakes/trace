use super::conventions::*;
use super::literals::*;
use super::*;
use crate::SourceInput;
use trace_core::facts::BoundaryRole;
use trace_core::model::BridgeKind;

fn facts(path: &str, language: Language, src: &str) -> Vec<BoundaryFact> {
    crate::extract(SourceInput {
        path,
        language,
        source: src.as_bytes(),
    })
    .expect("extract")
    .boundaries
}

fn names(facts: &[BoundaryFact], kind: BridgeKind, role: BoundaryRole) -> Vec<String> {
    let mut v: Vec<String> = facts
        .iter()
        .filter(|f| f.kind == kind && f.role == role)
        .map(|f| f.name.clone())
        .collect();
    v.sort();
    v
}

fn detail<'a>(f: &'a BoundaryFact, key: &str) -> Option<&'a str> {
    f.detail.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

#[test]
fn rule_syntax_convention_rows_load_for_their_languages() {
    for (key, _) in crate::testing::TABLE_FILES {
        for row in convention_rows().get(key).into_iter().flatten() {
            assert!(row.bridge.is_some(), "{key} {}: bridge kind", row.rule);
            assert!(row.symbol.is_some() || row.pattern.is_some(), "{key} {}: anchor", row.rule);
        }
    }
    let rust = conventions(Language::Rust);
    assert!(rust
        .iter()
        .any(|r| r.rule == "export_function_attribute" && r.bridge == Some(BridgeKind::Pyo3)));
    // Java reads its own rows, C++ the C rows.
    assert!(conventions(Language::Java)
        .iter()
        .any(|r| r.rule == "rpc_stub_factory"));
    assert!(conventions(Language::Cpp)
        .iter()
        .any(|r| r.rule == "registration_call"));
    assert!(conventions(Language::Cpp)
        .iter()
        .any(|r| r.rule == "addon_export_member"));
    assert!(conventions(Language::C)
        .iter()
        .all(|r| r.rule != "addon_export_member"));
    assert!(conventions(Language::Bash).is_empty());
}

#[test]
fn rule_convention_patterns_name_the_generated_service() {
    assert_eq!(placeholder_in("<Service>Stub", "<Service>", "GreeterStub").as_deref(), Some("Greeter"));
    assert_eq!(
        placeholder_in("New<Service>Client", "<Service>", "NewGreeterClient").as_deref(),
        Some("Greeter")
    );
    assert_eq!(placeholder_in("<Service>Stub", "<Service>", "Stub"), None, "never empty");
    assert_eq!(placeholder_in("resolve_<field>", "<field>", "resolve_user").as_deref(), Some("user"));
    assert_eq!(placeholder_in("resolve_<field>", "<field>", "helper"), None);
    assert_eq!(last_segment("Napi::Object::Set"), "Set");
    assert_eq!(last_segment("graphql-tag.gql"), "gql");
    assert_eq!(last_segment("name"), "name");
    assert_eq!(symbol_segments("Napi::Function::New"), vec!["Napi", "Function", "New"]);
    assert!(symbol_written("lib.type", &["lib".to_string(), "type".to_string()]));
    assert!(symbol_written("lib.type", &["type".to_string()]), "imported by name");
    assert!(!symbol_written("lib.type", &["other".to_string(), "type".to_string()]));
}

#[test]
fn rule_jni_names_follow_the_jni_mangling_spec() {
    assert_eq!(jni_mangle(&["com", "ex"], &["Foo"], "bar"), "Java_com_ex_Foo_bar");
    assert_eq!(jni_mangle(&["com", "ex"], &["Foo", "In"], "bar_x"), "Java_com_ex_Foo_00024In_bar_1x");
    assert_eq!(jni_mangle(&[], &["\u{dc}n\u{ef}"], "m"), "Java__000dcn_000ef_m");
    assert_eq!(camel_case("get_user_name"), "getUserName");
    assert_eq!(camel_case("_private_x"), "_privateX");
}

#[test]
fn rule_rust_abi_and_binding_attribute_rows_give_exports() {
    let src = r#"
#[no_mangle]
pub extern "C" fn add(a: i32, b: i32) -> i32 { a + b }
extern "C" { fn compress(x: u8) -> i32; }
#[pyfunction]
#[pyo3(name = "sum_as")]
fn sum(a: usize) -> usize { a }
#[pyclass(name = "Counter")]
struct RawCounter { n: u32 }
#[pymethods]
impl RawCounter {
    #[new]
    fn py_new() -> Self { RawCounter { n: 0 } }
    fn bump(&mut self) {}
}
#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(sum, m)?)?;
    m.add_class::<RawCounter>()?;
    Ok(())
}
#[wasm_bindgen(js_name = doIt)]
pub fn do_it() {}
#[napi]
pub fn get_user_name() {}
"#;
    let f = facts("src/lib.rs", Language::Rust, src);
    assert_eq!(names(&f, BridgeKind::CAbi, BoundaryRole::Provides), vec!["add"]);
    assert_eq!(names(&f, BridgeKind::CAbi, BoundaryRole::Uses), vec!["compress"]);
    let py = names(&f, BridgeKind::Pyo3, BoundaryRole::Provides);
    for want in ["sum_as", "Counter", "RawCounter.__new__", "RawCounter.bump", "_core"] {
        assert!(py.contains(&want.to_string()), "{want} in {py:?}");
    }
    let module = f.iter().find(|x| x.name == "_core").unwrap();
    assert_eq!(detail(module, "registers"), Some("RawCounter,sum"));
    assert_eq!(detail(module, "module_init"), Some("true"));
    let new = f.iter().find(|x| x.name == "RawCounter.__new__").unwrap();
    assert_eq!(detail(new, "constructor"), Some("true"));
    assert_eq!(detail(new, "impl_type"), Some("RawCounter"));
    assert_eq!(names(&f, BridgeKind::WasmBindgen, BoundaryRole::Provides), vec!["doIt"]);
    assert_eq!(names(&f, BridgeKind::Napi, BoundaryRole::Provides), vec!["getUserName"]);
    assert!(f
        .iter()
        .filter(|x| x.kind == BridgeKind::CAbi)
        .all(|x| x.decl.is_some()));
}

#[test]
fn rule_binding_members_follow_their_row_export_policy() {
    let src = r#"
#[wasm_bindgen]
pub struct Universe { w: u32 }
#[wasm_bindgen]
impl Universe {
    #[wasm_bindgen(constructor)]
    pub fn create() -> Universe { Universe { w: 0 } }
    pub fn tick(&mut self) {}
    fn private_helper(&self) {}
}
#[wasm_bindgen]
pub enum Cell { Dead = 0, Alive = 1 }
#[napi]
pub struct Engine {}
#[napi]
impl Engine {
    #[napi]
    pub fn start_now(&self) {}
    pub fn not_marked(&self) {}
}
#[wasm_bindgen]
extern "C" { fn alert(s: &str); }
"#;
    let f = facts("src/lib.rs", Language::Rust, src);
    let wasm = names(&f, BridgeKind::WasmBindgen, BoundaryRole::Provides);
    for want in ["Universe", "Universe.tick", "Cell", "Cell.Alive", "Cell.Dead"] {
        assert!(wasm.contains(&want.to_string()), "{want} in {wasm:?}");
    }
    // The constructor flag makes the member the class itself; private members are not
    // exported under the public-members policy.
    assert_eq!(wasm.iter().filter(|n| *n == "Universe").count(), 2, "{wasm:?}");
    assert!(
        !wasm
            .iter()
            .any(|n| n.contains("private_helper") || n.contains("create")),
        "{wasm:?}"
    );
    let napi = names(&f, BridgeKind::Napi, BoundaryRole::Provides);
    assert_eq!(napi, vec!["Engine", "Engine.startNow"], "only marked members, camelCase");
    // An extern block of a binding imports from JavaScript: no C ABI use.
    assert!(names(&f, BridgeKind::CAbi, BoundaryRole::Uses).is_empty());
}

#[test]
fn rule_c_exports_jni_and_cpython_method_tables() {
    let src = r#"
#include <Python.h>
static int helper(int x) { return x; }
int exported(int x) { return helper(x); }
int prototype_only(int);
JNIEXPORT jint JNICALL Java_com_ex_Foo_bar(JNIEnv *env, jobject o) { return 0; }
static PyObject *spam_system(PyObject *self, PyObject *args) { return NULL; }
static PyMethodDef SpamMethods[] = {
    {"system", spam_system, METH_VARARGS, "Execute a shell command."},
    {NULL, NULL, 0, NULL}
};
static struct PyModuleDef spammodule = { PyModuleDef_HEAD_INIT, "spam", NULL, -1, SpamMethods };
"#;
    let f = facts("native/spam.c", Language::C, src);
    assert_eq!(names(&f, BridgeKind::CAbi, BoundaryRole::Provides), vec!["exported"]);
    assert_eq!(names(&f, BridgeKind::CAbi, BoundaryRole::Uses), vec!["prototype_only"]);
    assert_eq!(names(&f, BridgeKind::Jni, BoundaryRole::Provides), vec!["Java_com_ex_Foo_bar"]);
    let cpy: Vec<&BoundaryFact> = f.iter().filter(|x| x.kind == BridgeKind::Cpython).collect();
    assert_eq!(cpy.len(), 1);
    assert_eq!(cpy[0].name, "system");
    assert_eq!(detail(cpy[0], "module"), Some("spam"));
    assert!(cpy[0].decl.is_some(), "table row points at spam_system");
}

#[test]
fn rule_cpp_exports_need_c_linkage() {
    let src = "int mangled(int x) { return x; }\nextern \"C\" { int plain(int x) { return x; } }\n";
    let f = facts("x.cpp", Language::Cpp, src);
    assert_eq!(names(&f, BridgeKind::CAbi, BoundaryRole::Provides), vec!["plain"]);
}

#[test]
fn rule_node_api_registrations_and_addon_loads() {
    let c = r#"
#include <node_api.h>
static napi_value Hello(napi_env env, napi_callback_info info) { return NULL; }
static napi_value Bye(napi_env env, napi_callback_info info) { return NULL; }
static napi_value Init(napi_env env, napi_value exports) {
    napi_value fn;
    napi_create_function(env, "hello", NAPI_AUTO_LENGTH, Hello, NULL, &fn);
    napi_property_descriptor desc = DECLARE_NAPI_METHOD("bye", Bye);
    return exports;
}
"#;
    let f = facts("addon/native.c", Language::C, c);
    assert_eq!(names(&f, BridgeKind::Napi, BoundaryRole::Provides), vec!["bye", "hello"]);
    assert!(f
        .iter()
        .filter(|x| x.kind == BridgeKind::Napi)
        .all(|x| x.decl.is_some()));
    let js = r#"
const addon = require("./build/addon.node");
const native = require("bindings")("native");
function main() {
  addon.sumValues(1, 2);
  native.run();
  other.run();
}
"#;
    let f = facts("js/index.js", Language::JavaScript, js);
    assert_eq!(names(&f, BridgeKind::Napi, BoundaryRole::Uses), vec!["run", "sumValues"]);
    let run = f.iter().find(|x| x.name == "run").unwrap();
    assert_eq!(detail(run, "module"), Some("native"));
}

#[test]
fn rule_java_native_methods_use_jni_names() {
    let src = r#"
package com.ex;
class Foo {
  static class In { native int bar_x(String s); }
  public int plain() { return 0; }
}
"#;
    let f = facts("src/com/ex/Foo.java", Language::Java, src);
    assert_eq!(names(&f, BridgeKind::Jni, BoundaryRole::Uses), vec!["Java_com_ex_Foo_00024In_bar_1x"]);
    assert!(names(&f, BridgeKind::Http, BoundaryRole::Provides).is_empty());
}

/// cgo `//export name` (directly above the function, possibly inside a comment block)
/// provides the C symbol `name`; `// export x` (a space after `//`) is not the directive.
#[test]
fn rule_cgo_export_directive_provides_the_c_symbol() {
    let src = "package main\n\nimport \"C\"\n\n// waitForUnlock blocks.\n//export wait_for_unlock\nfunc waitForUnlock() C.int { return 0 }\n\n// export not_exported\nfunc notExported() {}\n\n//export far_away\n\nfunc detached() {}\n\nfunc register(db *C.sqlite3) {\n\tvar n C.int = C.SQLITE_OK\n\tC.sqlite3_unlock_notify(db, (*[0]byte)(C.wait_for_unlock), nil)\n\t_ = C.free\n\t_ = n\n}\n";
    let f = facts("notify.go", Language::Go, src);
    assert_eq!(names(&f, BridgeKind::CAbi, BoundaryRole::Provides), vec!["wait_for_unlock"]);
    assert!(f
        .iter()
        .filter(|x| x.kind == BridgeKind::CAbi)
        .all(|x| x.decl.is_some()));
    // Calls and value uses of `C.name`; types (`*C.sqlite3`, `C.int`) and pseudo-functions
    // are not uses. Constants are uses that only match a C function of that name.
    let mut uses = names(&f, BridgeKind::Cgo, BoundaryRole::Uses);
    uses.sort();
    assert_eq!(uses, vec!["SQLITE_OK", "sqlite3_unlock_notify", "wait_for_unlock"]);
    assert!(f
        .iter()
        .filter(|x| x.kind == BridgeKind::Cgo && x.role == BoundaryRole::Uses)
        .all(|x| x.owner.is_some()));
}

#[test]
fn rule_generated_rpc_names_come_from_rows() {
    let src = r#"package main

// #include "foo.h"
// int add(int a, int b) { return a + b; }
import "C"

type server struct {
	pb.UnimplementedGreeterServer
}

func main() {
	C.add(1, 2)
	s := grpc.NewServer()
	pb.RegisterGreeterServer(s, &server{})
	c := pb.NewGreeterClient(conn)
	c.SayHello(ctx, req)
	r := lib.Group("/v1")
	r.GET("/users/:id", getUser)
}
"#;
    let f = facts("main.go", Language::Go, src);
    assert_eq!(names(&f, BridgeKind::Cgo, BoundaryRole::Uses), vec!["add"]);
    assert_eq!(names(&f, BridgeKind::Cgo, BoundaryRole::Provides), vec!["add"]);
    let grpc_p = names(&f, BridgeKind::Grpc, BoundaryRole::Provides);
    assert_eq!(grpc_p, vec!["Greeter/*".to_string(), "Greeter/*".to_string()]);
    assert_eq!(names(&f, BridgeKind::Grpc, BoundaryRole::Uses), vec!["Greeter/SayHello"]);
    // Routes are derived channel effects, never syntax facts.
    assert!(names(&f, BridgeKind::Http, BoundaryRole::Provides).is_empty());

    let py = r#"
import helloworld_pb2_grpc

class Greeter(helloworld_pb2_grpc.GreeterServicer):
    def SayHello(self, request, context):
        return None

def run():
    stub = helloworld_pb2_grpc.GreeterStub(channel)
    stub.SayHello(req)
"#;
    let f = facts("svc/server.py", Language::Python, py);
    assert_eq!(names(&f, BridgeKind::Grpc, BoundaryRole::Provides), vec!["Greeter/*"]);
    assert_eq!(names(&f, BridgeKind::Grpc, BoundaryRole::Uses), vec!["Greeter/SayHello"]);

    let js = r#"
function start(server) {
  server.addService(proto.Greeter.service, { sayHello: sayHelloImpl });
}
function sayHelloImpl(call, cb) { cb(null, {}); }
function client() {
  const c = new GreeterClient("localhost:50051", grpc.credentials.createInsecure());
  c.sayHello({ name: "x" }, () => {});
}
"#;
    let f = facts("svc/server.js", Language::JavaScript, js);
    assert_eq!(names(&f, BridgeKind::Grpc, BoundaryRole::Provides), vec!["Greeter/sayHello"]);
    assert_eq!(names(&f, BridgeKind::Grpc, BoundaryRole::Uses), vec!["Greeter/sayHello"]);
}

#[test]
fn rule_graphql_documents_and_resolver_maps() {
    let src = r#"
const Q = gql`
  query GetUser($id: ID!) { user(id: $id) { name ...F } me: viewer { id } }
  fragment F on User { email }
  mutation { createUser(name: "x") { id } }
`;
const resolvers = { Query: { user: (p, a) => db.get(a.id), viewer() { return null; } } };
const config = { Settings: { user: (p) => p } };
"#;
    let f = facts("web/q.js", Language::JavaScript, src);
    let uses = names(&f, BridgeKind::Graphql, BoundaryRole::Uses);
    assert_eq!(uses, vec!["Mutation.createUser", "Query.user", "Query.viewer"]);
    let provides = names(&f, BridgeKind::Graphql, BoundaryRole::Provides);
    assert_eq!(provides, vec!["Query.user", "Query.viewer"], "only root types of the specification");
}

#[test]
fn rule_graphql_python_resolver_conventions_come_from_rows() {
    let src = r#"
import strawberry
import graphene
from ariadne import QueryType

@strawberry.type
class Query:
    @strawberry.field
    def user_name(self) -> str:
        return ""

    def helper(self) -> int:
        return 1

class Mutation(graphene.ObjectType):
    def resolve_create_user(self, info):
        return None

    def other(self):
        return None

query = QueryType()

@query.field("hello")
def resolve_hello(_, info):
    return "hi"
"#;
    let f = facts("server/schema.py", Language::Python, src);
    let provides = names(&f, BridgeKind::Graphql, BoundaryRole::Provides);
    assert_eq!(provides, vec!["Mutation.createUser", "Query.hello", "Query.userName"]);
    assert!(f.iter().all(|x| detail(x, "framework").is_none()), "no package names in facts");
}

#[test]
fn rule_graphql_field_annotations_come_from_rows() {
    let src = r#"
class Resolvers {
  @QueryMapping public String greeting() { return ""; }
  @SchemaMapping(typeName = "Book", field = "author") public Author author(Book b) { return null; }
  @MutationMapping(name = "addBook") public Book add() { return null; }
  @Deprecated public Book plain() { return null; }
}
"#;
    let f = facts("src/Resolvers.java", Language::Java, src);
    let provides = names(&f, BridgeKind::Graphql, BoundaryRole::Provides);
    assert_eq!(provides, vec!["Book.author", "Mutation.addBook", "Query.greeting"]);
}

#[test]
fn rule_routes_clients_messages_and_processes_are_not_syntax_facts() {
    // Registration-, client-, message- and process-shaped calls: all derived channel
    // effects now (trace-bridge), so syntax gives none of them.
    let src = r#"
import subprocess
app = make_app()

@app.get("/users/{user_id}")
def read_user(user_id: int):
    return http.get("http://api.example.com/v1/profiles")

def run():
    subprocess.run(["node", "scripts/build.js"])
    bus.publish("orders", "x")
    lib = loader.load("./libfoo.so")
    lib.compress(1)
"#;
    let f = facts("app/main.py", Language::Python, src);
    assert!(f.is_empty(), "{f:?}");
    let js = r#"
const app = makeApp();
app.get("/users/:id", (req, res) => res.send("x"));
export async function load(id) { await fetch(`/api/users/${id}`, { method: "DELETE" }); }
"#;
    let f = facts("web/app.ts", Language::TypeScript, js);
    assert!(f.is_empty(), "{f:?}");
}

#[test]
fn rule_python_path_constants_name_mount_prefixes() {
    let src = r#"
API_V1 = "/api/v1"
TITLE = "service"

class Settings:
    PREFIX = "/items"

settings = Settings()
"#;
    let f = facts("app/core/config.py", Language::Python, src);
    let provides = names(&f, BridgeKind::Http, BoundaryRole::Provides);
    assert_eq!(provides, vec!["CONST API_V1", "CONST Settings.PREFIX", "INSTANCE settings"]);
    let c = f.iter().find(|x| x.name == "CONST API_V1").unwrap();
    assert_eq!(detail(c, "value"), Some("/api/v1"));
}

#[test]
fn negative_controls_emit_nothing() {
    // Same names without any boundary: plain functions, map lookups, dynamic calls.
    let src = r#"
def add(a, b):
    return a + b

def get(d):
    return d.get("/users", None)
"#;
    let f = facts("plain.py", Language::Python, src);
    assert!(f.is_empty(), "{f:?}");
    let f = facts("plain.c", Language::C, "static int add(int a) { return a; }\n");
    assert!(f.is_empty(), "{f:?}");
}
