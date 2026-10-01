//! The walker: construction, the tree walk with its scope contexts, imports and bindings.

use std::collections::{HashMap, HashSet};

use trace_core::facts::{AnonymousKind, FileFacts, Import, Reference, Scope};
use trace_core::languages::{in_family, Family};
use trace_core::text::LineIndex;
use trace_core::Language;
use tree_sitter::Node;

use super::{calls::unwrap_node, prepare, query::Captures, Ctx, Eager, Store, Walker};
use crate::names::Names;
use crate::node::{children_with_fields, has_direct_token, is_pick, named_children, pick, span, text};
use crate::spec::SyntaxSpec;

impl<'a, 't> Walker<'a, 't> {
    pub(super) fn new(
        spec: &'static SyntaxSpec,
        language: Language,
        source: &'a [u8],
        lines: &'a LineIndex,
        caps: &'a Captures<'t>,
    ) -> Self {
        let mut prepared = HashMap::with_capacity(caps.defs.len());
        let mut skip_ids = HashSet::new();
        let mut decorator_ids = HashSet::new();
        for (id, pending) in &caps.defs {
            let prep = prepare(spec, pending);
            for d in &prep.decorators {
                decorator_ids.insert(d.id());
            }
            if let Some(name) = pending.name {
                skip_ids.insert(name.id());
            }
            prepared.insert(*id, prep);
        }
        // The anonymous function whose body is a definition's `@body` is that definition's
        // value (`const f = () => {}`), never a separate synthetic scope.
        let mut claimed = HashSet::new();
        for pending in caps.defs.values() {
            if let Some(parent) = pending.body.and_then(|b| b.parent()) {
                if parent.id() != pending.def.id() && spec.anonymous_functions.contains(&parent.kind()) {
                    claimed.insert(parent.id());
                }
            }
        }
        let mut callee_ids = HashSet::with_capacity(caps.calls.len());
        for cap in caps.calls.values() {
            let callee = cap.callee.or_else(|| {
                spec.call_shape(cap.call.kind())
                    .and_then(|s| pick(cap.call, s.function_field))
            });
            if let Some(c) = callee {
                callee_ids.insert(c.id());
            }
        }
        Walker {
            spec,
            language,
            source,
            lines,
            caps,
            prepared,
            decls: Vec::new(),
            body_owner: HashMap::new(),
            skip_ids,
            callee_ids,
            decorator_ids,
            comments: Vec::new(),
            idents: Vec::new(),
            namespaces: Vec::new(),
            calls: Vec::with_capacity(caps.calls.len()),
            call_index: HashMap::with_capacity(caps.calls.len()),
            chains: Vec::new(),
            names: Names::default(),
            bare_refs: Vec::new(),
            claimed,
            argument_ids: HashSet::new(),
            type_ref_ids: HashSet::new(),
            export_nodes: Vec::new(),
            exports: Vec::new(),
            py_bindings: Vec::new(),
        }
    }

    fn anonymous_kind(&self, kind: &str) -> Option<AnonymousKind> {
        if self.spec.anonymous_functions.contains(&kind) {
            Some(AnonymousKind::Lambda)
        } else if self.spec.generator_expressions.contains(&kind) {
            Some(AnonymousKind::GeneratorExpression)
        } else {
            None
        }
    }

    pub(super) fn run(&mut self, root: Node<'t>, facts: &mut FileFacts) {
        let mut stack: Vec<(Node<'t>, Ctx)> = vec![(root, Ctx::default())];
        let mut kids: Vec<(Node<'t>, Option<&'static str>)> = Vec::new();
        while let Some((node, mut ctx)) = stack.pop() {
            if node.is_error() || node.is_missing() {
                facts.error_count += 1;
            }
            if !node.is_named() {
                continue;
            }
            let kind = node.kind();
            if self.spec.is_comment(kind) {
                self.comments.push(span(node));
                continue;
            }
            let id = node.id();

            // Scope transitions. A body node first enters its declaration (a lambda body may
            // itself be a lambda or generator expression).
            let body_of = self.body_owner.get(&id).copied();
            if let Some(d) = body_of {
                let callable = facts.declarations[d as usize].kind.is_callable();
                ctx.owner = if callable { Some(d) } else { None };
                ctx.lexical = Some(d);
                ctx.scope = Some(d);
            }
            let mut decl_here = None;
            let mut generator_outer = None;
            if self.prepared.contains_key(&id) {
                if let Some(d) = self.declare(node, &ctx, facts) {
                    decl_here = Some(d);
                    ctx.lexical = Some(d);
                }
            } else if let Some(anonymous) = self.anonymous_kind(kind).filter(|_| !self.claimed.contains(&id))
            {
                let d = self.declare_anonymous(node, anonymous, &ctx, facts);
                decl_here = Some(d);
                if anonymous == AnonymousKind::GeneratorExpression {
                    generator_outer = Some(Eager {
                        owner: ctx.owner,
                        lexical: ctx.lexical,
                        scope: ctx.scope,
                    });
                    ctx.owner = Some(d);
                    ctx.scope = Some(d);
                }
                ctx.lexical = Some(d);
            }
            if body_of.is_none()
                && decl_here.is_none()
                && (self.spec.is_lazy(kind) || self.spec.class_bodies.contains(&kind))
            {
                ctx.owner = None;
            }
            if self.decorator_ids.contains(&id) {
                ctx.in_decorator = true;
                self.decorator_chain(node, ctx.scope);
            }
            if self.spec.imports.contains(&kind) {
                ctx.in_import = true;
                self.import(node, &ctx, facts);
            }
            if self.spec.import_calls.contains(&kind) {
                self.import_call(node, &ctx, facts);
            }
            if kind == "export_statement" && in_family(self.language, Family::JavaScript) {
                self.export_nodes.push(node);
            }
            // CommonJS `module.exports = require('./x')` at module level (`names::es_exports`).
            if kind == "assignment_expression" && self.language == Language::JavaScript && ctx.scope.is_none()
            {
                self.export_nodes.push(node);
            }
            if self.spec.binding_kinds.contains(&kind) {
                ctx.binding = true;
            }
            if self.spec.deletes.contains(&kind) {
                // `del x`, `del o.a`: the operands are store positions.
                ctx.store = Store::Write;
            }
            if self.spec.type_contexts.contains(&kind) {
                ctx.in_type = true;
            }
            if self.spec.scope_declarations.contains(&kind) {
                // `global x` / `nonlocal x`: `x` is not a variable of this scope.
                for child in named_children(node) {
                    if self.spec.is_identifier(child.kind()) {
                        self.names.bind_outer(ctx.scope, text(child, self.source).trim());
                    }
                }
            }
            if let Some(ns) = self.spec.namespaces.iter().find(|n| n.kind == kind) {
                if let Some(name) = pick(node, ns.first) {
                    // Nested names (`namespace a::b`, TS `namespace A.B`) join their
                    // segments with `.`.
                    let segments: Vec<String> = if name.named_child_count() == 0 {
                        vec![text(name, self.source).trim().to_string()]
                    } else {
                        named_children(name)
                            .into_iter()
                            .map(|s| text(s, self.source).trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect()
                    };
                    let name = segments.join(".");
                    if !name.is_empty() {
                        let full = match ctx.namespace {
                            Some(p) => format!("{}.{}", self.namespaces[p as usize], name),
                            None => name,
                        };
                        self.namespaces.push(full);
                        ctx.namespace = Some(self.namespaces.len() as u32 - 1);
                    }
                }
            }

            // Facts of this node.
            if self.spec.is_name_like(kind) {
                self.idents.push((node.start_byte() as u32, node.end_byte() as u32));
            }
            self.reference(node, &ctx, facts);
            self.binding(node, kind, &ctx);
            let caps = self.caps;
            if let Some(cap) = caps.calls.get(&id) {
                self.call_site(cap, &ctx, facts);
            }
            self.assignment(node, &ctx, facts);

            // Children, with derived contexts.
            children_with_fields(node, &mut kids);
            let first_clause = generator_outer.and_then(|_| {
                kids.iter()
                    .find(|(c, _)| c.kind() == self.spec.generator_clause.kind)
                    .map(|(c, _)| c.id())
            });
            let generator = generator_outer.zip(first_clause);
            for &(child, field) in kids.iter().rev() {
                let child_ctx = self.child_ctx(node, kind, decl_here, generator, &ctx, child, field);
                stack.push((child, child_ctx));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn child_ctx(
        &self,
        node: Node<'t>,
        kind: &str,
        decl_here: Option<u32>,
        generator: Option<(Eager, usize)>,
        ctx: &Ctx,
        child: Node<'t>,
        field: Option<&'static str>,
    ) -> Ctx {
        let mut c = *ctx;
        c.attr_property = false;
        c.keyword_name = false;
        c.eager = None;
        c.export = false;
        c.label = false;
        if field.is_some_and(|f| self.spec.type_fields.contains(&f)) {
            c.in_type = true;
        }
        if let Some(e) = self.spec.export_specifiers.iter().find(|e| e.kind == kind) {
            if is_pick(node, e.first, child) {
                c.export = true;
            } else if is_pick(node, e.second, child) {
                c.label = true;
            }
        }
        if let Some(d) = decl_here {
            let syntax = &self.decls[d as usize];
            // Bodyless callables: non-header children are the body.
            if syntax.header_mode && !syntax.is_header_child(child, field) {
                c.owner = Some(d);
                c.scope = Some(d);
            }
            // Parameter lists bind in the declaration's own scope.
            if field.is_some_and(|f| self.spec.param_fields.contains(&f)) {
                c.scope = Some(d);
            }
        }
        // Generator expressions: only the first clause's iterable runs eagerly, in the
        // enclosing context.
        if let Some((outer, first)) = generator {
            if first == child.id() {
                c.eager = Some(outer);
            }
        }
        if let Some(outer) = ctx.eager {
            if field == Some(self.spec.generator_clause.second) {
                c.owner = outer.owner;
                c.lexical = outer.lexical;
                c.scope = outer.scope;
            }
        }
        if self
            .spec
            .keyword_arguments
            .iter()
            .any(|k| k.kind == kind && is_pick(node, k.first, child))
        {
            c.keyword_name = true;
        }
        // Binding positions.
        if let Some(f) = field {
            if self.spec.store_fields.iter().any(|p| p.kind == kind && p.first == f) {
                c.binding = true;
                c.store = if self.spec.declaring_stores.contains(&kind) {
                    Store::Declare
                } else {
                    Store::Write
                };
            }
            if self.spec.load_fields.iter().any(|p| p.kind == kind && p.first == f) {
                c.binding = false;
                c.store = Store::None;
            }
        }
        // Operator-selected stores (R `x <- v`, `v -> x`).
        if self.spec.operator_stores.iter().any(|o| {
            o.kind == kind
                && is_pick(node, o.pick, child)
                && o.operators.iter().any(|op| has_direct_token(node, op))
        }) {
            c.binding = true;
            c.store = Store::Write;
        }
        if let Some(m) = self.spec.member(kind) {
            if is_pick(node, m.property_field, child) {
                c.attr_property = true;
                c.binding = ctx.binding || (self.callee_ids.contains(&node.id()) && !ctx.in_decorator);
                if !ctx.binding {
                    c.store = Store::None;
                }
            } else {
                c.binding = false;
                c.store = Store::None;
            }
        } else if self.spec.subscript(kind).is_some() {
            c.binding = false;
            c.store = Store::None;
        }
        c
    }
}

impl<'a, 't> Walker<'a, 't> {
    // ---- names ---------------------------------------------------------------------

    /// Record the bindings of an import statement.
    pub(super) fn import(&mut self, node: Node<'t>, ctx: &Ctx, facts: &mut FileFacts) {
        let scope = match ctx.scope {
            Some(d) => Scope::Decl(d),
            None => Scope::Module,
        };
        for binding in crate::names::read_imports(self.spec, node, self.source) {
            self.record_import(binding, ctx, scope, facts);
        }
    }

    /// Bindings made by a loader call (`local m = require("m")`); the bound identifier is a
    /// name of the import, not a variable write.
    fn import_call(&mut self, node: Node<'t>, ctx: &Ctx, facts: &mut FileFacts) {
        let bindings = crate::names::read_call_imports(self.spec, node, self.source);
        if bindings.is_empty() {
            return;
        }
        let scope = match ctx.scope {
            Some(d) => Scope::Decl(d),
            None => Scope::Module,
        };
        for binding in bindings {
            if let Some(n) = binding.name {
                self.skip_ids.insert(n.id());
            }
            self.record_import(binding, ctx, scope, facts);
        }
    }

    /// One import binding: name resolution, the `Import` record, the `import` (or, for
    /// re-exports, `export`) reference at the imported name, and re-export records.
    fn record_import(
        &mut self,
        binding: crate::names::ImportBinding<'t>,
        ctx: &Ctx,
        scope: Scope,
        facts: &mut FileFacts,
    ) {
        use trace_core::facts::RefKind;
        self.names.bind_import(ctx.scope, &binding);
        let at = span(binding.node);
        let name_at = binding.name.map(span);
        if let Some(name) = binding.name {
            let spelled = text(name, self.source).trim().to_string();
            if !spelled.is_empty() {
                facts.references.push(Reference {
                    span: span(name),
                    name: spelled,
                    owner: ctx.owner,
                    in_decorator: false,
                    local: false,
                    kind: if binding.export {
                        RefKind::Export
                    } else {
                        RefKind::Import
                    },
                });
            }
        }
        if binding.export {
            let where_ = name_at.unwrap_or(at);
            self.exports.push(trace_core::facts::Export {
                exported: binding.local.clone(),
                target: binding.target.clone(),
                span: where_,
                line: self.lines.line1(where_.start),
            });
        }
        facts.imports.push(Import {
            local: binding.local,
            target: binding.target,
            kind: binding.kind,
            scope,
            span: at,
            line: self.lines.line1(at.start),
        });
    }

    /// Identifiers in binding positions bind their name in the current scope.
    pub(super) fn binding(&mut self, node: Node<'t>, kind: &str, ctx: &Ctx) {
        if !ctx.binding
            || ctx.in_import
            || ctx.keyword_name
            || ctx.attr_property
            || !self.spec.is_identifier(kind)
            || node.named_child_count() != 0
            || self.skip_ids.contains(&node.id())
        {
            return;
        }
        let name = text(node, self.source);
        self.names.bind_local(ctx.scope, name.trim());
        if self.language == Language::Python {
            self.py_bindings
                .push((span(node), name.trim().to_string(), ctx.scope));
        }
    }

    /// A decorator that is a dotted name (not a call) is a qualified-name candidate.
    fn decorator_chain(&mut self, node: Node<'t>, scope: Option<u32>) {
        let children = named_children(node);
        let expr = match children.as_slice() {
            [only] => *only,
            _ => node,
        };
        let expr = unwrap_node(self.spec, expr);
        if self.spec.is_identifier(expr.kind()) || self.spec.member(expr.kind()).is_some() {
            self.chains.push((expr, scope));
        }
    }
}
