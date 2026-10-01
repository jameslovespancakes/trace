//! Statements: assignments and their targets, `with` targets, implicit data-model
//! operations.

use trace_core::facts::{BindTarget, Expr, FileFacts, FlowFact, ImplicitKind, ImplicitOp, Scope};
use trace_core::Language;
use tree_sitter::Node;

use super::{expr::YIELDED, library::LibraryOp, LCtx, Lowerer};
use crate::node::{has_direct_token, named_children, nth_named, pick, span, text};
use crate::python::{expr_context, ExprContext};

impl<'a, 't> Lowerer<'a, 't> {
    /// Facts a statement / expression node contributes in `ctx` (active scopes only).
    pub(super) fn statement(&self, node: Node<'t>, ctx: LCtx, facts: &mut FileFacts) {
        let kind = node.kind();
        if let Some(a) = self.spec.assignment(kind) {
            self.bind(node, a.first, a.second, ctx, facts);
        }
        if self.spec.returns.contains(&kind) {
            if let Scope::Decl(function) = ctx.scope {
                if let Some(value) = nth_named(node, 0) {
                    let value = self.single(value);
                    facts.flow.push(FlowFact::Return {
                        function,
                        value: self.lower(value),
                    });
                }
            }
        }
        if !self.lib && self.spec.language == Language::Python && ctx.class.is_none() {
            self.with_target(node, ctx, facts);
        }
        // `yield v` in a generator: the value is one of the generator's elements.
        if !self.lib && self.spec.yields.contains(&kind) {
            let delegating = self
                .spec
                .delegating_yields
                .iter()
                .any(|y| y.kind == kind && has_direct_token(node, y.token));
            if let (Scope::Decl(function), false, Some(value)) = (ctx.scope, delegating, nth_named(node, 0)) {
                let value = self.single(value);
                facts.flow.push(FlowFact::Bind {
                    target: BindTarget::Var {
                        scope: Scope::Decl(function),
                        name: YIELDED.to_string(),
                    },
                    value: self.lower(value),
                    scope: ctx.scope,
                });
            }
        }
        if self.spec.call_shape(kind).is_some() {
            facts.flow.push(FlowFact::Eval {
                scope: ctx.scope,
                call: self.lower(node),
            });
        }
        if self.spec.implicit_ops {
            self.implicit(node, ctx, facts);
        }
        if self.lib {
            self.library_statement(node, ctx, facts);
        }
    }

    /// `with e as x` (Python language rule): `x` receives `e.__enter__()` (`await
    /// e.__aenter__()` in `async with`). The synthetic call is zero-width at the target name
    /// (no syntax call, no library knowledge, never a candidate site).
    fn with_target(&self, node: Node<'t>, ctx: LCtx, facts: &mut FileFacts) {
        let Some(w) = self.spec.with_items.iter().find(|w| w.kind == node.kind()) else {
            return;
        };
        let Some(value) = pick(node, w.first).filter(|v| v.kind() == "as_pattern") else {
            return;
        };
        let (Some(expr), Some(alias)) = (nth_named(value, 0), value.child_by_field_name("alias")) else {
            return;
        };
        let target = if self.spec.is_identifier(alias.kind()) {
            alias
        } else {
            match nth_named(alias, 0) {
                Some(t) if self.spec.is_identifier(t.kind()) => t,
                _ => return,
            }
        };
        let asynchronous = node
            .parent()
            .and_then(|clause| clause.parent())
            .is_some_and(|statement| has_direct_token(statement, "async"));
        let at = trace_core::model::ByteSpan::new(target.start_byte() as u32, target.start_byte() as u32);
        let enter = Expr::Call {
            func: Box::new(Expr::Attr {
                object: Box::new(self.lower(expr)),
                attr: if asynchronous { "__aenter__" } else { "__enter__" }.to_string(),
                attr_span: at,
                span: at,
            }),
            func_span: at,
            args: Vec::new(),
            kwargs: Vec::new(),
            span: at,
            is_new: false,
        };
        let value = if asynchronous {
            Expr::Await(Box::new(enter))
        } else {
            enter
        };
        facts.flow.push(FlowFact::Bind {
            target: BindTarget::Var {
                scope: ctx.scope,
                name: text(target, self.source).trim().to_string(),
            },
            value,
            scope: ctx.scope,
        });
    }

    /// A one-element expression list stands for its element.
    pub(super) fn single(&self, node: Node<'t>) -> Node<'t> {
        if self.spec.lists.contains(&node.kind()) {
            let items = named_children(node);
            if items.len() == 1 {
                return items[0];
            }
        }
        node
    }

    fn bind(&self, node: Node<'t>, left: &str, right: &str, ctx: LCtx, facts: &mut FileFacts) {
        let (Some(target), Some(mut value)) = (pick(node, left), pick(node, right)) else {
            return;
        };
        if target.id() == value.id() {
            return;
        }
        // Chained assignment `a = b = v`: every target receives the innermost value.
        for _ in 0..16 {
            match self.spec.assignment(value.kind()).and_then(|a| pick(value, a.second)) {
                Some(inner) => value = inner,
                None => break,
            }
        }
        let targets = if self.spec.lists.contains(&target.kind()) {
            named_children(target)
        } else {
            vec![target]
        };
        let values = if self.spec.lists.contains(&value.kind()) {
            named_children(value)
        } else {
            vec![value]
        };
        if targets.len() > 1 && values.len() != targets.len() {
            // Library mode: destructuring one value binds every target to it
            // (element-insensitive: `cb, args = self._ready.popleft()`).
            if self.lib && values.len() == 1 {
                for t in &targets {
                    self.bind_target(*t, values[0], ctx, facts);
                }
            }
            return;
        }
        for (i, t) in targets.iter().enumerate() {
            let v = if targets.len() > 1 {
                values[i]
            } else {
                self.single(value)
            };
            self.bind_target(*t, v, ctx, facts);
        }
    }

    pub(super) fn bind_target(&self, target: Node<'t>, value: Node<'t>, ctx: LCtx, facts: &mut FileFacts) {
        let target = crate::extract::unwrap_node(self.spec, target);
        let kind = target.kind();
        if (self.spec.is_name_like(kind) && target.named_child_count() == 0) || self.spec.is_identifier(kind)
        {
            let name = text(target, self.source).trim().to_string();
            if name.is_empty() {
                return;
            }
            let expr = self.lower(value);
            match ctx.class {
                Some(class) => {
                    facts.flow.push(FlowFact::Bind {
                        target: BindTarget::Member {
                            class,
                            name: name.clone(),
                        },
                        value: expr.clone(),
                        scope: ctx.scope,
                    });
                    facts.flow.push(FlowFact::Bind {
                        target: BindTarget::Field { name },
                        value: expr,
                        scope: ctx.scope,
                    });
                }
                None => facts.flow.push(FlowFact::Bind {
                    target: BindTarget::Var {
                        scope: ctx.scope,
                        name,
                    },
                    value: expr,
                    scope: ctx.scope,
                }),
            }
        } else if let Some(m) = self.spec.member(kind) {
            let (Some(object), Some(property)) =
                (pick(target, m.object_field), pick(target, m.property_field))
            else {
                return;
            };
            if property.named_child_count() != 0 {
                return;
            }
            let name = text(property, self.source).trim().to_string();
            let expr = self.lower(value);
            facts.flow.push(FlowFact::Bind {
                target: BindTarget::Field { name: name.clone() },
                value: expr.clone(),
                scope: ctx.scope,
            });
            facts.flow.push(FlowFact::Bind {
                target: BindTarget::FieldOf {
                    object: self.lower(object),
                    name,
                },
                value: expr,
                scope: ctx.scope,
            });
        } else if let Some(s) = self.spec.subscript(kind) {
            if self.spec.implicit_ops {
                if let Some(object) = pick(target, s.first) {
                    self.push_implicit(facts, ctx, ImplicitKind::SubscriptStore, object, target);
                }
            }
            // `o[k] = v` with `k` a known string (literal, or each element of a literal list
            // it iterates) is the member store `o.k = v` for every such key.
            if !self.lib && self.spec.dynamic_members {
                if let Some(object) = pick(target, s.first) {
                    let key = target.child_by_field_name("index").or_else(|| {
                        named_children(target)
                            .into_iter()
                            .rev()
                            .find(|c| c.id() != object.id())
                    });
                    if let Some(keys) = key.and_then(|k| self.literal_keys(k)) {
                        let expr = self.lower(value);
                        let object = self.lower(object);
                        for name in keys {
                            facts.flow.push(FlowFact::Bind {
                                target: BindTarget::Field { name: name.clone() },
                                value: expr.clone(),
                                scope: ctx.scope,
                            });
                            facts.flow.push(FlowFact::Bind {
                                target: BindTarget::FieldOf {
                                    object: object.clone(),
                                    name,
                                },
                                value: expr.clone(),
                                scope: ctx.scope,
                            });
                        }
                    }
                }
            }
            if self.lib {
                if let Some(object) = pick(target, s.first) {
                    let key = named_children(target)
                        .into_iter()
                        .rev()
                        .find(|c| c.id() != object.id())
                        .map(|k| self.lower(k))
                        .unwrap_or(Expr::Opaque);
                    self.ops.borrow_mut().push(LibraryOp::IndexStore {
                        scope: ctx.scope,
                        object: self.lower(object),
                        key,
                        value: self.lower(value),
                    });
                }
            }
        }
    }

    /// Python data-model operations (flow.py `implicit`).
    pub(super) fn implicit(&self, node: Node<'t>, ctx: LCtx, facts: &mut FileFacts) {
        let kind = node.kind();
        if let Some(s) = self.spec.subscript(kind) {
            let op = match expr_context(node) {
                ExprContext::Load => Some(ImplicitKind::SubscriptLoad),
                ExprContext::Del => Some(ImplicitKind::SubscriptDelete),
                ExprContext::Store => None,
            };
            if let (Some(op), Some(object)) = (op, pick(node, s.first)) {
                self.push_implicit(facts, ctx, op, object, node);
            }
        }
        if self.spec.member(kind).is_some() && expr_context(node) == ExprContext::Load {
            self.push_implicit(facts, ctx, ImplicitKind::DescriptorGet, node, node);
        }
        if let Some(w) = self.spec.with_items.iter().find(|w| w.kind == kind) {
            if let Some(mut value) = pick(node, w.first) {
                if value.kind() == "as_pattern" {
                    if let Some(inner) = nth_named(value, 0) {
                        value = inner;
                    }
                }
                self.push_implicit(facts, ctx, ImplicitKind::WithEnter, value, value);
            }
        }
        for f in self.spec.for_loops {
            if f.kind == kind {
                if let Some(iterable) = pick(node, f.iterable) {
                    self.push_implicit(facts, ctx, ImplicitKind::Iterate, iterable, iterable);
                }
            }
        }
    }

    fn push_implicit(
        &self,
        facts: &mut FileFacts,
        ctx: LCtx,
        kind: ImplicitKind,
        subject: Node<'t>,
        at: Node<'t>,
    ) {
        let at_span = span(at);
        facts.implicit.push(ImplicitOp {
            scope: ctx.scope,
            kind,
            subject: self.lower(subject),
            span: at_span,
            line: self.lines.line1(at_span.start),
        });
    }
}
