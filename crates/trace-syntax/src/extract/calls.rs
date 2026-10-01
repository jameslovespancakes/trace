//! References, call sites, conversions, callback arguments and assignments, plus the node
//! helpers they share with lowering (receivers, activations, decorators).

use trace_core::facts::{
    Activation, ArgSlot, Assignment, AssignmentKind, CallSite, CallbackArg, FileFacts, Reference,
};
use trace_core::model::ByteSpan;
use tree_sitter::Node;

use super::{query::CallCapture, Ctx, Store, WalkCall, Walker};
use crate::names::Binding;
use crate::node::{has_direct_token, is_pick, named_children, pick, slice, span, text};
use crate::spec::SyntaxSpec;

impl<'a, 't> Walker<'a, 't> {
    /// Non-call uses of names (SPEC §6.3 `RefKind`): reads, store targets (`write`,
    /// attribute targets included), uncalled arguments, decorator expressions, re-export
    /// list entries and type positions. Import bindings are recorded by `record_import`.
    pub(super) fn reference(&mut self, node: Node<'t>, ctx: &Ctx, facts: &mut FileFacts) {
        use trace_core::facts::RefKind;
        if self.skip_ids.contains(&node.id()) || ctx.label || ctx.in_import {
            return;
        }
        let kind = node.kind();
        let write = ctx.binding
            && !ctx.keyword_name
            && (ctx.store == Store::Write || (ctx.store == Store::Declare && ctx.attr_property));
        let typed = ctx.in_type || self.type_ref_ids.contains(&node.id());
        if ctx.binding && !write && !typed {
            return;
        }
        let type_name = self.spec.type_names.contains(&kind) && node.named_child_count() == 0;
        if ctx.attr_property {
            if node.named_child_count() > 0 || !self.spec.is_name_like(kind) {
                return;
            }
        } else if !self.spec.is_identifier(kind) && !type_name {
            return;
        }
        if self.callee_ids.contains(&node.id()) && !ctx.in_decorator {
            return;
        }
        let name = text(node, self.source);
        if name.trim().is_empty() {
            return;
        }
        if !ctx.attr_property && self.spec.is_identifier(kind) {
            self.bare_refs.push((facts.references.len(), ctx.scope));
        }
        let ref_kind = if ctx.export {
            RefKind::Export
        } else if write {
            RefKind::Write
        } else if ctx.in_decorator {
            RefKind::Decorator
        } else if self.argument_ids.contains(&node.id()) {
            RefKind::Argument
        } else if typed || type_name {
            RefKind::Type
        } else {
            RefKind::Read
        };
        facts.references.push(Reference {
            span: span(node),
            name: name.into_owned(),
            owner: ctx.owner,
            in_decorator: ctx.in_decorator,
            local: false,
            kind: ref_kind,
        });
    }

    pub(super) fn call_site(&mut self, cap: &CallCapture<'t>, ctx: &Ctx, facts: &mut FileFacts) {
        let call = cap.call;
        let shape = self.spec.call_shape(call.kind());
        let callee = cap
            .callee
            .or_else(|| shape.and_then(|s| pick(call, s.function_field)));
        let Some(callee) = callee else {
            return;
        };
        // A call that is itself a declared name is not a call.
        if self.skip_ids.contains(&callee.id()) {
            return;
        }
        // Calling a type by grammar converts a value and runs no function (C++ `int(x)`,
        // Go `(*T)(x)`): no call site; a named type is a `type` reference.
        if let Some(type_name) = self.conversion(callee, ctx, facts) {
            if let Some(n) = type_name {
                self.type_ref_ids.insert(n.id());
            }
            return;
        }
        let receiver_node = shape
            .filter(|s| !s.receiver_field.is_empty())
            .and_then(|s| pick(call, s.receiver_field));
        let callee_span = match receiver_node {
            Some(r) => ByteSpan::new(r.start_byte() as u32, callee.end_byte() as u32),
            None => span(callee),
        };
        let callee_text =
            slice(self.source, callee_span.start as usize, callee_span.end as usize).into_owned();
        let (member, receiver) = member_and_receiver(self.spec, self.source, callee, receiver_node);
        let args = shape.and_then(|s| pick(call, s.arguments_field));
        let arg_count = args.map_or(0, |a| {
            if self.spec.generator_expressions.contains(&a.kind()) {
                1
            } else {
                named_children(a)
                    .iter()
                    .filter(|c| !self.spec.separators.contains(&c.kind()))
                    .count() as u32
            }
        });
        // A call of the function named by a literal argument (`send(:m)`, `do.call("f")`).
        let by_name = args.and_then(|a| self.name_call_argument(member.as_deref(), a));
        let split_receiver = receiver_node.and(receiver.clone());
        facts.calls.push(CallSite {
            owner: ctx.owner,
            lexical_owner: ctx.lexical,
            span: span(call),
            callee_span,
            callee: callee_text.clone(),
            member,
            receiver,
            line: self.lines.line1(call.start_byte() as u32),
            activation: activation(self.spec, call),
            is_new: cap.is_new || shape.is_some_and(|s| s.is_new),
            arg_count,
        });
        self.call_index.insert(call.id(), facts.calls.len() as u32 - 1);
        self.calls.push(WalkCall {
            callee,
            receiver: receiver_node,
            arguments: args,
            scope: ctx.scope,
        });
        if let Some(args) = args {
            if !self.spec.generator_expressions.contains(&args.kind()) {
                let named = by_name.as_ref().map(|(n, _, _)| n.id());
                self.callbacks(args, callee_span, &callee_text, ctx.owner, named, facts);
            }
        }
        if let Some((name_node, name, name_span)) = by_name {
            // The named function is called, not referenced.
            self.callee_ids.insert(name_node.id());
            facts.calls.push(CallSite {
                owner: ctx.owner,
                lexical_owner: ctx.lexical,
                span: span(call),
                callee_span: name_span,
                callee: name.clone(),
                member: Some(name),
                receiver: split_receiver,
                line: self.lines.line1(call.start_byte() as u32),
                activation: activation(self.spec, call),
                is_new: false,
                arg_count: arg_count.saturating_sub(1),
            });
            self.calls.push(WalkCall {
                callee: name_node,
                receiver: receiver_node,
                arguments: None,
                scope: ctx.scope,
            });
        }
    }

    /// A call whose callee is a type by grammar: `Some(type name node)` of a conversion
    /// (`Some(None)` for built-in types, which name nothing of the repository), `None` for
    /// a call (SPEC §6.3). Go `(*T)(x)` converts unless the operand's root name is a local
    /// value of the function (then it is a pointer to a function, called).
    fn conversion(&self, callee: Node<'t>, ctx: &Ctx, facts: &FileFacts) -> Option<Option<Node<'t>>> {
        if self.spec.type_callees.contains(&callee.kind()) {
            return Some(None);
        }
        if self.spec.pointer_conversions.is_empty() || !crate::spec::type_call_is_conversion(self.language) {
            return None;
        }
        // A parenthesized dereference: `(*T)`, `(*pkg.T)`, `(*T[int])`.
        let inner = self.spec.unwrap_pick(callee.kind()).and_then(|p| pick(callee, p))?;
        let form = self
            .spec
            .pointer_conversions
            .iter()
            .find(|f| f.kind == inner.kind())?;
        if !has_direct_token(inner, form.second) {
            return None;
        }
        let mut base = pick(inner, form.first)?;
        for _ in 0..8 {
            match self.spec.subscript(base.kind()) {
                Some(s) => base = pick(base, s.first)?,
                None => break,
            }
        }
        let name = match self.spec.member(base.kind()) {
            Some(m) => pick(base, m.property_field)?,
            None => base,
        };
        let mut root = base;
        for _ in 0..8 {
            match self.spec.member(root.kind()) {
                Some(m) => root = pick(root, m.object_field)?,
                None => break,
            }
        }
        if !self.spec.is_identifier(root.kind()) || root.named_child_count() != 0 {
            return None;
        }
        let root_name = text(root, self.source);
        match self.names.binding(root_name.trim(), ctx.scope, &facts.declarations) {
            Binding::Parameter | Binding::Variable => None,
            Binding::Other => Some(Some(name)),
        }
    }

    /// The literal argument naming the called function of a call by name
    /// ([`crate::spec::NameCall`]): its name node, the name and the name's span (a string's
    /// quotes excluded).
    fn name_call_argument(
        &self,
        member: Option<&str>,
        args: Node<'t>,
    ) -> Option<(Node<'t>, String, ByteSpan)> {
        let member = member?;
        let rule = self.spec.name_calls.iter().find(|n| n.member == member)?;
        let arg = crate::detail::argument_nodes(self.spec, args, self.source)
            .into_iter()
            .find(|a| match &a.slot {
                ArgSlot::Positional { index, exact: true } => *index == rule.position,
                ArgSlot::Keyword(k) => !rule.keyword.is_empty() && k == rule.keyword,
                _ => false,
            })?;
        let value = arg.value;
        if !rule.kinds.contains(&value.kind()) {
            return None;
        }
        let (node, name) = if self.spec.is_identifier(value.kind()) && value.named_child_count() == 0 {
            (value, text(value, self.source).trim().to_string())
        } else {
            // A string literal without interpolation: its only content.
            let content =
                value
                    .child_by_field_name("content")
                    .or_else(|| match named_children(value).as_slice() {
                        [only] if only.kind().ends_with("content") => Some(*only),
                        _ => None,
                    })?;
            (content, text(content, self.source).into_owned())
        };
        if name.is_empty() || name.contains(char::is_whitespace) {
            return None;
        }
        let end = node.end_byte() as u32;
        let start = end.saturating_sub(name.len() as u32).max(node.start_byte() as u32);
        Some((node, name, ByteSpan::new(start, end)))
    }

    fn callbacks(
        &mut self,
        args: Node<'t>,
        callee_span: ByteSpan,
        callee: &str,
        owner: Option<u32>,
        called_by_name: Option<usize>,
        facts: &mut FileFacts,
    ) {
        for a in crate::detail::argument_nodes(self.spec, args, self.source) {
            // The argument naming the function of a call by name is a call, not a callback.
            if called_by_name.is_some_and(|id| {
                id == a.value.id() || a.value.child_by_field_name("content").is_some_and(|c| c.id() == id)
            }) {
                continue;
            }
            // Positional index among the call's arguments (exact positions only) or keyword
            // name.
            let (index, keyword) = match &a.slot {
                ArgSlot::Positional { index, exact: true } => (Some(*index), None),
                ArgSlot::Keyword(k) => (None, Some(k.clone())),
                _ => (None, None),
            };
            // Forms matched on the argument itself first, then on its unwrapped value.
            let (value, found) = match self.callback_form(a.node) {
                Some(found) => (a.node, Some(found)),
                None => {
                    let value = unwrap_node(self.spec, a.value);
                    (value, self.callback_name(value))
                }
            };
            let Some((arg_node, name)) = found else {
                continue;
            };
            if name.trim().is_empty() {
                continue;
            }
            let arg_span = span(arg_node);
            self.argument_ids.insert(arg_node.id());
            facts.callbacks.push(CallbackArg {
                call_callee_span: callee_span,
                callee: callee.to_string(),
                arg_span,
                argument: text(value, self.source).into_owned(),
                name,
                owner,
                index,
                keyword,
            });
        }
    }

    /// The node naming the function an argument value passes, and that name: a plain name,
    /// the property of a member access, or a language callback form.
    fn callback_name(&self, value: Node<'t>) -> Option<(Node<'t>, String)> {
        let kind = value.kind();
        if self.spec.is_identifier(kind) {
            return Some((value, text(value, self.source).into_owned()));
        }
        if let Some(m) = self.spec.member(kind) {
            // Scala `x.y` evaluates the parameterless member `y`: its value is passed, not a
            // function, unless the object is a placeholder (`_.name` is a function).
            if self.spec.member_values_are_calls
                && !pick(value, m.object_field).is_some_and(|o| self.spec.placeholders.contains(&o.kind()))
            {
                return None;
            }
            return match pick(value, m.property_field) {
                Some(p) if p.named_child_count() == 0 => Some((p, text(p, self.source).into_owned())),
                _ => None,
            };
        }
        self.callback_form(value)
    }

    /// A [`crate::spec::CallbackForm`] of the language matching `node`: the name node and
    /// the name.
    fn callback_form(&self, node: Node<'t>) -> Option<(Node<'t>, String)> {
        self.spec
            .callback_forms
            .iter()
            .filter(|f| f.kind == node.kind() && named_children(node).len() >= f.min_named)
            .find_map(|f| {
                let (guard, wanted) = f.guard;
                if !guard.is_empty()
                    && pick(node, guard).is_none_or(|g| text(g, self.source).trim() != wanted)
                {
                    return None;
                }
                let name = pick(node, f.name).filter(|n| f.name_kinds.contains(&n.kind()))?;
                let spelled = text(name, self.source);
                let trimmed = spelled.trim().to_string();
                (!trimmed.is_empty()).then_some((name, trimmed))
            })
    }

    pub(super) fn assignment(&mut self, node: Node<'t>, ctx: &Ctx, facts: &mut FileFacts) {
        self.module_data(node, ctx, facts);
        let kind = node.kind();
        if let Some(a) = self.spec.assignment(kind) {
            if let (Some(left), Some(right)) = (pick(node, a.first), pick(node, a.second)) {
                // A dotted-name value (`run = functools.partial`) is a qualified-name candidate.
                let value = unwrap_node(self.spec, right);
                if self.spec.is_identifier(value.kind()) || self.spec.member(value.kind()).is_some() {
                    self.chains.push((value, ctx.scope));
                }
                if left.id() != right.id() {
                    let targets = if self.spec.lists.contains(&left.kind()) {
                        named_children(left)
                    } else {
                        vec![left]
                    };
                    for target in targets {
                        let target = unwrap_node(self.spec, target);
                        let entry = if self.spec.is_name_like(target.kind())
                            || self.spec.self_kinds.contains(&target.kind())
                        {
                            Some((text(target, self.source).into_owned(), AssignmentKind::Name))
                        } else if let Some(m) = self.spec.member(target.kind()) {
                            pick(target, m.property_field)
                                .filter(|p| p.named_child_count() == 0)
                                .map(|p| (text(p, self.source).into_owned(), AssignmentKind::Attribute))
                        } else {
                            None
                        };
                        if let Some((name, kind)) = entry {
                            facts.assignments.push(Assignment {
                                target: name,
                                kind,
                                span: span(node),
                                line: self.lines.line1(node.start_byte() as u32),
                            });
                        }
                    }
                }
            }
        }
        if let Some(k) = self.spec.keyword_arguments.iter().find(|k| k.kind == kind) {
            if let Some(name) = pick(node, k.first) {
                facts.assignments.push(Assignment {
                    target: text(name, self.source).into_owned(),
                    kind: AssignmentKind::Keyword,
                    span: span(node),
                    line: self.lines.line1(node.start_byte() as u32),
                });
            }
        }
    }
}

/// Texts of a decorator / attribute node (outermost first). Structural: attribute lists are
/// split into their attributes; `@name(args)` keeps `name(args)`; Python/TS decorators keep
/// their expression; Rust `#[attr]` keeps `attr`.
pub(crate) fn decorator_texts(node: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    let kind = node.kind();
    let children = named_children(node);
    if (kind.ends_with("_list") || kind.ends_with("_group")) && !children.is_empty() {
        for child in children {
            if !child.kind().ends_with("specifier") {
                decorator_texts(child, source, out);
            }
        }
        return;
    }
    let value = if let Some(name) = node.child_by_field_name("name") {
        slice(source, name.start_byte(), node.end_byte()).into_owned()
    } else if let Some(first) = children.first() {
        slice(source, first.start_byte(), node.end_byte()).into_owned()
    } else {
        text(node, source).into_owned()
    };
    let value = value.trim();
    let value = value
        .strip_suffix(']')
        .filter(|_| kind == "attribute_item")
        .unwrap_or(value);
    if !value.is_empty() {
        out.push(value.trim().to_string());
    }
}

pub(super) fn only_whitespace(source: &[u8], start: u32, end: u32) -> bool {
    let (start, end) = (start as usize, end as usize);
    if start > end || end > source.len() {
        return false;
    }
    source[start..end]
        .iter()
        .all(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'\x0c'))
}

/// Strip transparent wrappers (parentheses, casts, generic instantiation).
pub(crate) fn unwrap_node<'t>(spec: &SyntaxSpec, node: Node<'t>) -> Node<'t> {
    let mut current = node;
    for _ in 0..16 {
        match spec.unwrap_pick(current.kind()).and_then(|p| pick(current, p)) {
            Some(inner) => current = inner,
            None => break,
        }
    }
    current
}

/// Member name and receiver identifier of a callee.
pub(crate) fn member_and_receiver(
    spec: &SyntaxSpec,
    source: &[u8],
    callee: Node<'_>,
    receiver: Option<Node<'_>>,
) -> (Option<String>, Option<String>) {
    let callee = unwrap_node(spec, callee);
    let leaf_name = |n: Node<'_>| -> Option<String> {
        (n.named_child_count() == 0 && (spec.is_name_like(n.kind()) || !n.is_named()))
            .then(|| text(n, source).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    if let Some(r) = receiver {
        return (leaf_name(callee), receiver_name(spec, source, r));
    }
    if spec.is_name_like(callee.kind()) {
        return (leaf_name(callee), None);
    }
    if let Some(m) = spec.member(callee.kind()) {
        let member = pick(callee, m.property_field).and_then(|p| {
            (p.named_child_count() == 0)
                .then(|| text(p, source).trim().to_string())
                .filter(|s| !s.is_empty())
        });
        let receiver = pick(callee, m.object_field).and_then(|o| receiver_name(spec, source, o));
        return (member, receiver);
    }
    (None, None)
}

/// Identifier immediately before the member (`store` in `self.store.save`), never a
/// `self`/`this` receiver.
fn receiver_name(spec: &SyntaxSpec, source: &[u8], object: Node<'_>) -> Option<String> {
    let object = unwrap_node(spec, object);
    let kind = object.kind();
    if spec.self_kinds.contains(&kind) {
        return None;
    }
    let name = if spec.is_name_like(kind) && object.named_child_count() == 0 {
        text(object, source).trim().to_string()
    } else if let Some(m) = spec.member(kind) {
        let p = pick(object, m.property_field)?;
        if p.named_child_count() != 0 {
            return None;
        }
        text(p, source).trim().to_string()
    } else if spec.is_identifier(kind) {
        // Identifier kinds with inner structure (PHP `$name`).
        text(object, source).trim().to_string()
    } else {
        return None;
    };
    (!name.is_empty() && !spec.self_names.contains(&name.as_str())).then_some(name)
}

/// How the call's result is consumed by its syntactic parent.
pub(crate) fn activation(spec: &SyntaxSpec, call: Node<'_>) -> Activation {
    let mut node = call;
    let mut parent = call.parent();
    while let Some(p) = parent {
        if spec.unwrap_pick(p.kind()).is_some() {
            node = p;
            parent = p.parent();
        } else {
            break;
        }
    }
    let Some(p) = parent else {
        return Activation::Plain;
    };
    let kind = p.kind();
    if spec.awaits.contains(&kind) {
        return Activation::Await;
    }
    for f in spec.for_loops {
        if f.kind == kind
            && is_pick(p, f.iterable, node)
            && (f.token.is_empty() || has_direct_token(p, f.token))
        {
            return Activation::Iterate;
        }
    }
    if spec
        .delegating_yields
        .iter()
        .any(|y| y.kind == kind && has_direct_token(p, y.token))
    {
        return Activation::Iterate;
    }
    if spec.iterate_parents.contains(&kind) {
        return Activation::Iterate;
    }
    Activation::Plain
}
