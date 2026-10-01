//! Lexical scopes for every grammar (SPEC §6.3, general fixes rule 2): local bindings
//! (`FileFacts::local_spans`, `Reference::local`) and member accesses
//! (`FileFacts::member_accesses`).
//!
//! **Local bindings.** One pre-order walk builds a scope tree — the file (root), functions
//! (every callable declaration and every lazy scope of `SyntaxSpec::lazy_scopes`: named
//! functions, lambdas, closures, blocks passed as callbacks), class bodies and, where the
//! language has them, block scopes ([`ScopeRules::blocks`]) — and records bindings per scope:
//!
//! * *local* bindings: parameters (the callable's parameter list, patterns included),
//!   `let` / `val` / `var` / `const` / `local` / `:=` declarations, loop, catch, match-arm and
//!   `if let` patterns, closure parameters, and in languages where an assignment declares
//!   (PHP, R) assignment targets
//!   ([`ScopeRules::binders`]);
//! * *other* bindings: names declared by `def` / `fn` / `class` / function declarations and
//!   imports, and every binding made directly in a class body (members) or at file level
//!   (module variables).
//!
//! A bare identifier (never a member name, a static path segment, a label, a type position
//! or an import) resolves to the nearest scope that binds its name, walking outwards; the
//! binding decides: the identifier is local when every visible binding of that scope is a
//! local binding. Order-sensitive languages ([`ScopeRules::ordered`]) see a declaration only after
//! it (`let x = x + 1` reads the outer `x`); JavaScript sees `let` / `const` in the whole
//! block and `var` in the whole function. Named functions that do not capture ([`ScopeRules::closed`]:
//! Rust `fn` items, PHP functions) never see the locals of enclosing functions.
//! Class-body bindings are visible from methods only in languages with an implicit receiver
//! ([`ScopeRules::class_members_visible`]). Local binding identifiers and the uses that resolve to
//! them are the local spans. Python keeps its own rule (`crate::names::Names::binding`,
//! computed by the extractor), this module only adds its member accesses.
//!
//! **Member accesses.** The member identifier of every access on a value — member access
//! nodes of `SyntaxSpec::member_access`, split method calls with a receiver (Java `o.m()`,
//! PHP `$o->m()`), C# conditional access `o?.M` —
//! with the receiver's root identifier and whether the receiver is the self reference. Static
//! paths ([`ScopeRules::statics`]: Rust `a::b`, C++ `ns::f`, PHP `A::f`, R `pkg::f`) and import
//! statements are not member accesses.
//!
//! Syntax only: node kinds, fields and positions; identifier text is compared only with other
//! identifiers and with the language's self names. No regular expressions.

use std::collections::{HashMap, HashSet};

use trace_core::facts::{FileFacts, MemberAccess};
use trace_core::model::ByteSpan;
use trace_core::Language;
use tree_sitter::Node;

use crate::extract::{unwrap_node, DeclSyntax};
use crate::node::{children_with_fields, find_descendant, has_direct_token, pick, span, text};
use crate::spec::SyntaxSpec;

/// A local-binding form.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Binder {
    /// Node kind (`""`: any node that has `pick`).
    pub kind: &'static str,
    /// Pick of the binding pattern (`""`: the node itself).
    pub pick: &'static str,
    /// Binds in the innermost *function* scope (JS `var`, PHP / R assignments) instead of the
    /// innermost scope.
    pub hoist: bool,
    /// Required direct anonymous tokens (any of them; empty: none).
    pub tokens: &'static [&'static str],
}

pub(crate) const fn bind(kind: &'static str, pick: &'static str) -> Binder {
    Binder {
        kind,
        pick,
        hoist: false,
        tokens: &[],
    }
}

pub(crate) const fn hoisted(kind: &'static str, pick: &'static str) -> Binder {
    Binder {
        kind,
        pick,
        hoist: true,
        tokens: &[],
    }
}

pub(crate) const fn with_tokens(
    kind: &'static str,
    pick: &'static str,
    hoist: bool,
    tokens: &'static [&'static str],
) -> Binder {
    Binder {
        kind,
        pick,
        hoist,
        tokens,
    }
}

/// A node kind with two picks (static path segments, member object / property).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pair {
    pub kind: &'static str,
    pub first: &'static str,
    pub second: &'static str,
}

pub(crate) const fn pair(kind: &'static str, first: &'static str, second: &'static str) -> Pair {
    Pair { kind, first, second }
}

/// Per-language scoping rules (node kinds of the pinned grammars).
#[derive(Debug)]
pub(crate) struct ScopeRules {
    /// Block scopes (languages with block scoping; the node's subtree is one scope).
    pub blocks: &'static [&'static str],
    /// Local binding forms besides parameter lists.
    pub binders: &'static [Binder],
    /// Callables that do not see the locals of enclosing functions.
    pub closed: &'static [&'static str],
    /// A binding is visible only after its declaration (else in its whole scope).
    pub ordered: bool,
    /// Leaf kinds that bind a name inside patterns besides `SyntaxSpec::identifiers`.
    pub pattern_leaves: &'static [&'static str],
    /// Patterns are built from application nodes (Haskell `Just x`): call kinds are
    /// descended.
    pub pattern_calls: bool,
    /// Pattern identifiers starting with an uppercase letter are constants, not bindings
    /// (Scala stable identifier patterns).
    pub lowercase_patterns: bool,
    /// Extra parameter-list fields of callables (Go receivers and named results).
    pub param_fields: &'static [&'static str],
    /// Static paths: both picks are type / module / namespace segments, never values.
    pub statics: &'static [Pair],
    /// Class-body bindings are visible to bare names in methods (implicit receiver).
    pub class_members_visible: bool,
}

pub(crate) const NONE: ScopeRules = ScopeRules {
    blocks: &[],
    binders: &[],
    closed: &[],
    ordered: true,
    pattern_leaves: &[],
    pattern_calls: false,
    lowercase_patterns: false,
    param_fields: &[],
    statics: &[],
    class_members_visible: false,
};

/// Fields of a binding pattern that hold values, types or bodies (never bindings).
const SKIP_FIELDS: &[&str] = &[
    "value",
    "default_value",
    "default",
    "right",
    "type",
    "return_type",
    "alternative",
    "body",
    "arguments",
    "parameters",
    "condition",
    "size",
    "operator",
    "external_name",
    "dimensions",
    "init",
    "initializer",
    "consequence",
    "update",
];

/// The member part of a member access node: `(object, property)` (property may be a leaf of
/// any name kind). Static paths are excluded.
pub(crate) fn member_parts<'t>(
    spec: &SyntaxSpec,
    language: Language,
    node: Node<'t>,
) -> Option<(Option<Node<'t>>, Node<'t>)> {
    let rules = spec.scopes;
    let kind = node.kind();
    if rules.statics.iter().any(|s| s.kind == kind) {
        return None;
    }
    if let Some(m) = spec.member(kind) {
        let property = pick(node, m.property_field)?;
        return Some((pick(node, m.object_field), property));
    }
    if language == Language::CSharp && kind == "member_binding_expression" {
        // `o?.M`: `conditional_access_expression(condition: o, member_binding_expression(.M))`.
        let property = node.child_by_field_name("name")?;
        let object = node
            .parent()
            .filter(|p| p.kind() == "conditional_access_expression")
            .and_then(|p| p.child_by_field_name("condition"));
        return Some((object, property));
    }
    if let Some(shape) = spec.call_shape(kind) {
        if !shape.receiver_field.is_empty() {
            let receiver = pick(node, shape.receiver_field)?;
            let name = pick(node, shape.function_field)?;
            return Some((Some(receiver), name));
        }
    }
    None
}

/// Whether `node` is the language's self reference (`this`, `self`, `cls`, `$this`).
pub(crate) fn is_self(spec: &SyntaxSpec, node: Node<'_>, source: &[u8]) -> bool {
    if spec.self_kinds.contains(&node.kind()) {
        return true;
    }
    if node.end_byte() - node.start_byte() > 16 {
        return false;
    }
    let t = text(node, source);
    spec.self_names.contains(&t.trim())
}

/// Identifier nodes bound by a pattern (declared names of `let` / `val` / parameters /
/// loop variables / destructuring), skipping values, types, bodies, member targets and
/// nested functions.
pub(crate) fn binding_names<'t>(
    spec: &SyntaxSpec,
    language: Language,
    root: Node<'t>,
    source: &[u8],
) -> Vec<Node<'t>> {
    let rules = spec.scopes;
    let mut out = Vec::new();
    let mut stack = vec![root];
    let mut seen = 0usize;
    while let Some(n) = stack.pop() {
        seen += 1;
        if seen > 1_024 {
            break;
        }
        let kind = n.kind();
        if spec.is_identifier(kind) || rules.pattern_leaves.contains(&kind) {
            if !is_self(spec, n, source) {
                let upper = rules.lowercase_patterns
                    && n.id() != root.id()
                    && text(n, source)
                        .trim()
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_uppercase());
                if !upper {
                    out.push(n);
                }
            }
            continue;
        }
        if n.id() != root.id() && stops_pattern(spec, rules, language, n) {
            continue;
        }
        let mut cursor = n.walk();
        let mut kids: Vec<Node<'t>> = Vec::new();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                let field = cursor.field_name();
                let skipped =
                    field.is_some_and(|f| SKIP_FIELDS.contains(&f) || spec.type_fields.contains(&f));
                if child.is_named() && !child.is_extra() && !skipped {
                    kids.push(child);
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        stack.extend(kids.into_iter().rev());
    }
    out
}

/// Nodes a binding pattern never descends into.
fn stops_pattern(spec: &SyntaxSpec, rules: &ScopeRules, language: Language, n: Node<'_>) -> bool {
    let kind = n.kind();
    spec.subscript(kind).is_some()
        || (!rules.pattern_calls && spec.call_shape(kind).is_some())
        || spec.anonymous_functions.contains(&kind)
        || spec.is_lazy(kind)
        || spec.type_contexts.contains(&kind)
        || spec.class_bodies.contains(&kind)
        || spec.is_comment(kind)
        || kind.contains("attribute")
        || kind.contains("annotation")
        || kind.contains("modifier")
        || kind.contains("decorator")
        || rules.statics.iter().any(|s| s.kind == kind)
        || member_parts(spec, language, n).is_some()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Root,
    Function,
    Class,
    Block,
}

struct ScopeRec {
    parent: Option<usize>,
    kind: Kind,
    closed: bool,
}

/// One binding of a name: `(scope, local, visible from byte)`.
type Bound = (usize, bool, u32);

struct Use {
    span: ByteSpan,
    name: String,
    scope: usize,
    at: u32,
}

#[derive(Clone, Copy, Default)]
struct Ctx {
    scope: usize,
    in_type: bool,
    in_import: bool,
    /// Inside a member name, a static path segment or a label (keyword-argument names): no
    /// value uses, no member accesses.
    label: bool,
}

/// What the walk needs to know about a declaration node.
struct DeclInfo {
    callable: bool,
    is_type: bool,
    synthetic: bool,
    /// Declared name (`None` for out-of-line members with a container).
    binds: Option<String>,
}

struct Walk<'a, 't> {
    spec: &'a SyntaxSpec,
    rules: &'static ScopeRules,
    language: Language,
    source: &'a [u8],
    defs: HashMap<usize, DeclInfo>,
    name_ids: HashSet<usize>,
    scopes: Vec<ScopeRec>,
    bindings: HashMap<String, Vec<Bound>>,
    binding_ids: HashSet<usize>,
    /// Nodes that are never value uses: member names, static path segments, keyword labels,
    /// split-call method names.
    skip_ids: HashSet<usize>,
    /// Local binding identifiers `(span, name, scope)`, checked against the scope's other
    /// bindings of the same name at the end.
    candidates: Vec<(ByteSpan, String, usize)>,
    uses: Vec<Use>,
    /// Import statements / loader calls and the scope they bind in.
    import_nodes: Vec<(ByteSpan, usize)>,
    members: Vec<MemberAccess>,
    locals: bool,
    /// Ties the walker's methods to the tree lifetime of the nodes they visit.
    _tree: std::marker::PhantomData<Node<'t>>,
}

/// Fill `facts.member_accesses` and `facts.local_spans`; set `Reference::local` for every
/// language but Python (`python_locals`: the Python local spans computed by the extractor).
pub(crate) fn run<'t>(
    spec: &SyntaxSpec,
    language: Language,
    root: Node<'t>,
    source: &[u8],
    decls: &[DeclSyntax<'t>],
    facts: &mut FileFacts,
    python_locals: Option<Vec<ByteSpan>>,
) {
    let mut defs = HashMap::with_capacity(decls.len());
    let mut name_ids = HashSet::with_capacity(decls.len());
    for (i, syntax) in decls.iter().enumerate() {
        let Some(d) = facts.declarations.get(i) else {
            continue;
        };
        name_ids.insert(syntax.name_node.id());
        defs.insert(
            syntax.def.id(),
            DeclInfo {
                callable: d.kind.is_callable(),
                is_type: d.kind.is_type(),
                synthetic: syntax.synthetic.is_some(),
                binds: (d.container.is_none() && syntax.synthetic.is_none()).then(|| d.name.clone()),
            },
        );
    }
    let locals = python_locals.is_none();
    let mut walk = Walk {
        spec,
        rules: spec.scopes,
        language,
        source,
        defs,
        name_ids,
        scopes: Vec::new(),
        bindings: HashMap::new(),
        binding_ids: HashSet::new(),
        skip_ids: HashSet::new(),
        candidates: Vec::new(),
        uses: Vec::new(),
        import_nodes: Vec::new(),
        members: Vec::new(),
        locals,
        _tree: std::marker::PhantomData,
    };
    walk.walk(root);

    let mut members = std::mem::take(&mut walk.members);
    members.sort_by_key(|m| (m.span.start, m.span.end));
    members.dedup_by_key(|m| (m.span.start, m.span.end));
    facts.member_accesses = members;

    let mut spans = match python_locals {
        Some(spans) => spans,
        None => {
            for import in &facts.imports {
                if import.local.is_empty() || import.local == "*" {
                    continue;
                }
                let scope = walk
                    .import_nodes
                    .iter()
                    .filter(|(at, _)| at.start <= import.span.start && import.span.end <= at.end)
                    .map(|(_, s)| *s)
                    .next_back()
                    .unwrap_or(0);
                walk.bind_name(import.local.clone(), scope, false, 0);
            }
            walk.resolve()
        }
    };
    spans.sort_by_key(|s| (s.start, s.end));
    spans.dedup_by_key(|s| (s.start, s.end));
    if locals {
        for r in &mut facts.references {
            r.local = spans
                .binary_search_by_key(&(r.span.start, r.span.end), |s| (s.start, s.end))
                .is_ok();
        }
    }
    facts.local_spans = spans;
}

impl<'a, 't> Walk<'a, 't> {
    fn open(&mut self, parent: usize, kind: Kind, closed: bool) -> usize {
        self.scopes.push(ScopeRec {
            parent: Some(parent),
            kind,
            closed,
        });
        self.scopes.len() - 1
    }

    fn function_scope(&self, mut scope: usize) -> usize {
        loop {
            let rec = &self.scopes[scope];
            if rec.kind == Kind::Function {
                return scope;
            }
            match rec.parent {
                Some(p) => scope = p,
                None => return scope,
            }
        }
    }

    fn bind_name(&mut self, name: String, scope: usize, local: bool, from: u32) {
        if name.is_empty() {
            return;
        }
        self.bindings.entry(name).or_default().push((scope, local, from));
    }

    fn bind_node(&mut self, node: Node<'t>, scope: usize, from: u32) {
        let name = text(node, self.source).trim().to_string();
        if name.is_empty() {
            return;
        }
        self.binding_ids.insert(node.id());
        let local = matches!(self.scopes[scope].kind, Kind::Function | Kind::Block);
        if local {
            self.candidates.push((span(node), name.clone(), scope));
        }
        self.bind_name(name, scope, local, from);
    }

    fn walk(&mut self, root: Node<'t>) {
        self.scopes.push(ScopeRec {
            parent: None,
            kind: Kind::Root,
            closed: false,
        });
        let mut stack: Vec<(Node<'t>, Ctx)> = vec![(root, Ctx::default())];
        let mut kids: Vec<(Node<'t>, Option<&'static str>)> = Vec::new();
        while let Some((node, mut ctx)) = stack.pop() {
            if !node.is_named() {
                continue;
            }
            let kind = node.kind();
            if self.spec.is_comment(kind) {
                continue;
            }
            if self.spec.imports.contains(&kind) {
                ctx.in_import = true;
                self.import_nodes.push((span(node), ctx.scope));
            } else if self.spec.import_calls.contains(&kind) {
                self.import_nodes.push((span(node), ctx.scope));
            }
            if self.spec.type_contexts.contains(&kind) {
                ctx.in_type = true;
            }
            // Member names, static path segments and labels: nothing below is a value use.
            if self.skip_ids.contains(&node.id()) {
                ctx.label = true;
            }
            if self.locals {
                ctx.scope = self.enter(node, kind, ctx.scope);
            }

            // Uses: identifiers are terminal.
            if self.spec.is_identifier(kind) {
                if self.locals
                    && !ctx.label
                    && !ctx.in_type
                    && !ctx.in_import
                    && !self.binding_ids.contains(&node.id())
                    && !self.name_ids.contains(&node.id())
                {
                    let name = text(node, self.source).trim().to_string();
                    if !name.is_empty() && !self.spec.self_names.contains(&name.as_str()) {
                        self.uses.push(Use {
                            span: span(node),
                            name,
                            scope: ctx.scope,
                            at: node.start_byte() as u32,
                        });
                    }
                }
                continue;
            }

            // Member accesses and the nodes that are never value uses.
            if let Some((object, property)) = member_parts(self.spec, self.language, node) {
                self.skip_ids.insert(property.id());
                if !ctx.in_import && !ctx.label {
                    self.member(object, property);
                }
            }
            if let Some(shape) = self.spec.call_shape(kind) {
                // Split method calls (`o.m()`, bare `m()` with an implicit receiver): the method
                // name is never a variable.
                if !shape.receiver_field.is_empty() {
                    if let Some(name) = pick(node, shape.function_field) {
                        self.skip_ids.insert(name.id());
                    }
                }
            }
            for s in self.rules.statics.iter().filter(|s| s.kind == kind) {
                for sel in [s.first, s.second] {
                    if let Some(n) = pick(node, sel) {
                        self.skip_ids.insert(n.id());
                    }
                }
            }
            for k in self.spec.keyword_arguments.iter().filter(|k| k.kind == kind) {
                if let Some(n) = pick(node, k.first) {
                    self.skip_ids.insert(n.id());
                }
            }
            if self.spec.argument_wrappers.contains(&kind) {
                if let Some(n) = node.child_by_field_name("name") {
                    self.skip_ids.insert(n.id());
                }
            }

            children_with_fields(node, &mut kids);
            for &(child, field) in kids.iter().rev() {
                let mut c = ctx;
                if field.is_some_and(|f| self.spec.type_fields.contains(&f)) {
                    c.in_type = true;
                }
                stack.push((child, c));
            }
        }
    }

    /// Scope transitions and bindings made by `node`; returns the scope of its subtree.
    fn enter(&mut self, node: Node<'t>, kind: &str, scope: usize) -> usize {
        let id = node.id();
        let mut inner = scope;
        let mut opened = false;
        let mut function = false;
        if let Some(info) = self.defs.get(&id) {
            let binds = info.binds.clone();
            let (callable, is_type, synthetic) = (info.callable, info.is_type, info.synthetic);
            if let Some(name) = binds {
                if !synthetic {
                    self.bind_name(name, scope, false, 0);
                }
            }
            if callable {
                inner = self.open(scope, Kind::Function, self.rules.closed.contains(&kind));
                opened = true;
                function = true;
            } else if is_type {
                inner = self.open(scope, Kind::Class, false);
                opened = true;
            }
        }
        if !opened {
            if self.spec.is_lazy(kind) {
                inner = self.open(scope, Kind::Function, self.rules.closed.contains(&kind));
                opened = true;
                function = true;
            } else if self.spec.class_bodies.contains(&kind) {
                inner = self.open(scope, Kind::Class, false);
                opened = true;
            } else if self.rules.blocks.contains(&kind) {
                inner = self.open(scope, Kind::Block, false);
                opened = true;
            }
        }
        if function {
            let from = node.start_byte() as u32;
            for list in self.parameter_lists(node) {
                for n in binding_names(self.spec, self.language, list, self.source) {
                    self.bind_node(n, inner, from);
                }
            }
        }
        self.binders(node, kind, inner, opened);
        inner
    }

    /// Parameter lists of a callable node (`SyntaxSpec::param_fields`, the rules' extra
    /// fields, else the first `SyntaxSpec::param_lists` node before the body).
    fn parameter_lists(&self, node: Node<'t>) -> Vec<Node<'t>> {
        let mut lists: Vec<Node<'t>> = Vec::new();
        for field in self.spec.param_fields.iter().chain(self.rules.param_fields) {
            if let Some(list) = pick(node, field) {
                if !lists.iter().any(|l| l.id() == list.id()) {
                    lists.push(list);
                }
            }
        }
        if lists.is_empty() && !self.spec.param_lists.is_empty() {
            let limit = node
                .child_by_field_name("body")
                .map_or(usize::MAX, |b| b.start_byte());
            if let Some(list) = find_descendant(node, 256, |n| {
                self.spec.param_lists.contains(&n.kind()) && n.start_byte() < limit
            }) {
                lists.push(list);
            }
        }
        lists
    }

    fn binders(&mut self, node: Node<'t>, kind: &str, scope: usize, opened: bool) {
        let rules = self.rules;
        for b in rules.binders {
            if !b.kind.is_empty() && b.kind != kind {
                continue;
            }
            if !b.tokens.is_empty() && !b.tokens.iter().any(|t| has_direct_token(node, t)) {
                continue;
            }
            let target = if b.pick.is_empty() {
                Some(node)
            } else {
                pick(node, b.pick)
            };
            let Some(target) = target else {
                continue;
            };
            let into = if b.hoist {
                self.function_scope(scope)
            } else {
                scope
            };
            let from = if !rules.ordered {
                0
            } else if opened || b.hoist {
                node.start_byte() as u32
            } else {
                node.end_byte() as u32
            };
            for n in binding_names(self.spec, self.language, target, self.source) {
                if self.binding_ids.contains(&n.id()) {
                    continue;
                }
                self.bind_node(n, into, from);
            }
        }
    }

    fn member(&mut self, object: Option<Node<'t>>, property: Node<'t>) {
        let kind = property.kind();
        let leaf = property.named_child_count() == 0 || self.spec.is_identifier(kind);
        let name_like = self.spec.is_name_like(kind) || kind.ends_with("identifier") || kind == "name";
        if !(leaf && name_like) {
            return;
        }
        let (receiver_root, self_receiver) = self.receiver_root(object);
        self.members.push(MemberAccess {
            span: span(property),
            receiver_root,
            self_receiver,
        });
    }

    /// Root identifier of a receiver that is a name or a dotted name path, and whether the
    /// receiver is the self reference itself.
    fn receiver_root(&self, object: Option<Node<'t>>) -> (Option<String>, bool) {
        let Some(object) = object else {
            return (None, false);
        };
        let mut current = unwrap_node(self.spec, object);
        if is_self(self.spec, current, self.source) {
            return (None, true);
        }
        for _ in 0..16 {
            if is_self(self.spec, current, self.source) {
                return (None, false);
            }
            let kind = current.kind();
            if self.spec.is_identifier(kind) {
                let t = text(current, self.source).trim().to_string();
                return ((!t.is_empty()).then_some(t), false);
            }
            if self.spec.call_shape(kind).is_some() {
                return (None, false);
            }
            match member_parts(self.spec, self.language, current) {
                Some((Some(inner), _)) => current = unwrap_node(self.spec, inner),
                _ => return (None, false),
            }
        }
        (None, false)
    }

    /// Local spans: local binding identifiers not shadowed by another binding of their own
    /// scope, and uses whose nearest visible binding scope binds them only locally.
    fn resolve(&self) -> Vec<ByteSpan> {
        let mut out = Vec::new();
        for (at, name, scope) in &self.candidates {
            let only_local = self
                .bindings
                .get(name)
                .is_some_and(|all| all.iter().filter(|b| b.0 == *scope).all(|b| b.1));
            if only_local {
                out.push(*at);
            }
        }
        for u in &self.uses {
            if self.resolves_local(u) {
                out.push(u.span);
            }
        }
        out
    }

    fn resolves_local(&self, u: &Use) -> bool {
        let Some(all) = self.bindings.get(&u.name) else {
            return false;
        };
        let mut scope = Some(u.scope);
        let mut crossed = false;
        while let Some(i) = scope {
            let rec = &self.scopes[i];
            let visible = rec.kind != Kind::Class || self.rules.class_members_visible;
            if visible {
                let mut any = false;
                let mut only_local = true;
                for &(s, local, from) in all {
                    if s != i || from > u.at || (crossed && local) {
                        continue;
                    }
                    any = true;
                    only_local &= local;
                }
                if any {
                    return only_local;
                }
            }
            if rec.kind == Kind::Function && rec.closed {
                crossed = true;
            }
            scope = rec.parent;
        }
        false
    }
}
