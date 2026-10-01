//! Per-call structure ([`CallDetail`]) and qualified names, built after the walk (when every
//! synthetic declaration and every binding of the file is known).
//!
//! * Arguments in source order with their [`ArgSlot`]: positional index (counting only
//!   positional arguments; `exact = false` after an unpacked `*args`/spread), keyword name
//!   (`keyword_argument`, named argument wrappers), `Unpack` / `UnpackKeywords`. A generator
//!   expression that is the whole argument list (`all(x for x in xs)`) is positional 0.
//!   Values are lowered to [`Expr`] (lambdas and generator expressions refer to their
//!   synthetic declarations).
//! * Receiver: the object of a member callee (`self.store` in `self.store.save()`), or the
//!   receiver child of split method calls (Java `obj.m()`), lowered.
//! * Callee path: the callee's dotted name resolved by [`crate::names::Names::resolve`].
//! * Qualified names: argument values, decorator expressions and assigned values that are
//!   dotted names resolving through imports/builtins.
//! * Identity guards ([`crate::guards`], languages with `identity_guards`): for a bare-name
//!   callee bound only as a parameter of its function (never rebound), the expressions the
//!   enclosing conditions prove it is not.

use trace_core::facts::{ArgSlot, Argument, CallDetail, FileFacts, QualifiedName};
use tree_sitter::Node;

use crate::extract::{unwrap_node, Collected};
use crate::lower::{lower_in, synthetic_map};
use crate::names::{chain, Binding};
use crate::node::{named_children, nth_named, pick, span, text};
use crate::spec::SyntaxSpec;

/// One argument node of a call's argument list.
pub(crate) struct ArgNode<'t> {
    /// Direct child of the argument list (the list itself for a generator-expression list).
    pub node: Node<'t>,
    pub slot: ArgSlot,
    /// The value expression (operand of unpacked arguments).
    pub value: Node<'t>,
}

/// Arguments of an argument-list node in source order.
pub(crate) fn argument_nodes<'t>(spec: &SyntaxSpec, list: Node<'t>, source: &[u8]) -> Vec<ArgNode<'t>> {
    if spec.generator_expressions.contains(&list.kind()) {
        return vec![ArgNode {
            node: list,
            slot: ArgSlot::Positional {
                index: 0,
                exact: true,
            },
            value: list,
        }];
    }
    let mut out = Vec::new();
    let mut index = 0u32;
    let mut exact = true;
    for arg in named_children(list) {
        let kind = arg.kind();
        // Named separators (R `comma`) are no arguments.
        if spec.separators.contains(&kind) {
            continue;
        }
        let (slot, value) = if spec.spreads.contains(&kind) {
            let slot = if spec.keyword_spreads.contains(&kind) {
                ArgSlot::UnpackKeywords
            } else {
                ArgSlot::Unpack
            };
            (slot, nth_named(arg, 0))
        } else if let Some(k) = spec.keyword_arguments.iter().find(|k| k.kind == kind) {
            let name = pick(arg, k.first).map(|n| text(n, source).trim().to_string());
            match name.filter(|n| !n.is_empty()) {
                Some(name) => (ArgSlot::Keyword(name), pick(arg, k.second)),
                None => continue,
            }
        } else if spec.argument_wrappers.contains(&kind) {
            if let Some(name) = wrapper_keyword(arg) {
                let value = nth_named(arg, -1).filter(|v| v.id() != name.id());
                (ArgSlot::Keyword(text(name, source).trim().to_string()), value)
            } else {
                match nth_named(arg, 0) {
                    Some(inner) if spec.spreads.contains(&inner.kind()) => {
                        (ArgSlot::Unpack, nth_named(inner, 0))
                    }
                    inner => (ArgSlot::Positional { index, exact }, inner),
                }
            }
        } else {
            (ArgSlot::Positional { index, exact }, Some(arg))
        };
        match slot {
            ArgSlot::Positional { .. } => index += 1,
            ArgSlot::Unpack => exact = false,
            _ => {}
        }
        if let Some(value) = value {
            out.push(ArgNode {
                node: arg,
                slot,
                value,
            });
        }
    }
    out
}

/// The keyword of an argument wrapper: its `name` field, else (`name = value`) the named
/// child before its `=` token.
pub(crate) fn wrapper_keyword(arg: Node<'_>) -> Option<Node<'_>> {
    if let Some(name) = arg.child_by_field_name("name") {
        return Some(name);
    }
    let mut cursor = arg.walk();
    let eq = arg.children(&mut cursor).find(|c| !c.is_named() && c.kind() == "=")?;
    named_children(arg)
        .into_iter()
        .rev()
        .find(|c| c.end_byte() <= eq.start_byte())
}

/// Fill `facts.call_details` (aligned with `facts.calls`) and `facts.qualified_names`.
pub(crate) fn build(spec: &SyntaxSpec, source: &[u8], collected: &Collected<'_>, facts: &mut FileFacts) {
    let synth = synthetic_map(spec, &collected.decls);
    let decls = &facts.declarations;
    let resolve = |node: Node<'_>, scope: Option<u32>| -> Option<String> {
        chain(spec, node, source).and_then(|c| collected.names.resolve(spec, &c, scope, decls))
    };
    let mut qualified: Vec<QualifiedName> = Vec::new();
    let mut details: Vec<CallDetail> = Vec::with_capacity(collected.calls.len());
    for (i, call) in collected.calls.iter().enumerate() {
        let receiver_node = call.receiver.or_else(|| {
            let callee = unwrap_node(spec, call.callee);
            spec.member(callee.kind()).and_then(|m| pick(callee, m.object_field))
        });
        let receiver = receiver_node.map(|r| lower_in(spec, r, source, &synth));
        let callee_path = match call.receiver {
            Some(r) => chain(spec, r, source).and_then(|mut c| {
                let member = text(call.callee, source).trim().to_string();
                if member.is_empty() {
                    return None;
                }
                c.rest.push(member);
                collected.names.resolve(spec, &c, call.scope, decls)
            }),
            None => resolve(call.callee, call.scope),
        };
        let arguments = call
            .arguments
            .map(|list| argument_nodes(spec, list, source))
            .unwrap_or_default()
            .into_iter()
            .map(|a| {
                let value = unwrap_node(spec, a.value);
                if let Some(path) = resolve(value, call.scope) {
                    qualified.push(QualifiedName {
                        span: span(value),
                        path,
                    });
                }
                Argument {
                    slot: a.slot,
                    span: span(value),
                    value: lower_in(spec, value, source, &synth),
                    has_string: crate::lower::holds_string(value),
                }
            })
            .collect();
        let not_identical = if spec.identity_guards && call.receiver.is_none() {
            let callee = unwrap_node(spec, call.callee);
            let name = text(callee, source).trim().to_string();
            let bare = spec.is_identifier(callee.kind()) && callee.named_child_count() == 0;
            if bare && collected.names.binding(&name, call.scope, decls) == Binding::Parameter {
                crate::guards::not_identical(call.callee, &name, source)
                    .into_iter()
                    .map(|e| lower_in(spec, e, source, &synth))
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        details.push(CallDetail {
            call: i as u32,
            receiver,
            arguments,
            callee_path,
            not_identical,
        });
    }
    for &(node, scope) in &collected.chains {
        let value = unwrap_node(spec, node);
        if let Some(path) = resolve(value, scope) {
            qualified.push(QualifiedName {
                span: span(value),
                path,
            });
        }
    }
    qualified.sort_by_key(|q| (q.span.start, q.span.end));
    qualified.dedup_by(|a, b| a.span == b.span);
    facts.call_details = details;
    facts.qualified_names = qualified;
}

#[cfg(test)]
#[path = "../tests/unit/detail.rs"]
mod tests;
