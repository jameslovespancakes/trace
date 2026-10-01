//! Batch A answers: `documentSymbol`, `prepareCallHierarchy`, `definition` (calls, callbacks,
//! non-call uses, header bases) and implementation / type-hierarchy preparation, mapped onto
//! syntax declarations as edges, value references, callbacks and implementations.

use crate::mapping::{DeclRef, DeclTable};
use crate::SemanticError;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use trace_core::facts::{Activation, CallSite, CallbackArg, RefKind, Reference};
use trace_core::model::{ByteSpan, EdgeKind, ExecutionModel, Resolution, SymbolKind, UnresolvedKind};
use trace_core::semantics::{SemEdge, SemImplementation, SemLibraryCall, SemUnresolved, SemValueRef};

use super::*;

/// Edge kind of an implementation of `base`: `implements` for interface / trait / protocol
/// members and stubs, else `overrides`.
pub(super) fn implementation_kind(decls: &DeclTable<'_>, base: DeclRef<'_>) -> EdgeKind {
    let d = decls.decl(base);
    let in_interface = d
        .parent
        .and_then(|p| {
            decls.get(DeclRef {
                path: base.path,
                decl: p,
            })
        })
        .is_some_and(|p| p.kind == SymbolKind::Interface);
    if in_interface || d.is_stub {
        EdgeKind::Implements
    } else {
        EdgeKind::Overrides
    }
}

/// Apply batch-A / batch-C answers.
#[allow(clippy::too_many_arguments)]
pub(super) fn process_results<'a>(
    tags: Vec<Tag<'a>>,
    results: Vec<Result<Value, SemanticError>>,
    decls: &DeclTable<'a>,
    uris: &dyn UriResolver,
    opts: &Options<'_>,
    out: &mut BTreeMap<&'a str, FileOut>,
    incomplete: &mut HashSet<(String, u32)>,
    covered: &mut HashSet<(&'a str, usize)>,
    fallback_asked: &mut HashSet<(&'a str, usize)>,
    follow: &mut Followups<'a>,
) {
    for (tag, result) in tags.into_iter().zip(results) {
        let Some(o) = out.get_mut(tag.path()) else { continue };
        if let Tag::Fallback(path, ci, _) = &tag {
            fallback_asked.insert((*path, *ci));
        }
        let value = match result {
            Ok(value) => value,
            Err(e) => {
                let at = match &tag {
                    Tag::Symbols(_) => CountAt::File,
                    Tag::Prepare(r) => CountAt::At(decls.decl(*r).name_span.start),
                    Tag::Fallback(path, ci, _) => CountAt::At(
                        decls
                            .facts(path)
                            .and_then(|f| f.calls.get(*ci))
                            .map_or(0, |c| c.callee_span.start),
                    ),
                    Tag::Callback(_, cb) => CountAt::At(cb.arg_span.start),
                    Tag::ValueRef(_, reference) | Tag::Base(_, reference, _) => {
                        CountAt::At(reference.span.start)
                    }
                    Tag::Implementation(_) | Tag::TypePrepare(_) => CountAt::Impl,
                };
                if !o.outside(opts.hooks, tag.path(), &e) {
                    o.fail(&e, at);
                }
                continue;
            }
        };
        match tag {
            Tag::Symbols(path) => count_unmapped_symbols(&value, path, decls, opts.python, o),
            Tag::Prepare(r) => {
                let mut matching: Vec<Value> = into_items(value)
                    .into_iter()
                    .filter(|item| map_item(item, uris, decls) == Some(r))
                    .collect();
                if matching.len() == 1 {
                    follow.owners.push(r);
                    follow.items.extend(matching.pop());
                } else {
                    o.count_at("prepare_not_unique", decls.decl(r).name_span.start);
                }
            }
            Tag::Implementation(base) => {
                let kind = implementation_kind(decls, base);
                for implementor in implementation_targets(&value, base, decls, uris) {
                    o.implementations.push(SemImplementation {
                        base: base.decl,
                        implementor: decls.uid(implementor),
                        kind,
                    });
                }
            }
            Tag::TypePrepare(r) => {
                let mut matching: Vec<Value> = into_items(value)
                    .into_iter()
                    .filter(|item| map_item(item, uris, decls) == Some(r))
                    .collect();
                if matching.len() == 1 {
                    follow.types.extend(matching.pop().map(|item| (r, item)));
                }
            }
            Tag::Fallback(path, ci, owner) => {
                let Some(facts) = decls.facts(path) else { continue };
                let c = &facts.calls[ci];
                if definition_call(path, c, owner, &value, decls, uris, opts, o, incomplete) {
                    covered.insert((path, ci));
                }
            }
            Tag::Callback(path, cb) => callback(path, cb, &value, decls, uris, opts.python, o),
            Tag::ValueRef(path, reference) => {
                value_reference(path, reference, &value, decls, uris, opts.python, o)
            }
            Tag::Base(path, reference, declared) => {
                if declared {
                    value_reference(path, reference, &value, decls, uris, opts.python, o);
                }
                library_base(path, reference, &value, decls, uris, opts, o);
            }
        }
    }
}

/// Declarations named like `base` that implementation locations designate
/// (`Location | Location[] | LocationLink[]`): the declaration at the location's selection
/// start, else the unique declaration of that name whose name lies inside the location's
/// range. The base itself, synthetic scopes, non-callables and locations outside the index
/// are ignored.
pub(super) fn implementation_targets<'a>(
    value: &Value,
    base: DeclRef<'a>,
    decls: &DeclTable<'a>,
    uris: &dyn UriResolver,
) -> Vec<DeclRef<'a>> {
    let name = decls.decl(base).name.as_str();
    named_targets(value, name, decls, uris)
        .into_iter()
        .filter(|t| *t != base)
        .collect()
}

/// Callable, non-synthetic declarations named `name` that implementation locations
/// designate (the declaration at the selection start, else the unique declaration of that
/// name inside the location's range), in answer order, unique; locations outside the index
/// are ignored.
pub(super) fn named_targets<'a>(
    value: &Value,
    name: &str,
    decls: &DeclTable<'a>,
    uris: &dyn UriResolver,
) -> Vec<DeclRef<'a>> {
    let items: Vec<&Value> = match value {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![value],
        _ => Vec::new(),
    };
    let mut out: Vec<DeclRef<'a>> = Vec::new();
    for item in items {
        let Some(uri) = item
            .get("targetUri")
            .or_else(|| item.get("uri"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(path) = uris.rel_of(uri).and_then(|rel| decls.path_key(&rel)) else {
            continue;
        };
        let selection = item.get("targetSelectionRange").or_else(|| item.get("range"));
        let full = item.get("targetRange").or_else(|| item.get("range"));
        let at_start = selection
            .and_then(|r| r.get("start"))
            .and_then(position)
            .and_then(|(l, c)| decls.at_lsp(path, l, c))
            .filter(|r| decls.decl(*r).name == name);
        let found = at_start.or_else(|| {
            let range = full?;
            let byte = |key: &str| {
                range
                    .get(key)
                    .and_then(position)
                    .and_then(|(l, c)| decls.byte_of(path, l, c))
            };
            decls.by_name_within(path, name, byte("start")?, byte("end")?)
        });
        let Some(t) = found else { continue };
        if decls.is_synthetic(t) || !decls.decl(t).kind.is_callable() || out.contains(&t) {
            continue;
        }
        out.push(t);
    }
    out
}

/// One `textDocument/definition` result at a call's member identifier (servers without call
/// hierarchy, code no prepared item covers, and the fallback of batch C). `owner` is the
/// call's executing owner (`<module>` for module/class-level code). Returns whether the
/// call is covered (the server answered with any location).
#[allow(clippy::too_many_arguments)]
pub(super) fn definition_call<'a>(
    path: &'a str,
    c: &CallSite,
    owner: u32,
    value: &Value,
    decls: &DeclTable<'a>,
    uris: &dyn UriResolver,
    opts: &Options<'_>,
    o: &mut FileOut,
    incomplete: &mut HashSet<(String, u32)>,
) -> bool {
    let mut locations = definition_locations(value);
    if locations.is_empty() {
        return false; // blind: reported as no_semantic_target
    }
    if opts.hooks.answer_policy().dedupe_locations {
        let mut seen = HashSet::new();
        locations.retain(|l| seen.insert(*l));
    }
    let (mapped, externals) = resolve_locations(&locations, uris, decls);
    let language = file_language(decls, path);
    let conversion = language.is_some_and(trace_syntax::spec::type_call_is_conversion);
    let targets: Vec<DeclRef<'a>> = mapped
        .into_iter()
        .filter(|t| !decls.is_synthetic(*t))
        .filter(|t| c.member.as_deref().is_none_or(|m| decls.decl(*t).name == m))
        .collect();
    // Alias locations: a location inside an indexed file that maps to no declaration (a
    // using-declaration / alias line) never makes the answer external when another location
    // maps to a declaration named like the member.
    let outside: Vec<ServerLocation<'_>> = externals
        .iter()
        .copied()
        .filter(|(uri, _)| uris.rel_of(uri).and_then(|rel| decls.path_key(&rel)).is_none())
        .collect();
    let external = if targets.is_empty() {
        !externals.is_empty()
    } else {
        !outside.is_empty()
    };
    // Conversion languages: a call answered only by type declarations outside the index is a
    // type use (builtin / library type), resolved elsewhere, never an unresolved call.
    if conversion && targets.is_empty() && !outside.is_empty() && locations_are_types(&outside) {
        o.resolved_elsewhere
            .push(ByteSpan::new(member_point(c), c.callee_span.end));
        o.count_at("type_conversion", c.callee_span.start);
        return true;
    }
    // Locations outside the index: library calls (trace-library derives what they do).
    let cx = crate::external::ExternalContext {
        prepared: opts.prepared,
        hooks: opts.hooks,
    };
    let mut first_library: Option<(LibraryKey, String, Option<String>)> = None;
    for &(uri, (line, character)) in &externals {
        let uri = crate::mapping::canonical_uri(uri);
        if let Some((file, target)) = crate::external::classify(&uri, line, character, &cx) {
            if first_library.is_none() {
                first_library =
                    Some(((uri.clone(), line, character), file.package.clone(), target.symbol.clone()));
            }
            o.library_call(file, c.callee_span, c.line, target);
        }
    }
    // Library dispatch: a call through a receiver answered only by a library declaration.
    if let (true, Some((key, package, symbol))) = (targets.is_empty(), &first_library) {
        if let Some(candidate) = dispatch_candidate(path, c, owner, key, package, symbol.as_deref(), decls) {
            o.dispatch_candidates.push(candidate);
        }
    }
    // Scala applications (`crate::engine::rules::scala_apply`), then overloads: several in-index targets
    // narrowed by the call's argument count (C / C++: argument lists and lookup completeness,
    // `cpp_calls`).
    let before = targets.len();
    let targets = crate::engine::rules::scala_apply::narrow_application(targets, c, language, decls);
    if targets.len() < before {
        o.count_at("scala_application", c.callee_span.start);
    }
    let before = targets.len();
    let (targets, provable) = if rules::cpp_calls::is_c_family(language) && before > 1 {
        let n = rules::cpp_calls::narrow(targets, c, decls, path);
        (n.targets, n.proven)
    } else {
        (narrow_by_arity(targets, c, language, decls, path), true)
    };
    if targets.len() < before {
        o.count_at("arity_narrowed", c.callee_span.start);
    }
    // A single definition answer is proof.
    if targets.len() == 1 && !external && provable {
        if conversion && decls.decl(targets[0]).kind.is_type() {
            o.count_at("type_conversion", c.callee_span.start);
        }
        o.edges
            .push(call_edge(c, owner, targets[0], decls, Resolution::Definition, conversion));
    } else {
        let mut candidates: Vec<String> = targets.iter().map(|t| decls.uid(*t)).collect();
        candidates.sort();
        candidates.dedup();
        if external {
            incomplete.insert((path.to_string(), c.callee_span.start));
        }
        o.unresolved.push(SemUnresolved {
            owner: Some(owner),
            kind: UnresolvedKind::ExternalOrAmbiguous,
            at: c.callee_span,
            line: c.line,
            callee: c.callee.clone(),
            candidates,
        });
    }
    true
}

/// The edge of call `c` (executed by `owner`) resolved to `target`: `constructor` when a
/// type is called (`new C()`, Python `C()`, Rust tuple structs) or `new` meets a
/// constructor, else the activation kind of the target's execution model. `conversion`:
/// the file's language converts a value when a type is called
/// (`trace_syntax::spec::type_call_is_conversion`, Go `T(x)`): a type target is a type use
/// (`references`), no constructor runs.
pub(super) fn call_edge<'a>(
    c: &CallSite,
    owner: u32,
    target: DeclRef<'a>,
    decls: &DeclTable<'a>,
    resolution: Resolution,
    conversion: bool,
) -> SemEdge {
    let d = decls.decl(target);
    let kind = if conversion && d.kind.is_type() {
        EdgeKind::References
    } else if d.kind.is_type() || (c.is_new && d.kind == SymbolKind::Constructor) {
        EdgeKind::Constructor
    } else {
        activation_kind(c.activation, d.execution)
    };
    SemEdge {
        owner,
        target: decls.uid(target),
        kind,
        at: c.callee_span,
        line: c.line,
        resolution,
    }
}

/// A function passed as an argument, resolved by definition (a reference, never a proven
/// call). Attribute arguments (`payments.parse_webhook`) resolve to the declaration the
/// compiler binds, which may be an abstract/protocol method: dispatch to implementations is
/// inference's job. Module-level arguments are owned by `<module>`.
pub(super) fn callback<'a>(
    path: &'a str,
    cb: &CallbackArg,
    value: &Value,
    decls: &DeclTable<'a>,
    uris: &dyn UriResolver,
    python: bool,
    o: &mut FileOut,
) {
    let Some(owner) = decls.facts(path).and_then(|f| f.executing_owner(cb.owner)) else {
        return;
    };
    let locations = definition_locations(value);
    let (mapped, _) = resolve_locations(&locations, uris, decls);
    // Resolved elsewhere: the server answered, but with no declaration of this name (a
    // library / builtin symbol, a local binding, another symbol).
    if !locations.is_empty() && !mapped.iter().any(|t| decls.decl(*t).name == cb.name) {
        o.resolved_elsewhere.push(cb.arg_span);
        return;
    }
    // A parameter/local on a def line never maps (name-position rule); still require
    // the resolved declaration's own name.
    let targets: Vec<DeclRef<'a>> = mapped
        .into_iter()
        .filter(|t| {
            let d = decls.decl(*t);
            d.kind.is_callable() && d.name == cb.name && !decls.is_synthetic(*t)
        })
        .collect();
    if let Some(target) = unique_target(targets, decls, python) {
        o.edges.push(SemEdge {
            owner,
            target: decls.uid(target),
            kind: EdgeKind::PassesCallback,
            at: cb.arg_span,
            line: decls.line1(path, cb.arg_span.start).unwrap_or(0),
            resolution: Resolution::Definition,
        });
    }
}

/// A header base answered only by locations outside the index: the library class
/// (`FileSemantics::library_bases`, first classified location).
pub(super) fn library_base(
    path: &str,
    reference: &Reference,
    value: &Value,
    decls: &DeclTable<'_>,
    uris: &dyn UriResolver,
    opts: &Options<'_>,
    o: &mut FileOut,
) {
    let locations = definition_locations(value);
    let (mapped, externals) = resolve_locations(&locations, uris, decls);
    if !mapped.is_empty() {
        return;
    }
    let cx = crate::external::ExternalContext {
        prepared: opts.prepared,
        hooks: opts.hooks,
    };
    for &(uri, (line, character)) in &externals {
        let uri = crate::mapping::canonical_uri(uri);
        if let Some((file, target)) = crate::external::classify(&uri, line, character, &cx) {
            let index = o.intern_library_file(file);
            o.library_bases.push(SemLibraryCall {
                at: reference.span,
                line: decls.line1(path, reference.span.start).unwrap_or(0),
                file: index,
                decl_line: target.decl_line,
                decl_column: target.decl_column,
                symbol: target.symbol,
            });
            return;
        }
    }
}

/// Edge kind of a resolved non-call use (SPEC §8.5 "Non-call uses"); `None` for arguments
/// (the callback path records `passes_callback`).
pub(crate) fn use_edge_kind(kind: RefKind) -> Option<EdgeKind> {
    match kind {
        RefKind::Read | RefKind::Decorator | RefKind::Type => Some(EdgeKind::References),
        RefKind::Write => Some(EdgeKind::Writes),
        RefKind::Import => Some(EdgeKind::Imports),
        RefKind::Export => Some(EdgeKind::Reexports),
        RefKind::Argument => None,
    }
}

/// Uses that stay flow inputs (`SemValueRef`): values read at the reference.
pub(super) fn is_value_read(kind: RefKind) -> bool {
    matches!(kind, RefKind::Read | RefKind::Decorator | RefKind::Argument)
}

/// A non-call use anywhere in a queried file (module/class level included): an edge owned
/// by `executing_owner(reference.owner)` of the use's kind, plus a value reference for reads.
pub(super) fn value_reference<'a>(
    path: &'a str,
    reference: &Reference,
    value: &Value,
    decls: &DeclTable<'a>,
    uris: &dyn UriResolver,
    python: bool,
    o: &mut FileOut,
) {
    let locations = definition_locations(value);
    let (mapped, _) = resolve_locations(&locations, uris, decls);
    // Import bindings may rename (`import { a as b }` records the local binding): the
    // compiler's target decides; other uses must resolve to a declaration of that name.
    let import = reference.kind == RefKind::Import;
    let targets: Vec<DeclRef<'a>> = mapped
        .into_iter()
        .filter(|t| !decls.is_synthetic(*t))
        .filter(|t| import || decls.decl(*t).name == reference.name)
        .collect();
    // Resolved elsewhere: the server answered, but with no declaration of this name (a
    // library / builtin symbol, a local binding, another symbol).
    if targets.is_empty() && !locations.is_empty() {
        o.resolved_elsewhere.push(reference.span);
        return;
    }
    let Some(target) = unique_target(targets, decls, python) else {
        return;
    };
    let line = decls.line1(path, reference.span.start).unwrap_or(0);
    let uid = decls.uid(target);
    if is_value_read(reference.kind) {
        o.value_refs.push(SemValueRef {
            at: reference.span,
            line,
            target: uid.clone(),
        });
    }
    let Some(kind) = use_edge_kind(reference.kind) else {
        return;
    };
    // The declaration's own name is not a use of it.
    if target.path == path && decls.decl(target).name_span.contains(reference.span.start) {
        return;
    }
    let Some(owner) = decls.facts(path).and_then(|f| f.executing_owner(reference.owner)) else {
        return;
    };
    o.edges.push(SemEdge {
        owner,
        target: uid,
        kind,
        at: reference.span,
        line,
        resolution: Resolution::Definition,
    });
}

/// Exactly one target after the (Python-only) stub/implementation collapse.
pub(super) fn unique_target<'a>(
    targets: Vec<DeclRef<'a>>,
    decls: &DeclTable<'a>,
    python: bool,
) -> Option<DeclRef<'a>> {
    let mut uids: std::collections::BTreeSet<String> = targets.iter().map(|t| decls.uid(*t)).collect();
    if python {
        uids = crate::stubs::collapse_stub_pairs(uids);
    }
    if uids.len() != 1 {
        return None;
    }
    let uid = uids.into_iter().next()?;
    decls.by_uid(&uid)
}

pub(super) fn constructor_bridge<'a>(
    located: &[(DeclRef<'a>, (u32, u32))],
    decls: &DeclTable<'a>,
    out: &mut BTreeMap<&'a str, FileOut>,
) {
    let is_located = |r: DeclRef<'a>| {
        located
            .binary_search_by(|(l, _)| (l.path, l.decl).cmp(&(r.path, r.decl)))
            .is_ok()
    };
    for &(class, _) in located {
        if decls.decl(class).kind != SymbolKind::Class {
            continue;
        }
        let Some(facts) = decls.facts(class.path) else {
            continue;
        };
        // Python runs the last definition of `__init__` in the class body.
        let init = facts
            .declarations
            .iter()
            .enumerate()
            .filter(|(_, d)| d.parent == Some(class.decl) && d.name == "__init__")
            .map(|(i, _)| i as u32)
            .next_back();
        let Some(init) = init else { continue };
        let init = DeclRef {
            path: class.path,
            decl: init,
        };
        if !is_located(init) {
            continue;
        }
        if let Some(o) = out.get_mut(class.path) {
            o.edges.push(SemEdge {
                owner: class.decl,
                target: decls.uid(init),
                kind: EdgeKind::Constructor,
                at: decls.decl(init).name_span,
                line: decls.name_line(init).unwrap_or(0),
                resolution: Resolution::ConstructorDeclaration,
            });
        }
    }
}

/// Edge kind of a call-hierarchy result at a point (pyright.py step 3).
pub(super) fn edge_kind(
    call: Option<&CallSite>,
    execution: ExecutionModel,
    item_kind: Option<u64>,
) -> EdgeKind {
    match call {
        Some(call) => activation_kind(call.activation, execution),
        None if item_kind == Some(7) => EdgeKind::PropertyGet,
        None => EdgeKind::References,
    }
}

/// Kind of a call to a target with `execution`, given how the call site consumes it.
pub(crate) fn activation_kind(activation: Activation, execution: ExecutionModel) -> EdgeKind {
    match (execution, activation) {
        (ExecutionModel::Ordinary, _) => EdgeKind::Calls,
        (_, Activation::Await) => EdgeKind::Awaits,
        (_, Activation::Iterate) => EdgeKind::Iterates,
        (ExecutionModel::Coroutine, Activation::Plain) => EdgeKind::CreatesCoroutine,
        (_, Activation::Plain) => EdgeKind::CreatesGenerator,
    }
}

impl<'a> Shard<'a, '_> {
    /// Batch A (step 3 of the module docs): `documentSymbol` for files with syntax errors,
    /// `prepareCallHierarchy`, implementations / type-hierarchy preparation and `definition`
    /// for calls, callbacks, non-call uses and header bases, as one pipelined batch.
    pub(super) fn batch_a(&mut self, session: &mut dyn Session) -> Result<(), SemanticError> {
        let mut calls: Vec<(String, Value)> = Vec::new();
        let mut tags: Vec<Tag<'a>> = Vec::new();
        // (top-level directory, absolute import target) -> index of the request answering it.
        let mut shared_imports: HashMap<(&'a str, &'a str), usize> = HashMap::new();
        let mut copies: Vec<(usize, Tag<'a>)> = Vec::new();
        let mut value_ref_kinds: BTreeMap<&'static str, usize> = BTreeMap::new();
        if self.has_symbols {
            for f in self.active.iter().filter(|f| f.facts.error_count > 0) {
                calls.push((
                    "textDocument/documentSymbol".to_string(),
                    json!({"textDocument": {"uri": self.uri_of[f.path]}}),
                ));
                tags.push(Tag::Symbols(f.path));
            }
        }
        if self.has_hierarchy {
            for &(r, (line, character)) in &self.located {
                if !self.decls.decl(r).kind.is_callable() {
                    continue;
                }
                calls.push((
                    "textDocument/prepareCallHierarchy".to_string(),
                    json!({
                        "textDocument": {"uri": self.uri_of[r.path]},
                        "position": {"line": line, "character": character}
                    }),
                ));
                tags.push(Tag::Prepare(r));
                self.attempted.insert(r);
            }
        }
        // Implementations of interface / trait / abstract members and subtypes of types
        // (SPEC section 8.5a), pipelined with the rest of batch A.
        if self.has_implementation || self.has_type_hierarchy {
            for &(r, (line, character)) in &self.located {
                let Some(facts) = self.decls.facts(r.path) else { continue };
                if self.implementations_reused(r.path) {
                    continue;
                }
                let position = json!({
                    "textDocument": {"uri": self.uri_of[r.path]},
                    "position": {"line": line, "character": character}
                });
                if implementable(facts, r.decl) {
                    // Request diet: only a same-named callable elsewhere can be an answer.
                    if self.has_implementation && named_elsewhere(self.decls, r) {
                        calls.push(("textDocument/implementation".to_string(), position));
                        tags.push(Tag::Implementation(r));
                    }
                } else if self.has_type_hierarchy
                    && self.decls.decl(r).kind.is_type()
                    && type_members(self.decls, r)
                        .iter()
                        .any(|m| named_elsewhere(self.decls, *m))
                {
                    calls.push(("textDocument/prepareTypeHierarchy".to_string(), position));
                    tags.push(Tag::TypePrepare(r));
                }
            }
        }
        if self.has_definition {
            for f in &self.active {
                let facts: &'a FileFacts = f.facts;
                for (ci, c) in facts.calls.iter().enumerate() {
                    let Some(owner) = facts.executing_owner(c.owner) else {
                        continue;
                    };
                    if !self
                        .decls
                        .get(DeclRef {
                            path: f.path,
                            decl: owner,
                        })
                        .is_some_and(|d| d.kind.is_executable())
                    {
                        continue;
                    }
                    // With call hierarchy only code no prepared item covers (module level,
                    // class bodies, lambdas there) is resolved here; without it, every call.
                    if self.has_hierarchy && prepared_root(facts, owner).is_some() {
                        continue;
                    }
                    if self.reused_at(f.path, c.callee_span.start) {
                        self.covered.insert((f.path, ci));
                        continue;
                    }
                    if self.in_inactive(f.path, c.callee_span.start) {
                        record_inactive(f.path, ci, c, owner, &mut self.out, &mut self.covered);
                        continue;
                    }
                    let has_callback = self.with_callback.contains(&(f.path, c.callee_span.start));
                    let answer = call_answer(
                        f,
                        c,
                        self.decls,
                        self.opts,
                        self.shell_scopes.get(f.path),
                        has_callback,
                        &mut self.programs,
                    );
                    if !matches!(answer, CallAnswer::Ask) {
                        apply_call_answer(
                            answer,
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
                    let Some((line, character)) = self.decls.lsp_of(f.path, member_point(c)) else {
                        continue;
                    };
                    if has_callback
                        && self.opts.syntax_answers
                        && member_undeclared(f.path, c, self.decls, self.shell_scopes.get(f.path))
                    {
                        if let Some(o) = self.out.get_mut(f.path) {
                            o.asked_by_name.insert(ci);
                        }
                    }
                    calls.push(definition_request(&self.uri_of[f.path], line, character));
                    tags.push(Tag::Fallback(f.path, ci, owner));
                }
            }
            for f in &self.active {
                let facts: &'a FileFacts = f.facts;
                // Bare names that syntax proves are local variables (parameters, assignments)
                // can only be defined by their own binding, never by a declaration.
                let locals: HashSet<u32> = facts
                    .references
                    .iter()
                    .filter(|r| r.local)
                    .map(|r| r.span.start)
                    .collect();
                for cb in &facts.callbacks {
                    let Some(owner) = facts.executing_owner(cb.owner) else {
                        continue;
                    };
                    let owner_ref = DeclRef {
                        path: f.path,
                        decl: owner,
                    };
                    if !self.decls.get(owner_ref).is_some_and(|d| d.kind.is_executable())
                        || !self.decls.is_callable_name(&cb.name)
                        || locals.contains(&cb.arg_span.start)
                        || facts.is_local(cb.arg_span)
                        || self.reused_at(f.path, cb.arg_span.start)
                    {
                        continue;
                    }
                    let Some((line, character)) = self.decls.lsp_of(f.path, cb.arg_span.start) else {
                        continue;
                    };
                    calls.push(definition_request(&self.uri_of[f.path], line, character));
                    tags.push(Tag::Callback(f.path, cb));
                }
                // Python scoping: a bare name can only denote an indexed declaration declared in
                // this file or bound by one of its imports (or any name after a wildcard
                // import); other bare names are module variables or builtins.
                let visible: Option<HashSet<&str>> = (self.opts.python
                    && !facts
                        .imports
                        .iter()
                        .any(|i| i.kind == trace_core::facts::ImportKind::Wildcard))
                .then(|| {
                    facts
                        .declarations
                        .iter()
                        .map(|d| d.name.as_str())
                        .chain(facts.imports.iter().map(|i| i.local.as_str()))
                        .collect()
                });
                for reference in &facts.references {
                    // Header bases are asked whatever their name: a library answer is a library
                    // base (`crate::bases`).
                    if crate::bases::is_header_base(facts, reference)
                        && !self.reused_at(f.path, reference.span.start)
                    {
                        let Some((line, character)) = self.decls.lsp_of(f.path, reference.span.start) else {
                            continue;
                        };
                        let declared = self.decls.is_declared_name(&reference.name);
                        if declared {
                            *value_ref_kinds.entry(reference.kind.as_str()).or_insert(0) += 1;
                        }
                        calls.push(definition_request(&self.uri_of[f.path], line, character));
                        tags.push(Tag::Base(f.path, reference, declared));
                        continue;
                    }
                    if reference.local
                        || facts.is_local(reference.span)
                        || !self.decls.is_declared_name(&reference.name)
                        || self.reused_at(f.path, reference.span.start)
                    {
                        continue;
                    }
                    if let Some(visible) = &visible {
                        if !visible.contains(reference.name.as_str())
                            && reference.kind != RefKind::Import
                            && !is_attribute_name(f.source, reference.span.start)
                        {
                            continue;
                        }
                    }
                    // Python `from pkg.mod import name` (absolute): the imported name resolves
                    // to the same declaration in every importing file of one top-level
                    // directory (same search roots), so one definition request answers all.
                    let shared = if self.opts.python && reference.kind == RefKind::Import {
                        absolute_import_key(facts, reference, f.path)
                    } else {
                        None
                    };
                    if let Some(key) = &shared {
                        if let Some(&first) = shared_imports.get(key) {
                            copies.push((first, Tag::ValueRef(f.path, reference)));
                            continue;
                        }
                    }
                    let Some((line, character)) = self.decls.lsp_of(f.path, reference.span.start) else {
                        continue;
                    };
                    if let Some(key) = shared {
                        shared_imports.insert(key, calls.len());
                    }
                    *value_ref_kinds.entry(reference.kind.as_str()).or_insert(0) += 1;
                    calls.push(definition_request(&self.uri_of[f.path], line, character));
                    tags.push(Tag::ValueRef(f.path, reference));
                }
            }
        }
        if trace_core::env::profile() {
            let mut kinds: Vec<String> = value_ref_kinds.iter().map(|(k, n)| format!("{k}={n}")).collect();
            kinds.sort();
            eprintln!(
                "profile-semantic: batch_a requests={} value_refs {} shared_imports={}",
                calls.len(),
                kinds.join(" "),
                copies.len()
            );
        }
        let mut results = if calls.is_empty() {
            Vec::new()
        } else {
            request_unique(session, calls)?
        };
        // Answers shared by identical absolute imports (a failed request is counted once).
        for (first, tag) in copies {
            if let Some(Ok(value)) = results.get(first) {
                let value = value.clone();
                tags.push(tag);
                results.push(Ok(value));
            }
        }

        process_results(
            tags,
            results,
            self.decls,
            self.uris,
            self.opts,
            &mut self.out,
            &mut self.incomplete,
            &mut self.covered,
            &mut self.fallback_asked,
            &mut self.follow,
        );
        Ok(())
    }

    /// Batch C (step 5 of the module docs): the definition fallback.
    pub(super) fn batch_c(&mut self, session: &mut dyn Session) -> Result<(), SemanticError> {
        // 5. Batch C (definition fallback, both compiler facts): calls of a prepared scope that
        //    no outgoing-call range covered (servers other than Pyright: rust-analyzer loses
        //    calls in builder chains / associated functions to its call hierarchy), and calls
        //    of declarations whose prepare did not map back uniquely (every server).
        if self.has_hierarchy && self.has_definition {
            let mut calls: Vec<(String, Value)> = Vec::new();
            let mut tags: Vec<Tag<'a>> = Vec::new();
            for f in &self.active {
                let facts: &'a FileFacts = f.facts;
                for (ci, c) in facts.calls.iter().enumerate() {
                    if self.covered.contains(&(f.path, ci))
                        || self.fallback_asked.contains(&(f.path, ci))
                        || self.reused_at(f.path, c.callee_span.start)
                    {
                        continue;
                    }
                    let Some(owner) = c.owner else { continue };
                    let Some(root) = prepared_root(facts, owner) else {
                        continue;
                    };
                    let root = DeclRef {
                        path: f.path,
                        decl: root,
                    };
                    let failed = self.attempted.contains(&root) && !self.prepared_ok.contains(&root);
                    if self.opts.python && !failed {
                        continue;
                    }
                    if self.in_inactive(f.path, c.callee_span.start) {
                        record_inactive(f.path, ci, c, owner, &mut self.out, &mut self.covered);
                        continue;
                    }
                    let has_callback = self.with_callback.contains(&(f.path, c.callee_span.start));
                    let answer = call_answer(
                        f,
                        c,
                        self.decls,
                        self.opts,
                        self.shell_scopes.get(f.path),
                        has_callback,
                        &mut self.programs,
                    );
                    if !matches!(answer, CallAnswer::Ask) {
                        apply_call_answer(
                            answer,
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
                    let Some((line, character)) = self.decls.lsp_of(f.path, member_point(c)) else {
                        continue;
                    };
                    if has_callback
                        && self.opts.syntax_answers
                        && member_undeclared(f.path, c, self.decls, self.shell_scopes.get(f.path))
                    {
                        if let Some(o) = self.out.get_mut(f.path) {
                            o.asked_by_name.insert(ci);
                        }
                    }
                    calls.push(definition_request(&self.uri_of[f.path], line, character));
                    tags.push(Tag::Fallback(f.path, ci, owner));
                    if let Some(o) = self.out.get_mut(f.path) {
                        o.count_at("definition_fallback", c.callee_span.start);
                    }
                }
            }
            if !calls.is_empty() {
                let results = request_unique(session, calls)?;
                let mut late = Followups::default();
                process_results(
                    tags,
                    results,
                    self.decls,
                    self.uris,
                    self.opts,
                    &mut self.out,
                    &mut self.incomplete,
                    &mut self.covered,
                    &mut self.fallback_asked,
                    &mut late,
                );
            }
        }
        Ok(())
    }
}
