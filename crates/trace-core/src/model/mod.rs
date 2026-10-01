//! The assembled, persisted index model: symbols, edges, sites, decisions.
//!
//! Dense ids ([`FileId`], [`SymbolId`], [`EdgeId`]) are positions in the index vectors and are
//! reassigned on every assembly. Anything persisted *across* builds (semantic cache, library
//! knowledge) uses stable [`Symbol::uid`] strings instead.

use std::fmt;
use std::ops::Range;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::facts::FileFacts;
use crate::fingerprint::Hash32;
use crate::languages::{Language, LanguageSupport, SupportLevel};
use crate::semantics::FileSemantics;

mod edges;
mod sites;

pub use edges::*;
pub use sites::*;

macro_rules! dense_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub struct $name(pub u32);
        impl $name {
            #[inline]
            pub const fn idx(self) -> usize { self.0 as usize }
        }
    };
}

dense_id!(
    /// Index into [`Index::files`].
    FileId
);
dense_id!(
    /// Index into [`Index::symbols`].
    SymbolId
);
dense_id!(
    /// Index into [`crate::Graph::edges`] (materialized edges of all tiers).
    EdgeId
);

/// Half-open byte range `[start, end)` into raw file bytes (BOM included).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ByteSpan {
    pub start: u32,
    pub end: u32,
}

impl ByteSpan {
    #[inline]
    pub const fn new(start: u32, end: u32) -> Self {
        ByteSpan { start, end }
    }
    #[inline]
    pub const fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.end <= self.start
    }
    /// `start <= byte < end`.
    #[inline]
    pub const fn contains(self, byte: u32) -> bool {
        self.start <= byte && byte < self.end
    }
    /// `self` fully encloses `other`.
    #[inline]
    pub const fn encloses(self, other: ByteSpan) -> bool {
        self.start <= other.start && other.end <= self.end
    }
    #[inline]
    pub fn range(self) -> Range<usize> {
        self.start as usize..self.end as usize
    }
}

/// Byte span plus 1-based inclusive display lines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Span {
    pub bytes: ByteSpan,
    pub start_line: u32,
    pub end_line: u32,
}

/// A position in an indexed file: byte span plus 1-based line of `bytes.start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Location {
    pub file: FileId,
    pub bytes: ByteSpan,
    pub line: u32,
}

/// Kinds of declarations that become symbols.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    /// Free function (incl. named arrow/function expressions bound to a name).
    Function,
    /// Function lexically inside a class/impl/interface (incl. Python `__init__`).
    Method,
    /// Language-level constructor declaration (JS/TS `constructor`, Java/C#/C++ ctors).
    Constructor,
    /// Class, struct, enum, record, object, module-like type container.
    Class,
    /// Interface, trait, protocol (Python `Protocol` classes remain `Class` + base spelling).
    Interface,
    /// Synthetic `<module>` declaration of a file (`FileFacts::module_decl`): the executing
    /// owner of module-level / class-body code (imports, top-level calls, registrations).
    /// Never a type, never callable by name; edges *from* it are ordinary facts.
    Module,
}

impl SymbolKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Constructor => "constructor",
            SymbolKind::Class => "class",
            SymbolKind::Interface => "interface",
            SymbolKind::Module => "module",
        }
    }
    /// Functions, methods, constructors and the synthetic `<module>` scope: symbols that can
    /// own executed code (edges, unresolved calls, sites).
    pub const fn is_executable(self) -> bool {
        self.is_callable() || matches!(self, SymbolKind::Module)
    }
    /// Functions, methods and constructors.
    pub const fn is_callable(self) -> bool {
        matches!(self, SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor)
    }
    /// Classes and interfaces.
    pub const fn is_type(self) -> bool {
        matches!(self, SymbolKind::Class | SymbolKind::Interface)
    }
}

/// How calling a function executes its body (port of codepath `execution_model`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionModel {
    #[default]
    Ordinary,
    Generator,
    Coroutine,
    AsyncGenerator,
}

/// A declared symbol, assembled from a syntax [`crate::facts::Declaration`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Symbol {
    pub id: SymbolId,
    /// Stable identity: `"{path}:{qualified_name}"`, suffixed `"#{k}"` (k >= 2) for the k-th
    /// same-named declaration in one file (redefinitions). Also the CLI "exact id".
    pub uid: String,
    pub file: FileId,
    /// Declaration index within the file's [`FileFacts::declarations`].
    pub decl: u32,
    pub name: String,
    /// Dotted lexical path within the file, e.g. `Session.login`, `outer.inner`.
    pub qualified_name: String,
    pub kind: SymbolKind,
    pub language: Language,
    /// Full declaration span including decorators / attributes / export wrappers.
    pub span: Span,
    /// The declared identifier.
    pub name_span: ByteSpan,
    /// Byte where the body begins (signature = `span.bytes.start..body_start`).
    pub body_start: u32,
    /// Lexically enclosing declaration (class for methods, function for nested functions).
    pub parent: Option<SymbolId>,
    /// Receiver/impl type name when not lexically nested (Rust `impl X`, Go `func (s *X)`).
    pub container: Option<String>,
    /// Docstring or adjacent leading comment, at most 1200 bytes.
    pub doc: Option<String>,
    /// Decorator / attribute / annotation source texts, outermost first.
    pub decorators: Vec<String>,
    /// Base class / implemented interface spellings (source text).
    pub bases: Vec<String>,
    /// Positional then keyword-only parameter names (variadics excluded).
    pub parameters: Vec<String>,
    pub execution: ExecutionModel,
    /// Declaration without implementation: abstract/interface/protocol method, `.pyi` stub,
    /// `@overload` signature, trait method without default body, C prototype.
    pub is_stub: bool,
    /// Test function/method (by language test conventions).
    pub is_test: bool,
    /// Additional 1-based lines at which this declaration is also declared (Python overload
    /// group: all `@overload` lines map to the implementation).
    pub declaration_lines: Vec<u32>,
    /// A semantic backend covered this symbol's file in the last build.
    pub semantic: bool,
}

impl Symbol {
    /// Synthetic scopes (`<module>`, `<lambda>`, `<genexpr>`): names that are never valid
    /// identifiers. Hidden from search, context, overview and bare-name selectors; shown as
    /// owners of call sites (`src/app.ts:<module>` line 42).
    pub fn is_synthetic(&self) -> bool {
        self.name.starts_with('<')
    }
}

/// Free-form, bounded diagnostic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Machine kind, e.g. `syntax_error`, `unmapped_declaration`, `backend_unavailable`.
    pub kind: String,
    pub file: Option<String>,
    pub message: String,
}

impl Diagnostic {
    pub fn new(kind: impl Into<String>, file: Option<String>, message: impl Into<String>) -> Self {
        Diagnostic {
            kind: kind.into(),
            file,
            message: message.into(),
        }
    }
}

/// One semantic backend execution during a build.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BackendRun {
    /// `pyright`, `typescript`, `rust-analyzer`, `lsp:gopls`, ...
    pub backend: String,
    pub languages: Vec<Language>,
    /// Files in the backend's partition (all were in the snapshot).
    pub files: u32,
    /// Files whose results were (re)queried in this run.
    pub queried_files: u32,
    pub requests: u64,
    pub seconds: f64,
    pub ok: bool,
    pub error: Option<String>,
    pub tool_version: Option<String>,
    /// Readiness of the server before queries (SPEC §8.8): `Some(true)` = it signalled
    /// readiness (progress end, `language/status` ready, quiescent), `Some(false)` = the
    /// readiness wait timed out (results may be incomplete; `status` warns
    /// `server_not_ready`), `None` = the server offers no readiness signal.
    #[serde(default)]
    pub ready: Option<bool>,
}

/// A file excluded from indexing, with the reason (never silently dropped).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmittedFile {
    pub path: String,
    /// `symlink`, `file_size_limit`, `sensitive`, `non_utf8_path`, `unsupported_path`,
    /// `unreadable`, `forbidden_root` (see [`crate::inventory`]).
    pub reason: String,
}

/// One indexed source file with its cached per-file analysis results.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileRecord {
    /// Repository-relative, `/`-separated.
    pub path: String,
    pub language: Language,
    /// blake3 of the exact bytes.
    pub hash: Hash32,
    pub size: u64,
    /// Modification time (ns since UNIX epoch; 0 if unavailable). Freshness shortcut only.
    pub mtime_ns: u64,
    pub support: SupportLevel,
    /// Syntax facts; `None` when no grammar is compiled in or parsing failed. Persisted by
    /// [`crate::cache::save_index`] as a separate per-file block (decoded in parallel), not
    /// as part of the record.
    #[serde(skip)]
    pub facts: Option<FileFacts>,
    /// Semantic results; `None` when no backend covered this file. Persisted with `facts`.
    #[serde(skip)]
    pub semantic: Option<FileSemantics>,
    /// First symbol id of this file; symbols of a file are contiguous in decl order.
    pub first_symbol: u32,
    pub symbol_count: u32,
    pub diagnostics: Vec<Diagnostic>,
    /// `SupportLevel::Pending` files: why the file is not analysed yet (pending language or
    /// sub-project; set up on first use). No `skip_serializing_if` (postcard layout).
    #[serde(default)]
    pub pending: Option<String>,
}

impl FileRecord {
    /// Symbol id of declaration `decl` in this file.
    pub fn symbol_of_decl(&self, decl: u32) -> Option<SymbolId> {
        (decl < self.symbol_count).then(|| SymbolId(self.first_symbol + decl))
    }
}

/// Versions and fingerprints that decide cache validity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IndexHeader {
    pub schema: u32,
    pub trace_version: String,
    /// Canonical absolute root (display form, no `\\?\` prefix).
    pub root: String,
    pub built_unix: f64,
    /// `trace_syntax::EXTRACTOR_VERSION` used for `facts`.
    pub syntax_version: u32,
    /// `trace_infer::INFER_VERSION` used for `sites`.
    pub infer_version: u32,
    /// `trace_bridge::BRIDGE_VERSION` used for `bridges`.
    pub bridge_version: u32,
    /// blake3 over sorted (path, hash) of sources + configuration files.
    pub inventory_fingerprint: Hash32,
    /// Count of full (non-incremental) builds and of incremental updates since the last full one.
    pub full_builds: u32,
    pub incremental_updates: u32,
}

/// The complete persisted index. Stored as `index.bin` (see [`crate::cache`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Index {
    pub header: IndexHeader,
    /// Sorted by path.
    pub files: Vec<FileRecord>,
    /// Configuration files fingerprinted (not parsed, not executed): (path, hash).
    pub configs: Vec<(String, Hash32)>,
    pub omitted: Vec<OmittedFile>,
    pub symbols: Vec<Symbol>,
    /// Proven edges only (execution, deferred and reference kinds). Sorted, deduplicated.
    pub edges: Vec<Edge>,
    pub unresolved: Vec<Unresolved>,
    pub value_refs: Vec<ValueRef>,
    pub sites: Vec<Site>,
    /// `decisions[i].site == i`.
    pub decisions: Vec<Decision>,
    /// Cross-language links (sorted by (kind, from, to, from_at)); materialized as
    /// `EdgeKind::Bridge` edges by `Graph::new`.
    pub bridges: Vec<Bridge>,
    pub support: Vec<LanguageSupport>,
    pub backend_runs: Vec<BackendRun>,
    pub diagnostics: Vec<Diagnostic>,
    /// Opaque per-phase incremental state (`crate::delta::PhaseState`).
    #[serde(default)]
    pub phase_state: Vec<crate::delta::PhaseState>,
    /// Files whose semantics are stale after an interface change of a dependency (resolved
    /// in the following `index --watch` batches or by the next command).
    #[serde(default)]
    pub stale: std::collections::BTreeSet<String>,
    /// Call sites whose receiver value provably comes only from library-created objects
    /// (value flow). Sorted by location. Filled by flow, read by completeness and status.
    #[serde(default)]
    pub library_receivers: Vec<LibraryReceiver>,
}

/// A call site whose receiver is only ever an object created by a library.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LibraryReceiver {
    /// Callee span of the call.
    pub at: Location,
    /// Library (qualified symbol / package text) that creates the receiver object (evidence).
    pub library: String,
}

impl Index {
    #[inline]
    pub fn symbol(&self, id: SymbolId) -> &Symbol {
        &self.symbols[id.idx()]
    }
    #[inline]
    pub fn file(&self, id: FileId) -> &FileRecord {
        &self.files[id.idx()]
    }
    #[inline]
    pub fn file_path(&self, id: FileId) -> &str {
        &self.files[id.idx()].path
    }
    /// Symbols declared in `file`, in declaration order.
    pub fn symbols_of(&self, file: FileId) -> &[Symbol] {
        let f = self.file(file);
        let a = f.first_symbol as usize;
        &self.symbols[a..a + f.symbol_count as usize]
    }
    /// Binary search by path (files are sorted by path).
    pub fn file_by_path(&self, path: &str) -> Option<FileId> {
        self.files
            .binary_search_by(|f| f.path.as_str().cmp(path))
            .ok()
            .map(|i| FileId(i as u32))
    }
    /// Innermost symbol of `file` whose span contains `byte`.
    pub fn symbol_at(&self, file: FileId, byte: u32) -> Option<SymbolId> {
        self.symbols_of(file)
            .iter()
            .filter(|s| s.span.bytes.contains(byte))
            .min_by_key(|s| s.span.bytes.len())
            .map(|s| s.id)
    }
    /// The decision for site `i`, if decisions are present.
    pub fn decision(&self, site: u32) -> Option<&Decision> {
        self.decisions.get(site as usize)
    }

    /// Structural consistency check of every dense id and the proven-only invariant of
    /// [`Index::edges`].
    ///
    /// Run on every loaded cache so that a structurally inconsistent index (checksum valid
    /// but produced by a buggy or foreign writer) is rebuilt instead of causing index panics.
    /// Candidate-subset rules for decisions are enforced where edges are materialized
    /// ([`crate::Graph::new`]), so they can never create links outside a site's candidates.
    /// Errors are [`crate::CoreError::CacheCorrupt`].
    pub fn validate(&self) -> crate::error::Result<()> {
        use crate::error::CoreError;
        let fail = |what: String| -> crate::error::Result<()> { Err(CoreError::CacheCorrupt(what)) };
        let n_files = self.files.len();
        let n_symbols = self.symbols.len();
        let sym_ok = |id: SymbolId| id.idx() < n_symbols;
        let loc_ok = |loc: &Location| loc.file.idx() < n_files && loc.bytes.start <= loc.bytes.end;

        if !self.files.windows(2).all(|w| w[0].path < w[1].path) {
            return fail("files are not strictly sorted by path".into());
        }
        let mut expected_first = 0u32;
        for (fi, f) in self.files.iter().enumerate() {
            if f.first_symbol != expected_first {
                return fail(format!("{}: symbol range is not contiguous", f.path));
            }
            let end = f.first_symbol as usize + f.symbol_count as usize;
            if end > n_symbols {
                return fail(format!("{}: symbol range out of bounds", f.path));
            }
            for (k, s) in self.symbols[f.first_symbol as usize..end].iter().enumerate() {
                if s.file.idx() != fi || s.decl as usize != k {
                    return fail(format!("{}: symbol {} misplaced", f.path, s.uid));
                }
            }
            expected_first = end as u32;
        }
        if expected_first as usize != n_symbols {
            return fail("symbols not covered by file ranges".into());
        }
        for (i, s) in self.symbols.iter().enumerate() {
            if s.id.idx() != i || s.parent.is_some_and(|p| !sym_ok(p)) {
                return fail(format!("symbol {} has an invalid id or parent", s.uid));
            }
        }
        for e in &self.edges {
            if !sym_ok(e.from) || !sym_ok(e.to) || !loc_ok(&e.at) {
                return fail("edge endpoint or location out of range".into());
            }
            if e.tier != e.kind.tier()
                || e.tier != Tier::Proven
                || e.site.is_some()
                || e.bridge.is_some()
                || e.kind == EdgeKind::Bridge
            {
                return fail(format!("stored edge {} is not a proven fact", e.kind));
            }
        }
        for b in &self.bridges {
            let contract_ok = b.contract.is_none_or(|f| f.idx() < n_files);
            if !sym_ok(b.from)
                || !sym_ok(b.to)
                || !loc_ok(&b.from_at)
                || !loc_ok(&b.to_at)
                || !contract_ok
                || b.tier < b.kind.max_tier()
            {
                return fail(format!("bridge {} {} is inconsistent", b.kind, b.label));
            }
        }
        for u in &self.unresolved {
            if u.owner.is_some_and(|o| !sym_ok(o))
                || !u.candidates.iter().all(|&c| sym_ok(c))
                || !loc_ok(&u.at)
            {
                return fail("unresolved entry out of range".into());
            }
        }
        for r in &self.value_refs {
            if !sym_ok(r.target) || !loc_ok(&r.at) {
                return fail("value reference out of range".into());
            }
        }
        for s in &self.sites {
            let ids_ok = sym_ok(s.owner)
                && s.declared_target.is_none_or(sym_ok)
                && s.candidates.iter().all(|&c| sym_ok(c))
                && s.flow_candidates.iter().all(|&c| sym_ok(c))
                && s.field_only.iter().all(|&c| sym_ok(c));
            if !ids_ok || !loc_ok(&s.at) {
                return fail(format!("site {} is inconsistent", s.id));
            }
        }
        if !self.decisions.is_empty() {
            if self.decisions.len() != self.sites.len() {
                return fail("decisions not aligned with sites".into());
            }
            for (i, (d, s)) in self.decisions.iter().zip(&self.sites).enumerate() {
                let ids_ok = d.targets.iter().all(|&t| sym_ok(t));
                if d.site as usize != i || !ids_ok {
                    return fail(format!("decision for site {} is inconsistent", s.id));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../tests/unit/model/mod.rs"]
mod tests;
