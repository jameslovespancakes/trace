//! Identity guards (Python `is` / `is not`) that narrow a bare-name callee.
//!
//! A call `f(...)` runs only where every enclosing condition has the truth value of the
//! branch containing it. When such a condition implies `f is not E` — `if f is E: ... else:
//! f()`, `if f is not E: f()`, `f is E or f()`, `f() if f is not E else x`, or an earlier
//! `if f is E: return` in the same block — the call can never invoke the object `E` denotes.
//! Conditions are read from the call up to the enclosing function, lambda or class; only
//! plain identity comparisons, `not`, `and` (true branch) and `or` (false branch) are
//! understood, anything else implies nothing. Whether the name can be rebound between guard
//! and call is decided by the caller (`crate::detail`: parameters never rebound only).

use tree_sitter::Node;

use crate::node::{named_children, text};

/// Scope boundaries: conditions outside them do not guard the call.
const BOUNDARIES: [&str; 4] = ["function_definition", "lambda", "class_definition", "module"];
/// Statements after which the rest of a block does not run.
const EXITS: [&str; 4] = ["return_statement", "raise_statement", "continue_statement", "break_statement"];

/// Expressions `name` is known not to be identical to wherever `node` is evaluated.
pub(crate) fn not_identical<'t>(node: Node<'t>, name: &str, source: &[u8]) -> Vec<Node<'t>> {
    let mut out = Vec::new();
    let mut child = node;
    while let Some(parent) = child.parent() {
        if BOUNDARIES.contains(&parent.kind()) {
            break;
        }
        let mut guard = |cond: Option<Node<'t>>, truth: bool| {
            if let Some(cond) = cond {
                implied(cond, truth, name, source, &mut out);
            }
        };
        match parent.kind() {
            "if_statement" | "elif_clause" if is_field(parent, "consequence", child) => {
                guard(parent.child_by_field_name("condition"), true);
            }
            _ => {}
        }
        match parent.kind() {
            // Inside `elif` / `else`: every earlier condition of the chain was false.
            "elif_clause" | "else_clause" => {
                if let Some(chain) = parent.parent().filter(|p| p.kind() == "if_statement") {
                    guard(chain.child_by_field_name("condition"), false);
                    let mut cursor = chain.walk();
                    for alt in chain.children_by_field_name("alternative", &mut cursor) {
                        if alt.id() == parent.id() {
                            break;
                        }
                        guard(alt.child_by_field_name("condition"), false);
                    }
                }
            }
            "conditional_expression" => {
                let parts = named_children(parent);
                if let [consequence, condition, alternative] = parts.as_slice() {
                    if consequence.id() == child.id() {
                        guard(Some(*condition), true);
                    } else if alternative.id() == child.id() {
                        guard(Some(*condition), false);
                    }
                }
            }
            "boolean_operator" if is_field(parent, "right", child) => {
                let op = parent
                    .child_by_field_name("operator")
                    .map(|o| text(o, source).trim().to_string());
                match op.as_deref() {
                    Some("and") => guard(parent.child_by_field_name("left"), true),
                    Some("or") => guard(parent.child_by_field_name("left"), false),
                    _ => {}
                }
            }
            "block" => {
                // Earlier `if <cond>: <... exit>` statements without alternatives.
                let mut sibling = child.prev_named_sibling();
                while let Some(s) = sibling {
                    if s.kind() == "if_statement"
                        && s.child_by_field_name("alternative").is_none()
                        && s.child_by_field_name("consequence")
                            .and_then(|b| b.named_child(b.named_child_count().saturating_sub(1) as u32))
                            .is_some_and(|last| EXITS.contains(&last.kind()))
                    {
                        guard(s.child_by_field_name("condition"), false);
                    }
                    sibling = s.prev_named_sibling();
                }
            }
            _ => {}
        }
        child = parent;
    }
    out
}

fn is_field(parent: Node<'_>, field: &str, child: Node<'_>) -> bool {
    parent
        .child_by_field_name(field)
        .is_some_and(|c| c.id() == child.id())
}

/// Collect `E` for every `name is not E` implied by `cond` having truth value `truth`.
fn implied<'t>(cond: Node<'t>, truth: bool, name: &str, source: &[u8], out: &mut Vec<Node<'t>>) {
    match cond.kind() {
        "parenthesized_expression" => {
            if let [inner] = named_children(cond).as_slice() {
                implied(*inner, truth, name, source, out);
            }
        }
        "not_operator" => {
            if let Some(arg) = cond.child_by_field_name("argument") {
                implied(arg, !truth, name, source, out);
            }
        }
        "boolean_operator" => {
            let op = cond
                .child_by_field_name("operator")
                .map(|o| text(o, source).trim().to_string());
            let both = matches!((op.as_deref(), truth), (Some("and"), true) | (Some("or"), false));
            if both {
                for side in ["left", "right"] {
                    if let Some(s) = cond.child_by_field_name(side) {
                        implied(s, truth, name, source, out);
                    }
                }
            }
        }
        "comparison_operator" => {
            let operands = named_children(cond);
            let [left, right] = operands.as_slice() else {
                return;
            };
            let mut cursor = cond.walk();
            let ops: Vec<String> = cond
                .children(&mut cursor)
                .filter(|c| !c.is_named())
                .map(|c| text(c, source).split_whitespace().collect::<Vec<_>>().join(" "))
                .collect();
            let is_not = match ops.join(" ").as_str() {
                "is" => false,
                "is not" => true,
                _ => return,
            };
            if is_not != truth {
                return;
            }
            let is_name = |n: &Node<'_>| n.kind() == "identifier" && text(*n, source).trim() == name;
            if is_name(left) {
                out.push(*right);
            } else if is_name(right) {
                out.push(*left);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
#[path = "../tests/unit/guards.rs"]
mod tests;
