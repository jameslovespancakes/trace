//! trace-syntax: tree-sitter parsing and syntax fact extraction.
//!
//! One tree-sitter core (0.26) shared by every grammar. For each supported language a
//! tags-style query file (`queries/<lang>.scm`, capture contract in SPEC §6.2) finds
//! declarations, calls and references; the language's [`SyntaxSpec`] (one file per language
//! in [`languages`]: grammar, query, node-kind tables and language rules; the struct and its
//! shapes in [`spec`]) drives scope ownership, value-flow lowering ([`lower`]), types, imports
//! and call-site views. Adding a language = one `languages/<lang>.rs` file, its query and one
//! conformance fixture (`tests/unit/conformance.rs`). Python is first-class
//! ([`python`]): docstrings, overload groups, execution models, data-model operations.
//!
//! Accuracy facts for inference (SPEC §6.3): lambdas and generator expressions are synthetic
//! declarations (`<lambda>`, `<genexpr>`) with their creating scope and syntactic consumer
//! (`FileFacts::anonymous`); every call has a [`trace_core::facts::CallDetail`] (receiver
//! expression, positional/keyword argument slots with lowered values, callee path resolved
//! through imports and builtins); imports and qualified dotted names are recorded
//! (`FileFacts::imports`, `FileFacts::qualified_names`).
//!
//! General fixes (extractor 10): lexical scopes for every grammar give
//! `FileFacts::local_spans` / `Reference::local` and `FileFacts::member_accesses`
//! (`scopes`); declared and constructed types (`typefacts`) and documentation-comment
//! annotations (`annotations`: JSDoc, PHPDoc, read from comment nodes only)
//! give `FileFacts::types`; header base / trait / protocol spellings are `type` references;
//! prototypes and signatures are `is_stub` declarations; property-assigned functions are
//! declared for their receiver path.
//!
//! Output: [`trace_core::facts::FileFacts`] per file. Never executes or imports target code;
//! never uses regular expressions on source text.
//!
//! Parallelism: [`extract_many`] runs on rayon with one `tree_sitter::Parser` per worker
//! thread (thread-local), grammars and compiled queries are shared (`OnceLock`).
//!
//! Unsupported languages are never silently dropped: [`extract`] returns
//! [`SyntaxError::NoGrammar`] and the pipeline inventories the file as
//! `SupportLevel::Inventoried` with a `no_grammar` diagnostic.

mod callsite;
pub mod grammar;
pub mod header;
mod interface;
pub mod language_rules;
pub mod languages;
pub mod lower;
mod python;
pub mod similar;
pub mod spec;
pub mod testing;
pub mod uses;

mod annotations;
pub mod boundary;
mod detail;
mod edit;
mod extract;
mod guards;
mod names;
mod node;
mod parse;
mod scopes;
mod typefacts;

use trace_core::facts::FileFacts;
use trace_core::Language;

pub use callsite::{site_views, ArgumentView, Guard, SiteView};
pub use grammar::{grammar, grammar_error, Grammar};
pub use languages::syntax;
pub use similar::{fingerprints, jaccard, Fingerprint};
pub use spec::SyntaxSpec;
pub use uses::{result_use, ResultUse, UseKind, UseSite};

/// Version of the extraction rules. Bump whenever `FileFacts` content for the same input
/// may change; cached facts with another version are re-extracted.
/// 5: synthetic `<module>` + anonymous scopes in every language, `Reference::kind`,
/// `FileFacts::{module_decl, exports, boundaries}`.
/// 6: `new K(..)` allocations lower their type name (`Expr::Name`) instead of an opaque value.
/// 7: wasm-bindgen exports: `returns_self` on functions returning the exported type, and
/// one export per variant of an exported C-style enum (`Cell.Alive`); Python path constants
/// (`CONST` / `INSTANCE` http facts) and `prefix_ref` on non-literal mount prefixes; gRPC
/// calls on stubs created inline by the generated factory (`pb.NewXClient(conn).M()`).
/// 8: Go composite literals `T{..}` lower as allocations of `T` (`literal_allocations`);
/// cgo `//export name` above a Go function provides the C symbol `name` (`c_abi` fact).
/// 9: Haskell point-free bindings and backticked infix calls; Bash `source` of variable paths.
/// 10: `FileFacts::{types, local_spans, member_accesses}` (declared / constructed /
/// comment-annotated types, lexical shadowing for every language, member accesses),
/// declarations for signatures and prototypes (`is_stub`), conformance facts (Haskell
/// instances, extension containers, header `type` references),
/// property-assigned functions named by their receiver path (general fixes plan, package S).
/// 11 (trace launch): `CallbackArg::{index, keyword}`, `FileFacts::interface`, header
/// language in `FileFacts::language`.
/// 12: `CallbackArg::{index, keyword}` filled for every language (block arguments keyword
/// `&`), callback forms (`SyntaxSpec::callback_forms`: method / callable references,
/// `&f`, scoped paths, PHP `f(...)`), interface fingerprint from the extractor's tree.
/// 15 (language fixes): calls of types that convert a value are not calls
/// (`spec::type_call_is_conversion`) and the syntax fixes of the language-fixes workflow.
/// 16: a named function expression is a function value (`Lambda` designates it).
/// 17: seven languages without a grammar any more (the `Language` enum and
/// `TypeSubject` changed shape; cached facts of every language are re-extracted).
/// 18: a Go parameter declaration naming several parameters (`a, b string`) declares each.
/// 19 (merged): Scala eta expansion is the grammar's `method_value` node (callbacks `f _` /
/// `obj.m _` again); Bash case labels render `subject == label`; a `.h` header testing
/// whether `__cplusplus` is defined is C per file (with 18's Go parameter rule).
/// 20: a module-level JavaScript `module.exports = require('<spec>')` is a whole-module
/// re-export (`FileFacts::exports` `*`, like `export * from`).
/// 21: initialized Python module assignments have separate data-definition spans; no
/// new callable symbols or graph ownership changes.
/// 22: separate identifier bags from parsed body spans, excluding declaration headers.
pub const EXTRACTOR_VERSION: u32 = 23;

/// Maximum depth of lowered [`trace_core::facts::Expr`] trees (deeper -> `Opaque`).
pub(crate) const MAX_EXPR_DEPTH: usize = 32;

/// Maximum bytes kept for a docstring / leading comment.
pub(crate) const MAX_DOC_BYTES: usize = 1_200;

#[derive(Debug, thiserror::Error)]
pub enum SyntaxError {
    #[error("no grammar compiled in for {0}")]
    NoGrammar(Language),
    #[error("tree-sitter rejected the {language} grammar: {message}")]
    Grammar { language: Language, message: String },
    #[error("invalid {language} query: {message}")]
    Query { language: Language, message: String },
    #[error("parsing {path} timed out after {ms} ms")]
    Timeout { path: String, ms: u64 },
    #[error("parsing {path} failed")]
    ParseFailed { path: String },
}

/// Input for batch extraction.
#[derive(Clone, Copy, Debug)]
pub struct SourceInput<'a> {
    /// Repository-relative path (used for test-file conventions and `.pyi` detection).
    pub path: &'a str,
    pub language: Language,
    /// Exact file bytes (BOM/CRLF preserved; spans index into these bytes).
    pub source: &'a [u8],
}

/// Extract facts from one file.
///
/// Files that parse with errors still yield facts for valid regions (`error_count > 0`);
/// only grammar absence, timeouts and hard parser failures are errors.
pub fn extract(input: SourceInput<'_>) -> Result<FileFacts, SyntaxError> {
    extract::extract_file(input)
}

/// Parse `source` with the grammar of `language` (for readers of source-like manifests and
/// library files; spans index into `source`).
pub fn parse_tree(language: Language, source: &[u8]) -> Result<tree_sitter::Tree, SyntaxError> {
    let grammar = grammar(language).ok_or(SyntaxError::NoGrammar(language))?;
    crate::parse::parse(grammar, "", source)
}

/// Extract many files in parallel (rayon). Output order matches input order.
pub fn extract_many(inputs: &[SourceInput<'_>]) -> Vec<Result<FileFacts, SyntaxError>> {
    use rayon::prelude::*;
    inputs.par_iter().map(|i| extract(*i)).collect()
}

/// True when `path` follows a test-file convention for `language` (SPEC §9.6), after the
/// project's own runner configuration `project` (`testing::TestConfig`).
pub fn is_test_path(path: &str, language: Language, project: &testing::TestConfig) -> bool {
    testing::is_test_path(path, language, project)
}

#[cfg(test)]
#[path = "../tests/unit/support/mod.rs"]
pub(crate) mod test_support;

/// One conformance fixture and test per language.
#[cfg(test)]
#[path = "../tests/unit/conformance.rs"]
mod conformance_tests;

/// Rule tests of the extraction (one test per rule, every language it applies to).
#[cfg(test)]
#[path = "../tests/unit/rules.rs"]
mod rules_tests;
