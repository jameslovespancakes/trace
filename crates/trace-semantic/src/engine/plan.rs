//! Request planning: which syntax calls are answered from syntax (SPEC section 8.8 rules 1-3,
//! `LanguageRules::bare_call_binding`), which are asked, the request tags that route each answer back, the
//! request diet predicates and duplicate-free request batches ([`request_unique`]).

use crate::backend::SemanticFile;
use crate::mapping::{DeclRef, DeclTable};
use crate::SemanticError;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use trace_core::facts::{CallSite, CallbackArg, FileFacts, Reference};
use trace_core::model::{ByteSpan, Resolution, SymbolKind, UnresolvedKind};
use trace_core::semantics::SemUnresolved;
use trace_core::Language;
use trace_syntax::language_rules::{rules, BareCallBinding};

use super::*;

/// What a batch-A / batch-C request was for.
pub(super) enum Tag<'a> {
    Symbols(&'a str),
    Prepare(DeclRef<'a>),
    /// `definition` at a call's member identifier: `(path, call index, executing owner)`.
    Fallback(&'a str, usize, u32),
    Callback(&'a str, &'a CallbackArg),
    ValueRef(&'a str, &'a Reference),
    /// `definition` at a header base's name (`bases`): `(path, reference, declared)`;
    /// `declared`: an indexed declaration carries its name (also a value reference).
    Base(&'a str, &'a Reference, bool),
    /// `textDocument/implementation` at an implementable member's name.
    Implementation(DeclRef<'a>),
    /// `textDocument/prepareTypeHierarchy` at a type's name.
    TypePrepare(DeclRef<'a>),
}

impl<'a> Tag<'a> {
    pub(super) fn path(&self) -> &'a str {
        match self {
            Tag::Symbols(path)
            | Tag::Fallback(path, _, _)
            | Tag::Callback(path, _)
            | Tag::ValueRef(path, _)
            | Tag::Base(path, _, _) => path,
            Tag::Prepare(r) | Tag::Implementation(r) | Tag::TypePrepare(r) => r.path,
        }
    }
}

/// Whether `r` is a stub (no body) that owns no syntax call.
pub(super) fn calls_nothing(decls: &DeclTable<'_>, r: DeclRef<'_>) -> bool {
    decls.decl(r).is_stub
        && decls
            .facts(r.path)
            .is_some_and(|f| !f.calls.iter().any(|c| c.owner == Some(r.decl)))
}

/// Answers collected from a batch for the next one.
#[derive(Default)]
pub(super) struct Followups<'a> {
    /// Uniquely prepared call-hierarchy items and their declarations (batch B).
    pub(super) owners: Vec<DeclRef<'a>>,
    pub(super) items: Vec<Value>,
    /// Uniquely prepared type-hierarchy items and their types (subtype rounds).
    pub(super) types: Vec<(DeclRef<'a>, Value)>,
}

/// Whether a callable can be implemented or overridden by another declaration: a member
/// (its parent is a class / interface / trait / protocol) that is declared in an interface,
/// is a stub (bodiless / `abstract`), or carries an `abstract` / `virtual` / `open` modifier
/// in its decorators. Constructors and synthetic scopes never are.
pub(crate) fn implementable(facts: &FileFacts, decl: u32) -> bool {
    let Some(d) = facts.declarations.get(decl as usize) else {
        return false;
    };
    if !d.kind.is_callable() || d.kind == SymbolKind::Constructor || facts.is_synthetic(decl) {
        return false;
    }
    let Some(parent) = d.parent.and_then(|p| facts.declarations.get(p as usize)) else {
        return false;
    };
    if !parent.kind.is_type() {
        return false;
    }
    parent.kind == SymbolKind::Interface || d.is_stub || d.decorators.iter().any(|t| overridable_modifier(t))
}

/// `abstract` (`@abstractmethod`, `abstract`), `virtual` or `open` in a decorator /
/// modifier text.
pub(super) fn overridable_modifier(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("abstract")
        || lower
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|w| w == "virtual" || w == "open")
}

/// Callable members of a type declaration named `name`: its children, or out-of-line
/// members of the same file whose container is the type's name (Rust `impl`, Go receivers).
pub(super) fn members_named<'a>(decls: &DeclTable<'a>, owner: DeclRef<'a>, name: &str) -> Vec<DeclRef<'a>> {
    let type_name = decls.decl(owner).name.as_str();
    decls
        .named(name)
        .iter()
        .copied()
        .filter(|m| {
            let d = decls.decl(*m);
            m.path == owner.path
                && *m != owner
                && d.kind.is_callable()
                && (d.parent == Some(owner.decl) || d.container.as_deref() == Some(type_name))
        })
        .collect()
}

/// Callable, overridable members of a type (children or same-file out-of-line members).
pub(super) fn type_members<'a>(decls: &DeclTable<'a>, owner: DeclRef<'a>) -> Vec<DeclRef<'a>> {
    let type_name = decls.decl(owner).name.as_str();
    decls
        .decls_of(owner.path)
        .into_iter()
        .filter(|m| {
            let d = decls.decl(*m);
            *m != owner
                && d.kind.is_callable()
                && d.kind != SymbolKind::Constructor
                && !decls.is_synthetic(*m)
                && (d.parent == Some(owner.decl)
                    || (d.parent.is_none() && d.container.as_deref() == Some(type_name)))
        })
        .collect()
}

/// Rule 2 (module docs): the declaration a bare call names when syntax proves it: exactly
/// one named declaration of the member in the partition (for shell scripts: in the
/// script's sourced `scope`), in the call's file, callable, visible under
/// `LanguageRules::bare_call_binding`; never for member accesses or local bindings.
pub(crate) fn syntax_definition<'a>(
    file: &SemanticFile<'a>,
    c: &CallSite,
    decls: &DeclTable<'a>,
    scope: Option<&BTreeSet<&str>>,
) -> Option<DeclRef<'a>> {
    let rule = rules(file.language).bare_call_binding?;
    let member = c.member.as_deref()?;
    if c.callee.trim() != member || c.receiver.is_some() {
        return None;
    }
    let facts = file.facts;
    let span = ByteSpan::new(member_point(c), c.callee_span.end);
    if facts.is_local(span) || facts.member_access(span).is_some() {
        return None;
    }
    let candidates: Vec<DeclRef<'a>> = decls
        .named(member)
        .iter()
        .copied()
        .filter(|d| scope.is_none_or(|s| s.contains(d.path)))
        .collect();
    let [only] = candidates.as_slice() else {
        return None;
    };
    let path = decls.path_key(file.path)?;
    if only.path != path {
        return None;
    }
    let d = decls.decl(*only);
    if !d.kind.is_callable() {
        return None;
    }
    if let BareCallBinding::FileLevel { declared_before } = rule {
        let top_level = d.parent.is_none_or(|p| facts.module_decl == Some(p));
        if !top_level || d.container.is_some() || d.qualified_name != d.name {
            return None;
        }
        if declared_before && d.name_span.start > c.callee_span.start {
            return None;
        }
    }
    Some(*only)
}

/// What to do instead of a `definition` request at a call's member (module docs).
#[derive(Clone, Copy)]
pub(super) enum CallAnswer<'a> {
    /// Ask the server.
    Ask,
    /// A local binding (rule 1).
    Local,
    /// No partition declaration carries the name (rule 3).
    NotDeclared,
    /// The only same-file declaration (rule 2).
    Syntax(DeclRef<'a>),
    /// A bare command no declaration carries that the hooks find as an external program
    /// (`Server::external_program`): resolved elsewhere.
    ExternalProgram,
}

/// Whether no declaration of the partition (for shell scripts: of the sourced `scope`, bare
/// calls only) carries the call's member name, or every one is a nested function out of
/// scope at the bare call `c` of `path` ([`crate::engine::rules::scoping`]).
pub(super) fn member_undeclared(
    path: &str,
    c: &CallSite,
    decls: &DeclTable<'_>,
    scope: Option<&BTreeSet<&str>>,
) -> bool {
    let Some(m) = c.member.as_deref() else { return false };
    match scope {
        // Shell scripts: no declaration in the sourced scope (bare calls only).
        Some(s) if c.receiver.is_none() && c.callee.trim() == m => {
            !decls.named(m).iter().any(|d| s.contains(d.path))
        }
        _ => {
            !decls.is_declared_name(m)
                || crate::engine::rules::scoping::only_out_of_scope_declarations(decls, path, c)
        }
    }
}

/// A bare command (no receiver, the callee is the member name) that no declaration carries
/// and that the hooks find as an external program of this machine (memoised per run).
pub(super) fn external_program(
    path: &str,
    c: &CallSite,
    decls: &DeclTable<'_>,
    opts: &Options<'_>,
    scope: Option<&BTreeSet<&str>>,
    programs: &mut HashMap<String, bool>,
) -> bool {
    let Some(m) = c.member.as_deref() else { return false };
    if c.receiver.is_some() || c.callee.trim() != m || !member_undeclared(path, c, decls, scope) {
        return false;
    }
    if let Some(&found) = programs.get(m) {
        return found;
    }
    let found = opts.hooks.external_program(m, opts.prepared);
    programs.insert(m.to_string(), found);
    found
}

/// The answer of a call without a request (module docs, SPEC section 8.8), or `Ask`.
/// `has_callback`: the call carries a callback argument, so rule 3 never applies (library
/// behaviour needs the library target the server gives).
pub(super) fn call_answer<'a>(
    file: &SemanticFile<'a>,
    c: &CallSite,
    decls: &DeclTable<'a>,
    opts: &Options<'_>,
    scope: Option<&BTreeSet<&str>>,
    has_callback: bool,
    programs: &mut HashMap<String, bool>,
) -> CallAnswer<'a> {
    if !opts.syntax_answers {
        return CallAnswer::Ask;
    }
    let span = ByteSpan::new(member_point(c), c.callee_span.end);
    if file.facts.is_local(span) {
        return CallAnswer::Local;
    }
    if member_undeclared(file.path, c, decls, scope) {
        if has_callback {
            return CallAnswer::Ask;
        }
        if external_program(file.path, c, decls, opts, scope, programs) {
            return CallAnswer::ExternalProgram;
        }
        return CallAnswer::NotDeclared;
    }
    match syntax_definition(file, c, decls, scope) {
        Some(target) => CallAnswer::Syntax(target),
        None => CallAnswer::Ask,
    }
}

/// Apply a syntax answer to call `ci` of `path` (never [`CallAnswer::Ask`]).
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_call_answer<'a>(
    answer: CallAnswer<'a>,
    path: &'a str,
    ci: usize,
    c: &CallSite,
    owner: u32,
    decls: &DeclTable<'a>,
    out: &mut BTreeMap<&'a str, FileOut>,
    incomplete: &mut HashSet<(String, u32)>,
    covered: &mut HashSet<(&'a str, usize)>,
) {
    let Some(o) = out.get_mut(path) else { return };
    match answer {
        CallAnswer::Ask => return,
        CallAnswer::Syntax(target) => {
            // Rule 2 targets are callables (never a type a conversion could name).
            o.edges
                .push(call_edge(c, owner, target, decls, Resolution::SyntaxDefinition, false));
            o.count_at("syntax_definition", c.callee_span.start);
        }
        CallAnswer::ExternalProgram => {
            // Keyed like syntax references: the command's member identifier.
            o.resolved_elsewhere
                .push(ByteSpan::new(member_point(c), c.callee_span.end));
            o.count_at("external_program", c.callee_span.start);
        }
        CallAnswer::Local | CallAnswer::NotDeclared => {
            // Exactly what a definition answer outside the index gives (`definition_call`).
            incomplete.insert((path.to_string(), c.callee_span.start));
            o.unresolved.push(SemUnresolved {
                owner: Some(owner),
                kind: UnresolvedKind::ExternalOrAmbiguous,
                at: c.callee_span,
                line: c.line,
                callee: c.callee.clone(),
                candidates: Vec::new(),
            });
            o.count_at(
                if matches!(answer, CallAnswer::Local) {
                    "local_call"
                } else {
                    "external_by_name"
                },
                c.callee_span.start,
            );
        }
    }
    covered.insert((path, ci));
}

/// Syntax calls of one file keyed by the end of their callee (the member identifier end)
/// and by the end of the whole call expression: outgoing-call ranges are matched to every
/// call whose callee ends where the range ends; ranges covering a whole invocation (jdtls,
/// Roslyn) to the call whose expression ends there.
pub(super) struct CallIndex {
    pub(super) by_end: HashMap<u32, Vec<usize>>,
    pub(super) by_call_end: HashMap<u32, Vec<usize>>,
}

impl CallIndex {
    pub(super) fn new(facts: &FileFacts) -> Self {
        let mut by_end: HashMap<u32, Vec<usize>> = HashMap::with_capacity(facts.calls.len());
        let mut by_call_end: HashMap<u32, Vec<usize>> = HashMap::with_capacity(facts.calls.len());
        for (i, c) in facts.calls.iter().enumerate() {
            by_end.entry(c.callee_span.end).or_default().push(i);
            by_call_end.entry(c.span.end).or_default().push(i);
        }
        CallIndex { by_end, by_call_end }
    }

    /// The syntax call an analyzer range `[start, end)` designates: the smallest call whose
    /// callee ends at `end` and contains `start` (the member identifier of `Type::f(..)`, of
    /// any step of a builder chain, of `obj.m()`); else the smallest call whose whole
    /// expression ends at `end`, starts at or before `start` and whose callee ends after
    /// `start` (a range over the invocation `m(args)` / `o.m(args)`, never an inner call of the
    /// receiver or the arguments); else the smallest call whose callee contains `start`.
    pub(super) fn matching(&self, facts: &FileFacts, start: u32, end: u32) -> Option<usize> {
        let by_end = self.by_end.get(&end).and_then(|calls| {
            calls
                .iter()
                .copied()
                .filter(|&i| facts.calls[i].callee_span.start <= start)
                .min_by_key(|&i| facts.calls[i].callee_span.len())
        });
        let whole = || {
            self.by_call_end.get(&end).and_then(|calls| {
                calls
                    .iter()
                    .copied()
                    .filter(|&i| {
                        let c = &facts.calls[i];
                        c.span.start <= start && c.callee_span.end > start
                    })
                    .min_by_key(|&i| facts.calls[i].span.len())
            })
        };
        by_end.or_else(whole).or_else(|| innermost_call_index(facts, start))
    }
}

/// Nearest named (preparable) ancestor of an executing owner: the owner itself when it is
/// a named callable, else the enclosing named callable of a synthetic scope. `None` for code
/// no prepared item covers (`<module>`, class bodies, lambdas at module/class level).
pub(super) fn prepared_root(facts: &FileFacts, owner: u32) -> Option<u32> {
    let mut current = Some(owner);
    let mut steps = 0usize;
    while let Some(d) = current {
        let decl = facts.declarations.get(d as usize)?;
        if !facts.is_synthetic(d) {
            return decl.kind.is_callable().then_some(d);
        }
        steps += 1;
        if steps > facts.declarations.len() {
            return None;
        }
        current = decl.parent;
    }
    None
}

/// Request diet: whether another callable of the partition carries `r`'s name (outside `r`'s
/// own type): only such a declaration can be an implementation / override of `r`.
pub(super) fn named_elsewhere(decls: &DeclTable<'_>, r: DeclRef<'_>) -> bool {
    let d = decls.decl(r);
    decls.named(&d.name).iter().any(|m| {
        if m.path == r.path && m.decl == r.decl {
            return false;
        }
        let md = decls.decl(*m);
        let same_owner = m.path == r.path && md.parent == d.parent && md.container == d.container;
        md.kind.is_callable() && !same_owner
    })
}

/// A request whose only parameter is a hierarchy item.
pub(super) fn item_request(method: &str, item: Value) -> (String, Value) {
    let mut params = Map::new();
    params.insert("item".into(), item);
    (method.to_string(), Value::Object(params))
}

/// `request_many` with identical requests sent once (a decorator call's member identifier
/// is asked both as a call and as a decorator reference): every duplicate receives the
/// same answer (a failure is reported to each as a worker error naming the original).
pub(super) fn request_unique(
    session: &mut dyn Session,
    calls: Vec<(String, Value)>,
) -> Result<Vec<Result<Value, SemanticError>>, SemanticError> {
    let mut first: HashMap<(String, String), usize> = HashMap::with_capacity(calls.len());
    let mut slot: Vec<usize> = Vec::with_capacity(calls.len());
    let mut unique: Vec<(String, Value)> = Vec::with_capacity(calls.len());
    for (method, params) in calls {
        let key = (method.clone(), params.to_string());
        let i = *first.entry(key).or_insert_with(|| {
            unique.push((method, params));
            unique.len() - 1
        });
        slot.push(i);
    }
    if unique.len() == slot.len() {
        return session.request_many(unique);
    }
    let answers = session.request_many(unique)?;
    let mut answers: Vec<Option<Result<Value, SemanticError>>> = answers.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(slot.len());
    // Clone for all but the last use of each answer.
    let mut remaining = vec![0usize; answers.len()];
    for &i in &slot {
        remaining[i] += 1;
    }
    for &i in &slot {
        remaining[i] -= 1;
        let answer = if remaining[i] == 0 {
            answers[i].take()
        } else {
            answers[i].as_ref().map(|a| match a {
                Ok(v) => Ok(v.clone()),
                Err(e) => Err(SemanticError::Worker(format!("duplicate of a failed request: {e}"))),
            })
        };
        out.push(answer.unwrap_or_else(|| Err(SemanticError::Worker("missing answer".into()))));
    }
    Ok(out)
}

/// Whether the identifier starting at `start` is an attribute (`x.name`): the previous
/// non-whitespace byte is a `.`.
pub(super) fn is_attribute_name(source: &[u8], start: u32) -> bool {
    source[..(start as usize).min(source.len())]
        .iter()
        .rev()
        .find(|b| !b.is_ascii_whitespace() && **b != b'\\')
        .is_some_and(|b| *b == b'.')
}

/// Sharing key of a Python import reference: `(top-level directory, target)` for an
/// absolute `from pkg.mod import name` binding whose target ends with the referenced name.
pub(super) fn absolute_import_key<'a>(
    facts: &'a FileFacts,
    reference: &Reference,
    path: &'a str,
) -> Option<(&'a str, &'a str)> {
    let import = facts.imports.iter().find(|i| {
        i.span.encloses(reference.span)
            && i.kind == trace_core::facts::ImportKind::Member
            && !i.target.starts_with('.')
            && i.target.rsplit('.').next() == Some(reference.name.as_str())
    })?;
    let top = path.split('/').next().filter(|t| *t != path).unwrap_or("");
    Some((top, import.target.as_str()))
}

/// `inner` is `outer` or lexically nested in it (parent chain).
pub(super) fn nested_in(facts: &FileFacts, inner: u32, outer: u32) -> bool {
    let mut current = Some(inner);
    let mut steps = 0usize;
    while let Some(d) = current {
        if d == outer {
            return true;
        }
        steps += 1;
        if steps > facts.declarations.len() {
            return false;
        }
        current = facts.declarations.get(d as usize).and_then(|d| d.parent);
    }
    false
}

pub(super) fn definition_request(uri: &str, line: u32, character: u32) -> (String, Value) {
    (
        "textDocument/definition".to_string(),
        json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": character}}),
    )
}

/// One analysis over a shard (module docs): the queried files, what the server can answer,
/// the syntax-side plan and what the batches found so far. Built by [`Shard::plan`]; the
/// batches ([`Shard::batch_a`], [`Shard::batch_b`], [`Shard::batch_c`]) and the answer phases
/// run in order over it.
pub(super) struct Shard<'a, 's> {
    pub(super) decls: &'s DeclTable<'a>,
    pub(super) uris: &'s dyn UriResolver,
    pub(super) opts: &'s Options<'s>,
    /// Library locations of answers are classified through the preflight's library roots.
    pub(super) cx: crate::external::ExternalContext<'s>,
    /// Every file of the shard, sorted by path, once.
    pub(super) queried: Vec<&'s SemanticFile<'a>>,
    /// Queried files that are asked (inside the build, valid UTF-8).
    pub(super) active: Vec<&'s SemanticFile<'a>>,
    pub(super) has_hierarchy: bool,
    pub(super) has_symbols: bool,
    pub(super) has_definition: bool,
    pub(super) has_implementation: bool,
    pub(super) has_type_hierarchy: bool,
    /// Calls in the server's inactive preprocessor regions are `inactive_code` (clangd).
    pub(super) inactive_policy: bool,
    pub(super) uri_of: HashMap<&'a str, String>,
    /// Calls carrying a callback or a string argument: (path, callee start), always asked.
    pub(super) with_callback: HashSet<(&'a str, u32)>,
    /// Inactive preprocessor regions reported before the batches, per file.
    pub(super) inactive: HashMap<&'a str, Vec<ByteSpan>>,
    pub(super) call_index: HashMap<&'a str, CallIndex>,
    /// Shell scripts: the sourced scope of each queried script (rules 2 and 3).
    pub(super) shell_scopes: HashMap<&'a str, BTreeSet<&'a str>>,
    /// Reused and analysed units per file.
    pub(super) units: HashMap<String, (u32, u32)>,
    /// Declaration name positions from syntax (validated by `prepareCallHierarchy`).
    pub(super) located: Vec<(DeclRef<'a>, (u32, u32))>,
    pub(super) out: BTreeMap<&'a str, FileOut>,
    pub(super) incomplete: HashSet<(String, u32)>,
    /// Syntax calls covered by an analyzer answer (or a syntax answer): (path, call index).
    pub(super) covered: HashSet<(&'a str, usize)>,
    pub(super) fallback_asked: HashSet<(&'a str, usize)>,
    /// External-program answers of the hooks, per command name (one lookup per run).
    pub(super) programs: HashMap<String, bool>,
    /// Declarations sent to `prepareCallHierarchy` (batch A) and those whose outgoing calls
    /// were answered (batch B).
    pub(super) attempted: HashSet<DeclRef<'a>>,
    pub(super) prepared_ok: HashSet<DeclRef<'a>>,
    /// Batch A results that batch B follows up.
    pub(super) follow: Followups<'a>,
}

impl<'a, 's> Shard<'a, 's> {
    /// Plan the analysis of `shard`: capabilities, the files asked, the syntax-side indexes and
    /// declaration positions (steps 1 and 2 of the module docs).
    pub(super) fn plan(
        session: &dyn Session,
        shard: &[&'s SemanticFile<'a>],
        decls: &'s DeclTable<'a>,
        uris: &'s dyn UriResolver,
        opts: &'s Options<'s>,
    ) -> Result<Self, SemanticError> {
        let mut queried: Vec<&'s SemanticFile<'a>> = shard.to_vec();
        queried.sort_by(|a, b| a.path.cmp(b.path));
        queried.dedup_by(|a, b| a.path == b.path);
        let mut out: BTreeMap<&'a str, FileOut> =
            queried.iter().map(|f| (f.path, FileOut::default())).collect();
        let caps = session.capabilities().clone();
        // Backends measured more accurate with `definition` at call sites skip call hierarchy
        // (request diet A/B).
        let has_hierarchy =
            capability_enabled(&caps, "callHierarchyProvider") && (opts.python || !opts.calls_by_definition);
        let has_symbols = capability_enabled(&caps, "documentSymbolProvider");
        let has_definition = capability_enabled(&caps, "definitionProvider") || opts.python;
        let has_implementation = capability_enabled(&caps, "implementationProvider");
        let has_type_hierarchy = capability_enabled(&caps, "typeHierarchyProvider");
        if opts.python && !has_hierarchy {
            return Err(SemanticError::Capability("callHierarchyProvider".into()));
        }
        let mut uri_of: HashMap<&'a str, String> = HashMap::with_capacity(queried.len());
        let mut active: Vec<&'s SemanticFile<'a>> = Vec::with_capacity(queried.len());
        for f in &queried {
            uri_of.insert(f.path, uris.uri_of(f.path)?);
            // Not part of the build on this machine (another platform / language version /
            // build variant): nothing is asked; its calls are unknown (blind sites below).
            if let Some(reason) = opts.hooks.outside_build_file(f.path, opts.prepared) {
                if let Some(o) = out.get_mut(f.path) {
                    o.outside_build = Some(reason);
                    o.count("outside_build_file");
                }
                continue;
            }
            if lsp_text(f.source).is_some() {
                active.push(*f);
            } else if let Some(o) = out.get_mut(f.path) {
                o.count("invalid_utf8");
            }
        }
        // Library locations of answers are classified through the preflight's library roots.
        let cx = crate::external::ExternalContext {
            prepared: opts.prepared,
            hooks: opts.hooks,
        };
        // Syntax calls carrying a callback argument, or a string (a possible channel key: a route,
        // a URL, a topic): (path, callee start) -> always asked (library behaviour needs the
        // library target).
        let mut with_callback: HashSet<(&'a str, u32)> = HashSet::new();
        for f in &active {
            for cb in &f.facts.callbacks {
                with_callback.insert((f.path, cb.call_callee_span.start));
            }
            for d in &f.facts.call_details {
                if d.arguments.iter().any(|a| a.has_string) {
                    if let Some(c) = f.facts.calls.get(d.call as usize) {
                        with_callback.insert((f.path, c.callee_span.start));
                    }
                }
            }
        }
        // Inactive preprocessor regions the server reported so far (clangd), per file.
        let inactive_policy = opts.hooks.answer_policy().inactive_regions;
        let inactive: HashMap<&'a str, Vec<ByteSpan>> = if inactive_policy {
            inactive_regions(session, &active, decls, uris)
        } else {
            HashMap::new()
        };
        let in_inactive = |path: &str, byte: u32| {
            inactive
                .get(path)
                .is_some_and(|spans| spans.iter().any(|s| s.contains(byte)))
        };
        let call_index: HashMap<&'a str, CallIndex> =
            active.iter().map(|f| (f.path, CallIndex::new(f.facts))).collect();
        // Shell scripts: the sourced scope of each queried script (rules 2 and 3).
        let is_shell = |l: Language| rules(l).bare_call_binding == Some(BareCallBinding::Shell);
        let shell_scopes: HashMap<&'a str, BTreeSet<&'a str>> = if active.iter().any(|f| is_shell(f.language))
        {
            let scopes = ShellScopes::new(decls.paths().filter_map(|p| Some((p, decls.facts(p)?))));
            active
                .iter()
                .filter(|f| is_shell(f.language))
                .map(|f| (f.path, scopes.closure(f.path)))
                .collect()
        } else {
            HashMap::new()
        };
        // Declaration reuse: positions inside reused units are never asked.
        let reused_at = |path: &str, byte: u32| opts.reuse_of(path).is_some_and(|r| r.covers(byte));
        let mut units: HashMap<String, (u32, u32)> = HashMap::new();
        for f in &active {
            let total = reuse_units(f.facts).len() as u32;
            let reused = opts.reuse_of(f.path).map_or(0, |r| r.spans.len() as u32);
            units.insert(f.path.to_string(), (reused, total.saturating_sub(reused)));
        }

        // 1. Declaration positions from syntax (validated later by prepareCallHierarchy).
        //    Synthetic declarations (`<module>`, `<lambda>`, `<genexpr>`) have no name.
        let mut located: Vec<(DeclRef<'a>, (u32, u32))> = Vec::new();
        for f in &active {
            for r in decls.decls_of(f.path) {
                if f.facts.is_synthetic(r.decl) {
                    continue;
                }
                let name_at = decls.decl(r).name_span.start;
                // Declarations in inactive preprocessor regions are not compiled: nothing to ask.
                if reused_at(f.path, name_at) || in_inactive(f.path, name_at) {
                    continue;
                }
                if let Some(pos) = decls.lsp_of(r.path, name_at) {
                    located.push((r, pos));
                }
            }
        }
        located.sort_by(|a, b| (a.0.path, a.0.decl).cmp(&(b.0.path, b.0.decl)));

        // 2. Constructor bridge: class -> its own `__init__` (never a guessed base).
        if opts.python {
            constructor_bridge(&located, decls, &mut out);
        }

        Ok(Shard {
            decls,
            uris,
            opts,
            cx,
            queried,
            active,
            has_hierarchy,
            has_symbols,
            has_definition,
            has_implementation,
            has_type_hierarchy,
            inactive_policy,
            uri_of,
            with_callback,
            inactive,
            call_index,
            shell_scopes,
            units,
            located,
            out,
            incomplete: HashSet::new(),
            covered: HashSet::new(),
            fallback_asked: HashSet::new(),
            programs: HashMap::new(),
            attempted: HashSet::new(),
            prepared_ok: HashSet::new(),
            follow: Followups::default(),
        })
    }

    /// Whether `byte` of `path` lies in an inactive preprocessor region reported before the
    /// batches.
    pub(super) fn in_inactive(&self, path: &str, byte: u32) -> bool {
        self.inactive
            .get(path)
            .is_some_and(|spans| spans.iter().any(|s| s.contains(byte)))
    }

    /// Declaration reuse: positions inside reused units are never asked.
    pub(super) fn reused_at(&self, path: &str, byte: u32) -> bool {
        self.opts.reuse_of(path).is_some_and(|r| r.covers(byte))
    }

    /// Whether the implementations of `path` are reused whole.
    pub(super) fn implementations_reused(&self, path: &str) -> bool {
        self.opts.reuse_of(path).is_some_and(|r| r.implementations.is_some())
    }
}
