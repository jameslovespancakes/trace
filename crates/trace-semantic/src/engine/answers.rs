//! Per-file answers: the output accumulator ([`FileOut`]), location mapping onto declarations,
//! the function-type rule session ([`function_types`]) and the raw answers kept per unit for
//! declaration reuse ([`answers_of`]).

use crate::backend::SemanticFile;
use crate::cache::{reuse_units, FileAnswers, FileReuse, UnitAnswers};
use crate::languages::Server;
use crate::mapping::{DeclRef, DeclTable};
use crate::SemanticError;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use trace_core::model::{ByteSpan, Diagnostic, Provider};
use trace_core::semantics::{
    FileSemantics, LibraryFile, SemCallbackParam, SemEdge, SemImplementation, SemLibraryCall,
    SemLibraryDispatch, SemUnresolved, SemValueRef,
};

use super::*;

/// Function-type-rule session over the engine's LSP session (module docs): memoised
/// answers, a recording dry run, then the real run.
pub(super) struct FnTypeRequests<'s, 'u> {
    pub(super) session: &'s mut dyn Session,
    pub(super) uris: &'u dyn UriResolver,
    /// (method, params text) -> answer.
    pub(super) answers: HashMap<(String, String), Value>,
    /// Dry run: requests are recorded and answered with an error.
    pub(super) record: Option<Vec<(String, Value)>>,
}

impl crate::backends::fntype::FnTypeSession for FnTypeRequests<'_, '_> {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, SemanticError> {
        let key = (method.to_string(), params.to_string());
        if let Some(found) = self.answers.get(&key) {
            return Ok(found.clone());
        }
        if let Some(recorded) = &mut self.record {
            recorded.push((method.to_string(), params));
            return Err(SemanticError::Capability(format!("{method} (recorded for pipelining)")));
        }
        let mut results = self.session.request_many(vec![(method.to_string(), params)])?;
        let value = results
            .pop()
            .unwrap_or_else(|| Err(SemanticError::Protocol("missing response".into())))?;
        self.answers.insert(key, value.clone());
        Ok(value)
    }
    fn request_many(
        &mut self,
        calls: Vec<(String, Value)>,
    ) -> Result<Vec<Result<Value, SemanticError>>, SemanticError> {
        self.session.request_many(calls)
    }
    fn uri_of(&self, rel: &str) -> Result<String, SemanticError> {
        self.uris.uri_of(rel)
    }
    fn read_location(&mut self, uri: &str) -> Option<(std::path::PathBuf, Vec<u8>)> {
        let crate::mapping::UriTarget::File(path) = crate::mapping::parse_uri(uri)? else {
            return None;
        };
        let bytes = std::fs::read(&path).ok()?;
        Some((path, bytes))
    }
}

/// Where a diagnostic count belongs (declaration reuse re-attributes counts by position).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CountAt {
    /// The whole file (documentSymbol, invalid UTF-8): answers are not kept.
    File,
    /// A position (inside a reuse unit or module-level code).
    At(u32),
    /// Implementation / type-hierarchy requests (reused with the implementations).
    Impl,
}

/// Per-file accumulator.
#[derive(Default)]
pub(super) struct FileOut {
    pub(super) edges: Vec<SemEdge>,
    pub(super) unresolved: Vec<SemUnresolved>,
    pub(super) value_refs: Vec<SemValueRef>,
    pub(super) implementations: Vec<SemImplementation>,
    pub(super) resolved_elsewhere: Vec<ByteSpan>,
    pub(super) callback_params: Vec<SemCallbackParam>,
    pub(super) library_files: Vec<LibraryFile>,
    pub(super) library_calls: Vec<SemLibraryCall>,
    pub(super) outside_build: Option<String>,
    /// Server-specific per-file extras (`Server::file_expansions`).
    pub(super) expanded: Vec<trace_core::semantics::ExpandedMacro>,
    /// Calls through library-declared abstract members with their in-index implementations.
    pub(super) library_dispatch: Vec<SemLibraryDispatch>,
    /// Header bases located outside the index (`FileSemantics::library_bases`).
    pub(super) library_bases: Vec<SemLibraryCall>,
    /// Library calls with a receiver whose implementations may be asked (library dispatch,
    /// module docs); consumed before the blind-site step, never persisted.
    pub(super) dispatch_candidates: Vec<DispatchCandidate>,
    /// Syntax call indices asked only because they carry a callback argument (rule 3 did
    /// not apply): without a server answer they get the rule-3 result.
    pub(super) asked_by_name: HashSet<usize>,
    /// Diagnostics of those extras (`bounded` when the run's budget ran out).
    pub(super) extra_diagnostics: Vec<Diagnostic>,
    pub(super) counts: Vec<(String, CountAt)>,
    pub(super) first_error: Option<String>,
}

impl FileOut {
    /// A count of the whole file.
    pub(super) fn count(&mut self, kind: &str) {
        self.counts.push((kind.to_string(), CountAt::File));
    }

    /// A count at a position of the file.
    pub(super) fn count_at(&mut self, kind: &str, point: u32) {
        self.counts.push((kind.to_string(), CountAt::At(point)));
    }

    /// A count of the implementation / type-hierarchy requests.
    pub(super) fn count_impl(&mut self, kind: &str) {
        self.counts.push((kind.to_string(), CountAt::Impl));
    }

    /// A failed request (counted where it was asked).
    pub(super) fn fail(&mut self, error: &SemanticError, at: CountAt) {
        self.counts.push(("request_failed".to_string(), at));
        if self.first_error.is_none() {
            self.first_error = Some(error.to_string());
        }
    }

    /// A request failure the server's hooks classify as "not part of the build on this
    /// machine" (gopls `no package metadata`): the file's reason.
    pub(super) fn outside(&mut self, hooks: &dyn Server, path: &str, error: &SemanticError) -> bool {
        if self.outside_build.is_some() {
            return true;
        }
        match hooks.outside_build(path, &error.to_string()) {
            Some(reason) => {
                self.outside_build = Some(reason);
                true
            }
            None => false,
        }
    }

    /// Merge the reused answers of unchanged units (declaration reuse).
    pub(super) fn absorb_reuse(&mut self, reuse: &FileReuse) {
        self.edges.extend(reuse.edges.iter().cloned());
        self.unresolved.extend(reuse.unresolved.iter().cloned());
        self.value_refs.extend(reuse.value_refs.iter().cloned());
        self.resolved_elsewhere
            .extend(reuse.resolved_elsewhere.iter().copied());
        self.callback_params.extend(reuse.callback_params.iter().cloned());
        for (file, call) in &reuse.library_calls {
            let index = self.intern_library_file(file.clone());
            let mut call = call.clone();
            call.file = index;
            self.library_calls.push(call);
        }
        self.library_dispatch.extend(reuse.library_dispatch.iter().cloned());
        for (kind, n, point) in &reuse.counts {
            for _ in 0..*n {
                self.counts.push((kind.clone(), CountAt::At(*point)));
            }
        }
        if let Some(implementations) = &reuse.implementations {
            self.implementations.extend(implementations.iter().cloned());
            for (kind, n) in &reuse.implementation_counts {
                for _ in 0..*n {
                    self.counts.push((kind.clone(), CountAt::Impl));
                }
            }
        }
    }

    pub(super) fn intern_library_file(&mut self, file: LibraryFile) -> u32 {
        let index = match self.library_files.iter().position(|f| *f == file) {
            Some(i) => i,
            None => {
                self.library_files.push(file);
                self.library_files.len() - 1
            }
        };
        index as u32
    }

    pub(super) fn finish(mut self, path: &str, provider: &Provider, fingerprint: &str) -> FileSemantics {
        self.edges.sort_by(|a, b| {
            (a.at.start, a.at.end, a.owner, &a.target, a.kind)
                .cmp(&(b.at.start, b.at.end, b.owner, &b.target, b.kind))
        });
        self.edges
            .dedup_by(|a, b| a.at == b.at && a.owner == b.owner && a.target == b.target && a.kind == b.kind);
        self.unresolved.sort_by_key(|u| (u.at.start, u.at.end, u.owner));
        self.unresolved
            .dedup_by(|a, b| a.at == b.at && a.owner == b.owner && a.kind == b.kind);
        self.value_refs
            .sort_by(|a, b| (a.at.start, a.at.end, &a.target).cmp(&(b.at.start, b.at.end, &b.target)));
        self.value_refs.dedup();
        self.implementations
            .sort_by(|a, b| (a.base, &a.implementor, a.kind).cmp(&(b.base, &b.implementor, b.kind)));
        self.implementations.dedup();
        self.resolved_elsewhere.sort_by_key(|s| (s.start, s.end));
        self.resolved_elsewhere.dedup();
        self.callback_params.sort_by(|a, b| {
            (a.call.start, a.call.end, a.arg.start, a.arg.end).cmp(&(
                b.call.start,
                b.call.end,
                b.arg.start,
                b.arg.end,
            ))
        });
        self.callback_params.dedup();
        // Library calls sorted by position (file indices re-interned in order).
        let mut calls: Vec<(LibraryFile, SemLibraryCall)> = self
            .library_calls
            .drain(..)
            .filter_map(|c| self.library_files.get(c.file as usize).cloned().map(|f| (f, c)))
            .collect();
        calls.sort_by(|a, b| {
            (a.1.at.start, a.1.at.end, a.1.decl_line, a.1.decl_column, &a.0.path).cmp(&(
                b.1.at.start,
                b.1.at.end,
                b.1.decl_line,
                b.1.decl_column,
                &b.0.path,
            ))
        });
        calls.dedup();
        let mut bases: Vec<(LibraryFile, SemLibraryCall)> = self
            .library_bases
            .drain(..)
            .filter_map(|c| self.library_files.get(c.file as usize).cloned().map(|f| (f, c)))
            .collect();
        bases.sort_by(|a, b| {
            (a.1.at.start, a.1.at.end, a.1.decl_line, a.1.decl_column, &a.0.path).cmp(&(
                b.1.at.start,
                b.1.at.end,
                b.1.decl_line,
                b.1.decl_column,
                &b.0.path,
            ))
        });
        bases.dedup();
        self.library_files.clear();
        for (file, mut call) in calls {
            call.file = self.intern_library_file(file);
            self.library_calls.push(call);
        }
        for (file, mut base) in bases {
            base.file = self.intern_library_file(file);
            self.library_bases.push(base);
        }
        let mut totals: BTreeMap<&str, u32> = BTreeMap::new();
        for (kind, _) in &self.counts {
            *totals.entry(kind.as_str()).or_insert(0) += 1;
        }
        let mut diagnostics: Vec<Diagnostic> = totals
            .iter()
            .map(|(&kind, &n)| {
                let mut message = format!("{n} {}", describe(kind));
                if kind == "request_failed" {
                    if let Some(first) = &self.first_error {
                        message.push_str(&format!(" (first: {first})"));
                    }
                }
                Diagnostic::new(kind, Some(path.to_string()), message)
            })
            .collect();
        diagnostics.append(&mut self.extra_diagnostics);
        self.expanded.sort_by_key(|e| (e.span.start, e.span.end));
        self.expanded.dedup();
        for dispatch in &mut self.library_dispatch {
            dispatch.implementations.sort();
            dispatch.implementations.dedup();
        }
        self.library_dispatch.sort_by(|a, b| {
            (a.at.start, a.at.end, a.owner, &a.library_symbol).cmp(&(
                b.at.start,
                b.at.end,
                b.owner,
                &b.library_symbol,
            ))
        });
        self.library_dispatch.dedup();
        FileSemantics {
            provider: provider.clone(),
            tool_fingerprint: fingerprint.to_string(),
            edges: self.edges,
            unresolved: self.unresolved,
            value_refs: self.value_refs,
            diagnostics,
            implementations: self.implementations,
            resolved_elsewhere: self.resolved_elsewhere,
            callback_params: self.callback_params,
            library_files: self.library_files,
            library_calls: self.library_calls,
            outside_build: self.outside_build,
            expanded: self.expanded,
            library_dispatch: self.library_dispatch,
            library_bases: self.library_bases,
        }
    }

    /// Record a library call (the library file is interned per file).
    pub(super) fn library_call(
        &mut self,
        file: LibraryFile,
        at: ByteSpan,
        line: u32,
        target: crate::external::SemLibraryCallTarget,
    ) {
        let index = self.intern_library_file(file);
        self.library_calls.push(SemLibraryCall {
            at,
            line,
            file: index,
            decl_line: target.decl_line,
            decl_column: target.decl_column,
            symbol: target.symbol,
        });
    }
}

pub(super) fn describe(kind: &str) -> &'static str {
    match kind {
        "invalid_utf8" => {
            "file is not valid UTF-8 and was not sent to the analyzer; its calls stay unresolved"
        }
        "unmapped_declaration" => {
            "analyzer symbols did not map to exactly one syntax declaration and were dropped"
        }
        "prepare_not_unique" => {
            "declarations whose call-hierarchy item did not map back uniquely; their calls stay unresolved"
        }
        "different_execution_scope" => {
            "outgoing-call ranges executed by another scope (lazy or nested bodies) were ignored"
        }
        "lazy_scope_call" => {
            "calls in lazy scopes without a declaration were resolved as value references at the callee"
        }
        "unmapped_position" => "analyzer positions outside the source were ignored",
        "request_failed" => "analyzer requests failed",
        "unmapped_owner" => "analyzer results owned by anonymous or unmapped callables were dropped",
        "unmapped_target" => {
            "resolved targets are not indexed declarations; recorded as external_or_ambiguous"
        }
        "unknown_edge_kind" => "analyzer edges of unknown kind were ignored",
        "not_quiescent" => "the analyzer did not report quiescence before queries (results may be partial)",
        "definition_fallback" => {
            "calls without an outgoing-call answer were resolved with textDocument/definition"
        }
        "syntax_definition" => {
            "calls of the only same-file declaration of their name were answered from syntax (no request)"
        }
        "external_by_name" => {
            "calls whose name no partition file declares were recorded as external (no request)"
        }
        "local_call" => "calls of local bindings were recorded as external (no request)",
        "subtypes_bounded" => "type hierarchy searches stopped at the request bound",
        "library_dispatch" => {
            "library calls were asked for their in-index implementations (one request per library declaration)"
        }
        "library_dispatch_bounded" => {
            "library declarations were not asked for implementations (request bound reached)"
        }
        "inactive_code" => {
            "calls in preprocessor regions the build does not compile were recorded as inactive code (no request)"
        }
        "template_dependent" => {
            "calls through a value typed by a template parameter were recorded as template dependent"
        }
        "external_program" => "commands found as external programs were recorded as resolved elsewhere",
        "type_conversion" => "calls of types that convert a value were recorded as type uses",
        "arity_narrowed" => "overloaded targets were narrowed by the call's argument count",
        "scala_application" => "applications were narrowed to the apply method or the constructed class",
        "outside_build_file" => "file is not part of the build on this machine; nothing was asked",
        _ => "occurrences",
    }
}

/// Run the function-type rule for `queries` with pipelined first requests (module docs):
/// a dry run records every query's first requests, they are sent as one batch, then the
/// real run answers from that batch (further hops are asked and memoised).
pub(super) fn function_types<'q>(
    session: &mut dyn Session,
    uris: &dyn UriResolver,
    queries: &[crate::backends::fntype::FnTypeQuery<'q>],
) -> Result<Vec<(&'q str, SemCallbackParam)>, SemanticError> {
    let mut requests = FnTypeRequests {
        session,
        uris,
        answers: HashMap::new(),
        record: Some(Vec::new()),
    };
    {
        let mut dry_cache = crate::backends::fntype::FnTypeCache::default();
        for q in queries {
            let _ = crate::backends::fntype::resolve(q, &mut requests, &mut dry_cache);
        }
    }
    let recorded = requests.record.take().unwrap_or_default();
    if !recorded.is_empty() {
        let keys: Vec<(String, String)> = recorded.iter().map(|(m, p)| (m.clone(), p.to_string())).collect();
        let results = request_unique(requests.session, recorded)?;
        for (key, result) in keys.into_iter().zip(results) {
            if let Ok(value) = result {
                requests.answers.insert(key, value);
            }
        }
    }
    let mut cache = crate::backends::fntype::FnTypeCache::default();
    let mut out = Vec::new();
    for q in queries {
        if let Some(param) = crate::backends::fntype::resolve(q, &mut requests, &mut cache) {
            out.push((q.path, param));
        }
    }
    Ok(out)
}

/// The raw answers of one analysed file per reuse unit (declaration reuse); `None` for
/// files whose answers cannot be reused (syntax errors, whole-file counts).
pub(super) fn answers_of<'a>(
    f: &SemanticFile<'a>,
    decls: &DeclTable<'a>,
    o: &FileOut,
    incomplete: &HashSet<(String, u32)>,
) -> Option<FileAnswers> {
    if f.facts.error_count > 0 || o.counts.iter().any(|(_, at)| *at == CountAt::File) {
        return None;
    }
    let unit_decls = reuse_units(f.facts);
    let mut units: Vec<UnitAnswers> = Vec::with_capacity(unit_decls.len());
    let mut spans: Vec<ByteSpan> = Vec::with_capacity(unit_decls.len());
    for &u in &unit_decls {
        let r = DeclRef {
            path: f.path,
            decl: u,
        };
        let span = decls.decl(r).span.bytes;
        let body = f.source.get(span.range())?;
        units.push(UnitAnswers {
            uid: decls.uid(r),
            body: trace_core::Hash32::of(body),
            start: span.start,
            line: decls.line1(f.path, span.start).unwrap_or(1),
            ..UnitAnswers::default()
        });
        spans.push(span);
    }
    // Units never overlap (outermost callables); an item must lie wholly inside one.
    let mut spoiled: HashSet<usize> = HashSet::new();
    let mut unit_of = |at: ByteSpan| -> Option<usize> {
        let i = spans.iter().position(|s| s.start <= at.start && at.start < s.end)?;
        if at.end > spans[i].end {
            spoiled.insert(i);
            return None;
        }
        Some(i)
    };
    for e in &o.edges {
        if let Some(i) = unit_of(e.at) {
            units[i].edges.push(e.clone());
        }
    }
    for u in &o.unresolved {
        if let Some(i) = unit_of(u.at) {
            units[i].unresolved.push(u.clone());
        }
    }
    for v in &o.value_refs {
        if let Some(i) = unit_of(v.at) {
            units[i].value_refs.push(v.clone());
        }
    }
    for s in &o.resolved_elsewhere {
        if let Some(i) = unit_of(*s) {
            units[i].resolved_elsewhere.push(*s);
        }
    }
    for p in &o.callback_params {
        if let Some(i) = unit_of(p.arg) {
            units[i].callback_params.push(p.clone());
        }
    }
    for c in &o.library_calls {
        let Some(file) = o.library_files.get(c.file as usize) else { continue };
        if let Some(i) = unit_of(c.at) {
            units[i].library_calls.push((file.clone(), c.clone()));
        }
    }
    for d in &o.library_dispatch {
        if let Some(i) = unit_of(d.at) {
            units[i].library_dispatch.push(d.clone());
        }
    }
    for (path, point) in incomplete {
        if path == f.path {
            if let Some(i) = unit_of(ByteSpan::new(*point, *point)) {
                units[i].incomplete.push(*point);
            }
        }
    }
    let mut unit_counts: Vec<BTreeMap<&str, u32>> = vec![BTreeMap::new(); units.len()];
    let mut implementation_counts: BTreeMap<&str, u32> = BTreeMap::new();
    for (kind, at) in &o.counts {
        match at {
            CountAt::At(point) => {
                if let Some(i) = unit_of(ByteSpan::new(*point, *point)) {
                    *unit_counts[i].entry(kind.as_str()).or_insert(0) += 1;
                }
            }
            CountAt::Impl => *implementation_counts.entry(kind.as_str()).or_insert(0) += 1,
            CountAt::File => {}
        }
    }
    for (unit, counts) in units.iter_mut().zip(unit_counts) {
        unit.counts = counts.into_iter().map(|(k, n)| (k.to_string(), n)).collect();
        unit.incomplete.sort_unstable();
        unit.incomplete.dedup();
    }
    let units = units
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !spoiled.contains(i))
        .map(|(_, u)| u)
        .collect();
    Some(FileAnswers {
        hash: f.hash,
        interface: f.facts.interface,
        decl_uids: decls.decls_of(f.path).into_iter().map(|r| decls.uid(r)).collect(),
        units,
        implementations: o.implementations.clone(),
        implementation_counts: implementation_counts
            .into_iter()
            .map(|(k, n)| (k.to_string(), n))
            .collect(),
        ..FileAnswers::default()
    })
}

/// Type-declaration node kinds of the conversion languages' grammars (Go `type T ...`,
/// `type T = ...`), read from library files to tell a conversion from a call.
pub(super) const TYPE_DECLARATION_KINDS: &[&str] = &["type_spec", "type_alias"];

/// Name positions (0-based line, UTF-16 character) of the type declarations of a file,
/// parsed once per process and file version.
pub(super) type TypeNames = Arc<HashSet<(u32, u32)>>;

/// Whether every location names a type declaration of its file (definition answers of a
/// conversion `T(x)` outside the index: builtin and library types). Files of languages
/// without conversions, unreadable files and virtual documents never do.
pub(super) fn locations_are_types(locations: &[ServerLocation<'_>]) -> bool {
    !locations.is_empty()
        && locations.iter().all(|&(uri, (line, character))| {
            let Some(crate::mapping::UriTarget::File(path)) = crate::mapping::parse_uri(uri) else {
                return false;
            };
            type_names(&path).is_some_and(|names| names.contains(&(line, character)))
        })
}

/// [`TypeNames`] of `path` (memoised by path, length and modification time).
pub(super) fn type_names(path: &Path) -> Option<TypeNames> {
    type Memo = Mutex<HashMap<PathBuf, (u64, Option<std::time::SystemTime>, TypeNames)>>;
    static MEMO: OnceLock<Memo> = OnceLock::new();
    let language =
        trace_core::languages::from_path(path).filter(|l| trace_syntax::spec::type_call_is_conversion(*l))?;
    let meta = std::fs::metadata(path).ok()?;
    let stamp = (meta.len(), meta.modified().ok());
    let memo = MEMO.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some((len, modified, names)) = memo.lock().ok().and_then(|m| m.get(path).cloned()) {
        if (len, modified) == stamp {
            return Some(names);
        }
    }
    let source = std::fs::read(path).ok()?;
    let tree = trace_syntax::parse_tree(language, &source).ok()?;
    let lines = trace_core::text::LineIndex::new(&source);
    let mut names: HashSet<(u32, u32)> = HashSet::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if TYPE_DECLARATION_KINDS.contains(&node.kind()) {
            if let Some(name) = node.child_by_field_name("name") {
                let byte = u32::try_from(name.start_byte()).unwrap_or(u32::MAX);
                names.insert(lines.utf16_of_byte(&source, byte));
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    let names: TypeNames = Arc::new(names);
    if let Ok(mut m) = memo.lock() {
        m.insert(path.to_path_buf(), (stamp.0, stamp.1, names.clone()));
    }
    Some(names)
}

/// Count `documentSymbol` results (hierarchical `DocumentSymbol[]` or flat
/// `SymbolInformation[]`) that do not map onto a declaration of `path` (files whose syntax
/// had errors: the server may see declarations the syntax tree lost).
pub(super) fn count_unmapped_symbols(
    value: &Value,
    path: &str,
    decls: &DeclTable<'_>,
    python: bool,
    o: &mut FileOut,
) {
    let mut stack: Vec<&Value> = value.as_array().map(|a| a.iter().collect()).unwrap_or_default();
    while let Some(item) = stack.pop() {
        if let Some(children) = item.get("children").and_then(Value::as_array) {
            stack.extend(children.iter());
        }
        let kind = item.get("kind").and_then(Value::as_u64).unwrap_or(0);
        if python && !crate::backends::pyright::SYMBOL_KINDS.contains(&kind) {
            continue;
        }
        let relevant = python || DECLARATION_KINDS.contains(&kind);
        if !relevant {
            continue;
        }
        let mapped = if let Some(selection) = item.get("selectionRange") {
            selection
                .get("start")
                .and_then(position)
                .map(|(line, character)| decls.at_lsp(path, line, character).is_some())
        } else if let Some(range) = item.get("location").and_then(|l| l.get("range")) {
            let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
            let bytes = |key: &str| {
                range
                    .get(key)
                    .and_then(position)
                    .and_then(|(l, c)| decls.byte_of(path, l, c))
            };
            Some(match (bytes("start"), bytes("end")) {
                (Some(start), Some(end)) => decls.by_name_within(path, name, start, end).is_some(),
                _ => false,
            })
        } else {
            None
        };
        if mapped == Some(false) {
            o.count("unmapped_declaration");
        }
    }
}

/// `Location | Location[] | LocationLink[] | null` -> (uri, target position).
pub(super) fn definition_locations(value: &Value) -> Vec<(&str, (u32, u32))> {
    let items: Vec<&Value> = match value {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![value],
        _ => Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|item| {
            let uri = item.get("targetUri").or_else(|| item.get("uri"))?.as_str()?;
            let range = item.get("targetSelectionRange").or_else(|| item.get("range"))?;
            Some((uri, position(range.get("start")?)?))
        })
        .collect()
}

/// A server location: (uri, (line, character)).
pub(super) type ServerLocation<'l> = (&'l str, (u32, u32));

/// Map locations to declarations; the second list holds every location that is not an
/// indexed declaration (library, builtin, unmapped position).
pub(super) fn resolve_locations<'a, 'l>(
    locations: &[ServerLocation<'l>],
    uris: &dyn UriResolver,
    decls: &DeclTable<'a>,
) -> (Vec<DeclRef<'a>>, Vec<ServerLocation<'l>>) {
    let mut mapped = Vec::new();
    let mut external = Vec::new();
    for &(uri, (line, character)) in locations {
        match locate(uri, line, character, uris, decls) {
            Some(r) => {
                if !mapped.contains(&r) {
                    mapped.push(r);
                }
            }
            None => external.push((uri, (line, character))),
        }
    }
    // A second location of the SAME declaration is the same answer, never a library call:
    // a server may answer `utils.notify = function(...)` with the field name AND the
    // `function` keyword of that line. Only a location inside a declaration another location
    // already mapped, on that declaration's name line, is merged; anything else stays.
    external.retain(|&(uri, (line, character))| {
        !same_declaration_line(uri, line, character, &mapped, uris, decls)
    });
    (mapped, external)
}

/// Location `(uri, line0, character)` lies inside one of `mapped` and on its name line.
pub(super) fn same_declaration_line(
    uri: &str,
    line0: u32,
    character: u32,
    mapped: &[DeclRef<'_>],
    uris: &dyn UriResolver,
    decls: &DeclTable<'_>,
) -> bool {
    let Some(rel) = uris.rel_of(uri) else { return false };
    let Some(path) = decls.path_key(&rel) else { return false };
    let Some(byte) = decls.byte_of(path, line0, character) else { return false };
    mapped.iter().any(|&r| {
        r.path == path
            && decls.get(r).is_some_and(|d| d.span.bytes.contains(byte))
            && decls.name_line(r) == Some(line0 + 1)
    })
}

/// A call-hierarchy item's declaration.
pub(super) fn map_item<'a>(
    item: &Value,
    uris: &dyn UriResolver,
    decls: &DeclTable<'a>,
) -> Option<DeclRef<'a>> {
    let uri = item.get("uri")?.as_str()?;
    let start = item
        .get("selectionRange")
        .or_else(|| item.get("range"))?
        .get("start")?;
    let (line, character) = position(start)?;
    locate(uri, line, character, uris, decls)
}

pub(super) fn locate<'a>(
    uri: &str,
    line: u32,
    character: u32,
    uris: &dyn UriResolver,
    decls: &DeclTable<'a>,
) -> Option<DeclRef<'a>> {
    let rel = uris.rel_of(uri)?;
    let path = decls.path_key(&rel)?;
    decls.at_lsp(path, line, character)
}

pub(super) fn into_items(value: Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items,
        Value::Object(_) => vec![value],
        _ => Vec::new(),
    }
}

impl<'a> Shard<'a, '_> {
    /// Step 6 of the module docs: syntax calls no answer covered.
    pub(super) fn blind_sites(&mut self, session: &mut dyn Session) {
        // 6. Blind sites: syntax calls not covered by any analyzer result (module level too).
        //    Calls in reused units come with their reused entries. A file the server says is
        //    not part of the build on this machine has unknown calls (`outside_build`). Without
        //    an answer: calls in the server's inactive preprocessor regions are `inactive_code`;
        //    calls asked only for their callback argument keep the rule-3 answer; bare commands
        //    the hooks find as external programs are resolved elsewhere; C++ calls whose callee
        //    depends on a template parameter are `template_dependent`.
        let inactive_now: HashMap<&'a str, Vec<ByteSpan>> = if self.inactive_policy {
            inactive_regions(&*session, &self.active, self.decls, self.uris)
        } else {
            HashMap::new()
        };
        for f in &self.queried {
            let facts: &'a FileFacts = f.facts;
            let mut blind: Vec<(usize, u32)> = Vec::new();
            for (ci, c) in facts.calls.iter().enumerate() {
                let Some(owner) = facts.executing_owner(c.owner) else {
                    continue;
                };
                if !self.covered.contains(&(f.path, ci)) && !self.reused_at(f.path, c.callee_span.start) {
                    blind.push((ci, owner));
                }
            }
            if blind.is_empty() {
                continue;
            }
            let outside = self.out.get(f.path).is_some_and(|o| o.outside_build.is_some());
            let dependent: HashSet<usize> =
                if trace_syntax::language_rules::rules(f.language).template_dependent_calls && !outside {
                    let indices: Vec<usize> = blind.iter().map(|(ci, _)| *ci).collect();
                    template_dependent_calls(f.source, facts, &indices)
                } else {
                    HashSet::new()
                };
            for (ci, owner) in blind {
                let c = &facts.calls[ci];
                if !outside {
                    let inactive = inactive_now
                        .get(f.path)
                        .is_some_and(|spans| spans.iter().any(|s| s.contains(c.callee_span.start)));
                    if inactive {
                        record_inactive(f.path, ci, c, owner, &mut self.out, &mut self.covered);
                        continue;
                    }
                    if external_program(
                        f.path,
                        c,
                        self.decls,
                        self.opts,
                        self.shell_scopes.get(f.path),
                        &mut self.programs,
                    ) {
                        apply_call_answer(
                            CallAnswer::ExternalProgram,
                            f.path,
                            ci,
                            c,
                            owner,
                            self.decls,
                            &mut self.out,
                            &mut self.incomplete,
                            &mut self.covered,
                        );
                        continue;
                    }
                    if self.out.get(f.path).is_some_and(|o| o.asked_by_name.contains(&ci)) {
                        apply_call_answer(
                            CallAnswer::NotDeclared,
                            f.path,
                            ci,
                            c,
                            owner,
                            self.decls,
                            &mut self.out,
                            &mut self.incomplete,
                            &mut self.covered,
                        );
                        continue;
                    }
                }
                let kind = if dependent.contains(&ci) {
                    UnresolvedKind::TemplateDependent
                } else {
                    UnresolvedKind::NoSemanticTarget
                };
                let o = self.out.get_mut(f.path).expect("queried file");
                if kind == UnresolvedKind::TemplateDependent {
                    o.count_at("template_dependent", c.callee_span.start);
                }
                o.unresolved.push(SemUnresolved {
                    owner: Some(owner),
                    kind,
                    at: c.callee_span,
                    line: c.line,
                    callee: c.callee.clone(),
                    candidates: Vec::new(),
                });
            }
        }
    }

    /// The function-type rule over the shard (module docs).
    pub(super) fn callback_params(&mut self, session: &mut dyn Session) -> Result<(), SemanticError> {
        // Function-type rule (DESIGN §1.10 item 2): callback arguments resolved to an in-index
        // callable whose receiving call has no in-index target (callbacks in reused units come
        // with their reused answers). Anonymous functions passed as arguments (closures, lambdas,
        // arrow functions) are in-index callables by construction and are asked the same way.
        let anonymous: Vec<Vec<CallbackArg>> = self
            .active
            .iter()
            .map(|f| crate::backends::fntype::anonymous_arguments(f.facts, f.source))
            .collect();
        let mut queries: Vec<crate::backends::fntype::FnTypeQuery<'_>> = Vec::new();
        for (fi, f) in self.active.iter().enumerate() {
            let Some(o) = self.out.get(f.path) else { continue };
            let route = self.opts.hooks.fn_type_route(f.language);
            // The receiving call has an in-index target: an edge keyed on its callee (the whole
            // callee, or the member identifier ending it: call-hierarchy ranges).
            let receiving_resolved = |cb: &CallbackArg| {
                o.edges.iter().any(|e| {
                    e.kind != EdgeKind::PassesCallback
                        && e.at.end == cb.call_callee_span.end
                        && cb.call_callee_span.start <= e.at.start
                })
            };
            for cb in &anonymous[fi] {
                if self.reused_at(f.path, cb.arg_span.start)
                    || receiving_resolved(cb)
                    || f.facts.callbacks.iter().any(|named| named.arg_span == cb.arg_span)
                {
                    continue;
                }
                let Some(call) = f.facts.calls.iter().find(|c| c.callee_span == cb.call_callee_span) else {
                    continue;
                };
                queries.push(crate::backends::fntype::FnTypeQuery {
                    language: f.language,
                    path: f.path,
                    source: f.source,
                    facts: f.facts,
                    call,
                    arg: cb,
                    route,
                });
            }
            for cb in &f.facts.callbacks {
                if self.reused_at(f.path, cb.arg_span.start) {
                    continue;
                }
                let resolved_in_index = o
                    .edges
                    .iter()
                    .any(|e| e.kind == EdgeKind::PassesCallback && e.at == cb.arg_span);
                if !resolved_in_index || receiving_resolved(cb) {
                    continue;
                }
                let Some(call) = f.facts.calls.iter().find(|c| c.callee_span == cb.call_callee_span) else {
                    continue;
                };
                queries.push(crate::backends::fntype::FnTypeQuery {
                    language: f.language,
                    path: f.path,
                    source: f.source,
                    facts: f.facts,
                    call,
                    arg: cb,
                    route,
                });
            }
        }
        if !queries.is_empty() {
            for (path, param) in function_types(session, self.uris, &queries)? {
                if let Some(o) = self.out.get_mut(path) {
                    o.callback_params.push(param);
                }
            }
        }
        Ok(())
    }

    /// Server-specific per-file extras (`Server::file_expansions`).
    pub(super) fn expansions(&mut self, session: &mut dyn Session) {
        // Server-specific per-file extras (rust-analyzer `expandMacro`, DESIGN §1.15) over the
        // same request session, one budget for the analysis. Every analysed file is asked (also
        // those with reused units: expansions are not part of the reused answers).
        let budget =
            std::sync::atomic::AtomicU32::new(crate::backends::rust_analyzer::MAX_EXPANSIONS_PER_RUN);
        {
            let mut requests = FnTypeRequests {
                session: &mut *session,
                uris: self.uris,
                answers: HashMap::new(),
                record: None,
            };
            for f in &self.active {
                let (expanded, diagnostic) =
                    self.opts
                        .hooks
                        .file_expansions(f, self.opts.prepared, &mut requests, &budget);
                if let Some(o) = self.out.get_mut(f.path) {
                    o.expanded.extend(expanded);
                    o.extra_diagnostics.extend(diagnostic);
                }
            }
        }
    }

    /// Merge reused answers, keep this analysis' raw answers per unit and finish every file.
    pub(super) fn finish(mut self) -> Analysis {
        // Declaration reuse: merge the answers of unchanged units, then keep this analysis'
        // raw answers per unit for the next one.
        for f in &self.active {
            let (Some(reuse), Some(o)) = (self.opts.reuse_of(f.path), self.out.get_mut(f.path)) else {
                continue;
            };
            o.absorb_reuse(reuse);
            self.incomplete
                .extend(reuse.incomplete.iter().map(|p| (f.path.to_string(), *p)));
        }
        let mut answers: HashMap<String, FileAnswers> = HashMap::new();
        for f in &self.active {
            if let Some(o) = self.out.get(f.path) {
                if let Some(a) = answers_of(f, self.decls, o, &self.incomplete) {
                    answers.insert(f.path.to_string(), a);
                }
            }
        }

        let files = self
            .out
            .into_iter()
            .map(|(path, o)| {
                (path.to_string(), o.finish(path, &self.opts.provider, self.opts.tool_fingerprint))
            })
            .collect();
        Analysis {
            files,
            incomplete: self.incomplete,
            answers,
            units: self.units,
        }
    }
}
