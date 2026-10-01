//! Cross-language boundary facts from syntax trees (NEXT.md P2b, SPEC §6.3 "Boundaries"),
//! and the lazy argument evaluation trace-bridge uses for derived channel effects.
//!
//! Called once per file at the end of `extract::extract_file`, after every other fact
//! (including the synthetic `<module>` declaration) exists, so `facts.declarations`,
//! `facts.calls`, `facts.call_details`, `facts.imports` and `facts.module_decl` can be
//! consulted.
//!
//! Emits [`trace_core::facts::BoundaryFact`]s into `facts.boundaries` (sorted by span start,
//! then kind, role, name). Tree-sitter nodes and already-extracted facts only: no regular
//! expressions over source text. Literal strings are read from string nodes. Embedded C (cgo
//! preambles) is parsed with the C grammar, never scanned textually.
//!
//! trace knows no package by name here (PLAN decision 14): HTTP routes and clients, message
//! topics, processes and FFI lookups are derived channel effects (trace-bridge); what this
//! module reads is language / ABI / protocol structure (C symbols, JNI names, cgo, the
//! CPython C-API and Node-API tables, GraphQL documents) plus the generic rules that the
//! `syntax_conventions` rows of the embedded library tables feed with every package-specific
//! name (binding attributes of procedural macros, names of generated RPC code, GraphQL
//! resolver conventions, addon loaders); every row says why it cannot be derived.
//!
//! Keys (`BoundaryFact::name`, matched by `trace-bridge`):
//! * `c_abi` — C symbol name. Provides: non-static C definitions, C++ definitions with C
//!   linkage, Rust `#[no_mangle]`/`#[export_name]` `extern "C"` functions. Uses: C/C++
//!   prototypes (`definition=false`), Rust `extern "C" { fn }` items. `detail.linkage` is
//!   `c`, `cpp_extern_c` or `rust`.
//! * `jni` — `Java_<mangled class>_<mangled method>`; Java `native` methods use it, C/C++
//!   `Java_*` definitions provide it (overload suffixes `__<sig>` are kept; matching strips).
//! * `cgo` — C name. Uses: Go `C.name(...)`; provides: functions declared or defined in the
//!   cgo preamble (`detail.preamble=true`).
//! * `pyo3` — Python-visible name: `<name>` for `#[pyfunction]`, `<Class>` for `#[pyclass]`,
//!   `<RustType>.<name>` for `#[pymethods]` members (`detail.impl_type`; `#[new]` is
//!   `<RustType>.__new__` with `constructor=true`), module facts `detail.module_init=true`
//!   with `registers` (comma-separated Rust names registered in the module function). The
//!   binding kinds (`pyo3`, `wasm_bindgen`, `napi`) come from the `syntax_conventions` rows
//!   of the Rust table (attribute names, name keys, member export policy, constructors).
//! * `wasm_bindgen` / `napi` — JS-visible name (`js_name` / napi camelCase), methods
//!   `<RustType>.<name>`, constructors `<RustType>`.
//! * `cpython` — `<name>` of a `PyMethodDef` row (`detail.module` from the file's
//!   `PyModuleDef` when present).
//! * `http` — only Python module constants `CONST <name>` (`detail.const`, `detail.value`)
//!   and module-level instances `INSTANCE <name>` (`detail.instance`, `detail.class`): the
//!   values a derived mount prefix reference may name (routes and clients are derived).
//! * `grpc` — `<Service>/<Method>` uses (stub calls) and `<Service>/*` server
//!   implementations (`detail.impl_type` / the class declaration), `<Service>/<method>` for
//!   handler tables (`rpc_add_service` rows).
//! * `graphql` — `<RootType>.<field>`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use trace_core::facts::{BoundaryFact, FileFacts};
use trace_core::text::LineIndex;
use trace_core::Language;
use tree_sitter::Node;

use crate::languages::syntax;
use crate::spec::SyntaxSpec;

mod c;
mod conventions;
mod cx;
mod go;
mod graphql;
mod grpc;
mod java;
mod literals;
mod napi;
mod python;
mod reader;
mod rust;
mod template;
mod walk;

use conventions::Convention;
pub use reader::{Annotation, ArgValue, CallString, Export, ParsedFile};
use template::RawTpl;
pub use template::{ArgRef, Tpl, TplPart, PLACEHOLDER};

/// Upper bound on visited nodes per file (the walk is linear; this only bounds pathological
/// generated files).
const MAX_NODES: usize = 2_000_000;

/// cgo pseudo-functions and libc helpers that are not C symbols of the repository.
const CGO_PSEUDO: [&str; 6] = ["CString", "GoString", "GoStringN", "GoBytes", "CBytes", "free"];
/// Upper bound on boundary facts per file.
const MAX_FACTS: usize = 20_000;
/// Nesting bound for expression evaluation.
const MAX_DEPTH: u32 = 10;

/// Append the boundary facts of one file to `facts.boundaries`.
pub(crate) fn extract(
    language: Language,
    path: &str,
    root: Node<'_>,
    source: &[u8],
    lines: &LineIndex,
    facts: &mut FileFacts,
) {
    let Some(spec) = syntax(language) else {
        return;
    };
    let mut out = {
        let mut cx = Cx::new(language, path, source, lines, spec, facts);
        cx.prepare(root);
        cx.walk(root);
        cx.python_constants(root);
        cx.out.into_inner()
    };
    for fact in &mut out {
        fact.detail.sort();
        fact.detail.dedup_by(|a, b| a.0 == b.0);
    }
    out.sort_by(|a, b| {
        (a.span.start, a.kind, a.role, &a.name, a.span.end).cmp(&(
            b.span.start,
            b.kind,
            b.role,
            &b.name,
            b.span.end,
        ))
    });
    out.dedup_by(|a, b| a.span == b.span && a.kind == b.kind && a.role == b.role && a.name == b.name);
    out.truncate(MAX_FACTS);
    facts.boundaries.extend(out);
    facts.boundaries.sort_by(|a, b| {
        (a.span.start, a.kind, a.role, &a.name).cmp(&(b.span.start, b.kind, b.role, &b.name))
    });
}

// ---------------------------------------------------------------------------------------
// Language / ABI constants (specifications, not packages)
// ---------------------------------------------------------------------------------------

/// Rust attributes that export or import C symbols (The Rust Reference, "Code generation
/// attributes" / "External blocks"): `#[no_mangle]`, `#[export_name = "x"]` on
/// definitions, `#[link_name = "x"]` on foreign items.
const RUST_NO_MANGLE: &str = "no_mangle";
const RUST_EXPORT_NAME: &str = "export_name";
const RUST_LINK_NAME: &str = "link_name";
/// JNI: a native method is implemented by the exported C function `Java_<mangled class>_<mangled
/// method>` (JNI specification, "Resolving Native Method Names").
const JNI_PREFIX: &str = "Java_";
/// CPython C-API (the ABI of CPython extension modules): method tables `PyMethodDef {ml_name,
/// ml_meth, ...}` and the module definition `PyModuleDef {PyModuleDef_HEAD_INIT, m_name, ...}`.
const CPYTHON_METHOD_TABLE: &str = "PyMethodDef";
const CPYTHON_METHOD_NAME: &str = "ml_name";
const CPYTHON_METHOD_FUNCTION: &str = "ml_meth";
const CPYTHON_MODULE_DEF: &str = "PyModuleDef";
const CPYTHON_MODULE_NAME: &str = "m_name";
/// Position of `m_name` in a positional `PyModuleDef` initializer.
const CPYTHON_MODULE_NAME_POSITION: usize = 1;
/// Node-API (the ABI of Node.js addons): `napi_create_function(env, name, length, cb, data,
/// result)` and `napi_property_descriptor {name, _, method, ...}` arrays.
const NAPI_CREATE_FUNCTION: &str = "napi_create_function";
const NAPI_CREATE_NAME_ARG: usize = 1;
const NAPI_CREATE_HANDLER_ARG: usize = 3;
const NAPI_PROPERTY_DESCRIPTOR: &str = "napi_property_descriptor";
/// Node.js loads a compiled addon when a `.node` file is required.
const NODE_ADDON_EXTENSION: &str = ".node";
/// GraphQL root operation types (GraphQL specification, "Root Operation Types").
const GRAPHQL_ROOT_TYPES: [&str; 3] = ["Query", "Mutation", "Subscription"];

// ---------------------------------------------------------------------------------------
// Per-file context
// ---------------------------------------------------------------------------------------

/// What a local name is bound to (same-file, scope-insensitive).
#[derive(Clone, Debug)]
enum Bind {
    /// Generated RPC client stub of service `service` (`rpc_*` rows).
    Stub { service: String },
    /// Loaded Node-API addon (`require('./x.node')`, an `addon_loader` row).
    Addon { module: String },
    /// GraphQL type object holding resolvers of `type_name` (`graphql_*_object` rows).
    GraphqlType { type_name: String },
}

struct ArgView<'t> {
    key: Option<String>,
    value: Node<'t>,
}

struct CallView<'t> {
    node: Node<'t>,
    /// Receiver / object expression of a member call.
    receiver: Option<Node<'t>>,
    /// Callee path segments (`["stub", "SayHello"]`, `["r", "Group", "()", "GET"]`).
    path: Vec<String>,
    member: String,
    args: Vec<ArgView<'t>>,
    is_new: bool,
    owner: Option<u32>,
}

impl<'t> CallView<'t> {
    fn positional(&self, index: usize) -> Option<Node<'t>> {
        self.args
            .iter()
            .filter(|a| a.key.is_none())
            .nth(index)
            .map(|a| a.value)
    }
    fn keyword(&self, keys: &[String]) -> Option<Node<'t>> {
        self.args
            .iter()
            .find(|a| a.key.as_ref().is_some_and(|k| keys.iter().any(|x| x == k)))
            .map(|a| a.value)
    }
    fn last_positional(&self) -> Option<Node<'t>> {
        self.args.iter().rfind(|a| a.key.is_none()).map(|a| a.value)
    }
    fn positional_count(&self) -> usize {
        self.args.iter().filter(|a| a.key.is_none()).count()
    }
    /// Receiver segments = path without the member.
    fn receiver_path(&self) -> &[String] {
        &self.path[..self.path.len().saturating_sub(1)]
    }
}

struct Cx<'a> {
    lang: Language,
    path: &'a str,
    src: &'a [u8],
    lines: &'a LineIndex,
    spec: &'static SyntaxSpec,
    facts: &'a FileFacts,
    /// `syntax_conventions` rows that apply to this language.
    conv: Vec<&'static Convention>,
    call_at: HashMap<(u32, u32), usize>,
    decl_by_name: HashMap<u32, u32>,
    decl_by_span: HashMap<(u32, u32), u32>,
    decl_by_start: HashMap<u32, u32>,
    callable_by_name: HashMap<String, Vec<u32>>,
    import_locals: HashSet<String>,
    consts: HashMap<String, RawTpl>,
    binds: HashMap<String, Bind>,
    cgo_includes: Vec<String>,
    out: RefCell<Vec<BoundaryFact>>,
    /// Constants this file does not bind (imported constants), resolved by the caller of
    /// [`eval_argument`].
    external: Option<&'a ConstantLookup<'a>>,
}

/// Resolves a constant name this file does not bind (an imported constant).
type ConstantLookup<'a> = dyn Fn(&str) -> Option<Tpl> + 'a;

#[cfg(test)]
#[path = "../../tests/unit/boundary/mod.rs"]
mod tests;
