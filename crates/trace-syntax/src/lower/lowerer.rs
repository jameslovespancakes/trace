//! The lowerer: indexing declaration syntax, the walk over scopes, declarations and their
//! members, decorators, literal keys and lists, implicit receivers.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use trace_core::facts::{AnonymousKind, BindTarget, Consumer, Expr, FileFacts, FlowFact, ParamKind, Scope};
use trace_core::text::LineIndex;
use tree_sitter::Node;

use super::{
    expr::is_function_value, expr::lower_expr_in, expr::Mode, expr::Synthetic, expr::MAX_KEY_ANCESTORS,
    expr::MAX_LIST_WALK, expr::MAX_LITERAL_KEYS, library::string_content, library::string_literal,
    library::CONTAINER_KINDS, library::HOLE_KINDS, library::LITERAL_PREFIX, library::STRING_KINDS, LCtx,
    Lowerer,
};
use crate::extract::DeclSyntax;
use crate::node::{has_direct_token, has_token, named_children, nth_named, pick, span, text};
use crate::spec::{Receiver, SyntaxSpec};

impl<'a, 't> Lowerer<'a, 't> {
    pub(super) fn new(
        spec: &'a SyntaxSpec,
        source: &'a [u8],
        lines: &'a LineIndex,
        decls: &'a [DeclSyntax<'t>],
        facts: &FileFacts,
    ) -> Self {
        let mut types = HashMap::new();
        for (i, d) in facts.declarations.iter().enumerate() {
            if d.kind.is_type() {
                types.entry(d.name.clone()).or_insert(i as u32);
            }
        }
        let mut lowerer = Lowerer {
            spec,
            source,
            lines,
            decls,
            partial: Vec::new(),
            def_index: HashMap::new(),
            decorator_ids: HashSet::new(),
            synth: Synthetic::new(),
            types,
            lib: false,
            ops: RefCell::new(Vec::new()),
        };
        lowerer.index();
        lowerer
    }

    pub(super) fn index(&mut self) {
        self.def_index.clear();
        self.decorator_ids.clear();
        self.synth.clear();
        let entries: Vec<(u32, usize, Vec<usize>, bool)> = (0..self.len())
            .filter_map(|i| {
                self.syntax(i as u32).map(|s| {
                    (
                        i as u32,
                        s.def.id(),
                        s.decorators.iter().map(|n| n.id()).collect(),
                        is_function_value(self.spec, s),
                    )
                })
            })
            .collect();
        for (i, def, decorators, synthetic) in entries {
            self.def_index.insert(def, i);
            self.decorator_ids.extend(decorators);
            if synthetic {
                self.synth.insert(def, i);
            }
        }
    }

    pub(super) fn len(&self) -> usize {
        if self.partial.is_empty() {
            self.decls.len()
        } else {
            self.partial.len()
        }
    }

    fn syntax(&self, d: u32) -> Option<&DeclSyntax<'t>> {
        if self.partial.is_empty() {
            self.decls.get(d as usize)
        } else {
            self.partial.get(d as usize).and_then(Option::as_ref)
        }
    }

    /// Library mode lowers with the synthetic declarations; the index facts name every
    /// declaration a function value expression defines (`function name() {}` values too).
    pub(super) fn lower(&self, node: Node<'_>) -> Expr {
        if self.lib {
            lower_expr_in(self.spec, node, self.source, 0, &self.synth, Mode::Library)
        } else {
            lower_expr_in(self.spec, node, self.source, 0, &self.def_index, Mode::Facts)
        }
    }

    pub(super) fn run(&mut self, root: Node<'t>, facts: &mut FileFacts) {
        if !self.partial.is_empty() && self.def_index.is_empty() {
            self.index();
        }
        let mut stack: Vec<(Node<'t>, LCtx)> = vec![(root, LCtx::body(Scope::Module, None, true))];
        let mut children: Vec<Node<'t>> = Vec::new();
        while let Some((node, ctx)) = stack.pop() {
            if !node.is_named() || node.is_extra() {
                continue;
            }
            let id = node.id();
            if let Some(&d) = self.def_index.get(&id) {
                self.declaration(d, ctx, facts, &mut stack);
                continue;
            }
            let mut child_ctx = ctx;
            if self.decorator_ids.contains(&id) {
                child_ctx.statements = false;
            } else if self.spec.is_lazy(node.kind()) {
                child_ctx.active = false;
            } else if ctx.active && ctx.statements {
                self.statement(node, ctx, facts);
            }
            children.clear();
            let mut cursor = node.walk();
            children.extend(node.named_children(&mut cursor));
            for &child in children.iter().rev() {
                stack.push((child, child_ctx));
            }
        }
    }

    fn declaration(&self, d: u32, ctx: LCtx, facts: &mut FileFacts, stack: &mut Vec<(Node<'t>, LCtx)>) {
        let Some(syntax) = self.syntax(d) else {
            return;
        };
        let Some(decl) = facts.declarations.get(d as usize) else {
            return;
        };
        let kind = decl.kind;
        let name = decl.name.clone();
        if kind.is_callable() {
            if ctx.active {
                for p in &syntax.params {
                    if let Some(default) = p.default {
                        facts.flow.push(FlowFact::Bind {
                            target: BindTarget::Var {
                                scope: Scope::Decl(d),
                                name: p.name.clone(),
                            },
                            value: self.lower(default),
                            scope: ctx.scope,
                        });
                    }
                }
            }
            if let Some(fact) = self.implicit_self(d, syntax, ctx, facts) {
                facts.flow.push(fact);
            }
            if ctx.active && !self.lib && syntax.synthetic.is_none() {
                self.member_declaration(d, syntax, ctx, facts);
            }
            if ctx.active && self.spec.decorators_wrap && !syntax.decorators.is_empty() {
                let target = match ctx.class {
                    Some(class) => BindTarget::Member {
                        class,
                        name: name.clone(),
                    },
                    None => BindTarget::Var {
                        scope: ctx.scope,
                        name: name.clone(),
                    },
                };
                let decorators = syntax
                    .decorators
                    .iter()
                    .map(|n| {
                        let expr = if n.kind() == "decorator" {
                            nth_named(*n, 0).unwrap_or(*n)
                        } else {
                            *n
                        };
                        self.lower_decorator(expr)
                    })
                    .collect();
                facts.flow.push(FlowFact::Decorated {
                    scope: ctx.scope,
                    target,
                    function: d,
                    decorators,
                });
            }
            // Synthetic declarations inside default values lower in their own scope.
            for p in &syntax.params {
                if let Some(default) = p.default {
                    stack.push((
                        default,
                        LCtx {
                            class: None,
                            statements: false,
                            ..ctx
                        },
                    ));
                }
            }
            if syntax.synthetic == Some(AnonymousKind::Lambda) {
                if let Some(body) = syntax.body {
                    // Expression bodies return their value; block bodies (other languages)
                    // lower to `Opaque` and return through their `return` statements.
                    let value = self.lower(body);
                    let python = self.spec.language == trace_core::Language::Python;
                    if python || !matches!(value, Expr::Opaque) {
                        facts.flow.push(FlowFact::Return { function: d, value });
                    }
                }
            }
            let inner = LCtx::body(Scope::Decl(d), None, true);
            if syntax.synthetic == Some(AnonymousKind::GeneratorExpression) {
                self.push_generator(syntax.def, ctx, inner, stack);
            } else {
                self.push_body(syntax, inner, stack);
            }
        } else {
            let inner = LCtx::body(ctx.scope, Some(d), ctx.active);
            self.push_body(syntax, inner, stack);
        }
    }

    /// A callable defined under a member name in a language with dynamic members
    /// (`SyntaxSpec::dynamic_members`: `obj.m = function () {}`) is stored in that member of the object
    /// (`FieldOf`; no field-name evidence: name matching already offers a named definition for
    /// calls on unknown receivers).
    fn member_declaration(&self, d: u32, syntax: &DeclSyntax<'t>, ctx: LCtx, facts: &mut FileFacts) {
        if !self.spec.dynamic_members {
            return;
        }
        let Some(access) = syntax.name_node.parent() else {
            return;
        };
        let Some(m) = self.spec.member(access.kind()) else {
            return;
        };
        let (Some(object), Some(property)) = (pick(access, m.object_field), pick(access, m.property_field))
        else {
            return;
        };
        let inside =
            access.start_byte() >= syntax.def.start_byte() && access.end_byte() <= syntax.def.end_byte();
        if property.id() != syntax.name_node.id() || !inside {
            return;
        }
        let name = text(property, self.source).trim().to_string();
        if name.is_empty() {
            return;
        }
        facts.flow.push(FlowFact::Bind {
            target: BindTarget::FieldOf {
                object: self.lower(object),
                name,
            },
            value: Expr::Lambda {
                span: span(syntax.def),
                function: Some(d),
            },
            scope: ctx.scope,
        });
    }

    /// A decorator expression; string keyword arguments of a decorator call are values of the
    /// decoration (`@provider(name="client")` renames what it provides): literal names
    /// (`Name { "<lit>client" }`, [`LITERAL_PREFIX`]).
    fn lower_decorator(&self, node: Node<'t>) -> Expr {
        let mut expr = self.lower(node);
        let Some(shape) = self.spec.call_shape(node.kind()) else {
            return expr;
        };
        let Some(list) = pick(node, shape.arguments_field) else {
            return expr;
        };
        let mut literals: Vec<(String, Expr)> = Vec::new();
        for arg in named_children(list) {
            let Some(k) = self.spec.keyword_arguments.iter().find(|k| k.kind == arg.kind()) else {
                continue;
            };
            let (Some(name), Some(value)) = (pick(arg, k.first), pick(arg, k.second)) else {
                continue;
            };
            if !STRING_KINDS.contains(&value.kind()) {
                continue;
            }
            let holes = named_children(value)
                .iter()
                .any(|c| HOLE_KINDS.contains(&c.kind()) || self.spec.interpolations.contains(&c.kind()));
            if holes {
                continue;
            }
            literals.push((
                text(name, self.source).trim().to_string(),
                Expr::Name {
                    name: format!("{LITERAL_PREFIX}{}", string_content(value, self.source)),
                    span: span(value),
                },
            ));
        }
        if let Expr::Call { kwargs, .. } = &mut expr {
            for (key, literal) in literals {
                let plain = |v: &Expr| match v {
                    Expr::Opaque => true,
                    Expr::Name { name, .. } => name == LITERAL_PREFIX,
                    _ => false,
                };
                if let Some(slot) = kwargs.iter_mut().find(|(n, v)| *n == key && plain(v)) {
                    slot.1 = literal;
                }
            }
        }
        expr
    }

    /// The string keys a computed member key stands for ([`DYNAMIC_MEMBERS`]): a string
    /// literal key, or a name bound to each element of a literal list of strings by an
    /// enclosing loop (`for (const m of LIST)`) or as the first parameter of a callback passed
    /// first to a method of the list (`LIST.forEach(function (m) { .. })`). `LIST` is a list
    /// literal or a name bound once in this file to one. `None` when unknown or longer than
    /// [`MAX_LITERAL_KEYS`].
    pub(super) fn literal_keys(&self, key: Node<'t>) -> Option<Vec<String>> {
        if STRING_KINDS.contains(&key.kind()) {
            return string_literal(self.spec, key, self.source).map(|s| vec![s]);
        }
        if !self.spec.is_identifier(key.kind()) {
            return None;
        }
        let name = text(key, self.source).trim().to_string();
        let mut node = key;
        for _ in 0..MAX_KEY_ANCESTORS {
            let parent = node.parent()?;
            for f in self.spec.for_loops {
                if parent.kind() != f.kind || !(f.token.is_empty() || has_direct_token(parent, f.token)) {
                    continue;
                }
                let (Some(target), Some(iterable)) = (pick(parent, f.target), pick(parent, f.iterable))
                else {
                    continue;
                };
                if self.spec.is_identifier(target.kind()) && text(target, self.source).trim() == name {
                    return self.literal_list(iterable);
                }
            }
            if self.spec.is_lazy(parent.kind()) {
                // A function boundary: the key must be its first parameter.
                let d = *self.def_index.get(&parent.id())?;
                let first = self.syntax(d)?.params.first()?;
                if first.name != name {
                    return None;
                }
                let list = parent.parent()?;
                let call = list.parent()?;
                let shape = self.spec.call_shape(call.kind())?;
                let is_list = pick(call, shape.arguments_field).is_some_and(|a| a.id() == list.id());
                let first_arg = named_children(list).first().is_some_and(|a| a.id() == parent.id());
                if !is_list || !first_arg {
                    return None;
                }
                let callee = pick(call, shape.function_field)?;
                let m = self.spec.member(callee.kind())?;
                return self.literal_list(pick(callee, m.object_field)?);
            }
            node = parent;
        }
        None
    }

    /// String elements of a literal list, or of the literal list a name is bound to exactly
    /// once in this file (and never assigned otherwise).
    fn literal_list(&self, node: Node<'t>) -> Option<Vec<String>> {
        let node = match self.spec.unwrap_pick(node.kind()) {
            Some(selector) => pick(node, selector)?,
            None => node,
        };
        if self.spec.is_identifier(node.kind()) {
            let name = text(node, self.source).trim().to_string();
            let value = self.list_binding(node, &name)?;
            return self.literal_elements(value);
        }
        self.literal_elements(node)
    }

    /// Elements of a list literal when every element is a plain string literal.
    fn literal_elements(&self, node: Node<'t>) -> Option<Vec<String>> {
        let kind = node.kind();
        let list = CONTAINER_KINDS.contains(&kind) && !self.spec.object_literals.contains(&kind);
        if !list {
            return None;
        }
        let mut out = Vec::new();
        for element in named_children(node) {
            if self.spec.is_comment(element.kind()) {
                continue;
            }
            if !STRING_KINDS.contains(&element.kind()) {
                return None;
            }
            out.push(string_literal(self.spec, element, self.source)?);
            if out.len() > MAX_LITERAL_KEYS {
                return None;
            }
        }
        (!out.is_empty()).then_some(out)
    }

    /// The value of the only assignment to `name` in the file of `at` (at most
    /// [`MAX_LIST_WALK`] nodes are visited; `None` when there are several or none).
    fn list_binding(&self, at: Node<'t>, name: &str) -> Option<Node<'t>> {
        let mut root = at;
        while let Some(p) = root.parent() {
            root = p;
        }
        let mut found: Option<Node<'t>> = None;
        let mut stack = vec![root];
        let mut visited = 0usize;
        while let Some(n) = stack.pop() {
            visited += 1;
            if visited > MAX_LIST_WALK {
                return None;
            }
            if let Some(a) = self.spec.assignment(n.kind()) {
                if let (Some(target), Some(value)) = (pick(n, a.first), pick(n, a.second)) {
                    let target = self.single(target);
                    if self.spec.is_identifier(target.kind()) && text(target, self.source).trim() == name {
                        if found.is_some() {
                            return None;
                        }
                        found = Some(self.single(value));
                    }
                }
            }
            stack.extend(named_children(n));
        }
        found
    }

    /// Generator expression children: the first clause's iterable in the enclosing context,
    /// everything else (element, conditions, later clauses) in the generator's scope.
    fn push_generator(&self, node: Node<'t>, outer: LCtx, inner: LCtx, stack: &mut Vec<(Node<'t>, LCtx)>) {
        let clause = self.spec.generator_clause;
        let children = named_children(node);
        let first = children.iter().find(|c| c.kind() == clause.kind).map(|c| c.id());
        let mut work: Vec<(Node<'t>, LCtx)> = Vec::with_capacity(children.len() + 2);
        for child in children {
            if Some(child.id()) != first {
                work.push((child, inner));
                continue;
            }
            let mut cursor = child.walk();
            if cursor.goto_first_child() {
                loop {
                    let n = cursor.node();
                    if n.is_named() && !n.is_extra() {
                        let ctx = if cursor.field_name() == Some(clause.second) {
                            outer
                        } else {
                            inner
                        };
                        work.push((n, ctx));
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
        stack.extend(work.into_iter().rev());
    }

    fn push_body(&self, syntax: &DeclSyntax<'t>, ctx: LCtx, stack: &mut Vec<(Node<'t>, LCtx)>) {
        if let Some(body) = syntax.body {
            stack.push((body, ctx));
        } else if syntax.header_mode {
            let mut cursor = syntax.def.walk();
            let mut children = Vec::new();
            if cursor.goto_first_child() {
                loop {
                    let child = cursor.node();
                    if child.is_named()
                        && !child.is_extra()
                        && !syntax.is_header_child(child, cursor.field_name())
                    {
                        children.push(child);
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
            for child in children.into_iter().rev() {
                stack.push((child, ctx));
            }
        }
    }

    fn implicit_self(
        &self,
        d: u32,
        syntax: &DeclSyntax<'t>,
        ctx: LCtx,
        facts: &FileFacts,
    ) -> Option<FlowFact> {
        if syntax.synthetic.is_some() && !facts.anonymous_of(d).is_some_and(|a| a.consumer == Consumer::Bound)
        {
            return None;
        }
        let decl = &facts.declarations[d as usize];
        match self.spec.receiver {
            Receiver::None => None,
            Receiver::FirstParam => {
                let class = ctx.class?;
                let first = syntax.params.first()?;
                if first.kind != ParamKind::Positional || decl.decorators.iter().any(|t| t == "staticmethod")
                {
                    return None;
                }
                Some(FlowFact::ImplicitSelf {
                    function: d,
                    param: first.name.clone(),
                    class,
                    is_class: decl.decorators.iter().any(|t| t == "classmethod"),
                })
            }
            Receiver::Implicit(name) => {
                let class = ctx.class?;
                Some(FlowFact::ImplicitSelf {
                    function: d,
                    param: name.to_string(),
                    class,
                    is_class: has_token(syntax.def, "static"),
                })
            }
            Receiver::SelfParam(name) => {
                if !syntax.params.iter().any(|p| p.name == name) {
                    return None;
                }
                let class = ctx
                    .class
                    .or_else(|| decl.parent.filter(|&p| facts.declarations[p as usize].kind.is_type()))
                    .or_else(|| decl.container.as_ref().and_then(|c| self.types.get(c).copied()))?;
                Some(FlowFact::ImplicitSelf {
                    function: d,
                    param: name.to_string(),
                    class,
                    is_class: false,
                })
            }
            Receiver::ReceiverField(field) => {
                let list = syntax.def.child_by_field_name(field)?;
                let first = named_children(list).into_iter().next()?;
                let param = first
                    .child_by_field_name("name")
                    .map(|n| text(n, self.source).trim().to_string())?;
                let class = decl.container.as_ref().and_then(|c| self.types.get(c).copied())?;
                Some(FlowFact::ImplicitSelf {
                    function: d,
                    param,
                    class,
                    is_class: false,
                })
            }
        }
    }
}
