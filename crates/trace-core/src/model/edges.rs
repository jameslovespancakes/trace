//! Edges of the index: kinds, evidence tiers, providers, resolutions, bridges, unresolved
//! sites and value references.

use super::*;

/// Edge kinds. Execution kinds are proven facts; inferred/possible kinds only come from sites.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum EdgeKind {
    // --- EXECUTION (proven) ---
    Calls = 0,
    Constructor,
    InvokedCallback,
    PropertyGet,
    Awaits,
    Iterates,
    StubImplementation,
    // --- DEFERRED (proven fact; body runs when consumed; in inferred/possible views) ---
    CreatesGenerator,
    CreatesCoroutine,
    // --- REFERENCE (proven, never traversed by execution views) ---
    References,
    PassesCallback,
    // --- INFERRED (edge-case classification) ---
    InferredDispatch,
    InferredCall,
    InferredCallback,
    InferredImplicit,
    // --- POSSIBLE (generated candidates) ---
    PossibleLink,
    // --- REFERENCE (proven non-call uses; never traversed by execution views) ---
    /// Assignment / rebinding of the target name (`app.redirect = f`, `obj.m = ...`).
    Writes,
    /// Import statement binding the target (`from flask.helpers import redirect`).
    Imports,
    /// Re-export of the target (`export { x } from './y'`, `pub use`, `__all__` in a
    /// package `__init__`).
    Reexports,
    // --- FAMILY (proven by LSP or by a language rule with a unique match) ---
    /// `from` (a concrete method) overrides `to` (a concrete base-class method).
    Overrides,
    /// `from` implements `to` (an interface / trait / protocol / abstract method).
    Implements,
    // --- BRIDGE (cross-language boundary; tier comes from `Index::bridges[edge.bridge]`) ---
    Bridge,
    // --- INFERRED REFERENCE (non-call uses decided from value flow; never traversed) ---
    /// Store into an attribute that the receiver's tracked values resolve to exactly one
    /// method (`app.redirect = f` where `app` is a pytest fixture's `Flask` instance). Only
    /// from `field_write` sites; tier inferred.
    InferredWrite,
}

impl EdgeKind {
    pub const ALL: [EdgeKind; 23] = [
        EdgeKind::Calls,
        EdgeKind::Constructor,
        EdgeKind::InvokedCallback,
        EdgeKind::PropertyGet,
        EdgeKind::Awaits,
        EdgeKind::Iterates,
        EdgeKind::StubImplementation,
        EdgeKind::CreatesGenerator,
        EdgeKind::CreatesCoroutine,
        EdgeKind::References,
        EdgeKind::PassesCallback,
        EdgeKind::InferredDispatch,
        EdgeKind::InferredCall,
        EdgeKind::InferredCallback,
        EdgeKind::InferredImplicit,
        EdgeKind::PossibleLink,
        EdgeKind::Writes,
        EdgeKind::Imports,
        EdgeKind::Reexports,
        EdgeKind::Overrides,
        EdgeKind::Implements,
        EdgeKind::Bridge,
        EdgeKind::InferredWrite,
    ];

    #[inline]
    pub const fn bit(self) -> u32 {
        1u32 << (self as u8)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            EdgeKind::Calls => "calls",
            EdgeKind::Constructor => "constructor",
            EdgeKind::InvokedCallback => "invoked_callback",
            EdgeKind::PropertyGet => "property_get",
            EdgeKind::Awaits => "awaits",
            EdgeKind::Iterates => "iterates",
            EdgeKind::StubImplementation => "stub_implementation",
            EdgeKind::CreatesGenerator => "creates_generator",
            EdgeKind::CreatesCoroutine => "creates_coroutine",
            EdgeKind::References => "references",
            EdgeKind::PassesCallback => "passes_callback",
            EdgeKind::InferredDispatch => "inferred_dispatch",
            EdgeKind::InferredCall => "inferred_call",
            EdgeKind::InferredCallback => "inferred_callback",
            EdgeKind::InferredImplicit => "inferred_implicit",
            EdgeKind::PossibleLink => "possible_link",
            EdgeKind::Writes => "writes",
            EdgeKind::Imports => "imports",
            EdgeKind::Reexports => "reexports",
            EdgeKind::Overrides => "overrides",
            EdgeKind::Implements => "implements",
            EdgeKind::Bridge => "bridge",
            EdgeKind::InferredWrite => "inferred_write",
        }
    }

    /// The only tier an edge of this kind may carry. Exception: [`EdgeKind::Bridge`] edges
    /// carry the tier of their [`Bridge`] record (proven, inferred or possible); `Bridge`
    /// edges are never stored in [`Index::edges`] (materialized by `Graph::new` only).
    pub const fn tier(self) -> Tier {
        match self {
            EdgeKind::InferredDispatch
            | EdgeKind::InferredCall
            | EdgeKind::InferredCallback
            | EdgeKind::InferredImplicit
            | EdgeKind::InferredWrite => Tier::Inferred,
            EdgeKind::PossibleLink => Tier::Possible,
            _ => Tier::Proven,
        }
    }

    /// Inferred kind for a site category (classify.py `INFERRED_KINDS`).
    pub const fn inferred_for(category: SiteCategory) -> EdgeKind {
        match category {
            SiteCategory::Dispatch => EdgeKind::InferredDispatch,
            SiteCategory::NoTarget | SiteCategory::Flow => EdgeKind::InferredCall,
            SiteCategory::Callback => EdgeKind::InferredCallback,
            SiteCategory::Implicit => EdgeKind::InferredImplicit,
        }
    }
}

impl fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EdgeKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        EdgeKind::ALL
            .iter()
            .copied()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| format!("unknown edge kind: {s}"))
    }
}

/// Evidence tiers, ordered: a view at tier T includes every edge with `edge.tier <= T`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Compiler / language-server facts and language rules (e.g. `.pyi` stub -> `.py`).
    Proven,
    /// Edge cases decided by a decision rule (`trace_infer::decide`).
    #[default]
    Inferred,
    /// Every remaining generated candidate (over-approximation).
    Possible,
}

impl Tier {
    pub const fn as_str(self) -> &'static str {
        match self {
            Tier::Proven => "proven",
            Tier::Inferred => "inferred",
            Tier::Possible => "possible",
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Tier {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "proven" => Ok(Tier::Proven),
            "inferred" => Ok(Tier::Inferred),
            "possible" => Ok(Tier::Possible),
            _ => Err(format!("unknown tier: {s} (expected proven|inferred|possible)")),
        }
    }
}

/// Who produced an edge.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Pyright,
    TypeScript,
    RustAnalyzer,
    /// Generic LSP backend; payload is the server name (`gopls`, `clangd`, ...).
    Lsp(String),
    /// Python packaging rule: `.pyi` beside `.py` is type-only; the `.py` runs.
    PythonStubRule,
    Deterministic,
    /// Candidate generation (possible tier only).
    Candidates,
    /// A language / ABI / packaging rule applied to compiler or syntax facts with a unique
    /// match (proven) or several matches (possible). Payload names the rule: `python-mro`,
    /// `rust-trait-impl`, `c-abi`, `jni-naming`, `cgo`, `pyo3`, `wasm-bindgen`, `napi`,
    /// `cpython-methoddef`, `dts-implementation`, `c-linkage`.
    Rule(String),
    /// A declared contract file (`openapi`, `proto`, `graphql`) or a framework route table
    /// (`http`); tier inferred (unique) or possible.
    Contract(String),
    /// Hand-written bridge manifest (codepath_next format), labelled `user_contract`.
    UserContract,
}

impl Provider {
    /// Display label, e.g. `pyright`, `lsp:gopls`.
    pub fn label(&self) -> String {
        match self {
            Provider::Pyright => "pyright".into(),
            Provider::TypeScript => "typescript".into(),
            Provider::RustAnalyzer => "rust-analyzer".into(),
            Provider::Lsp(name) => format!("lsp:{name}"),
            Provider::PythonStubRule => "python-stub-rule".into(),
            Provider::Deterministic => "deterministic".into(),
            Provider::Candidates => "candidates".into(),
            Provider::Rule(name) => format!("rule:{name}"),
            Provider::Contract(name) => format!("contract:{name}"),
            Provider::UserContract => "user_contract".into(),
        }
    }
}

/// How a link was resolved (provenance detail shown by `trace evidence`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// LSP `callHierarchy/outgoingCalls`.
    CallHierarchy,
    /// LSP `textDocument/definition` on a callee / argument / value reference.
    Definition,
    /// Class -> its own constructor declaration (`__init__`), not a guessed base.
    ConstructorDeclaration,
    /// TypeScript checker resolved signature.
    ResolvedSignature,
    /// `.pyi` declaration -> sibling `.py` declaration with the same qualified name.
    StubPackagingRule,
    /// Call whose only semantic targets were overloads in a sibling stub.
    OverloadedStubImplementation,
    /// Exactly one (non field-only) generated candidate.
    DeterministicUnique,
    /// Generated candidate, not decided.
    GeneratedCandidate,
    /// LSP `textDocument/implementation` / type hierarchy.
    Implementation,
    /// Inheritance rule (MRO / trait impl / `implements`) over a compiler-resolved or
    /// uniquely named base.
    InheritanceRule,
    /// LSP `textDocument/references` (or the TypeScript checker symbol identity).
    FindReferences,
    /// ABI naming rule (C symbol names, JNI `Java_<pkg>_<Class>_<method>`, cgo `C.name`).
    AbiNamingRule,
    /// Binding attribute / export table with a defined export name (PyO3, wasm-bindgen,
    /// N-API, CPython `PyMethodDef`).
    BindingAttribute,
    /// Declared contract (OpenAPI operation, proto rpc, GraphQL field) matched on both sides.
    ContractMatch,
    /// HTTP method + path template matched against a route registration.
    RouteMatch,
    /// Hand-written bridge manifest entry.
    Manifest,
    /// An import / use / re-export statement whose qualified path names exactly one
    /// declaration of the index (module path rules of `trace_infer::narrow::ModuleMap`);
    /// proven `imports` / `reexports` edges, provider `rule:import-path` (SPEC §7.12).
    ImportPath,
    /// Query-time only (`uses`): every candidate of the site is a member of the target's
    /// family (overloads, overrides, implementations, declarations), so the site uses the
    /// family whichever member runs; tier inferred unless a server proved it (SPEC §10.3).
    FamilyOverloads,
    /// Same-file definition answered from syntax because the language's scoping makes it
    /// unambiguous (exactly one declaration of the name in the partition, no local binding);
    /// replaces a `textDocument/definition` request with the identical answer (SPEC §8.8).
    SyntaxDefinition,
}

impl Resolution {
    pub const fn as_str(self) -> &'static str {
        match self {
            Resolution::CallHierarchy => "call_hierarchy",
            Resolution::Definition => "definition",
            Resolution::ConstructorDeclaration => "constructor_declaration",
            Resolution::ResolvedSignature => "resolved_signature",
            Resolution::StubPackagingRule => "stub_packaging_rule",
            Resolution::OverloadedStubImplementation => "overloaded_stub_implementation",
            Resolution::DeterministicUnique => "deterministic_unique",
            Resolution::GeneratedCandidate => "generated_candidate",
            Resolution::Implementation => "implementation",
            Resolution::InheritanceRule => "inheritance_rule",
            Resolution::FindReferences => "find_references",
            Resolution::AbiNamingRule => "abi_naming_rule",
            Resolution::BindingAttribute => "binding_attribute",
            Resolution::ContractMatch => "contract_match",
            Resolution::RouteMatch => "route_match",
            Resolution::Manifest => "manifest",
            Resolution::ImportPath => "import_path",
            Resolution::FamilyOverloads => "family_overloads",
            Resolution::SyntaxDefinition => "syntax_definition",
        }
    }
}

/// A directed link between two symbols.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub from: SymbolId,
    pub to: SymbolId,
    pub kind: EdgeKind,
    /// Always equals `kind.tier()`; stored for explicitness in every output.
    pub tier: Tier,
    pub provider: Provider,
    pub resolution: Resolution,
    /// Evidence location (call site / reference / declaration). Bridges: the crossing
    /// site on the `from` side (the other end is `Index::bridges[bridge].to_at`).
    pub at: Location,
    /// Index into [`Index::sites`] for inferred/possible edges.
    pub site: Option<u32>,
    /// Index into [`Index::bridges`] for `EdgeKind::Bridge` edges (always `None` otherwise).
    pub bridge: Option<u32>,
}

/// Kind of a cross-language boundary (`bridge:<kind>` in outputs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeKind {
    // Proven by one toolchain / packaging rule.
    /// `.d.ts` declaration -> JavaScript implementation (TypeScript module resolution).
    JsTs,
    /// C declaration <-> C++ definition across `extern "C"` (C linkage).
    CCpp,
    /// Python `.pyi` stub (incl. compiled-extension stubs) -> implementation.
    PythonStub,
    // Language / ABI rules (proven when exactly one match, else possible).
    /// Rust `#[no_mangle] extern "C"` / `extern "C" { fn }` <-> C/C++ (global C names).
    CAbi,
    /// Java `native` method <-> C/C++ `Java_<package>_<Class>_<method>`.
    Jni,
    /// Go cgo `C.name` <-> C declaration reachable from the cgo preamble.
    Cgo,
    /// Rust `#[pyfunction]` / `#[pymethods]` / `#[pymodule]` -> Python module attribute.
    Pyo3,
    /// `#[wasm_bindgen]` export -> JavaScript import.
    WasmBindgen,
    /// N-API export (`napi_create_function`, `#[napi]`) -> JavaScript.
    Napi,
    /// CPython `PyMethodDef` table entry -> Python-visible name.
    Cpython,
    // Declared contracts (inferred when unique, else possible).
    Grpc,
    Openapi,
    Graphql,
    /// HTTP route registration <-> client call with a literal/constant URL template.
    Http,
    // Weak boundaries (possible only).
    Subprocess,
    /// `ctypes` / `cffi` / `dlopen` symbol lookup.
    Ffi,
    /// Message / event name published in one language and consumed in another.
    Message,
    /// Hand-written manifest (codepath_next format).
    UserContract,
}

impl BridgeKind {
    pub const ALL: [BridgeKind; 18] = [
        BridgeKind::JsTs,
        BridgeKind::CCpp,
        BridgeKind::PythonStub,
        BridgeKind::CAbi,
        BridgeKind::Jni,
        BridgeKind::Cgo,
        BridgeKind::Pyo3,
        BridgeKind::WasmBindgen,
        BridgeKind::Napi,
        BridgeKind::Cpython,
        BridgeKind::Grpc,
        BridgeKind::Openapi,
        BridgeKind::Graphql,
        BridgeKind::Http,
        BridgeKind::Subprocess,
        BridgeKind::Ffi,
        BridgeKind::Message,
        BridgeKind::UserContract,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            BridgeKind::JsTs => "js_ts",
            BridgeKind::CCpp => "c_cpp",
            BridgeKind::PythonStub => "python_stub",
            BridgeKind::CAbi => "c_abi",
            BridgeKind::Jni => "jni",
            BridgeKind::Cgo => "cgo",
            BridgeKind::Pyo3 => "pyo3",
            BridgeKind::WasmBindgen => "wasm_bindgen",
            BridgeKind::Napi => "napi",
            BridgeKind::Cpython => "cpython",
            BridgeKind::Grpc => "grpc",
            BridgeKind::Openapi => "openapi",
            BridgeKind::Graphql => "graphql",
            BridgeKind::Http => "http",
            BridgeKind::Subprocess => "subprocess",
            BridgeKind::Ffi => "ffi",
            BridgeKind::Message => "message",
            BridgeKind::UserContract => "user_contract",
        }
    }

    /// Edge-kind label used in every output row: `bridge:<kind>`.
    pub const fn edge_label(self) -> &'static str {
        match self {
            BridgeKind::JsTs => "bridge:js_ts",
            BridgeKind::CCpp => "bridge:c_cpp",
            BridgeKind::PythonStub => "bridge:python_stub",
            BridgeKind::CAbi => "bridge:c_abi",
            BridgeKind::Jni => "bridge:jni",
            BridgeKind::Cgo => "bridge:cgo",
            BridgeKind::Pyo3 => "bridge:pyo3",
            BridgeKind::WasmBindgen => "bridge:wasm_bindgen",
            BridgeKind::Napi => "bridge:napi",
            BridgeKind::Cpython => "bridge:cpython",
            BridgeKind::Grpc => "bridge:grpc",
            BridgeKind::Openapi => "bridge:openapi",
            BridgeKind::Graphql => "bridge:graphql",
            BridgeKind::Http => "bridge:http",
            BridgeKind::Subprocess => "bridge:subprocess",
            BridgeKind::Ffi => "bridge:ffi",
            BridgeKind::Message => "bridge:message",
            BridgeKind::UserContract => "bridge:user_contract",
        }
    }

    /// Strongest tier a bridge of this kind may carry (`Tier` orders proven < inferred <
    /// possible, so a record is valid iff `record.tier >= kind.max_tier()`): toolchain / ABI
    /// rules can be proven, declared contracts and HTTP routes at most inferred, weak
    /// boundaries only possible, manifests inferred (labelled `user_contract`).
    pub const fn max_tier(self) -> Tier {
        match self {
            BridgeKind::JsTs
            | BridgeKind::CCpp
            | BridgeKind::PythonStub
            | BridgeKind::CAbi
            | BridgeKind::Jni
            | BridgeKind::Cgo
            | BridgeKind::Pyo3
            | BridgeKind::WasmBindgen
            | BridgeKind::Napi
            | BridgeKind::Cpython => Tier::Proven,
            BridgeKind::Grpc
            | BridgeKind::Openapi
            | BridgeKind::Graphql
            | BridgeKind::Http
            | BridgeKind::UserContract => Tier::Inferred,
            BridgeKind::Subprocess | BridgeKind::Ffi | BridgeKind::Message => Tier::Possible,
        }
    }
}

impl fmt::Display for BridgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for BridgeKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.strip_prefix("bridge:").unwrap_or(s);
        BridgeKind::ALL
            .iter()
            .copied()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| format!("unknown bridge kind: {s}"))
    }
}

/// A cross-language link with evidence on both sides (NEXT.md P2b). Never merged into
/// proven call edges: `Graph::new` materializes it as an `EdgeKind::Bridge` edge whose tier
/// is `tier` and whose `bridge` field indexes this record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bridge {
    pub kind: BridgeKind,
    /// Never stronger than `kind.max_tier()` (i.e. `tier >= kind.max_tier()` in `Tier`
    /// order); proven only for a unique match under a toolchain/ABI/binding rule.
    pub tier: Tier,
    /// Executing symbol on the using side (client caller, Python caller, JS importer, the
    /// C declaration for linkage rules).
    pub from: SymbolId,
    /// Symbol on the providing side (route handler, exported Rust/C function, JNI function,
    /// implementation).
    pub to: SymbolId,
    /// Evidence span on the `from` side (call / import / declaration).
    pub from_at: Location,
    /// Evidence span on the `to` side (registration / attribute / export / declaration).
    pub to_at: Location,
    pub provider: Provider,
    pub resolution: Resolution,
    /// Boundary name as matched: `POST /auth/login`, `Java_com_x_Y_m`, `mymod.func`,
    /// `helloworld.Greeter/SayHello`, `c:compress2`.
    pub label: String,
    /// Assumptions the link depends on (e.g. `module is imported as "mymod"`, `base URL
    /// joins the path literally`). Shown by `evidence`.
    pub assumptions: Vec<String>,
    /// Number of candidates the rule matched (1 for proven/inferred-unique rows; every
    /// candidate of a non-unique match gets its own possible row with the same count).
    pub candidates: u32,
    /// Contract file that declared the boundary (OpenAPI/proto/GraphQL/manifest), if any.
    pub contract: Option<FileId>,
}

/// Why a syntax call site has no proven target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnresolvedKind {
    /// The semantic backend returned no target at all for this call site.
    NoSemanticTarget,
    /// Targets outside the index (library/builtin) or more than one target.
    ExternalOrAmbiguous,
    /// TypeScript: signature unresolved, `any`/`unknown`/union callee type.
    UnresolvedSignature,
    /// The callee depends on a template / generic parameter; known only per instantiation.
    TemplateDependent,
    /// The call is in code the build does not compile on this machine (an inactive `#if`
    /// region, ...).
    InactiveCode,
}

impl UnresolvedKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            UnresolvedKind::NoSemanticTarget => "no_semantic_target",
            UnresolvedKind::ExternalOrAmbiguous => "external_or_ambiguous",
            UnresolvedKind::UnresolvedSignature => "unresolved_signature",
            UnresolvedKind::TemplateDependent => "template_dependent",
            UnresolvedKind::InactiveCode => "inactive_code",
        }
    }
    /// Kinds that feed `no_target` candidate sites: the analyzer named no target. A
    /// TypeScript/JavaScript `unresolved_signature` callee (`any` / `unknown` receiver, e.g.
    /// an untyped callback parameter `res` in `app.get('/', (req, res) => res.redirect(..))`)
    /// is as blind as a missing target; external or ambiguous targets are not.
    pub const fn is_blind(self) -> bool {
        matches!(self, UnresolvedKind::NoSemanticTarget | UnresolvedKind::UnresolvedSignature)
    }
}

/// An explicit unknown: a call site without a proven target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Unresolved {
    /// Executing symbol; `None` for module-level code.
    pub owner: Option<SymbolId>,
    pub kind: UnresolvedKind,
    /// Callee expression span.
    pub at: Location,
    /// Exact callee expression text.
    pub callee: String,
    /// In-index candidates the backend reported (ambiguous case); may be empty.
    pub candidates: Vec<SymbolId>,
}

/// A semantically resolved value reference (identifier used as a value, not called).
/// Feeds value flow; never an edge by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ValueRef {
    pub at: Location,
    pub target: SymbolId,
}
