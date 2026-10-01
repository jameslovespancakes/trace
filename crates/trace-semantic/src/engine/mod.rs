//! The shared LSP analysis algorithm (Pyright steps 1–6; the generic backend's call hierarchy /
//! definition fallback), independent of process management so it can be tested with a
//! scripted session. Process pools and persistent sessions (`pool.rs`) call [`analyze`] once
//! per process with that process's shard of files; documents are opened by the pool.
//!
//! Every result is mapped onto syntax declarations through [`DeclTable`]; nothing is
//! invented: a call site either gets exactly one in-index target (proven edge), an explicit
//! `external_or_ambiguous` entry (targets outside the index or more than one), or an explicit
//! `no_semantic_target` entry (the server returned nothing covering it).
//!
//! Request plan per shard (two pipelined round trips instead of one per phase):
//! 1. Declaration positions come from syntax (`name_span`); `documentSymbol` is only asked for
//!    files whose syntax tree had errors (the server's symbols then report declarations the
//!    syntax missed as `unmapped_declaration`). Each position is still validated by
//!    `prepareCallHierarchy` mapping back to the same declaration.
//! 2. Batch A: `documentSymbol` (files with syntax errors), `prepareCallHierarchy` for every
//!    named callable (never synthetic declarations), `definition` at the member identifier
//!    of every call no prepared item covers (owner `<module>` — module and class-body code —
//!    or a synthetic scope without a named callable ancestor; every call when the server has
//!    no call hierarchy), and `definition` for callback arguments and non-call uses — except
//!    bare names syntax proves are local variables (`Reference::local`: bound only as
//!    parameters/variables of their function), whose definition is their own binding.
//! 3. Batch B: `callHierarchy/outgoingCalls` for every uniquely prepared item.
//! 4. Batch C (definition fallback): calls of prepared scopes that no outgoing-call range
//!    covered (servers other than Pyright) and calls of declarations whose prepare did not
//!    map back uniquely (all servers) get `definition` at their member identifier.
//!
//! Point-to-call matching: an outgoing-call range designates the syntax call whose callee
//! ENDS where the range ends (the member identifier of `obj.m()`, `Type::f()`, every step of
//! a builder chain `b.x().y().z()`), else the smallest callee containing the range start.
//! A syntax call is covered only when a range (or a definition answer) designates it.
//!
//! Non-call uses (`Reference::kind`): a unique in-index definition target becomes an edge
//! owned by `FileFacts::executing_owner(reference.owner)` — `read` / `decorator` / `type` ->
//! `references`, `write` -> `writes`, `import` -> `imports`, `export` -> `reexports`
//! (`argument` uses are the callback path's `passes_callback`) — and reads stay
//! [`trace_core::semantics::SemValueRef`]s (flow input).
//!
//! Attribution: each outgoing-call range is attributed to the syntax *executing* owner of the
//! point. Ranges executed by a declaration nested in the queried one (a nested function or a
//! synthetic `<lambda>` / `<genexpr>` scope) become edges of that nested declaration (the
//! resolution is the same compiler fact; only the executing scope differs). Ranges in lazy
//! scopes without any declaration (owner `None`: lambda bodies, generator elements and class
//! bodies when the syntax has no synthetic scope) cannot be edges; a unique in-index target
//! is recorded as a [`trace_core::semantics::SemValueRef`] at the callee's member identifier (the key syntax
//! references use; diagnostic `lazy_scope_call`), so inference can use the compiler's
//! resolution when it models when that scope runs.
//! Synthetic declarations are never sent to `prepareCallHierarchy` (they have no name).
//!
//! Implementations (engine 6, SPEC section 8.5a): batch A also asks
//! `textDocument/implementation` at the name of every implementable member of the queried
//! files ([`implementable`]: a callable member of an interface / trait / protocol, a stub
//! member, or one with an `abstract` / `virtual` / `open` modifier) when the server
//! advertises `implementationProvider`, and `textDocument/prepareTypeHierarchy` for every
//! type with callable members when it advertises `typeHierarchyProvider`; the first
//! `typeHierarchy/subtypes` round rides with batch B, deeper rounds (a subtype that does not
//! declare a member itself is searched further, up to [`MAX_TYPE_DEPTH`]) follow. Every
//! location / subtype member that maps onto a syntax declaration of the base member's name
//! becomes a [`trace_core::semantics::SemImplementation`] of the base file (`implements` for interface / stub
//! bases, else `overrides`); other locations are ignored.
//!
//! Resolved elsewhere: a non-call use (reference, callback argument) whose definition
//! answer has locations but none is a declaration of that name (a library / builtin symbol,
//! a local binding, another symbol) is recorded in `FileSemantics::resolved_elsewhere`.
//!
//! Syntax answers (SPEC section 8.8; `Options::syntax_answers`, identical edges): no
//! `definition` request is sent for
//! 1. names in `FileFacts::local_spans` (their definition is the local binding: references
//!    and callbacks are skipped, calls are recorded `external_or_ambiguous`, diagnostic
//!    `local_call`);
//! 2. a bare call whose name has exactly one declaration in the partition, in the same file,
//!    visible under the language's scoping (`LanguageRules::bare_call_binding`: Bash functions, file-level
//!    R / PHP functions; never member accesses): the edge is emitted with
//!    resolution `syntax_definition` (diagnostic `syntax_definition`);
//! 3. names no declaration of the partition carries: non-call references are skipped (their
//!    answer could only be dropped), calls are recorded `external_or_ambiguous` without
//!    candidates (diagnostic `external_by_name`): the definition path keeps only targets
//!    named like the call's member, so no answer could have produced an edge. The same for a
//!    bare call whose every declaration is a nested function out of scope at the call
//!    ([`crate::engine::rules::scoping`]: the compiler can never resolve the name to one of them).
//!
//! Shell scripts follow the server's scoping (bash-language-server with
//! `includeAllWorkspaceSymbols: false`): a bare call's candidate declarations are those of
//! the script and of the files it sources, transitively ([`crate::cache::ShellScopes`], an
//! over-approximation of the server's source resolution). Rule 2 applies when exactly one
//! candidate exists and it is in the calling script (then the server's answer is that one
//! declaration); rule 3 when no candidate exists.
//!
//! Request diet (DESIGN section 1.14.1, engine 7): `textDocument/implementation` is asked only for
//! implementable members whose name another callable of the partition declares, and
//! `prepareTypeHierarchy` only for types with such a member: every other answer maps to
//! nothing (implementations are always named like their base). `definition` at call sites
//! instead of `prepareCallHierarchy` + `outgoingCalls` for backends listed in
//! [`DEFINITION_CALL_BACKENDS`] or the A/B setting `debug.calls_by_definition` (the verify
//! stage measures both per server and records the winner in that list; Pyright always uses
//! call hierarchy).
//!
//! Declaration reuse (`Options::reuse`, [`crate::cache::FileReuse`]): nothing is asked
//! inside reused units (no prepare, definition, callback or reference request, no blind-site
//! entry, no function-type query) and implementations reused whole are not asked; the reused
//! answers are merged before the results are sorted, so the output equals a full analysis.
//! Every analysed file without syntax errors returns its raw answers per unit
//! ([`Analysis::answers`]) for the next reuse.
//!
//! Function-type rule: `fntype::resolve` runs once per callback argument or anonymous
//! function argument (`fntype::anonymous_arguments`) whose receiving call has no in-index
//! target. Its first requests are recorded in a dry run and sent as one
//! pipelined batch; the real run then answers them from that batch (later hops are asked
//! one at a time and memoised).
//!
//! Answer mapping rules of engine 8 (language fixes):
//! * Library calls from call hierarchy: an outgoing-call `to` item outside the index is
//!   classified like a definition location (`external::classify`) and recorded as a library
//!   call of the matched syntax call.
//! * Whole-invocation ranges (jdtls, Roslyn: `fromRanges` cover `m(args)` or `o.m(args)`)
//!   designate the call whose whole expression ends where the range ends; the edge `at` is
//!   that call's callee span, like every definition answer.
//! * Calls carrying a callback argument are always asked (rule 3 does not apply): library
//!   behaviour needs the library target. A server without an answer leaves the rule-3 result.
//! * Library dispatch: a call with a receiver answered only outside the index asks
//!   `textDocument/implementation` once per distinct library declaration (request diet: only
//!   when a method of the partition carries the member name; bounded by
//!   `semantic.max_library_dispatch_requests`); concrete in-index members named like the member
//!   become `FileSemantics::library_dispatch`.
//! * Type conversions (`trace_syntax::spec::type_call_is_conversion`): a call answered by a
//!   type declaration is a type use, `references` in the index and resolved elsewhere outside.
//! * Alias locations: a definition location inside an indexed file that maps to no
//!   declaration (a using-declaration, an alias line) never makes an answer external when
//!   another location maps to a declaration named like the member.
//! * Scala applications (`crate::engine::rules::scala_apply`): several in-index targets of one call made of
//!   `apply` methods and the types the callee names keep the `apply` methods; a class and
//!   its companion object (neither declaring nor inheriting an `apply`) keep the class
//!   (case class / creator application). Call hierarchy and definition answers alike.
//! * Overloads: several in-index targets are narrowed by the call's argument count
//!   (`trace_core::facts::arity_accepts`); exactly one left is the proven target.
//! * Files outside the build (`Server::outside_build_file`) are not asked at all; calls
//!   in the server's inactive preprocessor regions are `inactive_code`; unanswered calls
//!   through a receiver typed by a template parameter are `template_dependent`; bare commands
//!   the hooks find as external programs are resolved elsewhere.
//! * C / C++ ([`rules::cpp_calls`], engine 12): a function-like macro invocation calls what its
//!   expansion calls and is answered by `definition` (its own target is the macro); several
//!   targets are narrowed by the implicit-call and argument-list rules, and one left is
//!   proven only when the language's lookup makes the candidate set complete.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};
use trace_core::facts::{CallSite, CallbackArg, FileFacts};
use trace_core::model::{ByteSpan, EdgeKind, Provider, UnresolvedKind};
use trace_core::semantics::FileSemantics;

use crate::backend::SemanticFile;
use crate::cache::{reuse_units, FileAnswers, FileReuse, ShellScopes};
use crate::languages::{Prepared, Server};
use crate::lsp::{capability_enabled, LspClient};
use crate::mapping::DeclTable;
use crate::snapshot::Snapshot;
use crate::SemanticError;

mod answers;
mod batch_a;
mod batch_b;
mod library_dispatch;
mod plan;
pub(crate) mod rules;

use self::answers::*;
use self::batch_a::*;
use self::batch_b::*;
use self::library_dispatch::*;
use self::plan::*;
use self::rules::cpp_templates::*;
use self::rules::preprocessor::*;

pub(crate) use self::batch_a::{activation_kind, use_edge_kind};
pub(crate) use self::plan::implementable;

/// Version of this algorithm; part of the LSP backends' tool fingerprints so cached results
/// computed by an earlier algorithm are re-queried.
/// 4: `<module>` / synthetic-scope attribution, point-to-call matching by member end,
/// definition fallback, non-call use edges, generated rust-project.json.
/// 5: generated project configuration for clangd (`compile_flags.txt`) and jdtls (Eclipse
/// `.project`/`.classpath`, `-data` outside the snapshot) (`generic::generated_configs`).
/// 6: `FileSemantics::implementations` (textDocument/implementation, type hierarchy),
/// same-file definitions answered from syntax (`syntax_definition`), no definition requests
/// for local bindings (`FileFacts::local_spans`).
/// 7: request diet (implementation / type hierarchy only with a same-named callable
/// elsewhere), shell rules 2/3 over sourced scopes, per-declaration answer reuse, library
/// calls with canonical URIs, function-type rule requests.
/// 8 (language fixes): library dispatch answers, files outside the build, external programs,
/// template-dependent and inactive-code calls, arity filtering.
/// 9: calls passing a string (a possible channel key) are always asked.
/// 10: seven languages removed (their servers, answer policies and definition trust).
/// 11: outgoing calls of a bodiless declaration owning no syntax call are not asked; jdtls
/// answers call hierarchy without implementor search.
/// 12: bases of type declarations outside callables are asked also when no indexed
/// declaration carries their name; library answers are `FileSemantics::library_bases`.
/// C / C++ macro invocations, implicit calls and overloads by argument list (`cpp_calls`).
/// Rule 3 also for bare calls whose every declaration is a nested function out of scope at
/// the call (`crate::engine::rules::scoping`: Haskell, Scala); Scala application rules over call hierarchy
/// and definition answers (`crate::engine::rules::scala_apply`).
pub(crate) const ENGINE_VERSION: u32 = 12;

/// Backends whose calls are resolved with `definition` instead of call hierarchy (verify
/// A/B, DESIGN section 1.14.1).
/// * `lsp:haskell-language-server` (ShellCheck, 51 files): call hierarchy -> an
///   `outgoingCalls` answer took over the 60 s request limit (index failed); `definition` at
///   call sites -> 61.5 s cold, 91% of calls resolved (single definitions answer in ~0.06 s).
pub const DEFINITION_CALL_BACKENDS: &[&str] = &["lsp:haskell-language-server"];

/// Whether `backend` resolves calls with `definition` ([`DEFINITION_CALL_BACKENDS`] or the
/// A/B setting `debug.calls_by_definition`: backend ids or `all`).
pub(crate) fn calls_by_definition(backend: &str) -> bool {
    if DEFINITION_CALL_BACKENDS.contains(&backend) {
        return true;
    }
    trace_core::config::current()
        .debug
        .calls_by_definition
        .iter()
        .map(|id| id.trim())
        .any(|id| id.eq_ignore_ascii_case("all") || id == backend)
}

/// LSP `SymbolKind`s that are declarations for diagnostic counting in generic mode
/// (class, method, constructor, enum, interface, function, struct).
const DECLARATION_KINDS: [u64; 7] = [5, 6, 9, 10, 11, 12, 23];

/// Subtype levels searched below a base type for members it declares (type hierarchy).
pub(crate) const MAX_TYPE_DEPTH: usize = 4;
/// Whether the syntax answers of SPEC section 8.8 are enabled (setting
/// `debug.syntax_answers`, default on; off only for diagnosis and equivalence tests).
/// Results are identical either way.
pub(crate) fn syntax_answers_enabled() -> bool {
    trace_core::config::current().debug.syntax_answers
}

/// The request surface the algorithm needs (implemented by [`LspClient`]).
pub(crate) trait Session {
    fn capabilities(&self) -> &Value;
    fn request_many(
        &mut self,
        calls: Vec<(String, Value)>,
    ) -> Result<Vec<Result<Value, SemanticError>>, SemanticError>;
    /// Params of the notifications named `method` the server sent so far, oldest first
    /// (clangd `textDocument/inactiveRegions`). Sessions that keep none answer nothing.
    fn notifications_named(&self, method: &str) -> Vec<Value> {
        let _ = method;
        Vec::new()
    }
}

impl Session for LspClient {
    fn capabilities(&self) -> &Value {
        LspClient::capabilities(self)
    }
    fn notifications_named(&self, method: &str) -> Vec<Value> {
        // Inherent method: per-document notifications (inactive regions) are kept apart.
        LspClient::notifications_named(self, method)
    }
    fn request_many(
        &mut self,
        calls: Vec<(String, Value)>,
    ) -> Result<Vec<Result<Value, SemanticError>>, SemanticError> {
        LspClient::request_many(self, calls)
    }
}

/// Relative path <-> URI mapping of the analyzed workspace.
pub(crate) trait UriResolver {
    fn uri_of(&self, rel: &str) -> Result<String, SemanticError>;
    fn rel_of(&self, uri: &str) -> Option<String>;
}

impl UriResolver for Snapshot {
    fn uri_of(&self, rel: &str) -> Result<String, SemanticError> {
        crate::lsp::path_to_uri(&self.path_of(rel))
    }
    /// Local files inside the workspace map to their relative path; archive and virtual
    /// documents (`jar:`, `jdt:`, `csharp:`) and files elsewhere never do.
    fn rel_of(&self, uri: &str) -> Option<String> {
        match crate::mapping::parse_uri(uri)? {
            crate::mapping::UriTarget::File(path) => self.relative(&path),
            crate::mapping::UriTarget::Virtual(_) => None,
        }
    }
}

/// Algorithm switches.
#[derive(Clone)]
pub(crate) struct Options<'o> {
    pub provider: Provider,
    pub tool_fingerprint: &'o str,
    /// Pyright mode: documentSymbol kinds 5/6/7/9/12 only, constructor bridge, `.pyi`/`.py`
    /// definition collapse, call hierarchy required.
    pub python: bool,
    /// Answer definitions syntax proves identically without asking (module docs).
    pub syntax_answers: bool,
    /// The backend's hooks (answer policy, definition trust, function-type route).
    pub hooks: &'o dyn Server,
    /// The backend's preflight result.
    pub prepared: &'o Prepared,
    /// Resolve calls with `definition` instead of call hierarchy ([`calls_by_definition`]).
    pub calls_by_definition: bool,
    /// Answers of unchanged units per file (declaration reuse).
    pub reuse: Option<&'o HashMap<String, FileReuse>>,
}

impl std::fmt::Debug for Options<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("provider", &self.provider)
            .field("tool_fingerprint", &self.tool_fingerprint)
            .field("python", &self.python)
            .field("syntax_answers", &self.syntax_answers)
            .field("prepared", &self.prepared.backend)
            .field("calls_by_definition", &self.calls_by_definition)
            .field("reuse", &self.reuse.map(HashMap::len))
            .finish()
    }
}

impl<'o> Options<'o> {
    /// The reuse of `path`, when any.
    fn reuse_of(&self, path: &str) -> Option<&'o FileReuse> {
        self.reuse.and_then(|m| m.get(path))
    }
}

/// Fresh per-file results plus bookkeeping for the Python stub rule and declaration reuse.
#[derive(Debug, Default)]
pub(crate) struct Analysis {
    pub files: HashMap<String, FileSemantics>,
    /// `(path, callee start)` of `external_or_ambiguous` entries that also had targets
    /// outside the index (their candidate lists are incomplete).
    pub incomplete: HashSet<(String, u32)>,
    /// Raw answers per file (declaration reuse), for files without syntax errors.
    pub answers: HashMap<String, FileAnswers>,
    /// Per file: (reuse units answered from reuse, reuse units asked).
    pub units: HashMap<String, (u32, u32)>,
}

impl Analysis {
    /// Merge another shard's results (shards are disjoint by file).
    pub fn merge(&mut self, other: Analysis) {
        self.files.extend(other.files);
        self.incomplete.extend(other.incomplete);
        self.answers.extend(other.answers);
        self.units.extend(other.units);
    }
}

/// Run the algorithm over an initialized session whose documents for `shard` are open.
/// Returns fresh results for every file of `shard`.
pub(crate) fn analyze<'a>(
    session: &mut dyn Session,
    shard: &[&SemanticFile<'a>],
    decls: &DeclTable<'a>,
    uris: &dyn UriResolver,
    opts: &Options<'_>,
) -> Result<Analysis, SemanticError> {
    let mut run = Shard::plan(&*session, shard, decls, uris, opts)?;
    run.batch_a(session)?;
    run.batch_b(session)?;
    run.batch_c(session)?;
    run.library_dispatch(session)?;
    run.blind_sites(session);
    run.callback_params(session)?;
    run.expansions(session);
    Ok(run.finish())
}

/// Smallest syntax call whose callee span contains `point`.
pub(crate) fn innermost_call(facts: &FileFacts, point: u32) -> Option<&CallSite> {
    innermost_call_index(facts, point).map(|i| &facts.calls[i])
}

/// Index of [`innermost_call`].
fn innermost_call_index(facts: &FileFacts, point: u32) -> Option<usize> {
    facts
        .calls
        .iter()
        .enumerate()
        .filter(|(_, c)| c.callee_span.contains(point))
        .min_by_key(|(_, c)| c.callee_span.len())
        .map(|(i, _)| i)
}

/// Executing declaration at `point`: the owner of the smallest call whose callee contains
/// it, else of the smallest value reference at it (attribute/property access); `None` for
/// module/class level and lazy scopes without a declaration (pyright.py `Ownership`).
pub(crate) fn owner_at(facts: &FileFacts, point: u32) -> Option<u32> {
    if let Some(call) = innermost_call(facts, point) {
        return call.owner;
    }
    facts
        .references
        .iter()
        .filter(|r| r.span.contains(point) || r.span.start == point)
        .min_by_key(|r| r.span.len())
        .and_then(|r| r.owner)
}

/// Byte offset of a call's member identifier (end of the callee), else the callee start.
pub(crate) fn member_point(call: &CallSite) -> u32 {
    match &call.member {
        Some(member)
            if call.callee.ends_with(member.as_str()) && member.len() as u32 <= call.callee_span.len() =>
        {
            call.callee_span.end - member.len() as u32
        }
        _ => call.callee_span.start,
    }
}

/// `{"line": L, "character": C}`.
pub(crate) fn position(value: &Value) -> Option<(u32, u32)> {
    let line = u32::try_from(value.get("line")?.as_u64()?).ok()?;
    let character = u32::try_from(value.get("character")?.as_u64()?).ok()?;
    Some((line, character))
}

/// Text sent to servers: UTF-8 without the BOM (positions are mapped BOM-aware).
pub(crate) fn lsp_text(source: &[u8]) -> Option<&str> {
    let bytes = source.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(source);
    std::str::from_utf8(bytes).ok()
}

/// Exact text of a span (lossy only for invalid UTF-8).
pub(crate) fn slice_text(source: &[u8], span: ByteSpan) -> String {
    let end = (span.end as usize).min(source.len());
    let start = (span.start as usize).min(end);
    String::from_utf8_lossy(&source[start..end]).into_owned()
}

#[cfg(test)]
#[path = "../../tests/unit/engine/mod.rs"]
pub(crate) mod tests;
