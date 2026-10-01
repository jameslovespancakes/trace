//! Conditions governing a site: the enclosing call spine, branches (`if` / `else`, early
//! exits), short-circuit operands and ternaries.

use trace_core::Language;
use tree_sitter::Node;

use super::{
    cases::case_conditions, cases::haskell_guards, cases::is_case, render::encloses, render::field_children,
    render::field_of, render::operands, render::operator_text, Cond, Cx, Part, ALT_CLAUSES, AND_OPS,
    BOOLEANS, BRANCHES, EXITS, OR_OPS, SCAN_LIMIT, TERNARIES,
};
use crate::node::{named_children, pick};
use crate::spec::SyntaxSpec;

/// The smallest call whose callee (from the receiver start when split) contains `point`.
pub(super) fn smallest_call<'t>(spec: &SyntaxSpec, root: Node<'t>, point: usize) -> Option<Node<'t>> {
    let mut best: Option<(usize, Node<'t>)> = None;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.start_byte() > point || node.end_byte() <= point {
            continue;
        }
        if let Some(shape) = spec.call_shape(node.kind()) {
            if let Some(callee) = pick(node, shape.function_field) {
                let start = if shape.receiver_field.is_empty() {
                    callee.start_byte()
                } else {
                    pick(node, shape.receiver_field).map_or(callee.start_byte(), |r| r.start_byte())
                };
                if start <= point && point < callee.end_byte() {
                    let size = node.end_byte() - node.start_byte();
                    if best.is_none_or(|(s, _)| size < s) {
                        best = Some((size, node));
                    }
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    best.map(|(_, n)| n)
}

/// The outermost call of a curried application spine (Haskell `f a b` is
/// `apply(apply(f, a), b)`): the call whose function is this call, repeatedly.
pub(super) fn spine_top<'t>(spec: &SyntaxSpec, call: Node<'t>) -> Node<'t> {
    let Some(shape) = spec.call_shape(call.kind()) else { return call };
    if !shape.arguments_field.is_empty() {
        return call;
    }
    let mut top = call;
    for _ in 0..SCAN_LIMIT {
        let Some(parent) = top.parent() else { break };
        if parent.kind() != call.kind()
            || pick(parent, shape.function_field).is_none_or(|f| f.id() != top.id())
        {
            break;
        }
        top = parent;
    }
    top
}

/// Conditions from `node` up to its function, innermost first.
pub(super) fn collect_conditions<'t>(cx: &Cx<'_>, node: Node<'t>, out: &mut Vec<Cond<'t>>) {
    let mut child = node;
    for _ in 0..SCAN_LIMIT * 4 {
        let Some(parent) = child.parent() else { break };
        if cx.spec.is_lazy(parent.kind()) {
            break;
        }
        // Earlier statements of the same block that leave it (innermost = nearest first).
        let mut sibling = child.prev_named_sibling();
        let mut seen = 0usize;
        while let Some(s) = sibling {
            if let Some(found) = leaving_branch(s) {
                out.push(found);
            }
            seen += 1;
            if seen >= SCAN_LIMIT {
                break;
            }
            sibling = s.prev_named_sibling();
        }
        branch(cx, parent, child, out);
        child = parent;
    }
}

/// The condition of an earlier sibling `s` that leaves the block, for the code after it.
fn leaving_branch(s: Node<'_>) -> Option<Cond<'_>> {
    // `if c { return; }` used as a statement (Rust expression statements).
    let s = if s.kind() == "expression_statement" && s.named_child_count() == 1 {
        s.named_child(0)?
    } else {
        s
    };
    let kind = s.kind();
    // Rust `let P = v else { return };`: the code after runs when `v` matches `P`.
    if kind == "let_declaration" {
        let alternative = s.child_by_field_name("alternative")?;
        if !leaves(alternative) {
            return None;
        }
        let pattern = s.child_by_field_name("pattern")?;
        let value = s.child_by_field_name("value")?;
        return Some(Cond::label(value, &[pattern], true));
    }
    if !BRANCHES.contains(&kind) {
        return None;
    }
    let cond = condition_part(s)?;
    if has_alternative(s) {
        return None;
    }
    let body = consequence(s, cond)?;
    if !leaves(body) {
        return None;
    }
    Some(Cond::test(cond, false))
}

/// The condition of an if-like node: its `condition` field (every named part), Haskell
/// `if`, else the named children before `then` (Bash `elif`) or before the `consequence`
/// field.
fn condition_part(n: Node<'_>) -> Option<Part<'_>> {
    let named: Vec<Node<'_>> = field_children(n, "condition")
        .into_iter()
        .filter(|c| c.is_named() && !c.is_extra())
        .collect();
    if let (Some(first), Some(last)) = (named.first(), named.last()) {
        return Some(Part {
            first: *first,
            last: *last,
        });
    }
    if let Some(c) = n.child_by_field_name("if") {
        return Some(Part::one(c));
    }
    let limit = token(n, "then")
        .map(|t| t.start_byte())
        .or_else(|| n.child_by_field_name("consequence").map(|c| c.start_byte()))?;
    let named: Vec<Node<'_>> = named_children(n)
        .into_iter()
        .filter(|c| c.end_byte() <= limit)
        .collect();
    Some(Part {
        first: *named.first()?,
        last: *named.last()?,
    })
}

/// The consequence of an if-like node: its `consequence` / `body` field, else the first
/// unfielded named child after the condition that is no `else`.
fn consequence<'t>(n: Node<'t>, cond: Part<'t>) -> Option<Node<'t>> {
    if let Some(b) = n
        .child_by_field_name("consequence")
        .or_else(|| n.child_by_field_name("body"))
    {
        return Some(b);
    }
    let mut cursor = n.walk();
    if !cursor.goto_first_child() {
        return None;
    }
    loop {
        let c = cursor.node();
        if c.is_named()
            && !c.is_extra()
            && cursor.field_name().is_none()
            && c.start_byte() >= cond.last.end_byte()
            && !is_else_keyword(c)
            && !ALT_CLAUSES.contains(&c.kind())
        {
            return Some(c);
        }
        if !cursor.goto_next_sibling() {
            return None;
        }
    }
}

fn is_else_keyword(n: Node<'_>) -> bool {
    n.kind() == "else" && n.named_child_count() == 0
}

/// The first anonymous child token of `kind`.
fn token<'t>(n: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cursor = n.walk();
    let found = n.children(&mut cursor).find(|c| !c.is_named() && c.kind() == kind);
    found
}

fn has_alternative(n: Node<'_>) -> bool {
    if n.child_by_field_name("alternative").is_some() {
        return true;
    }
    let mut cursor = n.walk();
    let found = n
        .children(&mut cursor)
        .any(|c| is_else_keyword(c) || ALT_CLAUSES.contains(&c.kind()));
    found
}

/// Alternatives of an if-like node in source order: its `alternative` field(s), else its
/// alternative clause children.
pub(super) fn alternatives(n: Node<'_>) -> Vec<Node<'_>> {
    let fielded = field_children(n, "alternative");
    if !fielded.is_empty() {
        return fielded;
    }
    named_children(n)
        .into_iter()
        .filter(|c| ALT_CLAUSES.contains(&c.kind()))
        .collect()
}

/// Whether `child` follows an `else` keyword of `parent`.
fn after_else(parent: Node<'_>, child: Node<'_>) -> bool {
    let mut cursor = parent.walk();
    let found = parent
        .children(&mut cursor)
        .any(|c| is_else_keyword(c) && c.end_byte() <= child.start_byte());
    found
}

/// Whether a branch body always leaves the enclosing block: it is an exit statement or its
/// last statement is one.
fn leaves(body: Node<'_>) -> bool {
    leaves_at(body, 0)
}

fn leaves_at(body: Node<'_>, depth: usize) -> bool {
    if EXITS.contains(&body.kind()) {
        return true;
    }
    if depth > 8 {
        return false;
    }
    let mut last = None;
    let mut cursor = body.walk();
    for c in body.named_children(&mut cursor) {
        if !c.is_extra() {
            last = Some(c);
        }
    }
    match last {
        Some(l) if EXITS.contains(&l.kind()) => true,
        // `if c { return }` wrapped once more (statement lists inside blocks).
        Some(l)
            if l.named_child_count() > 0
                && ["block", "statement_list", "statements", "body", "compound_statement"]
                    .iter()
                    .any(|k| l.kind().contains(k)) =>
        {
            leaves_at(l, depth + 1)
        }
        Some(l) if l.kind() == "expression_statement" => {
            l.named_child(0).is_some_and(|e| EXITS.contains(&e.kind()))
        }
        _ => false,
    }
}

/// The condition `parent` puts on `child` (if any).
fn branch<'t>(cx: &Cx<'_>, parent: Node<'t>, child: Node<'t>, out: &mut Vec<Cond<'t>>) {
    let kind = parent.kind();
    if BRANCHES.contains(&kind) {
        let Some(cond) = condition_part(parent) else { return };
        // The condition itself and an initializer (`if v, ok := f(); ok {`) run first,
        // whatever the condition is.
        if cond.encloses(child) || matches!(field_of(parent, child), Some("initializer" | "init")) {
            return;
        }
        let alternatives = alternatives(parent);
        if let Some(pos) = alternatives.iter().position(|a| encloses(*a, child)) {
            // Earlier `elif` conditions of the chain, then the chain's own condition (innermost
            // first).
            for a in alternatives[..pos].iter().rev() {
                if let Some(c) = condition_part(*a) {
                    out.push(Cond::test(c, false));
                }
            }
            out.push(Cond::test(cond, false));
        } else if after_else(parent, child) {
            out.push(Cond::test(cond, false));
        } else {
            out.push(Cond::test(cond, true));
        }
        return;
    }
    if TERNARIES.contains(&kind) {
        ternary(parent, child, out);
        return;
    }
    if BOOLEANS.contains(&kind) {
        if kind == "list" && cx.language != Language::Bash {
            return;
        }
        let Some((left, right)) = operands(parent) else { return };
        if right.id() != child.id() {
            return;
        }
        // `a && site` runs when `a` holds, `a || site` when it does not.
        if let Some(and) = short_circuit(kind, &operator_text(parent, left, right, cx.source)) {
            out.push(Cond::node(left, and));
        }
        return;
    }
    if cx.language == Language::Haskell && kind == "match" {
        haskell_guards(cx, parent, child, out);
        return;
    }
    if is_case(cx.language, kind) {
        case_conditions(cx, parent, child, out);
    }
}

/// `Some(true)` for a short-circuit and, `Some(false)` for a short-circuit or. Word
/// operators count where they are keywords (not for user-defined infix functions).
pub(super) fn short_circuit(kind: &str, op: &str) -> Option<bool> {
    let op = match kind {
        "conjunction_expression" | "logical_and_expression" => "&&",
        "disjunction_expression" | "logical_or_expression" => "||",
        _ => op,
    };
    let words = !matches!(kind, "infix_expression" | "infix");
    if AND_OPS.contains(&op) && (words || op == "&&") {
        Some(true)
    } else if OR_OPS.contains(&op) && (words || op == "||") {
        Some(false)
    } else {
        None
    }
}

fn ternary<'t>(parent: Node<'t>, child: Node<'t>, out: &mut Vec<Cond<'t>>) {
    let fielded_cond = parent
        .child_by_field_name("condition")
        .or_else(|| parent.child_by_field_name("if"));
    // `c ? a : b` without fields: the condition is the first unfielded named child.
    let cond = fielded_cond.or_else(|| {
        parent.child_by_field_name("consequence")?;
        let mut cursor = parent.walk();
        if !cursor.goto_first_child() {
            return None;
        }
        loop {
            let c = cursor.node();
            if c.is_named() && !c.is_extra() && cursor.field_name().is_none() {
                return Some(c);
            }
            if !cursor.goto_next_sibling() {
                return None;
            }
        }
    });
    if let Some(cond) = cond {
        if encloses(cond, child) {
            return;
        }
        let in_alternative = ["alternative", "if_false", "else"]
            .iter()
            .any(|f| field_children(parent, f).iter().any(|a| encloses(*a, child)));
        out.push(Cond::node(cond, !in_alternative));
    } else {
        // Python `a if c else b`: named children (consequence, condition, alternative).
        let parts = named_children(parent);
        if parts.len() == 3 {
            if parts[0].id() == child.id() {
                out.push(Cond::node(parts[1], true));
            } else if parts[2].id() == child.id() {
                out.push(Cond::node(parts[1], false));
            }
        }
    }
}
