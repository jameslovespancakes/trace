//! Semantic results: the output contract of `trace-semantic`, one [`FileSemantics`] per file.
//!
//! Owners are declaration indices local to the file; targets are stable symbol uids so a
//! file's cached results survive re-assembly as long as the targets still exist.

use serde::{Deserialize, Serialize};

use crate::languages::Language;
use crate::model::{ByteSpan, Diagnostic, EdgeKind, Provider, Resolution, UnresolvedKind};

/// Per-file semantic output of one backend run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileSemantics {
    pub provider: Provider,
    /// Backend tool fingerprint (tool versions + trace version); mismatch => requery.
    pub tool_fingerprint: String,
    /// Proven edges whose owner is in this file.
    pub edges: Vec<SemEdge>,
    /// Syntax call sites without a proven target.
    pub unresolved: Vec<SemUnresolved>,
    /// Resolved value references located in this file.
    pub value_refs: Vec<SemValueRef>,
    pub diagnostics: Vec<Diagnostic>,
    /// Implementations / overrides of this file's abstract, interface, trait, protocol and
    /// virtual members reported by the server (`textDocument/implementation`, type
    /// hierarchy subtypes), recorded on the *base* side because that is where the request
    /// is made. Assembly turns each into a proven `implements` / `overrides` edge from the
    /// implementing member to the base member (`at` = the implementor's name span,
    /// provider = this file's provider, resolution `implementation`); unknown uids are
    /// dropped like edge targets. SPEC §8.5a.
    #[serde(default)]
    pub implementations: Vec<SemImplementation>,
    /// Identifier spans of non-call uses (`Reference`s, callback arguments) that the server
    /// resolved, but not to an indexed declaration of that name: a library / builtin symbol,
    /// a local binding (a definition location inside a function body that maps to no
    /// declaration), or another symbol. Uses with no answer at all are not listed; neither
    /// are local bindings syntax already proves (`FileFacts::local_spans`, never asked).
    /// Sorted, unique. The `uses` name scan counts them as resolved elsewhere
    /// (`server`, SPEC §10.1). Calls use `unresolved` entries of kind
    /// `external_or_ambiguous` for the same purpose, except two kinds of calls listed here at
    /// their member identifier (the key syntax references use), which are not calls of any
    /// declaration: type conversions answered by a type outside the index in languages where
    /// calling a type converts (`T(x)` in Go), and bare commands the server's hooks find as
    /// external programs of this machine (shell).
    #[serde(default)]
    pub resolved_elsewhere: Vec<ByteSpan>,
    /// Function-type rule answers: declared parameter types of library callees
    /// that receive a function of this file.
    #[serde(default)]
    pub callback_params: Vec<SemCallbackParam>,
    /// Library files that calls of this file resolved into (indexed by `library_calls.file`).
    #[serde(default)]
    pub library_files: Vec<LibraryFile>,
    /// Calls answered only by locations outside the index (library / stdlib declarations).
    #[serde(default)]
    pub library_calls: Vec<SemLibraryCall>,
    /// The server says this file is not part of the build on this machine (Go build tags,
    /// cfg(target_os), other Scala version, other platform): reason. Its calls are unknown.
    #[serde(default)]
    pub outside_build: Option<String>,
    /// Macro expansions the server delivered (rust-analyzer `expandMacro`, §1.15).
    #[serde(default)]
    pub expanded: Vec<ExpandedMacro>,
    /// Calls of this file through an abstract / interface / protocol / virtual member that a
    /// *library* declares, with the in-index implementations the server reported for that
    /// member (`textDocument/implementation`). Dispatch turns each into a dispatch site whose
    /// family is the listed implementations (`Site::declared_library`); unknown uids are
    /// dropped like edge targets. The call itself keeps its `external_or_ambiguous` entry and
    /// its library call. Only concrete members are listed (interface members and stubs
    /// declare the contract); sorted by `at`.
    #[serde(default)]
    pub library_dispatch: Vec<SemLibraryDispatch>,
    /// Bases of this file's type declarations (declared outside any callable) that the
    /// server located outside the index: `at` = the base's name identifier, the declaration
    /// = the library class (`file` indexes `library_files`). Value flow looks members the
    /// repository hierarchy does not declare up in them. Sorted by `at`.
    #[serde(default)]
    pub library_bases: Vec<SemLibraryCall>,
}

/// A call of this file through a library-declared abstract member (dispatch into the index).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemLibraryDispatch {
    /// Local declaration index of the executing symbol.
    pub owner: u32,
    /// Callee span in this file.
    pub at: ByteSpan,
    pub line: u32,
    /// Library symbol of the abstract member (evidence text): the server's name for it when
    /// it gives one, else `<package>.<member>`.
    pub library_symbol: Option<String>,
    /// Uids of the in-index implementations / overrides of that member, sorted, unique.
    pub implementations: Vec<String>,
}

/// Declared type of the library parameter that receives a function of this file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemCallbackParam {
    /// Callee span of the receiving call.
    pub call: ByteSpan,
    /// `CallbackArg::arg_span`.
    pub arg: ByteSpan,
    pub param_name: Option<String>,
    /// Declared type text (evidence), `""` when unknown.
    pub param_type: String,
    pub verdict: FnTypeVerdict,
    /// "label" | "declaration" | "language_rule" | "checker"
    pub route: String,
    pub library_symbol: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FnTypeVerdict {
    FunctionType,
    NotFunctionType,
    TopType,
    Unknown,
}

/// A library file a call resolved into (read, never written).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryFile {
    /// Absolute path of the declaration file.
    pub path: String,
    pub package: String,
    pub version: Option<String>,
    pub stdlib: bool,
    /// Readable source (derivation possible) vs declarations only (`.d.ts`, `.pyi`, class files).
    pub readable: bool,
    pub language: Language,
}

/// A call of this file answered by a declaration in a library file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemLibraryCall {
    /// Callee span in this file.
    pub at: ByteSpan,
    pub line: u32,
    /// Index into `FileSemantics::library_files`.
    pub file: u32,
    pub decl_line: u32,
    pub decl_column: u32,
    pub symbol: Option<String>,
}

/// One macro expansion delivered by the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpandedMacro {
    /// Macro call span in this file.
    pub span: ByteSpan,
    pub text: String,
}

/// One server-reported implementation of a base member declared in this file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemImplementation {
    /// Local declaration index of the base member (trait / interface / protocol / abstract /
    /// virtual method) in this file.
    pub base: u32,
    /// Uid of the implementing / overriding member (any indexed file).
    pub implementor: String,
    /// `EdgeKind::Implements` (base is an interface / trait / protocol member or a stub) or
    /// `EdgeKind::Overrides` (base is a concrete virtual member).
    pub kind: EdgeKind,
}

/// A proven edge from a declaration in this file to any indexed symbol.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemEdge {
    /// Local declaration index of the executing symbol.
    pub owner: u32,
    /// Target symbol uid.
    pub target: String,
    /// Proven kinds only. Besides execution kinds, backends report resolved non-call uses
    /// (`references` = read as a value, `writes`, `imports`, `reexports`, `passes_callback`)
    /// keyed by the `Reference::kind` of the syntax use, owned by
    /// `FileFacts::executing_owner(reference.owner)`, and — when the server offers
    /// `textDocument/implementation` — `overrides` / `implements` edges owned by the
    /// overriding declaration of this file. Never `bridge` or inferred/possible kinds.
    pub kind: EdgeKind,
    /// Evidence span in this file (LSP `fromRanges` / callee / reference).
    pub at: ByteSpan,
    pub line: u32,
    pub resolution: Resolution,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemUnresolved {
    pub owner: Option<u32>,
    pub kind: UnresolvedKind,
    /// Callee span.
    pub at: ByteSpan,
    pub line: u32,
    pub callee: String,
    /// In-index candidate uids (ambiguous results), possibly empty.
    pub candidates: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemValueRef {
    /// Identifier span in this file.
    pub at: ByteSpan,
    pub line: u32,
    /// Resolved declaration uid.
    pub target: String,
}
