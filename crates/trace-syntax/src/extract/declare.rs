//! Declarations: named and synthetic (`<lambda>`, `<genexpr>`) declarations, their
//! consumers, execution models, parameters and leading comments.

use trace_core::facts::{
    AnonymousKind, AnonymousScope, ArgSlot, Consumer, Declaration, FileFacts, Param, ParamKind,
};
use trace_core::languages::{in_family, Family};
use trace_core::model::{ByteSpan, ExecutionModel, Span, SymbolKind};
use trace_core::Language;
use tree_sitter::Node;

use super::{
    calls::decorator_texts, calls::only_whitespace, collect::cpp_scope_prefix, Ctx, DeclSyntax, ParamSyntax,
    Walker,
};
use crate::node::{
    find_descendant, has_direct_token, has_token, is_pick, named_children, pick, slice, span, text,
    truncate_bytes, type_name,
};
use crate::spec::NameAt;
use crate::MAX_DOC_BYTES;

impl<'a, 't> Walker<'a, 't> {
    // ---- declarations --------------------------------------------------------------

    pub(super) fn declare(&mut self, node: Node<'t>, ctx: &Ctx, facts: &mut FileFacts) -> Option<u32> {
        let id = node.id();
        let pending = self.caps.defs.get(&id)?;
        let name_node = pending.name?;
        let name = text(name_node, self.source).trim().to_string();
        let name_span = span(name_node);
        if name.is_empty() {
            return None;
        }
        let prep = self.prepared.get(&id)?;
        let d = facts.declarations.len() as u32;
        let parent = ctx.lexical;
        let parent_decl = parent.map(|p| &facts.declarations[p as usize]);
        let container = pending.container.and_then(|c| self.container_name(c));

        // Kind.
        let mut kind = pending.kind;
        if kind == SymbolKind::Function
            && (container.is_some() || parent_decl.is_some_and(|p| p.kind.is_type()))
        {
            kind = SymbolKind::Method;
        }
        if kind.is_callable() {
            let owner_type = container
                .as_deref()
                .or_else(|| parent_decl.filter(|p| p.kind.is_type()).map(|p| p.name.as_str()));
            let js_constructor = in_family(self.language, Family::JavaScript) && name == "constructor";
            let named_constructor =
                matches!(self.language, Language::Cpp) && owner_type.is_some_and(|t| t == name);
            if js_constructor || named_constructor {
                kind = SymbolKind::Constructor;
            }
        }

        // Qualified name: enclosing declaration (or namespace) + container + name.
        let mut qualified = match parent_decl {
            Some(p) => p.qualified_name.clone(),
            None => ctx
                .namespace
                .map(|n| self.namespaces[n as usize].clone())
                .unwrap_or_default(),
        };
        if let Some(c) = &container {
            let already = parent_decl.is_some_and(|p| &p.name == c);
            if !already {
                // C++ `a::B::f` out of line: the scopes before the container (`a`).
                if self.language == Language::Cpp && pending.container.is_some() {
                    for segment in cpp_scope_prefix(name_node, self.source) {
                        if !qualified.is_empty() {
                            qualified.push('.');
                        }
                        qualified.push_str(&segment);
                    }
                }
                if !qualified.is_empty() {
                    qualified.push('.');
                }
                qualified.push_str(c);
            }
        }
        if !qualified.is_empty() {
            qualified.push('.');
        }
        qualified.push_str(&name);

        // Spans.
        let span_bytes = ByteSpan::new(prep.start as u32, prep.end as u32);
        let span_value = Span {
            bytes: span_bytes,
            start_line: self.lines.line1(span_bytes.start),
            end_line: self
                .lines
                .line1(span_bytes.end.saturating_sub(1).max(span_bytes.start)),
        };
        let body = pending.body.or_else(|| {
            if kind.is_callable() {
                node.child_by_field_name("body")
            } else {
                None
            }
        });
        let callable_node = body
            .and_then(|b| b.parent())
            .filter(|p| p.start_byte() >= node.start_byte() && p.end_byte() <= node.end_byte())
            .unwrap_or(node);

        // Parameters.
        let params = if kind.is_callable() {
            self.read_params(&pending.params, pending.def, callable_node, body)
        } else {
            Vec::new()
        };
        self.names.declare_scope(d, ctx.scope);
        if container.is_none() {
            self.names.bind_declared(ctx.scope, &name);
        }
        for p in &params {
            if let Some(n) = p.name_node {
                self.skip_ids.insert(n.id());
                if self.language == Language::Python {
                    self.py_bindings.push((span(n), p.name.clone(), Some(d)));
                }
            }
            self.names.bind_parameter(d, &p.name);
        }

        // Body / header mode.
        let mut header_mode = false;
        let body_start = match body {
            Some(b) => b.start_byte() as u32,
            None if kind.is_callable() => {
                let probe = DeclSyntax {
                    def: node,
                    body: None,
                    header_mode: false,
                    name_node,
                    decorators: prep.decorators.clone(),
                    bases: pending.bases.clone(),
                    params: Vec::new(),
                    synthetic: None,
                };
                let mut cursor = node.walk();
                let mut first_body = None;
                if cursor.goto_first_child() {
                    loop {
                        let child = cursor.node();
                        if child.is_named()
                            && !child.is_extra()
                            && !probe.is_header_child(child, cursor.field_name())
                            && child.start_byte() > name_node.start_byte()
                        {
                            first_body = Some(child.start_byte() as u32);
                            break;
                        }
                        if !cursor.goto_next_sibling() {
                            break;
                        }
                    }
                }
                match first_body {
                    Some(start) if !self.spec.stub_when_bodiless => {
                        header_mode = true;
                        start
                    }
                    _ => span_bytes.end,
                }
            }
            None => span_bytes.end,
        };

        // Doc: adjacent comments, else a `@doc` capture (Python docstrings: python.rs).
        let doc = self.leading_comment(prep.start).or_else(|| {
            pending
                .doc
                .map(|n| truncate_bytes(text(n, self.source).trim().to_string(), MAX_DOC_BYTES))
        });

        let mut decorators = Vec::new();
        for n in &prep.decorators {
            decorator_texts(*n, self.source, &mut decorators);
        }
        let bases: Vec<String> = pending
            .bases
            .iter()
            .map(|n| text(*n, self.source).trim().to_string())
            .filter(|b| !b.is_empty())
            .collect();

        let execution = self.execution(kind, callable_node);
        // Function-like macros are definitions, never prototypes.
        let is_stub = kind.is_callable()
            && !self.spec.macro_definitions.contains(&node.kind())
            && ((body.is_none() && !header_mode && self.spec.stub_when_bodiless)
                || has_token(node, "abstract")
                || pending.stub);

        facts.declarations.push(Declaration {
            name,
            qualified_name: qualified,
            kind,
            span: span_value,
            name_span,
            body_start,
            parent,
            container,
            doc,
            decorators,
            bases,
            parameters: params
                .iter()
                .map(|p| Param {
                    name: p.name.clone(),
                    kind: p.kind,
                    has_default: p.default.is_some(),
                })
                .collect(),
            execution,
            is_stub,
            is_test: false,
            declaration_lines: Vec::new(),
            identifiers: Vec::new(),
        });
        if let Some(b) = body {
            self.body_owner.insert(b.id(), d);
        }
        self.decls.push(DeclSyntax {
            def: node,
            body,
            header_mode,
            name_node,
            decorators: prep.decorators.clone(),
            bases: pending.bases.clone(),
            params,
            synthetic: None,
        });
        Some(d)
    }

    /// Container of an out-of-line declaration from its `@container` capture (SPEC §6.3,
    /// general fixes rule 17). JS/TS property assignments (`obj.prop = function ...`) are
    /// declared for the receiver path, so `qualified_name` is the assignment path
    /// (`res.redirect`) and the selector `obj.prop` matches exactly; the self reference
    /// (`this.f = ...`, the lexical class names it already) and the CommonJS export object
    /// (`exports.f`, `module.exports.f`: module-level functions) name no container, and
    /// `X.prototype.m` is a method of `X`. Other languages: the bare type name.
    fn container_name(&self, node: Node<'t>) -> Option<String> {
        let js = in_family(self.language, Family::JavaScript);
        if !js {
            let name = type_name(node, self.source);
            return (!name.is_empty()).then_some(name);
        }
        let chain = crate::names::chain(self.spec, node, self.source)?;
        let mut parts: Vec<String> = Vec::with_capacity(chain.rest.len() + 1);
        parts.push(chain.root);
        parts.extend(chain.rest);
        if parts.len() > 1 && parts.last().is_some_and(|p| p == "prototype") {
            parts.pop();
        }
        let path = parts.join(".");
        if matches!(path.as_str(), "exports" | "module" | "module.exports") {
            return None;
        }
        Some(path)
    }

    /// A synthetic declaration for a lambda / generator expression (module docs).
    pub(super) fn declare_anonymous(
        &mut self,
        node: Node<'t>,
        kind: AnonymousKind,
        ctx: &Ctx,
        facts: &mut FileFacts,
    ) -> u32 {
        let d = facts.declarations.len() as u32;
        let parent = ctx.lexical;
        let name = match kind {
            AnonymousKind::Lambda => "<lambda>",
            AnonymousKind::GeneratorExpression => "<genexpr>",
        };
        let mut qualified = match parent {
            Some(p) => facts.declarations[p as usize].qualified_name.clone(),
            None => ctx
                .namespace
                .map(|n| self.namespaces[n as usize].clone())
                .unwrap_or_default(),
        };
        if !qualified.is_empty() {
            qualified.push('.');
        }
        qualified.push_str(name);
        let name_node = node.child(0).unwrap_or(node);
        let body = node.child_by_field_name("body");
        // Anonymous callables without a `body` field (Scala and Haskell lambdas, C# anonymous
        // methods, Rust async blocks): every non-header child is the body (header mode).
        let header_mode = kind == AnonymousKind::Lambda && body.is_none();
        let (params, eager, execution) = match kind {
            AnonymousKind::Lambda => {
                let params = self.read_params(&[], node, node, body);
                let execution = if self.language == Language::Python {
                    if body.is_some_and(crate::python::contains_own_yield) {
                        ExecutionModel::Generator
                    } else {
                        ExecutionModel::Ordinary
                    }
                } else {
                    // `async (x) => ...`, `function* () {}`, Rust `async move { }`.
                    self.execution(SymbolKind::Function, node)
                };
                (params, None, execution)
            }
            AnonymousKind::GeneratorExpression => {
                let clause = self.spec.generator_clause;
                let clauses: Vec<Node<'t>> = named_children(node)
                    .into_iter()
                    .filter(|c| c.kind() == clause.kind)
                    .collect();
                let eager = clauses.first().and_then(|c| pick(*c, clause.second)).map(span);
                let awaits = |n: Node<'t>| self.spec.awaits.contains(&n.kind());
                let asynchronous = clauses.iter().any(|c| has_direct_token(*c, "async"))
                    || body.is_some_and(|b| awaits(b) || find_descendant(b, 4_096, awaits).is_some());
                let execution = if asynchronous {
                    ExecutionModel::AsyncGenerator
                } else {
                    ExecutionModel::Generator
                };
                (Vec::new(), eager, execution)
            }
        };
        let whole = span(node);
        let body_start = match body {
            Some(b) => b.start_byte() as u32,
            None if header_mode => {
                let probe = DeclSyntax {
                    def: node,
                    body: None,
                    header_mode: true,
                    name_node,
                    decorators: Vec::new(),
                    bases: Vec::new(),
                    params: Vec::new(),
                    synthetic: Some(kind),
                };
                let mut cursor = node.walk();
                let mut first = None;
                if cursor.goto_first_child() {
                    loop {
                        let child = cursor.node();
                        if child.is_named()
                            && !child.is_extra()
                            && !probe.is_header_child(child, cursor.field_name())
                        {
                            first = Some(child.start_byte() as u32);
                            break;
                        }
                        if !cursor.goto_next_sibling() {
                            break;
                        }
                    }
                }
                first.unwrap_or(whole.end)
            }
            None => whole.end,
        };
        facts.declarations.push(Declaration {
            name: name.to_string(),
            qualified_name: qualified,
            kind: SymbolKind::Function,
            span: Span {
                bytes: whole,
                start_line: self.lines.line1(whole.start),
                end_line: self.lines.line1(whole.end.saturating_sub(1).max(whole.start)),
            },
            name_span: span(name_node),
            body_start,
            parent,
            container: None,
            doc: None,
            decorators: Vec::new(),
            bases: Vec::new(),
            parameters: params
                .iter()
                .map(|p| Param {
                    name: p.name.clone(),
                    kind: p.kind,
                    has_default: p.default.is_some(),
                })
                .collect(),
            execution,
            is_stub: false,
            is_test: false,
            declaration_lines: Vec::new(),
            identifiers: Vec::new(),
        });
        facts.anonymous.push(AnonymousScope {
            decl: d,
            kind,
            created_in: ctx.owner,
            consumer: self.consumer(node),
            eager,
        });
        self.names.declare_scope(d, ctx.scope);
        for p in &params {
            if let Some(n) = p.name_node {
                self.skip_ids.insert(n.id());
                if self.language == Language::Python {
                    self.py_bindings.push((span(n), p.name.clone(), Some(d)));
                }
            }
            self.names.bind_parameter(d, &p.name);
        }
        let body = match kind {
            AnonymousKind::Lambda => body,
            AnonymousKind::GeneratorExpression => None,
        };
        if let Some(b) = body {
            self.body_owner.insert(b.id(), d);
        }
        self.decls.push(DeclSyntax {
            def: node,
            body,
            header_mode,
            name_node,
            decorators: Vec::new(),
            bases: Vec::new(),
            params,
            synthetic: Some(kind),
        });
        d
    }

    /// How the value of `node` is consumed by its syntactic parent (parentheses skipped).
    pub(super) fn consumer(&self, node: Node<'t>) -> Consumer {
        let spec = self.spec;
        let mut child = node;
        let mut parent = node.parent();
        while let Some(p) = parent {
            if spec.unwrap_pick(p.kind()).is_none() {
                break;
            }
            child = p;
            parent = p.parent();
        }
        let Some(p) = parent else {
            return Consumer::Other;
        };
        let kind = p.kind();
        let call_of = |n: Node<'t>| self.call_index.get(&n.id()).copied();
        if let Some(shape) = spec.call_shape(kind) {
            let Some(call) = call_of(p) else {
                return Consumer::Other;
            };
            if is_pick(p, shape.function_field, child) {
                return Consumer::Called { call };
            }
            if is_pick(p, shape.arguments_field, child) {
                return self.argument_consumer(call, child, child);
            }
            // Block / trailing-lambda syntax attached to the call (Scala `f(x) { ... }`).
            return self.trailing_consumer(call, p);
        }
        // Trailing lambdas wrapped by the call syntax: a direct child of the call that is
        // neither callee nor arguments.
        if let Some(gp) = p.parent() {
            if let Some(shape) = spec.call_shape(gp.kind()) {
                if !is_pick(gp, shape.function_field, p)
                    && !is_pick(gp, shape.arguments_field, p)
                    && spec.anonymous_functions.contains(&node.kind())
                    && !spec.keyword_arguments.iter().any(|k| k.kind == kind)
                    && !spec.argument_wrappers.contains(&kind)
                    && self.call_of_list(p).is_none()
                {
                    if let Some(call) = call_of(gp) {
                        return self.trailing_consumer(call, gp);
                    }
                }
            }
        }
        if spec.keyword_spreads.contains(&kind) {
            return Consumer::Other;
        }
        if spec.spreads.contains(&kind) || spec.iterate_parents.contains(&kind) {
            return Consumer::Iterated;
        }
        let wrapper =
            spec.keyword_arguments.iter().any(|k| k.kind == kind) || spec.argument_wrappers.contains(&kind);
        if wrapper {
            return match p.parent().and_then(|list| self.call_of_list(list)) {
                Some((call, list)) => self.argument_consumer(call, list, p),
                None => Consumer::Other,
            };
        }
        if let Some((call, list)) = self.call_of_list(p) {
            return self.argument_consumer(call, list, child);
        }
        let in_field = |field: &str| {
            let mut cursor = p.walk();
            let found = p
                .children_by_field_name(field, &mut cursor)
                .any(|c| c.id() == child.id());
            found
        };
        if spec
            .for_loops
            .iter()
            .any(|f| f.kind == kind && is_pick(p, f.iterable, child))
            || (kind == spec.generator_clause.kind && in_field(spec.generator_clause.second))
        {
            return Consumer::Iterated;
        }
        if let Some(y) = spec.delegating_yields.iter().find(|y| y.kind == kind) {
            return if has_direct_token(p, y.token) {
                Consumer::Iterated
            } else {
                Consumer::Yielded
            };
        }
        if spec.awaits.contains(&kind) {
            return Consumer::Awaited;
        }
        if spec.returns.contains(&kind) {
            return Consumer::Returned;
        }
        let assigned = spec.assignment(kind).is_some_and(|a| is_pick(p, a.second, child));
        let default = spec.param_rule(kind).is_some_and(|r| is_pick(p, r.default, child));
        if assigned || default {
            return Consumer::Bound;
        }
        Consumer::Other
    }

    /// A block / trailing lambda of call `call` (node `call_node`): the argument after the
    /// positional arguments of its list (`exact = false`: the language may pass it apart
    /// from positional parameters).
    fn trailing_consumer(&self, call: u32, call_node: Node<'t>) -> Consumer {
        let index = self
            .spec
            .call_shape(call_node.kind())
            .and_then(|s| pick(call_node, s.arguments_field))
            .map_or(0, |list| {
                crate::detail::argument_nodes(self.spec, list, self.source)
                    .iter()
                    .filter(|a| matches!(a.slot, ArgSlot::Positional { .. }))
                    .count() as u32
            });
        Consumer::Argument {
            call,
            slot: ArgSlot::Positional { index, exact: false },
        }
    }

    /// `(call index, list)` when `list` is the argument list of a recorded call.
    fn call_of_list(&self, list: Node<'t>) -> Option<(u32, Node<'t>)> {
        let call = list.parent()?;
        let shape = self.spec.call_shape(call.kind())?;
        if !is_pick(call, shape.arguments_field, list) {
            return None;
        }
        Some((*self.call_index.get(&call.id())?, list))
    }

    /// The argument slot of `arg` (a direct child of `list`, or `list` itself for a
    /// generator-expression argument list).
    fn argument_consumer(&self, call: u32, list: Node<'t>, arg: Node<'t>) -> Consumer {
        let slot = crate::detail::argument_nodes(self.spec, list, self.source)
            .into_iter()
            .find(|a| a.node.id() == arg.id())
            .map(|a| a.slot);
        match slot {
            Some(ArgSlot::Unpack) => Consumer::Iterated,
            Some(slot) => Consumer::Argument { call, slot },
            None => Consumer::Other,
        }
    }
}

impl<'a, 't> Walker<'a, 't> {
    pub(super) fn execution(&self, kind: SymbolKind, callable: Node<'t>) -> ExecutionModel {
        if !kind.is_callable() {
            return ExecutionModel::Ordinary;
        }
        let (generator, asynchronous) = match self.language {
            Language::JavaScript | Language::TypeScript | Language::Tsx => (
                callable.kind().contains("generator") || has_direct_token(callable, "*"),
                has_token(callable, "async"),
            ),
            Language::Rust => (false, has_token(callable, "async")),
            _ => (false, false),
        };
        match (generator, asynchronous) {
            (true, true) => ExecutionModel::AsyncGenerator,
            (true, false) => ExecutionModel::Generator,
            (false, true) => ExecutionModel::Coroutine,
            (false, false) => ExecutionModel::Ordinary,
        }
    }

    /// Parameters from query captures (`explicit`), else from the callable's parameter list
    /// (searched inside `def` for declarator-style grammars).
    fn read_params(
        &self,
        explicit: &[(Node<'t>, ParamKind, Option<Node<'t>>)],
        def: Node<'t>,
        callable: Node<'t>,
        body: Option<Node<'t>>,
    ) -> Vec<ParamSyntax<'t>> {
        if !explicit.is_empty() {
            return explicit
                .iter()
                .map(|(n, kind, default)| ParamSyntax {
                    name: text(*n, self.source).trim().to_string(),
                    kind: *kind,
                    name_node: Some(*n),
                    default: *default,
                })
                .collect();
        }
        let list = self
            .spec
            .param_fields
            .iter()
            .find_map(|f| pick(callable, f))
            .or_else(|| {
                if self.spec.param_lists.is_empty() {
                    return None;
                }
                let limit = body.map_or(usize::MAX, |b| b.start_byte());
                find_descendant(def, 256, |n| {
                    self.spec.param_lists.contains(&n.kind()) && n.start_byte() < limit
                })
            });
        let Some(list) = list else {
            return Vec::new();
        };
        // A single parameter without a list (`x => x`).
        if self.spec.param_rule(list.kind()).is_some() {
            return self.read_param(list, 0).into_iter().collect();
        }
        let mut out = Vec::new();
        let mut keyword_only = false;
        let variadic_switches = !self.spec.keyword_separators.is_empty();
        for child in named_children(list) {
            if self.spec.keyword_separators.contains(&child.kind()) {
                keyword_only = true;
                continue;
            }
            let Some(mut p) = self.read_param(child, 0) else {
                continue;
            };
            if p.kind == ParamKind::Positional && keyword_only {
                p.kind = ParamKind::KeywordOnly;
            }
            if variadic_switches && p.kind == ParamKind::VarPositional {
                keyword_only = true;
            }
            // One declaration naming several parameters (Go `a, b string`): one each.
            let mut more = Vec::new();
            if let Some(NameAt::Pick(field)) = self.spec.param_rule(child.kind()).map(|r| r.name) {
                let mut cursor = child.walk();
                for n in child.children_by_field_name(field, &mut cursor).skip(1) {
                    if self.spec.is_identifier(n.kind()) || n.named_child_count() == 0 {
                        more.push(ParamSyntax {
                            name: text(n, self.source).trim().to_string(),
                            kind: p.kind,
                            name_node: Some(n),
                            default: None,
                        });
                    }
                }
            }
            out.push(p);
            out.extend(more);
        }
        out
    }

    fn read_param(&self, node: Node<'t>, depth: usize) -> Option<ParamSyntax<'t>> {
        let kind = node.kind();
        let Some(rule) = self.spec.param_rule(kind) else {
            // Destructuring patterns keep positional arity (named by their spelling).
            if kind.ends_with("pattern") {
                let name = truncate_bytes(text(node, self.source).trim().to_string(), 40);
                return Some(ParamSyntax {
                    name,
                    kind: ParamKind::Positional,
                    name_node: None,
                    default: None,
                });
            }
            return None;
        };
        let default = pick(node, rule.default);
        match rule.name {
            NameAt::Skip => None,
            NameAt::Itself => Some(ParamSyntax {
                name: text(node, self.source).to_string(),
                kind: rule.param_kind,
                name_node: Some(node),
                default,
            }),
            NameAt::Pick(selector) => {
                let inner = pick(node, selector)?;
                // A leaf, or an identifier kind with inner structure (PHP `variable_name`
                // = `$` + `name`): the whole node is the parameter's name.
                if inner.named_child_count() == 0 || self.spec.is_identifier(inner.kind()) {
                    return Some(ParamSyntax {
                        name: text(inner, self.source).trim().to_string(),
                        kind: rule.param_kind,
                        name_node: Some(inner),
                        default,
                    });
                }
                if depth >= 4 {
                    return None;
                }
                let mut nested = self.read_param(inner, depth + 1).or_else(|| {
                    find_descendant(inner, 32, |n| self.spec.is_identifier(n.kind())).map(|n| ParamSyntax {
                        name: text(n, self.source).to_string(),
                        kind: ParamKind::Positional,
                        name_node: Some(n),
                        default: None,
                    })
                })?;
                if rule.param_kind != ParamKind::Positional {
                    nested.kind = rule.param_kind;
                }
                if default.is_some() {
                    nested.default = default;
                }
                Some(nested)
            }
        }
    }

    /// Contiguous comments ending right before `start` (only whitespace between).
    fn leading_comment(&self, start: usize) -> Option<String> {
        let start = start as u32;
        let idx = self.comments.partition_point(|c| c.end <= start);
        if idx == 0 {
            return None;
        }
        let last = idx - 1;
        if !only_whitespace(self.source, self.comments[last].end, start) {
            return None;
        }
        let mut first = last;
        while first > 0
            && only_whitespace(self.source, self.comments[first - 1].end, self.comments[first].start)
        {
            first -= 1;
        }
        let a = self.comments[first].start as usize;
        let b = (self.comments[last].end as usize).min(a + MAX_DOC_BYTES);
        let doc = truncate_bytes(slice(self.source, a, b).trim_end().to_string(), MAX_DOC_BYTES);
        (!doc.trim().is_empty()).then_some(doc)
    }
}
