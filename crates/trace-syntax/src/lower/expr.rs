//! Expression lowering: syntax nodes to [`trace_core::facts::Expr`] (calls, literals,
//! allocations, object literals, generators) and the synthetic declaration map.

use std::collections::HashMap;

use trace_core::facts::Expr;
use tree_sitter::Node;

use super::{
    library::lower_library_expr, library::string_content, library::string_literal, library::HOLE_KINDS,
    library::KEYWORD_SPREAD, library::LITERAL_PREFIX, library::POSITIONAL_SPREAD, library::STRING_KINDS,
};
use crate::extract::DeclSyntax;
use crate::node::{named_children, nth_named, pick, span, text};
use crate::spec::SyntaxSpec;
use crate::MAX_EXPR_DEPTH;

/// Synthetic declaration index by syntax node id (lambdas, generator expressions).
pub(crate) type Synthetic = HashMap<usize, u32>;

/// Synthetic declarations of a declaration-syntax list (same indices as the facts).
pub(crate) fn synthetic_map(spec: &SyntaxSpec, decls: &[DeclSyntax<'_>]) -> Synthetic {
    decls
        .iter()
        .enumerate()
        .filter(|(_, d)| is_function_value(spec, d))
        .map(|(i, d)| (d.def.id(), i as u32))
        .collect()
}

/// Whether a declaration is a function value written as an expression: every synthetic
/// (anonymous) callable, and a named function expression (`forEach(function each(n) {..})`),
/// whose `Lambda` must designate it like an anonymous one.
pub(super) fn is_function_value(spec: &SyntaxSpec, d: &DeclSyntax<'_>) -> bool {
    d.synthetic.is_some() || spec.is_lazy(d.def.kind())
}

/// Lower one expression node of the extracted tree.
pub(crate) fn lower_in(spec: &SyntaxSpec, node: Node<'_>, source: &[u8], synth: &Synthetic) -> Expr {
    lower_expr_in(spec, node, source, 0, synth, Mode::Plain)
}

/// What an expression is lowered for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    /// Call details: the plain IR.
    Plain,
    /// Value-flow facts of the index: also object / table literals are values
    /// ([`OBJECT_CALLEE`]).
    Facts,
    /// Library mode (see [`lower_library`]).
    Library,
}

impl Mode {
    pub(super) fn lib(self) -> bool {
        self == Mode::Library
    }
}

/// Callee name of an object literal in the index flow facts (`{a: f}`):
/// `Call { Name("<object>"), kwargs: [(key, value)] }`, a new plain object whose members are
/// the keyed values.
pub const OBJECT_CALLEE: &str = "<object>";
/// Variable of a generator function bound to every value it yields (`yield v`):
/// `Bind { Var(function, "<yielded>"), v }`.
pub const YIELDED: &str = "<yielded>";
/// Keyed entries of one object literal that are lowered (larger literals keep the first ones).
pub(super) const MAX_OBJECT_FIELDS: usize = 256;
/// Number / boolean / null literal kinds of the grammars (the flow facts lower them, like
/// string literals, to the bare [`LITERAL_PREFIX`] name: a value without objects).
const SCALAR_LITERALS: &[&str] = &[
    "number",
    "integer",
    "float",
    "true",
    "false",
    "null",
    "undefined",
    "none",
    "nil",
    "integer_literal",
    "float_literal",
    "decimal_integer_literal",
    "decimal_floating_point_literal",
    "boolean_literal",
    "null_literal",
    "char_literal",
    "character_literal",
];
/// Keys of one literal list expanded by a computed member store (longer lists are not
/// expanded: the store stays unknown).
pub(super) const MAX_LITERAL_KEYS: usize = 64;
/// Syntax nodes visited when looking up the literal list bound to a name in one file.
pub(super) const MAX_LIST_WALK: usize = 200_000;
/// Ancestors walked from a computed key to the loop / callback binding it.
pub(super) const MAX_KEY_ANCESTORS: usize = 64;

/// `mode`: what the expression is lowered for ([`Mode`]).
pub(super) fn lower_expr_in(
    spec: &SyntaxSpec,
    node: Node<'_>,
    source: &[u8],
    depth: usize,
    synth: &Synthetic,
    mode: Mode,
) -> Expr {
    if depth > MAX_EXPR_DEPTH {
        return Expr::Opaque;
    }
    let kind = node.kind();
    if let Some(selector) = spec.unwrap_pick(kind) {
        return pick(node, selector)
            .map(|inner| lower_expr_in(spec, inner, source, depth + 1, synth, mode))
            .unwrap_or(Expr::Opaque);
    }
    if mode.lib() {
        if let Some(expr) = lower_library_expr(spec, node, source, depth, synth) {
            return expr;
        }
    }
    if mode == Mode::Facts {
        if spec.object_literals.contains(&kind) {
            return lower_object_literal(spec, node, source, depth, synth);
        }
        // Literals are values that hold no object; their text is left out so that editing a
        // literal never changes the facts other files see.
        if string_literal(spec, node, source).is_some() || SCALAR_LITERALS.contains(&kind) {
            return Expr::Name {
                name: LITERAL_PREFIX.to_string(),
                span: span(node),
            };
        }
    }
    if spec.is_identifier(kind) || spec.self_kinds.contains(&kind) {
        return Expr::Name {
            name: text(node, source).into_owned(),
            span: span(node),
        };
    }
    if let Some(m) = spec.member(kind) {
        let (Some(object), Some(property)) = (pick(node, m.object_field), pick(node, m.property_field))
        else {
            return Expr::Opaque;
        };
        return Expr::Attr {
            object: Box::new(lower_expr_in(spec, object, source, depth + 1, synth, mode)),
            attr: text(property, source).trim().to_string(),
            attr_span: span(property),
            span: span(node),
        };
    }
    if spec.generator_expressions.contains(&kind) {
        return lower_generator(spec, node, source, depth, synth, mode);
    }
    if spec.call_shape(kind).is_some() {
        return lower_call(spec, node, source, depth, synth, mode);
    }
    if let Some(lit) = spec.literal_allocations.iter().find(|l| l.kind == kind) {
        let mut expr = lower_literal_allocation(node, lit.first, source);
        // Library mode: the keyed fields of the literal (`&Server{Handler: h}`) are the
        // allocation's keyword arguments (derivation stores them into the fields).
        if mode.lib() {
            if let Expr::Call { kwargs, .. } = &mut expr {
                kwargs.extend(literal_fields(spec, node, source, depth, synth));
            }
        }
        return expr;
    }
    if let Some(shape) = spec.choices.iter().find(|c| c.kind == kind) {
        let operator_ok = shape.operators.is_empty() || {
            let mut cursor = node.walk();
            let found = node
                .children(&mut cursor)
                .any(|c| !c.is_named() && shape.operators.contains(&c.kind()));
            found
        };
        if operator_ok {
            let alternatives: Vec<Expr> = shape
                .alternatives
                .iter()
                .filter_map(|sel| pick(node, sel))
                .map(|alt| lower_expr_in(spec, alt, source, depth + 1, synth, mode))
                .collect();
            if !alternatives.is_empty() {
                return Expr::Choice(alternatives);
            }
        }
        return Expr::Opaque;
    }
    if spec.awaits.contains(&kind) {
        return nth_named(node, 0)
            .map(|inner| Expr::Await(Box::new(lower_expr_in(spec, inner, source, depth + 1, synth, mode))))
            .unwrap_or(Expr::Opaque);
    }
    if spec.is_lazy(kind) {
        return Expr::Lambda {
            span: span(node),
            function: synth.get(&node.id()).copied(),
        };
    }
    Expr::Opaque
}

/// `(e for x in it ...)` -> `Call { func: Lambda { function: <genexpr> }, args: [it] }`.
fn lower_generator(
    spec: &SyntaxSpec,
    node: Node<'_>,
    source: &[u8],
    depth: usize,
    synth: &Synthetic,
    mode: Mode,
) -> Expr {
    let Some(&function) = synth.get(&node.id()) else {
        return Expr::Opaque;
    };
    let clause = spec.generator_clause;
    let args = named_children(node)
        .into_iter()
        .find(|c| c.kind() == clause.kind)
        .and_then(|c| pick(c, clause.second))
        .map(|iterable| vec![lower_expr_in(spec, iterable, source, depth + 1, synth, mode)])
        .unwrap_or_default();
    let whole = span(node);
    Expr::Call {
        func: Box::new(Expr::Lambda {
            span: whole,
            function: Some(function),
        }),
        func_span: whole,
        args,
        kwargs: Vec::new(),
        span: whole,
        is_new: false,
    }
}

/// Type name of an allocation's type node: its text without generic arguments or a leading
/// namespace separator, when that is a (qualified) identifier.
fn allocated_type(node: Node<'_>, source: &[u8]) -> Option<String> {
    let owned = text(node, source);
    let full: &str = &owned;
    let name = full.split('<').next().unwrap_or(full).trim().trim_start_matches('\\');
    let ok = !name.is_empty()
        && name.starts_with(|c: char| c == '_' || c.is_alphabetic())
        && name
            .chars()
            .all(|c| c == '_' || c == '.' || c == '\\' || c == ':' || c.is_alphanumeric());
    ok.then(|| name.to_string())
}

/// Keyed elements of a literal allocation whose key is a field name (`T{Handler: h}`), as
/// `(field, value)` keyword arguments in library mode; positional elements are left out.
fn literal_fields(
    spec: &SyntaxSpec,
    node: Node<'_>,
    source: &[u8],
    depth: usize,
    synth: &Synthetic,
) -> Vec<(String, Expr)> {
    let Some(body) = node.child_by_field_name("body") else {
        return Vec::new();
    };
    pub(super) fn element(n: Node<'_>) -> Node<'_> {
        if n.kind() == "literal_element" {
            nth_named(n, 0).unwrap_or(n)
        } else {
            n
        }
    }
    let mut out = Vec::new();
    for pair in named_children(body)
        .into_iter()
        .filter(|c| c.kind() == "keyed_element")
    {
        let (Some(key), Some(value)) = (pair.child_by_field_name("key"), pair.child_by_field_name("value"))
        else {
            continue;
        };
        let key = element(key);
        if !spec.is_identifier(key.kind()) && key.kind() != "field_identifier" {
            continue;
        }
        let value = lower_expr_in(spec, element(value), source, depth + 1, synth, Mode::Library);
        out.push((text(key, source).trim().to_string(), value));
    }
    out
}

/// A literal allocation (`T{..}`, `pkg.T{..}`, `T[int]{..}`): `new T()` without arguments.
/// Anything but a (qualified) type name (slice, map, array, anonymous struct types) is
/// opaque.
fn lower_literal_allocation(node: Node<'_>, type_field: &str, source: &[u8]) -> Expr {
    let Some(mut ty) = pick(node, type_field) else {
        return Expr::Opaque;
    };
    if ty.kind() == "generic_type" {
        match ty.child_by_field_name("type") {
            Some(inner) => ty = inner,
            None => return Expr::Opaque,
        }
    }
    match allocated_type(ty, source) {
        Some(name) => Expr::Call {
            func: Box::new(Expr::Name { name, span: span(ty) }),
            func_span: span(ty),
            args: Vec::new(),
            kwargs: Vec::new(),
            span: span(node),
            is_new: true,
        },
        None => Expr::Opaque,
    }
}

fn lower_call(
    spec: &SyntaxSpec,
    node: Node<'_>,
    source: &[u8],
    depth: usize,
    synth: &Synthetic,
    mode: Mode,
) -> Expr {
    let lib = mode.lib();
    let Some(shape) = spec.call_shape(node.kind()) else {
        return Expr::Opaque;
    };
    let Some(callee) = pick(node, shape.function_field) else {
        return Expr::Opaque;
    };
    let receiver = if shape.receiver_field.is_empty() {
        None
    } else {
        pick(node, shape.receiver_field)
    };
    let (func, func_span) = match receiver {
        Some(r) => {
            let whole = trace_core::model::ByteSpan::new(r.start_byte() as u32, callee.end_byte() as u32);
            (
                Expr::Attr {
                    object: Box::new(lower_expr_in(spec, r, source, depth + 1, synth, mode)),
                    attr: text(callee, source).trim().to_string(),
                    attr_span: span(callee),
                    span: whole,
                },
                whole,
            )
        }
        None => (lower_expr_in(spec, callee, source, depth + 1, synth, mode), span(callee)),
    };
    // `new K(..)` names a type (Java `type_identifier`, PHP `name` / `qualified_name`,
    // generic `K<T>`): the allocation's class name, not a value expression.
    let func = match func {
        Expr::Opaque if shape.is_new => allocated_type(callee, source)
            .map(|name| Expr::Name {
                name,
                span: span(callee),
            })
            .unwrap_or(Expr::Opaque),
        other => other,
    };
    let lower = |n: Node<'_>| lower_expr_in(spec, n, source, depth + 1, synth, mode);
    let mut args = Vec::new();
    let mut kwargs = Vec::new();
    // Library mode: the position of the first positional spread (`f(a, *rest)`,
    // `f(...rest)`) and its value (None after a second spread: later positions unknown).
    let mut positional_spread: Option<(usize, Option<Node<'_>>)> = None;
    if let Some(list) = pick(node, shape.arguments_field) {
        if spec.generator_expressions.contains(&list.kind()) {
            args.push(lower(list));
        } else {
            for arg in named_children(list) {
                let kind = arg.kind();
                // Named separators (R `comma`) are no arguments: positions stay aligned with
                // the call details.
                if spec.separators.contains(&kind) {
                    continue;
                }
                // Library mode: `f(**kw)` passes every keyword the mapping holds (derivation
                // forwards them to the callee's keyword parameters).
                if lib && spec.keyword_spreads.contains(&kind) {
                    if let Some(value) = nth_named(arg, 0) {
                        kwargs.push((KEYWORD_SPREAD.to_string(), lower(value)));
                    }
                    continue;
                }
                if spec.spreads.contains(&kind) {
                    if lib {
                        positional_spread = match positional_spread {
                            None => Some((args.len(), nth_named(arg, 0))),
                            Some((at, _)) => Some((at, None)),
                        };
                    }
                    continue;
                }
                if let Some(k) = spec.keyword_arguments.iter().find(|k| k.kind == kind) {
                    if let (Some(name), Some(value)) = (pick(arg, k.first), pick(arg, k.second)) {
                        kwargs.push((text(name, source).trim().to_string(), lower(value)));
                    }
                    continue;
                }
                if spec.argument_wrappers.contains(&kind) {
                    // `name: value`, `name = value` (no name field).
                    if let Some(name) = crate::detail::wrapper_keyword(arg) {
                        if let Some(value) = nth_named(arg, -1).filter(|v| v.id() != name.id()) {
                            kwargs.push((text(name, source).trim().to_string(), lower(value)));
                        }
                    } else if let Some(value) = nth_named(arg, 0) {
                        if !spec.spreads.contains(&value.kind()) {
                            args.push(lower(value));
                        }
                    }
                    continue;
                }
                args.push(lower(arg));
            }
        }
    }
    // A spread after every positional argument passes the spread elements from its position
    // on (`kwargs: [("*", rest)]`). Positional arguments after a spread land at unknown
    // positions: they bind no parameter (never a guessed position).
    if let Some((at, value)) = positional_spread {
        match value {
            Some(v) if at == args.len() => kwargs.push((POSITIONAL_SPREAD.to_string(), lower(v))),
            _ => args.truncate(at),
        }
    }
    Expr::Call {
        func: Box::new(func),
        func_span,
        args,
        kwargs,
        span: span(node),
        is_new: shape.is_new,
    }
}

/// Object / table literal of the flow facts ([`OBJECT_CALLEE`]): keyed entries (`a: v`,
/// shorthand `a`, methods `m() {}`) become keyword arguments (at
/// most [`MAX_OBJECT_FIELDS`]); computed keys, spreads and positional entries are left out.
fn lower_object_literal(
    spec: &SyntaxSpec,
    node: Node<'_>,
    source: &[u8],
    depth: usize,
    synth: &Synthetic,
) -> Expr {
    let whole = span(node);
    let lower = |n: Node<'_>| lower_expr_in(spec, n, source, depth + 1, synth, Mode::Facts);
    let key_text = |key: Node<'_>| -> Option<String> {
        let k = key.kind();
        let s = if STRING_KINDS.contains(&k) {
            let holes = named_children(key)
                .iter()
                .any(|c| HOLE_KINDS.contains(&c.kind()) || spec.interpolations.contains(&c.kind()));
            if holes {
                return None;
            }
            string_content(key, source)
        } else if spec.is_identifier(k) || spec.name_kinds.contains(&k) || k == "number" {
            text(key, source).trim().to_string()
        } else {
            return None;
        };
        (!s.is_empty()).then_some(s)
    };
    let mut kwargs = Vec::new();
    for entry in named_children(node) {
        if kwargs.len() >= MAX_OBJECT_FIELDS {
            break;
        }
        let kind = entry.kind();
        let (key, value) = if spec.is_identifier(kind) && entry.named_child_count() == 0 {
            // `{ a }`: shorthand for `{ a: a }`.
            let name = text(entry, source).trim().to_string();
            (
                Some(name.clone()),
                Expr::Name {
                    name,
                    span: span(entry),
                },
            )
        } else if spec.is_lazy(kind) {
            // `{ m() { .. } }`: a method entry is its function value.
            (
                entry.child_by_field_name("name").and_then(key_text),
                Expr::Lambda {
                    span: span(entry),
                    function: synth.get(&entry.id()).copied(),
                },
            )
        } else {
            let Some(value) = entry.child_by_field_name("value") else {
                continue;
            };
            let key = entry
                .child_by_field_name("key")
                .or_else(|| entry.child_by_field_name("name"))
                .and_then(key_text);
            (key, lower(value))
        };
        if let Some(key) = key {
            kwargs.push((key, value));
        }
    }
    Expr::Call {
        func: Box::new(Expr::Name {
            name: OBJECT_CALLEE.to_string(),
            span: whole,
        }),
        func_span: whole,
        args: Vec::new(),
        kwargs,
        span: whole,
        is_new: false,
    }
}
