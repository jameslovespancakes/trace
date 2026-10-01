//! Syntax facts: the output contract of `trace-syntax`, one [`FileFacts`] per file.
//!
//! Facts are extracted from tree-sitter syntax trees only (never regex, never execution).
//! All indices (`u32` declaration indices, [`Scope::Decl`]) are local to the file and equal
//! the position in [`FileFacts::declarations`]; assembly maps them to global symbol ids.
//! Spans are byte ranges into the raw file bytes.

use serde::{Deserialize, Serialize};

use crate::languages::{Arity, Language};
use crate::model::{BridgeKind, ByteSpan, ExecutionModel, Span, SymbolKind};

/// Everything trace needs to know about one file's syntax.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FileFacts {
    pub language: Option<Language>,
    /// Number of ERROR/MISSING nodes; facts are still extracted from valid regions.
    pub error_count: u32,
    /// Declarations in pre-order (parents before children). Index = local decl id.
    pub declarations: Vec<Declaration>,
    /// Every call / `new` expression.
    pub calls: Vec<CallSite>,
    /// Identifiers used as values (not as a direct callee), incl. decorator expressions.
    pub references: Vec<Reference>,
    /// Uncalled name/attribute arguments of calls (possible callbacks).
    pub callbacks: Vec<CallbackArg>,
    /// Binding spellings (name = value statements; read by the bridge bindings).
    pub assignments: Vec<Assignment>,
    /// Flow-insensitive value-flow constraints (see `trace_infer::flow`).
    pub flow: Vec<FlowFact>,
    /// Implicit data-model operations (subscript, with, for, descriptor access).
    pub implicit: Vec<ImplicitOp>,
    /// Out-of-line type relations: Rust `impl Trait for Type`,
    /// Haskell `instance C T`.
    pub impls: Vec<ImplRelation>,
    /// Test blocks not declared as functions (JS/TS `it("...", () => {})`, `test(...)`).
    pub tests: Vec<TestBlock>,
    /// Synthetic declarations of anonymous function values and generator expressions
    /// (`<lambda>`, `<genexpr>`), with where they are created and how they are consumed.
    pub anonymous: Vec<AnonymousScope>,
    /// Structure of every call, aligned with `calls` (`call_details[i].call == i`); empty only
    /// for facts not produced by trace-syntax.
    pub call_details: Vec<CallDetail>,
    /// Import bindings (Python, JavaScript, TypeScript).
    pub imports: Vec<Import>,
    /// Qualified paths of dotted names rooted at an import or a builtin, for decorator
    /// expressions, call argument values and assigned values (callees: `CallDetail::callee_path`).
    /// Sorted by span.
    pub qualified_names: Vec<QualifiedName>,
    /// Index of the synthetic `<module>` declaration (kind `SymbolKind::Module`, name and
    /// qualified name `<module>`, span = whole file, `parent: None`, appended LAST so no
    /// other declaration index moves; no declaration has it as `parent`). Present for every
    /// file extracted by trace-syntax (EXTRACTOR_VERSION >= 5). It executes module-level and
    /// class-body code: `CallSite::owner` / `Reference::owner` stay `None` there (flow
    /// contract unchanged); consumers attribute them with [`FileFacts::executing_owner`].
    pub module_decl: Option<u32>,
    /// Re-exports (`export { a as b } from './m'`, `export * from './m'`, Rust `pub use`,
    /// names listed in a Python package `__init__` `__all__` that are bound by imports).
    pub exports: Vec<Export>,
    /// Cross-language boundary facts found in syntax trees (`trace-syntax/src/boundary/`):
    /// ABI exports/imports, binding attributes, route registrations, HTTP client calls,
    /// subprocess/FFI lookups, message names. Matched across files by `trace-bridge`.
    pub boundaries: Vec<BoundaryFact>,
    /// Declared and annotated types (SPEC §6.3 `TypeFact`): type positions in code
    /// (`val x: Foo`, `fun f(): Foo`, parameter types), constructed values (`x = Foo()`,
    /// `new Foo()`), and type annotations inside comments (JSDoc `@type` / `@returns` /
    /// `@param`, PHPDoc `@var` / `@return` / `@param`), parsed from the comment nodes of the
    /// syntax tree with the annotation grammar only (never regex over source code). Sorted by
    /// span. Consumed by syntax-only narrowing (receiver types) and the `uses` name scan
    /// (unrelated receiver types).
    #[serde(default)]
    pub types: Vec<TypeFact>,
    /// Identifier spans (sorted, unique) that the language's scoping rules prove denote a
    /// local variable or parameter binding (SPEC §6.3 lexical shadowing): the binding
    /// identifiers themselves (`let/val/var x = ...`, parameters, loop / catch / match-arm
    /// variables, pattern and closure-parameter bindings, PHP / R assignment targets
    /// inside a function) and every bare-name read, write or call that resolves to such a
    /// binding (nearest binding scope wins; block scopes where the language has them; named
    /// functions that do not capture — Rust `fn` items, PHP functions — never see
    /// enclosing locals). Never member names (`obj.x`), static path segments, labels or type
    /// positions; never names also bound by `def`/`fn`/`class`/imports in the deciding scope;
    /// never module-level variables or class members. Python keeps its function-scope rule
    /// (`global` / `nonlocal` aware). Filled for Python, JS/TS/TSX, Rust, Go, Java, C, C++, C#,
    /// Scala, PHP, R, Bash and Haskell;
    /// `Reference::local` is true exactly for references whose span is listed here.
    #[serde(default)]
    pub local_spans: Vec<ByteSpan>,
    /// Every identifier that is the member part of a member access on a value (sorted by
    /// span, unique): `x` in `obj.x`, `this.x`, `self.x`, `a.b.x`, `p->x`, `obj?.x`, C#
    /// `o?.X`, PHP `$o->x`, Java `o.x()`, in calls, reads, writes
    /// and callback arguments; a receiver that is an imported module (`os.path.join`,
    /// `ns.f` after `import * as ns`) is recorded too (consumers check the root's import).
    /// Not for static paths (Rust `Type::x`, C++ `ns::f`, PHP `A::f`, R
    /// `pkg::f`) and not inside import statements. Consumed by the `uses`
    /// member-binding rule (SPEC §10.1) and narrowing.
    #[serde(default)]
    pub member_accesses: Vec<MemberAccess>,
    /// Interface fingerprint of the file (`trace_syntax::interface::fingerprint`): changes
    /// only when what other files can see of this file changes.
    #[serde(default)]
    pub interface: crate::fingerprint::Hash32,
    /// Source definitions of Python module assignments and Go package bindings. Retrieval facts,
    /// NOT declarations/call-graph nodes; imports, locals and attribute stores are excluded.
    pub data_definitions: Vec<DataDefinition>,
    /// Identifier counts within parsed declaration bodies, keyed by body_start byte.
    /// Separate from whole-declaration identifiers so headers cannot masquerade as body hits.
    pub body_identifiers: std::collections::BTreeMap<u32, Vec<(String, u32)>>,
}

/// One syntactic assignment, not a claim about a binding's value at runtime. Reassignments
/// remain separate records; chained/destructured bindings share the complete statement span.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataDefinition {
    pub name: String,
    pub name_span: ByteSpan,
    pub span: Span,
    /// Nested in module control flow (if/try/loop/with/match), not unconditionally defined.
    pub conditional: bool,
}

impl FileFacts {
    /// Whether the file is test code by its contents: it has test facts (test blocks or test
    /// declarations) and every named callable is a test, lies inside a test block, or is
    /// nested in a test declaration (`#[cfg(test)] mod tests`). A source file with inline
    /// unit tests next to product code (the Rust convention) is product code.
    pub fn is_test_code(&self) -> bool {
        if self.tests.is_empty() && !self.declarations.iter().any(|d| d.is_test) {
            return false;
        }
        let in_test = |mut i: usize| -> bool {
            let span = self.declarations[i].span.bytes;
            if self.tests.iter().any(|t| t.span.encloses(span)) {
                return true;
            }
            loop {
                let d = &self.declarations[i];
                if d.is_test {
                    return true;
                }
                match d.parent {
                    Some(p) if (p as usize) < self.declarations.len() && p as usize != i => i = p as usize,
                    _ => return false,
                }
            }
        };
        self.declarations
            .iter()
            .enumerate()
            .filter(|(_, d)| d.kind.is_callable() && !d.name.starts_with('<'))
            .all(|(i, _)| in_test(i))
    }

    /// The structure of `calls[call]`, when present.
    pub fn call_detail(&self, call: usize) -> Option<&CallDetail> {
        self.call_details.get(call).filter(|d| d.call as usize == call)
    }

    /// The anonymous-scope record of declaration `decl`, if it is synthetic.
    pub fn anonymous_of(&self, decl: u32) -> Option<&AnonymousScope> {
        self.anonymous
            .binary_search_by_key(&decl, |a| a.decl)
            .ok()
            .map(|i| &self.anonymous[i])
    }

    /// Executing declaration for a syntax owner: `owner`, else the synthetic `<module>`
    /// declaration (module-level and class-body code). Every lazy scope (lambda, arrow
    /// function, closure, anonymous callback, generator expression) has its own synthetic
    /// declaration from EXTRACTOR_VERSION 5 on, so `None` owners are exactly module/class
    /// level.
    pub fn executing_owner(&self, owner: Option<u32>) -> Option<u32> {
        owner.or(self.module_decl)
    }

    /// Synthetic declarations: `<module>` and anonymous scopes (`<lambda>`, `<genexpr>`).
    /// They have no name a language server can prepare.
    pub fn is_synthetic(&self, decl: u32) -> bool {
        self.module_decl == Some(decl) || self.anonymous_of(decl).is_some()
    }

    /// Whether `span` is an identifier the syntax proves denotes a local / parameter binding
    /// ([`FileFacts::local_spans`], binary search).
    pub fn is_local(&self, span: ByteSpan) -> bool {
        self.local_spans
            .binary_search_by_key(&(span.start, span.end), |s| (s.start, s.end))
            .is_ok()
    }

    /// The member access whose member identifier is exactly `span`, if any.
    pub fn member_access(&self, span: ByteSpan) -> Option<&MemberAccess> {
        self.member_accesses
            .binary_search_by_key(&(span.start, span.end), |m| (m.span.start, m.span.end))
            .ok()
            .map(|i| &self.member_accesses[i])
    }

    /// Type facts about `subject` (declaration order).
    pub fn types_of<'f>(&'f self, subject: &'f TypeSubject) -> impl Iterator<Item = &'f TypeFact> + 'f {
        self.types.iter().filter(move |t| &t.subject == subject)
    }

    /// Qualified path recorded for exactly this expression span.
    pub fn qualified_name_at(&self, span: ByteSpan) -> Option<&str> {
        self.qualified_names
            .binary_search_by_key(&(span.start, span.end), |q| (q.span.start, q.span.end))
            .ok()
            .map(|i| self.qualified_names[i].path.as_str())
    }
}

/// What a synthetic declaration stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnonymousKind {
    /// Anonymous function value (Python `lambda`); declaration name `<lambda>`.
    Lambda,
    /// Generator expression: a generator function called once with its first iterable;
    /// declaration name `<genexpr>`. Its body runs only when the generator is consumed.
    GeneratorExpression,
}

/// A synthetic declaration (sorted by `decl`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnonymousScope {
    /// Index of the synthetic declaration in `FileFacts::declarations`.
    pub decl: u32,
    pub kind: AnonymousKind,
    /// Executing declaration that evaluates the expression (creates the function value or
    /// the generator object); `None` at module/class level.
    pub created_in: Option<u32>,
    /// Syntactic consumer of the value at its creation site.
    pub consumer: Consumer,
    /// Generator expressions: the first iterable, evaluated eagerly by `created_in`.
    pub eager: Option<ByteSpan>,
}

/// How the value of an expression is consumed by its syntactic parent (parentheses skipped).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Consumer {
    /// Argument of call `call` (index into `FileFacts::calls`).
    Argument {
        call: u32,
        slot: ArgSlot,
    },
    /// Called directly: `(lambda: x)()`.
    Called {
        call: u32,
    },
    /// Iterated: `for` loop / comprehension iterable, `yield from`, `*` unpacking, spread.
    Iterated,
    Awaited,
    /// Assigned (assignment / walrus value) or a parameter default (value flow decides).
    Bound,
    Returned,
    Yielded,
    Other,
}

/// Position of an argument in a call.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgSlot {
    /// `index` = number of positional arguments before it; `exact` is false after an
    /// unpacked argument (`*args`, spread), when the runtime position is only a lower bound.
    Positional { index: u32, exact: bool },
    /// Named argument (`key=value`, `name: value`).
    Keyword(String),
    /// `*args` / `...spread`.
    Unpack,
    /// `**kwargs`.
    UnpackKeywords,
}

/// One argument of a call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Argument {
    pub slot: ArgSlot,
    /// Span of the argument value (operand for unpacked arguments).
    pub span: ByteSpan,
    pub value: Expr,
    /// The value is a string (literal or template) or an object / list literal holding one:
    /// a possible channel key (`prefix="/items"`, `{url: "/api"}`), so the call's library
    /// target is always asked.
    #[serde(default)]
    pub has_string: bool,
}

/// Structure of `FileFacts::calls[call]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallDetail {
    pub call: u32,
    /// Receiver expression of a method call (`self.store` in `self.store.save(x)`), incl.
    /// `self`/`this`.
    pub receiver: Option<Expr>,
    /// Arguments in source order.
    pub arguments: Vec<Argument>,
    /// Qualified path of the callee when it is a dotted name whose root binding is an import
    /// or a builtin (`functools.partial`, `builtins.iter`, `itertools.islice`).
    pub callee_path: Option<String>,
    /// Identity guards of a bare-name callee (Python): the call only runs where the callee is
    /// *not* the object each of these expressions evaluates to — the `else` branch of
    /// `if f is g:`, the body of `if f is not g:`, the right operand of `f is g or f()`.
    /// Recorded only when the callee name is never rebound in its function besides as a
    /// parameter. Lowered in the call's scope.
    pub not_identical: Vec<Expr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportKind {
    /// `import a.b`, `import a.b as c`, `import * as ns from "m"`: binds a module.
    Module,
    /// `from a import b [as c]`, `import { b as c } from "a"`, default imports.
    Member,
    /// `from a import *` (`local` is `*`).
    Wildcard,
}

/// A name bound by an import statement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Import {
    /// Bound local name (`p` in `from functools import partial as p`; `os` in `import os.path`).
    pub local: String,
    /// Dotted path the name stands for (`functools.partial`, `os`); relative Python imports
    /// keep their leading dots (`.models.User`); JS/TS: `<module specifier>.<export>`
    /// (`default` for default imports).
    pub target: String,
    pub kind: ImportKind,
    /// Lexical scope binding the name (`Decl(d)` for function-level imports).
    pub scope: Scope,
    pub span: ByteSpan,
    pub line: u32,
}

/// A dotted name resolved through imports / builtins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualifiedName {
    /// Span of the whole dotted expression.
    pub span: ByteSpan,
    pub path: String,
}

/// A function/method/class/interface declaration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Declaration {
    pub name: String,
    /// Dotted lexical path, e.g. `Outer.inner`, `Session.login`.
    pub qualified_name: String,
    pub kind: SymbolKind,
    /// Full span including decorators/attributes/export wrappers and leading doc comments
    /// that are part of the node (Rust `///` handled as doc, not span).
    pub span: Span,
    pub name_span: ByteSpan,
    /// Start of the body (end of header/signature). Equal to `span.bytes.end` if no body.
    pub body_start: u32,
    /// Lexically enclosing declaration.
    pub parent: Option<u32>,
    /// Receiver/impl type name for out-of-line methods (Rust impl blocks, Go receivers, C++
    /// `A::f`), the extended type of Scala extension methods, the instance type of Haskell instance
    /// bindings, and the receiver path of property-assigned functions (JS/TS `res.redirect
    /// = function ...` -> `res`, `X.prototype.m = ...` -> `X`). With a container, `qualified_name` is
    /// `<enclosing>.<container>.<name>` (`res.redirect`, `Picker.set_selection`), so the
    /// selectors `obj.prop` and `Type:method` match it exactly. `this.f = ...`, `exports.f`
    /// and `module.exports.f` name no container.
    pub container: Option<String>,
    /// Docstring (Python first-statement string) or adjacent leading comment; <= 1200 bytes.
    pub doc: Option<String>,
    /// Decorator / attribute / annotation texts, outermost first.
    pub decorators: Vec<String>,
    /// Base / implemented-interface spellings.
    pub bases: Vec<String>,
    pub parameters: Vec<Param>,
    pub execution: ExecutionModel,
    pub is_stub: bool,
    pub is_test: bool,
    /// Additional 1-based declaration lines (Python overload group collapsed onto the impl).
    pub declaration_lines: Vec<u32>,
    /// Identifier / attribute-name occurrence counts inside the span (search + test mentions).
    /// Sorted by name.
    pub identifiers: Vec<(String, u32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    Positional,
    KeywordOnly,
    VarPositional,
    VarKeyword,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    pub kind: ParamKind,
    pub has_default: bool,
}

/// How a call's result is consumed at the call site (activation of lazy bodies).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activation {
    #[default]
    Plain,
    /// Directly awaited (`await f()`).
    Await,
    /// Directly iterated (`for x in f()`, `yield from f()`, `for..of`, spread).
    Iterate,
}

/// A call expression.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallSite {
    /// Declaration whose execution evaluates this call. `None` at module/class level and
    /// inside lazy scopes (lambda bodies, anonymous callbacks, generator element expressions,
    /// default values evaluate in the *enclosing* scope).
    pub owner: Option<u32>,
    /// Innermost enclosing declaration regardless of laziness (context display only).
    pub lexical_owner: Option<u32>,
    /// Whole call expression.
    pub span: ByteSpan,
    /// The function expression (callee), e.g. `self.store.save`.
    pub callee_span: ByteSpan,
    /// Exact callee text.
    pub callee: String,
    /// Called member identifier (last name segment) if the callee ends in an identifier.
    pub member: Option<String>,
    /// Receiver identifier immediately before the member (`store` in `self.store.save`),
    /// excluding `self`/`cls`/`this`.
    pub receiver: Option<String>,
    pub line: u32,
    pub activation: Activation,
    /// `new C()` / constructor-call syntax.
    pub is_new: bool,
    pub arg_count: u32,
}

/// Syntactic role of a non-call use of a name (NEXT.md item 4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    /// Read as a value (`x = f`, `return self.json.dumps`, `obj.attr` load).
    #[default]
    Read,
    /// Store position: assignment / augmented assignment / `del` target, incl. attribute
    /// targets (`app.redirect = f`) and class-field (re)definitions.
    Write,
    /// Passed as a call argument without being called (also recorded as `CallbackArg`).
    Argument,
    /// Inside a decorator / attribute / annotation expression (`in_decorator` is true too).
    Decorator,
    /// Name bound by an import statement (`from m import f`, `import { f } from`, `use a::f`).
    Import,
    /// Name listed in a re-export (`export { f }`, `pub use`, `__all__`).
    Export,
    /// Type position (annotation, `extends`/`implements`, generic argument).
    Type,
}

impl RefKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            RefKind::Read => "read",
            RefKind::Write => "write",
            RefKind::Argument => "argument",
            RefKind::Decorator => "decorator",
            RefKind::Import => "import",
            RefKind::Export => "export",
            RefKind::Type => "type",
        }
    }
}

/// An identifier used as a value (not called directly). Decorator expressions included.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reference {
    /// Span of the identifier itself (attribute name for `a.b`).
    pub span: ByteSpan,
    pub name: String,
    pub owner: Option<u32>,
    pub in_decorator: bool,
    /// A bare name whose nearest binding scope is a function that binds it only as a
    /// parameter or variable (never by `def`/`class` or an import): it denotes a local
    /// variable, never a declaration. From EXTRACTOR_VERSION 10 filled for every language
    /// with lexical scoping (block scopes, `let/val/var`, parameters, pattern bindings);
    /// true exactly when `span` is in [`FileFacts::local_spans`].
    pub local: bool,
    /// Syntactic role (reads, writes, arguments, imports, exports, types). Semantic backends
    /// turn resolved uses into `references` / `writes` / `imports` / `reexports` /
    /// `passes_callback` edges owned by `FileFacts::executing_owner(owner)`.
    pub kind: RefKind,
}

/// A re-export binding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Export {
    /// Exported name (`*` for wildcard re-exports).
    pub exported: String,
    /// Target path in `Import::target` form (`./m.a`, `crate::x::y`, `.models.User`).
    pub target: String,
    /// Span of the exported identifier (or the whole statement for wildcards).
    pub span: ByteSpan,
    pub line: u32,
}

/// Which side of a boundary a fact is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryRole {
    /// Makes a name reachable from another language (exported C symbol, JNI implementation,
    /// `#[pyfunction]`, route registration, gRPC servicer method, GraphQL resolver, message
    /// subscriber).
    Provides,
    /// Uses a name across a boundary (`extern "C" { fn }` / prototype, Java `native`, cgo
    /// `C.name`, Python import of an extension module attribute, HTTP client call, gRPC stub
    /// call, subprocess / FFI lookup, message publish).
    Uses,
}

/// A boundary fact found in a syntax tree (never regex). `name` is the normalized boundary
/// key both sides are matched on:
/// * `c_abi` / `cgo` / `ffi`: the C symbol name;
/// * `jni`: the mangled `Java_<pkg>_<Class>_<method>` name (both sides normalized);
/// * `pyo3` / `cpython` / `napi` / `wasm_bindgen`: `<module>.<exported name>` (module may be
///   empty when unknown);
/// * `http`: `<METHOD> <normalized path template>` (`GET /users/{}`; method `*` = any);
/// * `grpc`: `<package>.<Service>/<Method>`; `graphql`: `<Type>.<field>`;
/// * `subprocess`: repository-relative script path; `message`: the event/topic literal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundaryFact {
    pub kind: BridgeKind,
    pub role: BoundaryRole,
    pub name: String,
    /// Executing declaration (use [`FileFacts::executing_owner`] semantics: `None` = module).
    pub owner: Option<u32>,
    /// The declaration the fact is about (exported/annotated function, handler, native
    /// method, prototype), when it is a declaration of this file.
    pub decl: Option<u32>,
    /// Evidence span (attribute, registration call, client call, prototype, literal).
    pub span: ByteSpan,
    pub line: u32,
    /// Extra attributes sorted by key (`framework`, `method`, `path`, `arity`, `module`,
    /// `dynamic` = `true` when part of the key came from a non-literal expression, ...).
    pub detail: Vec<(String, String)>,
}

/// A name/attribute passed as a call argument without being called.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallbackArg {
    /// Callee span of the receiving call.
    pub call_callee_span: ByteSpan,
    pub callee: String,
    /// Identifier span of the argument (attribute name for `obj.method`).
    pub arg_span: ByteSpan,
    /// Exact text of the whole argument expression.
    pub argument: String,
    pub name: String,
    pub owner: Option<u32>,
    /// Positional index of the argument among the receiving call's arguments (`None` for a
    /// keyword argument or when syntax cannot tell).
    #[serde(default)]
    pub index: Option<u32>,
    /// Keyword / named-argument name when the function is passed by keyword.
    #[serde(default)]
    pub keyword: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentKind {
    Name,
    Attribute,
    Keyword,
}

/// A binding spelling: `x = ...`, `obj.attr = ...`, `f(key=...)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    /// Bound name (attribute name for attribute targets, keyword for keyword args).
    pub target: String,
    pub kind: AssignmentKind,
    /// Whole statement / keyword argument span.
    pub span: ByteSpan,
    pub line: u32,
}

/// Where a flow value is evaluated or bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Scope {
    Module,
    Decl(u32),
}

/// Expression IR for value flow. Unsupported expressions lower to [`Expr::Opaque`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Expr {
    Name {
        name: String,
        span: ByteSpan,
    },
    Attr {
        object: Box<Expr>,
        attr: String,
        /// Span of the attribute identifier only (value-reference key).
        attr_span: ByteSpan,
        span: ByteSpan,
    },
    Call {
        func: Box<Expr>,
        func_span: ByteSpan,
        args: Vec<Expr>,
        kwargs: Vec<(String, Expr)>,
        span: ByteSpan,
        is_new: bool,
    },
    /// Any of the alternatives (`a or b`, `x if c else y`, `c ? x : y`, `a ?? b`).
    Choice(Vec<Expr>),
    Await(Box<Expr>),
    /// Anonymous function value. `function` is its synthetic declaration (`<lambda>`) when the
    /// language models anonymous functions as declarations. A generator expression lowers to
    /// `Call { func: Lambda { function: <genexpr> }, args: [first iterable] }`.
    Lambda {
        span: ByteSpan,
        function: Option<u32>,
    },
    Opaque,
}

impl Expr {
    /// Source span when known.
    pub fn span(&self) -> Option<ByteSpan> {
        match self {
            Expr::Name { span, .. }
            | Expr::Attr { span, .. }
            | Expr::Call { span, .. }
            | Expr::Lambda { span, .. } => Some(*span),
            Expr::Await(inner) => inner.span(),
            Expr::Choice(_) | Expr::Opaque => None,
        }
    }
}

/// Assignment targets in the flow model (flow.py slots).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BindTarget {
    /// Variable `name` in `scope` (`('v', scope, name)`).
    Var { scope: Scope, name: String },
    /// Class-body member `name` of class declaration `class` (`('m', cls, name)`).
    Member { class: u32, name: String },
    /// Field-name slot shared by every object (`('a', name)`): weak, field-only evidence.
    Field { name: String },
    /// Class-specific attribute slot of whatever `object` evaluates to (`('attrof', obj, name)`).
    FieldOf { object: Expr, name: String },
}

/// One flow constraint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FlowFact {
    /// `target <- value`, where `value` is evaluated in `scope`.
    Bind {
        target: BindTarget,
        value: Expr,
        scope: Scope,
    },
    /// `return value` in function `function`.
    Return { function: u32, value: Expr },
    /// A call evaluated for its argument/parameter bindings (every call a statement evaluates).
    Eval { scope: Scope, call: Expr },
    /// Decorated definition: `target <- d0(d1(...(function)))`; decorators outermost first,
    /// evaluated in `scope`. Only emitted when decorators are present.
    Decorated {
        scope: Scope,
        target: BindTarget,
        function: u32,
        decorators: Vec<Expr>,
    },
    /// First parameter of a method receives instances (or the class family if `is_class`).
    ImplicitSelf {
        function: u32,
        param: String,
        class: u32,
        is_class: bool,
    },
}

/// Implicit data-model operation kinds (flow.py `DATA_MODEL`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImplicitKind {
    SubscriptStore,
    SubscriptLoad,
    SubscriptDelete,
    /// `with x:` — looks up both `__enter__` and `__exit__`.
    WithEnter,
    Iterate,
    /// Attribute load; `subject` is the whole attribute expression.
    DescriptorGet,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImplicitOp {
    pub scope: Scope,
    pub kind: ImplicitKind,
    /// Operand (subscript/with/for) or whole attribute expression (descriptor get).
    pub subject: Expr,
    pub span: ByteSpan,
    pub line: u32,
}

/// Out-of-line nominal relation: `type_name` implements/extends `trait_name`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImplRelation {
    pub type_name: String,
    pub trait_name: String,
    pub span: ByteSpan,
}

/// A member access on a value ([`FileFacts::member_accesses`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberAccess {
    /// Span of the member identifier.
    pub span: ByteSpan,
    /// Root identifier of the receiver when the receiver is a plain name or a dotted name
    /// path (`obj` in `obj.x`, `a` in `a.b.x`, `$o` in PHP `$o->x`); `None` for other expressions (calls, subscripts, literals), for the
    /// self reference and for paths rooted at it (`this.a.b()`: `b` has no root).
    pub receiver_root: Option<String>,
    /// The receiver is the language's self reference itself (`this.x`, `self.x`, `cls.x`,
    /// `$this->x`); false for deeper paths (`this.a.b`).
    pub self_receiver: bool,
}

/// What a [`TypeFact`] is about.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeSubject {
    /// Variable or parameter `name` bound in `scope` (`Scope::Decl(fn)` for parameters and
    /// locals, `Scope::Module` for module-level variables).
    Var { scope: Scope, name: String },
    /// Return type of callable declaration `decl`.
    Return { decl: u32 },
    /// Field / property `name` of type declaration `class`: typed fields and class-body
    /// bindings, `self.x = ...` / `this.x = ...` inside the type's methods, PHP properties
    /// (named without `$`, as in `$o->name`).
    Field { class: u32, name: String },
}

/// Where a [`TypeFact`] comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeSource {
    /// A type position of the language itself (annotation, declared parameter / return /
    /// field type).
    Declared,
    /// The bound value is a construction of the type, syntax only: `new Foo()`, Go `Foo{}` /
    /// `&Foo{}`, Rust `Foo { .. }`, `Foo::new()` / `Foo.new` / `Foo:new()` (`Self` names the
    /// enclosing type), and in languages whose construction is call syntax (Python, Scala)
    /// `Foo(...)` when `Foo` is a type declared in the file or its last segment starts with an
    /// uppercase letter (the languages' type naming convention).
    /// Consumers resolve `type_name` and ignore it when it names no type.
    Constructed,
    /// A documentation-comment annotation (JSDoc, PHPDoc).
    Comment,
}

/// A declared, constructed or annotated type (rules 10/11 of the general fixes plan).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeFact {
    pub subject: TypeSubject,
    /// The type spelling as written, generic arguments removed, nullable / optional markers
    /// (`?`, `| nil`, `Optional[...]`) stripped, qualification kept (`pkg.Foo`, `Foo::Bar`).
    /// Union spellings (`A|B`) are split into one fact per alternative.
    pub type_name: String,
    /// Span of the type spelling (inside the comment for [`TypeSource::Comment`]).
    pub span: ByteSpan,
    pub source: TypeSource,
}

/// A test block that is not a named declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestBlock {
    /// Test description string (literal text) or synthesized `file::line`.
    pub name: String,
    pub span: ByteSpan,
    pub line: u32,
    pub end_line: u32,
    /// Identifiers mentioned inside the block (sorted, deduplicated).
    pub mentions: Vec<String>,
}

/// Whether a declaration with `params` accepts a call with `args` positional arguments
/// (language arity rule). `receiver_call`: the call names the declaration through a receiver
/// (`obj.m(...)`, a constructor call binding the new instance), so a receiver parameter the
/// language spells in the list (Python `self` / `cls`, Rust `self`) is bound by the receiver,
/// not by an argument.
/// `Some(false)` only with positive evidence that no binding of the arguments exists;
/// `None` when the language rule cannot decide (unknown parameter lists, spreads, ...).
///
/// Contract for callers: `args` is the exact number of arguments of a call whose arguments
/// are all plain positional ones (no keyword / named argument, no spread / unpacked
/// argument, no block or trailing closure); `params` is the declaration's own parameter list
/// (`Declaration::parameters`), not one a decorator / attribute macro may have rewritten.
///
/// Rules (a parameter list is `required..=max` positional parameters: `required` = the
/// position of the last positional parameter without a default, `max` = every positional
/// parameter, unbounded with a variadic `*args` / `...xs` / `T...`; keyword-only and `**kw`
/// parameters bind no positional argument):
/// * Python: `required <= args <= max`. Through a receiver, a first parameter named `cls`
///   is bound by it; any other first parameter may be (`obj.m(a)`) or may not be
///   (`Class.m(obj, a)`, static methods) bound, so the call is accepted when either reading
///   accepts it.
/// * Java: `required <= args <= max` (no defaults; varargs; the receiver is never in the list).
/// * Rust: method-call syntax binds the `self` parameter; path calls (`Type::m(x, a)`) pass
///   it as an argument; `required <= args <= max`. Method-call syntax on a declaration without
///   `self` is left undecided.
/// * PHP: `args >= required` only (a user function silently accepts extra arguments).
/// * Every other language: `None`. JavaScript / TypeScript / R / Bash accept any count
///   at run time (or through optional parameters the list does not mark); Go parameter lists
///   name several parameters per declaration and multi-value calls fill several parameters;
///   C / C++ keep defaults on the declaration only and C-style `...` is not recorded;
///   Scala / C# have named, defaulted or variadic parameters the lists do not mark; Haskell
///   currying adds arguments syntax does not count.
pub fn arity_accepts(language: Language, params: &[Param], args: u32, receiver_call: bool) -> Option<bool> {
    /// (required, max) of a parameter list; `max` is `None` with a variadic parameter.
    fn bounds(params: &[Param]) -> (u32, Option<u32>) {
        let mut required = 0u32;
        let mut max = 0u32;
        let mut variadic = false;
        for p in params {
            match p.kind {
                ParamKind::Positional => {
                    max += 1;
                    if !p.has_default {
                        required = max;
                    }
                }
                ParamKind::VarPositional => variadic = true,
                ParamKind::KeywordOnly | ParamKind::VarKeyword => {}
            }
        }
        (required, (!variadic).then_some(max))
    }
    fn accepts(params: &[Param], args: u32) -> bool {
        let (required, max) = bounds(params);
        args >= required && max.is_none_or(|m| args <= m)
    }
    let first_positional = params.first().filter(|p| p.kind == ParamKind::Positional);
    match crate::languages::info(language).arity {
        Arity::ReceiverMayBindFirst { bound } => {
            if !receiver_call {
                return Some(accepts(params, args));
            }
            Some(match first_positional {
                Some(p) if p.name == bound => accepts(&params[1..], args),
                Some(_) => accepts(&params[1..], args) || accepts(params, args),
                None => accepts(params, args),
            })
        }
        Arity::Exact => Some(accepts(params, args)),
        Arity::ReceiverBinds { name } => {
            if !receiver_call {
                return Some(accepts(params, args));
            }
            match first_positional {
                Some(p) if p.name == name => Some(accepts(&params[1..], args)),
                _ => None,
            }
        }
        Arity::AtLeastRequired => Some(args >= bounds(params).0),
        Arity::Undecided => None,
    }
}

#[cfg(test)]
#[path = "../tests/unit/facts.rs"]
mod tests;
