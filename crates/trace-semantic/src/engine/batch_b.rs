//! Batch B answers: `callHierarchy/outgoingCalls` ranges matched to syntax calls (point-to-call
//! matching, overloads by arity) and the `typeHierarchy/subtypes` walks ([`TypeWalks`]).

use crate::mapping::{DeclRef, DeclTable};
use crate::SemanticError;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use trace_core::facts::CallSite;
use trace_core::model::{ByteSpan, EdgeKind, Resolution, UnresolvedKind};
use trace_core::semantics::{LibraryFile, SemEdge, SemImplementation, SemUnresolved, SemValueRef};
use trace_core::Language;

use super::*;

/// A base type's search for members its subtypes redeclare (type hierarchy).
pub(super) struct TypeWalk<'a> {
    pub(super) base: DeclRef<'a>,
    /// Callable members of the base type ([`type_members`]).
    pub(super) members: Vec<DeclRef<'a>>,
}

/// One pending `typeHierarchy/subtypes` request of a walk.
pub(super) struct Frontier {
    pub(super) walk: usize,
    pub(super) item: Value,
    /// Indices into the walk's members not yet redeclared on this path.
    pub(super) unmatched: Vec<usize>,
    pub(super) depth: usize,
}

/// Type-hierarchy walks of one shard (module docs): a subtype that declares a member of the
/// base records a [`SemImplementation`] on the base file; members it does not declare are
/// searched further down (up to [`MAX_TYPE_DEPTH`], at most `semantic.max_subtype_requests`).
pub(super) struct TypeWalks<'a> {
    pub(super) walks: Vec<TypeWalk<'a>>,
    pub(super) pending: Vec<Frontier>,
    pub(super) visited: HashSet<(usize, DeclRef<'a>)>,
    pub(super) sent: usize,
}

impl<'a> TypeWalks<'a> {
    pub(super) fn new(types: Vec<(DeclRef<'a>, Value)>, decls: &DeclTable<'a>) -> Self {
        let mut walks = Vec::new();
        let mut pending = Vec::new();
        let mut visited = HashSet::new();
        for (base, item) in types {
            let members = type_members(decls, base);
            if members.is_empty() {
                continue;
            }
            let walk = walks.len();
            visited.insert((walk, base));
            pending.push(Frontier {
                walk,
                item,
                unmatched: (0..members.len()).collect(),
                depth: 1,
            });
            walks.push(TypeWalk { base, members });
        }
        TypeWalks {
            walks,
            pending,
            visited,
            sent: 0,
        }
    }

    /// The next round of requests within the request bound (the rest is dropped with a
    /// `subtypes_bounded` diagnostic on the base file).
    pub(super) fn next_round(&mut self, out: &mut BTreeMap<&'a str, FileOut>) -> Vec<Frontier> {
        let mut round = std::mem::take(&mut self.pending);
        let room = trace_core::config::current()
            .semantic
            .max_subtype_requests
            .saturating_sub(self.sent);
        if round.len() > room {
            for f in round.drain(room..) {
                if let Some(o) = out.get_mut(self.walks[f.walk].base.path) {
                    o.count_impl("subtypes_bounded");
                }
            }
        }
        self.sent += round.len();
        round
    }

    /// Apply the answers of one round.
    pub(super) fn absorb(
        &mut self,
        round: Vec<Frontier>,
        results: Vec<Result<Value, SemanticError>>,
        decls: &DeclTable<'a>,
        uris: &dyn UriResolver,
        out: &mut BTreeMap<&'a str, FileOut>,
    ) {
        for (f, result) in round.into_iter().zip(results) {
            let base = self.walks[f.walk].base;
            let value = match result {
                Ok(value) => value,
                Err(e) => {
                    if let Some(o) = out.get_mut(base.path) {
                        o.fail(&e, CountAt::Impl);
                    }
                    continue;
                }
            };
            for sub in into_items(value) {
                let Some(s) = map_item(&sub, uris, decls) else { continue };
                if !decls.decl(s).kind.is_type() || !self.visited.insert((f.walk, s)) {
                    continue;
                }
                let mut rest = Vec::new();
                for &mi in &f.unmatched {
                    let member = self.walks[f.walk].members[mi];
                    let found = members_named(decls, s, &decls.decl(member).name);
                    if found.is_empty() {
                        rest.push(mi);
                        continue;
                    }
                    let kind = implementation_kind(decls, member);
                    if let Some(o) = out.get_mut(member.path) {
                        for implementor in found {
                            o.implementations.push(SemImplementation {
                                base: member.decl,
                                implementor: decls.uid(implementor),
                                kind,
                            });
                        }
                    }
                }
                if !rest.is_empty() && f.depth < MAX_TYPE_DEPTH {
                    self.pending.push(Frontier {
                        walk: f.walk,
                        item: sub,
                        unmatched: rest,
                        depth: f.depth + 1,
                    });
                }
            }
        }
    }
}

/// The matched syntax call (if any) and every outgoing-call choice at one point.
pub(super) type SiteChoices<'a> = (Option<usize>, Vec<Choice<'a>>);

/// One outgoing-call range grouped by evidence point.
pub(super) struct Choice<'a> {
    pub(super) target: Option<DeclRef<'a>>,
    pub(super) item_kind: Option<u64>,
    pub(super) end: u32,
    pub(super) line: u32,
    /// A `to` item outside the index classified as a library declaration: (canonical uri,
    /// line, column) of the declaration, the library file and the target.
    pub(super) library: Option<(LibraryKey, LibraryFile, crate::external::SemLibraryCallTarget)>,
}

/// Canonical library declaration of an answer: (canonical uri, 0-based line, character).
pub(super) type LibraryKey = (String, u32, u32);

/// Process one `callHierarchy/outgoingCalls` result; returns the indices of the syntax calls
/// it covered (ranges attributed to the prepared item or a scope nested in it).
/// `to` items outside the index are classified like definition locations and recorded as
/// library calls of the matched syntax call (module docs).
#[allow(clippy::too_many_arguments)]
pub(super) fn outgoing_calls<'a>(
    owner: DeclRef<'a>,
    value: &Value,
    decls: &DeclTable<'a>,
    uris: &dyn UriResolver,
    index: &CallIndex,
    cx: &crate::external::ExternalContext<'_>,
    o: &mut FileOut,
    incomplete: &mut HashSet<(String, u32)>,
) -> Vec<usize> {
    let path = owner.path;
    let (Some(facts), Some(source)) = (decls.facts(path), decls.source(path)) else {
        return Vec::new();
    };
    let language = file_language(decls, path);
    let conversion = language.is_some_and(trace_syntax::spec::type_call_is_conversion);
    // Key: (executing declaration or None for a lazy scope without one, point, matched
    // syntax call); value: the matched syntax call and the choices at that point. Ranges over
    // whole invocations share their start with the calls of their receiver chain
    // (`make().helper(1)` and `make()`), so the matched call is part of the key.
    let mut by_site: BTreeMap<(Option<u32>, u32, Option<usize>), SiteChoices<'a>> = BTreeMap::new();
    for call in value.as_array().map(Vec::as_slice).unwrap_or_default() {
        let Some(to) = call.get("to") else { continue };
        let target = map_item(to, uris, decls);
        let item_kind = to.get("kind").and_then(Value::as_u64);
        // Outside the index: a library declaration when the location is in a library.
        let library = if target.is_none() {
            library_item(to, cx)
        } else {
            None
        };
        let ranges = call
            .get("fromRanges")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for range in ranges {
            let point = range
                .get("start")
                .and_then(position)
                .and_then(|(l, c)| decls.byte_of(path, l, c).map(|b| (b, l)));
            let Some((point, line0)) = point else {
                o.count_at("unmapped_position", decls.decl(owner).name_span.start);
                continue;
            };
            let end = range
                .get("end")
                .and_then(position)
                .and_then(|(l, c)| decls.byte_of(path, l, c))
                .unwrap_or(point)
                .max(point);
            let matched = index.matching(facts, point, end);
            let executing = match matched {
                Some(i) => facts.calls[i].owner,
                None => owner_at(facts, point),
            };
            let scope = match executing {
                Some(e) if nested_in(facts, e, owner.decl) => Some(e),
                Some(_) => {
                    o.count_at("different_execution_scope", point);
                    continue;
                }
                None => None,
            };
            let entry = by_site
                .entry((scope, point, matched))
                .or_insert_with(|| (matched, Vec::new()));
            entry.1.push(Choice {
                target,
                item_kind,
                end,
                line: line0 + 1,
                library: library.clone(),
            });
        }
    }
    let mut covered = Vec::with_capacity(by_site.len());
    for (&(scope, point, _), (matched, choices)) in &by_site {
        let mut distinct: Vec<DeclRef<'a>> = Vec::new();
        for choice in choices {
            if let Some(t) = choice.target {
                if !distinct.contains(&t) {
                    distinct.push(t);
                }
            }
        }
        let external = choices.iter().any(|c| c.target.is_none());
        let call = matched.map(|i| &facts.calls[i]);
        let first = &choices[0];
        let Some(executing) = scope else {
            // Lazy scope without a declaration: a resolution fact, never an edge.
            match call {
                Some(c) if c.owner.is_none() && !external && distinct.len() == 1 => {
                    // Keyed like syntax references: the identifier (attribute name for
                    // `a.b`), i.e. the callee's member identifier.
                    o.value_refs.push(SemValueRef {
                        at: ByteSpan::new(member_point(c), c.callee_span.end),
                        line: c.line,
                        target: decls.uid(distinct[0]),
                    });
                    o.count_at("lazy_scope_call", point);
                }
                _ => o.count_at("different_execution_scope", point),
            }
            continue;
        };
        // C / C++ macro invocation: the invocation runs its expansion, so the in-index
        // targets it calls are called there (`rules::cpp_calls::expansion_callees`); the call's own
        // target (the macro) is left to the definition fallback.
        if let Some(c) = call.filter(|_| rules::cpp_calls::is_c_family(language)) {
            if rules::cpp_calls::macro_invocation(c, &distinct, decls) {
                o.count_at("macro_invocation", c.callee_span.start);
                for target in rules::cpp_calls::expansion_callees(&distinct, decls) {
                    let item_kind = choices
                        .iter()
                        .find(|ch| ch.target == Some(target))
                        .and_then(|ch| ch.item_kind);
                    o.edges.push(SemEdge {
                        owner: executing,
                        target: decls.uid(target),
                        kind: edge_kind(call, decls.decl(target).execution, item_kind),
                        at: c.callee_span,
                        line: c.line,
                        resolution: Resolution::CallHierarchy,
                    });
                }
                continue;
            }
        }
        if let Some(i) = *matched {
            covered.push(i);
        }
        // Scala applications (`apply` sugar, case class construction), then overloads:
        // several in-index targets narrowed by the call's argument count (C / C++: implicit
        // calls and argument lists, `cpp_calls`).
        let mut provable = true;
        if let Some(c) = call {
            if distinct.len() > 1 {
                let applied = crate::engine::rules::scala_apply::narrow_application(
                    distinct.clone(),
                    c,
                    language,
                    decls,
                );
                if applied.len() < distinct.len() {
                    o.count_at("scala_application", c.callee_span.start);
                    distinct = applied;
                }
            }
            if distinct.len() > 1 {
                let narrowed = if rules::cpp_calls::is_c_family(language) {
                    let n = rules::cpp_calls::narrow(distinct.clone(), c, decls, path);
                    provable = n.proven;
                    n.targets
                } else {
                    narrow_by_arity(distinct.clone(), c, language, decls, path)
                };
                if narrowed.len() < distinct.len() {
                    o.count_at("arity_narrowed", c.callee_span.start);
                    distinct = narrowed;
                }
            }
        }
        if !external && distinct.len() == 1 && provable {
            let target = distinct[0];
            // Ranges over the whole invocation (jdtls, Roslyn) are keyed on the callee, like
            // definition answers; member-identifier ranges keep their exact span.
            let (at, line) = match call {
                Some(c) if first.end > c.callee_span.end => (c.callee_span, c.line),
                // A member-identifier range followed by template / generic arguments
                // (`detail::f<T>(x)`) is keyed up to the end of the callee it designates.
                Some(c) if first.end < c.callee_span.end && point >= c.callee_span.start => {
                    (ByteSpan::new(point, c.callee_span.end), c.line)
                }
                _ => (ByteSpan::new(point, first.end), first.line),
            };
            // Calling a type converts a value in conversion languages: a type use.
            let type_use = conversion && decls.decl(target).kind.is_type();
            let kind = if type_use {
                o.count_at("type_conversion", point);
                EdgeKind::References
            } else {
                edge_kind(call, decls.decl(target).execution, first.item_kind)
            };
            o.edges.push(SemEdge {
                owner: executing,
                target: decls.uid(target),
                kind,
                at,
                line,
                resolution: Resolution::CallHierarchy,
            });
        } else {
            let (at, line, callee) = match call {
                Some(c) => (c.callee_span, c.line, c.callee.clone()),
                None => {
                    let at = ByteSpan::new(point, first.end);
                    (at, first.line, slice_text(source, at))
                }
            };
            let mut candidates: Vec<String> = distinct.iter().map(|t| decls.uid(*t)).collect();
            candidates.sort();
            candidates.dedup();
            if external {
                incomplete.insert((path.to_string(), at.start));
            }
            o.unresolved.push(SemUnresolved {
                owner: Some(executing),
                kind: UnresolvedKind::ExternalOrAmbiguous,
                at,
                line,
                callee,
                candidates,
            });
            // Library call: the first `to` item classified as a library declaration.
            let library = choices.iter().find_map(|ch| ch.library.as_ref());
            if let (Some(c), Some((key, file, target))) = (call, library) {
                if distinct.is_empty() {
                    let symbol = target.symbol.as_deref();
                    if let Some(candidate) =
                        dispatch_candidate(path, c, executing, key, &file.package, symbol, decls)
                    {
                        o.dispatch_candidates.push(candidate);
                    }
                }
                o.library_call(file.clone(), c.callee_span, c.line, target.clone());
            }
        }
    }
    covered
}

/// Language of a partition file (syntax facts, else the path's extension).
pub(super) fn file_language(decls: &DeclTable<'_>, path: &str) -> Option<Language> {
    decls
        .facts(path)
        .and_then(|f| f.language)
        .or_else(|| trace_core::languages::from_path(Path::new(path)))
}

/// Overload rule (language rule, `trace_core::facts::arity_accepts`): keep the targets whose
/// parameter list accepts the call's argument count; nothing is dropped without positive
/// evidence, and when no target would be left the list stays unchanged.
pub(super) fn narrow_by_arity<'a>(
    targets: Vec<DeclRef<'a>>,
    c: &CallSite,
    language: Option<Language>,
    decls: &DeclTable<'a>,
    path: &str,
) -> Vec<DeclRef<'a>> {
    let Some(language) = language else { return targets };
    if targets.len() < 2 {
        return targets;
    }
    let member = ByteSpan::new(member_point(c), c.callee_span.end);
    let receiver_call = decls.facts(path).is_some_and(|f| f.member_access(member).is_some());
    let kept: Vec<DeclRef<'a>> = targets
        .iter()
        .copied()
        .filter(|t| {
            let d = decls.decl(*t);
            !d.kind.is_callable()
                || trace_core::facts::arity_accepts(language, &d.parameters, c.arg_count, receiver_call)
                    != Some(false)
        })
        .collect();
    if kept.is_empty() {
        targets
    } else {
        kept
    }
}

impl<'a> Shard<'a, '_> {
    /// Batch B (step 4 of the module docs): outgoing calls of every uniquely prepared
    /// callable with the first subtype round, then the deeper subtype rounds.
    pub(super) fn batch_b(&mut self, session: &mut dyn Session) -> Result<(), SemanticError> {
        // 4. Batch B: outgoing calls of every uniquely prepared callable, plus the first
        //    `typeHierarchy/subtypes` round of every uniquely prepared type.
        let Followups { owners, items, types } = std::mem::take(&mut self.follow);
        // A bodiless declaration that owns no syntax call (abstract / interface / native method,
        // prototype) calls nothing: its outgoing calls are never asked (servers may still do
        // whole-file work per request, e.g. an implementor search per abstract method).
        let (owners, items): (Vec<DeclRef<'a>>, Vec<Value>) = owners
            .into_iter()
            .zip(items)
            .filter(|(r, _)| !calls_nothing(self.decls, *r))
            .unzip();
        let mut walks = TypeWalks::new(types, self.decls);
        let mut round = walks.next_round(&mut self.out);
        if !items.is_empty() || !round.is_empty() {
            let mut calls: Vec<(String, Value)> = items
                .into_iter()
                .map(|item| item_request("callHierarchy/outgoingCalls", item))
                .collect();
            let n_outgoing = calls.len();
            calls.extend(
                round
                    .iter()
                    .map(|f| item_request("typeHierarchy/subtypes", f.item.clone())),
            );
            let mut outgoing = session.request_many(calls)?;
            let subtypes = outgoing.split_off(n_outgoing.min(outgoing.len()));
            walks.absorb(std::mem::take(&mut round), subtypes, self.decls, self.uris, &mut self.out);
            for (r, result) in owners.iter().zip(outgoing) {
                let o = self.out.get_mut(r.path).expect("queried file");
                match result {
                    Ok(value) => {
                        self.prepared_ok.insert(*r);
                        let index = &self.call_index[r.path];
                        for ci in outgoing_calls(
                            *r,
                            &value,
                            self.decls,
                            self.uris,
                            index,
                            &self.cx,
                            o,
                            &mut self.incomplete,
                        ) {
                            self.covered.insert((r.path, ci));
                        }
                    }
                    Err(e) => {
                        if !o.outside(self.opts.hooks, r.path, &e) {
                            o.fail(&e, CountAt::At(self.decls.decl(*r).name_span.start));
                        }
                    }
                }
            }
        }
        // Deeper subtype rounds (subtypes that do not declare a member themselves).
        loop {
            let round = walks.next_round(&mut self.out);
            if round.is_empty() {
                break;
            }
            let calls = round
                .iter()
                .map(|f| item_request("typeHierarchy/subtypes", f.item.clone()))
                .collect();
            let results = session.request_many(calls)?;
            walks.absorb(round, results, self.decls, self.uris, &mut self.out);
        }
        Ok(())
    }
}
