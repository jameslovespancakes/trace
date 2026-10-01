//! Rendering: condition texts (negated where the site runs on the false branch), option-like
//! guards, one-line call texts and argument views.

use trace_core::Language;
use tree_sitter::Node;

use super::{conditions::short_circuit, ArgumentView, Cx, Guard, BOOLEANS, FLIP, PARENS, SCAN_LIMIT};
use crate::node::{named_children, pick, text};
use crate::spec::SyntaxSpec;

/// The field name under which `child` (a direct child) hangs from `parent`.
pub(super) fn field_of(parent: Node<'_>, child: Node<'_>) -> Option<&'static str> {
    let mut cursor = parent.walk();
    if !cursor.goto_first_child() {
        return None;
    }
    loop {
        if cursor.node().id() == child.id() {
            return cursor.field_name();
        }
        if !cursor.goto_next_sibling() {
            return None;
        }
    }
}

pub(super) fn field_children<'t>(node: Node<'t>, field: &str) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    let found: Vec<Node<'t>> = node.children_by_field_name(field, &mut cursor).collect();
    found
}

pub(super) fn encloses(outer: Node<'_>, inner: Node<'_>) -> bool {
    outer.start_byte() <= inner.start_byte() && inner.end_byte() <= outer.end_byte()
}

/// The two operands of a binary node: `left` / `lhs` / `left_operand` and `right` / `rhs` /
/// `right_operand` fields, else the first and last named children.
pub(super) fn operands(node: Node<'_>) -> Option<(Node<'_>, Node<'_>)> {
    let field = |names: [&str; 3]| names.iter().find_map(|f| node.child_by_field_name(f));
    let parts = named_children(node);
    let left = field(["left", "lhs", "left_operand"]).or_else(|| parts.first().copied())?;
    let right = field(["right", "rhs", "right_operand"]).or_else(|| parts.last().copied())?;
    (left.id() != right.id()).then_some((left, right))
}

/// The operator of a binary node: its `operator` / `op` field, else the children between
/// the operands joined by a space (`is not`, `not in`).
pub(super) fn operator_text(node: Node<'_>, left: Node<'_>, right: Node<'_>, source: &[u8]) -> String {
    if let Some(op) = node
        .child_by_field_name("operator")
        .or_else(|| node.child_by_field_name("op"))
    {
        return one_line(&text(op, source));
    }
    let mut cursor = node.walk();
    let tokens: Vec<String> = node
        .children(&mut cursor)
        .filter(|c| !c.is_extra() && c.start_byte() >= left.end_byte() && c.end_byte() <= right.start_byte())
        .map(|c| text(c, source).trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    tokens.join(" ")
}

/// Whether a node compares or combines two operands.
fn is_binary(kind: &str) -> bool {
    BOOLEANS.contains(&kind)
        || ["comparison", "equality", "relational", "binary", "infix"]
            .iter()
            .any(|k| kind.contains(k))
}

/// The operand of a negation (`!x`, `not x`): exactly an operator and one operand.
fn negated_operand<'t>(cond: Node<'t>, source: &[u8]) -> Option<Node<'t>> {
    let mut cursor = cond.walk();
    let children: Vec<Node<'t>> = cond.children(&mut cursor).filter(|c| !c.is_extra()).collect();
    let [op, operand] = children.as_slice() else { return None };
    let spelled = text(*op, source);
    (matches!(spelled.trim(), "!" | "not") && operand.is_named()).then_some(*operand)
}

pub(super) fn unwrap_parens(mut node: Node<'_>) -> Node<'_> {
    for _ in 0..8 {
        if !PARENS.contains(&node.kind()) {
            break;
        }
        let inner = node
            .child_by_field_name("value")
            .or_else(|| (node.named_child_count() == 1).then(|| node.named_child(0)).flatten());
        match inner {
            Some(i) => node = i,
            None => break,
        }
    }
    node
}

/// A condition as displayed: its text on one line, negated when `truthy` is false.
pub(super) fn render(cond: Node<'_>, truthy: bool, language: Language, source: &[u8]) -> String {
    let cond = unwrap_parens(cond);
    // Rust `if let P = v`.
    if cond.kind() == "let_condition" {
        if let (Some(pattern), Some(value)) =
            (cond.child_by_field_name("pattern"), cond.child_by_field_name("value"))
        {
            let t =
                format!("{} matches {}", one_line(&text(value, source)), one_line(&text(pattern, source)));
            return if truthy { t } else { format!("!({t})") };
        }
    }
    let t = one_line(&text(cond, source));
    if truthy || t.is_empty() {
        return t;
    }
    // `!x` / `not x` -> `x`.
    if let Some(operand) = negated_operand(cond, source) {
        return one_line(&text(unwrap_parens(operand), source));
    }
    // `a == b` -> `a != b`.
    if is_binary(cond.kind()) {
        if let Some((left, right)) = operands(cond) {
            let op = operator_text(cond, left, right, source);
            if let Some((_, flipped)) = FLIP.iter().find(|(o, _)| *o == op) {
                return format!(
                    "{} {flipped} {}",
                    one_line(&text(left, source)),
                    one_line(&text(right, source))
                );
            }
        }
    }
    negate_text(t, false, language)
}

/// `t` negated by prefix when `truthy` is false (module docs).
pub(super) fn negate_text(t: String, truthy: bool, language: Language) -> String {
    if truthy || t.is_empty() {
        return t;
    }
    let simple = !t.contains(char::is_whitespace);
    if language == Language::Bash {
        return format!("! {t}");
    }
    let not = matches!(language, Language::Python | Language::Haskell);
    match (not, simple) {
        (true, true) => format!("not {t}"),
        (true, false) => format!("not ({t})"),
        (false, true) => format!("!{t}"),
        (false, false) => format!("!({t})"),
    }
}

/// Option-like member names tested directly by `cond` (module docs).
pub(super) fn guards(cx: &Cx<'_>, cond: Node<'_>, truthy: bool, out: &mut Vec<Guard>) {
    guards_at(cx, cond, truthy, out, 0);
}

fn guards_at(cx: &Cx<'_>, cond: Node<'_>, truthy: bool, out: &mut Vec<Guard>, depth: usize) {
    if depth > 16 {
        return;
    }
    let cond = unwrap_parens(cond);
    let kind = cond.kind();
    if let Some(shape) = cx.spec.member_access.iter().find(|m| m.kind == kind) {
        if let Some(name) = pick(cond, shape.property_field) {
            let name = text(name, cx.source).trim().to_string();
            if !name.is_empty() && !out.iter().any(|g| g.name == name) {
                out.push(Guard { name, truthy });
            }
        }
        return;
    }
    if let Some(operand) = negated_operand(cond, cx.source) {
        guards_at(cx, operand, !truthy, out, depth + 1);
        return;
    }
    if BOOLEANS.contains(&kind) && (kind != "list" || cx.language == Language::Bash) {
        let Some((left, right)) = operands(cond) else { return };
        // `a && b` true: both true; `a || b` false: both false. The other cases fix
        // neither operand; their members still switch the site, with the same polarity.
        if short_circuit(kind, &operator_text(cond, left, right, cx.source)).is_some() {
            guards_at(cx, left, truthy, out, depth + 1);
            guards_at(cx, right, truthy, out, depth + 1);
        }
    }
}

/// Text with every line break (and the indentation after it) collapsed: to nothing after an
/// opening bracket or before a closing one (a trailing comma before it is dropped), else to
/// one space.
pub(crate) fn one_line(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut chars = t.trim().chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            let mut run = String::from(c);
            while let Some(&n) = chars.peek() {
                if !n.is_whitespace() {
                    break;
                }
                run.push(n);
                chars.next();
            }
            if !run.contains(['\n', '\r']) {
                out.push_str(&run);
                continue;
            }
            let next = chars.peek().copied();
            let closing = matches!(next, Some(')' | ']' | '}'));
            if closing && out.ends_with(',') {
                out.pop();
            }
            let opening = out.ends_with(['(', '[', '{']);
            if !opening && !closing {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The call on one line without comments.
pub(super) fn joined(call: Node<'_>, source: &[u8]) -> String {
    let (start, end) = (call.start_byte(), call.end_byte().min(source.len()));
    let mut comments = Vec::new();
    let mut stack = vec![call];
    while let Some(n) = stack.pop() {
        if n.is_extra() {
            comments.push((n.start_byte(), n.end_byte()));
            continue;
        }
        let mut cursor = n.walk();
        stack.extend(n.children(&mut cursor));
    }
    comments.sort_unstable();
    let mut bytes = Vec::with_capacity(end - start);
    let mut at = start;
    for (s, e) in comments {
        if s > at {
            bytes.extend_from_slice(&source[at..s.min(end)]);
        }
        at = at.max(e);
    }
    if at < end {
        bytes.extend_from_slice(&source[at..end]);
    }
    one_line(&String::from_utf8_lossy(&bytes))
}

/// The argument value nodes of `call`: its argument list, else (no argument-list field)
/// the `argument` children along the application spine (Bash words, Haskell `f a b`), else
/// the named children other than the callee.
fn argument_values<'t>(spec: &SyntaxSpec, call: Node<'t>) -> Vec<Node<'t>> {
    let Some(shape) = spec.call_shape(call.kind()) else { return Vec::new() };
    if !shape.arguments_field.is_empty() {
        // Named separators (R `comma`) are no arguments.
        return pick(call, shape.arguments_field)
            .map(named_children)
            .unwrap_or_default()
            .into_iter()
            .filter(|a| !spec.separators.contains(&a.kind()))
            .collect();
    }
    let mut groups: Vec<Vec<Node<'t>>> = Vec::new();
    let mut current = call;
    for _ in 0..SCAN_LIMIT {
        groups.push(
            field_children(current, "argument")
                .into_iter()
                .filter(|a| a.is_named() && !a.is_extra())
                .collect(),
        );
        match pick(current, shape.function_field) {
            Some(f) if f.kind() == current.kind() => current = f,
            _ => break,
        }
    }
    groups.reverse();
    let spine: Vec<Node<'t>> = groups.into_iter().flatten().collect();
    if !spine.is_empty() {
        return spine;
    }
    let callee = pick(call, shape.function_field).map(|c| c.id());
    named_children(call)
        .into_iter()
        .filter(|c| Some(c.id()) != callee)
        .collect()
}

/// Arguments of `call` (positional index / keyword, one-line text).
pub(super) fn arguments(spec: &SyntaxSpec, call: Node<'_>, source: &[u8]) -> Vec<ArgumentView> {
    let mut out = Vec::new();
    let mut position = 0u32;
    for arg in argument_values(spec, call) {
        let mut value = arg;
        let mut keyword = None;
        if let Some(pair) = spec.keyword_arguments.iter().find(|k| k.kind == arg.kind()) {
            keyword = pick(arg, pair.first).map(|n| text(n, source).trim().to_string());
            value = pick(arg, pair.second).unwrap_or(arg);
        } else if spec.argument_wrappers.contains(&arg.kind()) {
            keyword = crate::detail::wrapper_keyword(arg).map(|n| text(n, source).trim().to_string());
            if let Some(last) = named_children(arg).last() {
                value = *last;
            }
        }
        let text = one_line(&text(value, source));
        if keyword.is_some() {
            out.push(ArgumentView {
                position: None,
                keyword,
                text,
            });
        } else {
            out.push(ArgumentView {
                position: Some(position),
                keyword: None,
                text,
            });
            position += 1;
        }
    }
    out
}
