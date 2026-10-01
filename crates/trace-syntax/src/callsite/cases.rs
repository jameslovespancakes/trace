//! Switch / match cases: case headers, labels, wildcards, fall-through and Haskell guards.

use trace_core::Language;
use tree_sitter::Node;

use super::{
    render::encloses, render::field_children, render::unwrap_parens, Cond, Cx, CASES, SCAN_LIMIT, SUBJECTS,
    SWITCHES, WILDCARDS,
};
use crate::node::{named_children, text};

/// Whether `kind` is a case clause in `language`.
pub(super) fn is_case(language: Language, kind: &str) -> bool {
    match kind {
        "case_statement" => language != Language::Bash,
        "alternative" => language == Language::Haskell,
        "match" => false,
        _ => CASES.contains(&kind),
    }
}

/// Whether `kind` is a switch owning case clauses in `language`.
fn is_switch(language: Language, kind: &str) -> bool {
    SWITCHES.contains(&kind) || (language == Language::Bash && kind == "case_statement")
}

/// The header of a case clause.
struct CaseParts<'t> {
    labels: Vec<Node<'t>>,
    /// Guard conditions with their polarity.
    guards: Vec<(Node<'t>, bool)>,
    /// A default / wildcard case (runs for any subject).
    default: bool,
    /// Children starting here are the case body.
    body_start: usize,
}

fn case_parts<'t>(clause: Node<'t>, source: &[u8]) -> CaseParts<'t> {
    let mut parts = CaseParts {
        labels: Vec::new(),
        guards: Vec::new(),
        default: false,
        body_start: clause.start_byte(),
    };
    let named_field = |field: &str| -> Vec<Node<'t>> {
        field_children(clause, field)
            .into_iter()
            .filter(|n| n.is_named() && !n.is_extra())
            .collect()
    };
    match clause.kind() {
        "default_case" | "switch_default" | "default_statement" | "match_default_expression" => {
            parts.default = true;
            return parts;
        }
        "expression_case" => {
            parts.labels = clause
                .child_by_field_name("value")
                .map(named_children)
                .unwrap_or_default();
        }
        "type_case" => parts.labels = named_field("type"),
        "case_statement" | "switch_case" if clause.child_by_field_name("value").is_some() => {
            parts.labels = named_field("value");
        }
        // C / C++ `default:` (Bash `case_statement` is a switch, never a clause).
        "case_statement" => {
            parts.default = true;
            return parts;
        }
        "case_item" => parts.labels = named_field("value"),
        "match_conditional_expression" => {
            parts.labels = clause
                .child_by_field_name("conditional_expressions")
                .map(named_children)
                .unwrap_or_default();
        }
        "match_arm" => {
            if let Some(pattern) = clause.child_by_field_name("pattern") {
                let guard = pattern.child_by_field_name("condition");
                parts.labels = named_children(pattern)
                    .into_iter()
                    .filter(|n| guard.is_none_or(|g| g.id() != n.id()))
                    .collect();
                if let Some(g) = guard {
                    parts.guards.push((g, true));
                }
            }
        }
        "case_clause" => {
            if let Some(pattern) = clause.child_by_field_name("pattern") {
                // Scala `case P if c =>`.
                parts.labels.push(pattern);
                for g in named_children(clause) {
                    if g.kind() == "guard" {
                        if let Some(c) = g.child_by_field_name("condition") {
                            parts.guards.push((c, true));
                        }
                    }
                }
            } else {
                // Python `case P if c:`; `case _:` has an empty pattern.
                for c in named_children(clause) {
                    if c.kind() == "case_pattern" {
                        if c.named_child_count() > 0 {
                            parts.labels.push(c);
                        } else {
                            parts.default = true;
                        }
                    }
                }
                if let Some(g) = clause.child_by_field_name("guard") {
                    if let Some(e) = named_children(g).first() {
                        parts.guards.push((*e, true));
                    }
                }
            }
        }
        "alternative" => {
            if let Some(p) = clause.child_by_field_name("pattern") {
                parts.labels.push(p);
            }
        }
        _ => scan_case(clause, &mut parts),
    }
    let header_end = parts
        .labels
        .iter()
        .chain(parts.guards.iter().map(|(g, _)| g))
        .map(|n| n.end_byte())
        .max();
    if let Some(end) = header_end {
        parts.body_start = parts.body_start.max(end);
    }
    let before = parts.labels.len();
    parts.labels.retain(|l| !is_wildcard(*l, source));
    if before > 0 && parts.labels.is_empty() {
        parts.default = true;
    }
    if parts.labels.is_empty() && parts.guards.is_empty() {
        parts.default = true;
    }
    parts
}

/// Case clauses whose header is separated from the body by a token (C# sections and arms,
/// Java groups and rules): the named children before
/// `:` / `=>` / `->` are labels; after `when` / `where` / `if` they are guards.
fn scan_case<'t>(clause: Node<'t>, parts: &mut CaseParts<'t>) {
    let mut in_guard = false;
    let mut after_separator = false;
    let mut cursor = clause.walk();
    let children: Vec<Node<'t>> = clause.children(&mut cursor).filter(|c| !c.is_extra()).collect();
    for c in children {
        let kind = c.kind();
        if !c.is_named() {
            match kind {
                ":" | "=>" | "->" => {
                    parts.body_start = c.end_byte();
                    after_separator = true;
                }
                "when" | "where" | "if" => in_guard = true,
                "," => in_guard = false,
                "default" | "else" => parts.default = true,
                _ => {}
            }
            continue;
        }
        // Java groups repeat `case x:` labels before their statements.
        if after_separator && kind != "switch_label" {
            break;
        }
        match kind {
            "default_keyword" => parts.default = true,
            "where_keyword" => in_guard = true,
            "label" | "modifiers" => {}
            "when_clause" | "guard" => {
                if let Some(e) = named_children(c).last() {
                    parts.guards.push((*e, true));
                }
            }
            "switch_label" => {
                let inner = named_children(c);
                if inner.is_empty() {
                    parts.default = true;
                }
                for n in inner {
                    if n.kind() == "guard" {
                        if let Some(e) = named_children(n).last() {
                            parts.guards.push((*e, true));
                        }
                    } else {
                        parts.labels.push(n);
                    }
                }
            }
            _ if in_guard => parts.guards.push((c, true)),
            _ => parts.labels.push(c),
        }
    }
}

/// A wildcard label (`_`, `*`, `default`), looking through single-child wrappers.
fn is_wildcard(label: Node<'_>, source: &[u8]) -> bool {
    let mut n = label;
    for _ in 0..4 {
        if WILDCARDS.contains(&n.kind()) {
            return true;
        }
        if n.named_child_count() != 1 {
            break;
        }
        match n.named_child(0) {
            Some(c) => n = c,
            None => break,
        }
    }
    n.named_child_count() == 0 && matches!(text(n, source).trim(), "_" | "*")
}

/// Whether a clause has no statement after its header.
fn empty_body(clause: Node<'_>, body_start: usize) -> bool {
    !named_children(clause).iter().any(|c| c.start_byte() >= body_start)
}

/// Conditions of a site in the body of case clause `clause`.
pub(super) fn case_conditions<'t>(cx: &Cx<'_>, clause: Node<'t>, child: Node<'t>, out: &mut Vec<Cond<'t>>) {
    let parts = case_parts(clause, cx.source);
    if child.start_byte() < parts.body_start {
        // The site is in the case's labels or guard.
        return;
    }
    for (g, truthy) in parts.guards.iter().rev() {
        out.push(Cond::node(*g, *truthy));
    }
    let mut labels = parts.labels;
    let mut default = parts.default;
    if cx.spec.cases_fall_through && !default {
        let mut prev = clause.prev_named_sibling();
        for _ in 0..SCAN_LIMIT {
            let Some(p) = prev else { break };
            if p.is_extra() {
                prev = p.prev_named_sibling();
                continue;
            }
            if !is_case(cx.language, p.kind()) {
                break;
            }
            let earlier = case_parts(p, cx.source);
            if !empty_body(p, earlier.body_start) {
                break;
            }
            default |= earlier.default;
            let mut merged = earlier.labels;
            merged.append(&mut labels);
            labels = merged;
            prev = p.prev_named_sibling();
        }
    }
    if default || labels.is_empty() {
        return;
    }
    // Clauses outside a switch (Scala partial functions `{ case p => }`) add nothing.
    let Some(switch) = switch_of(cx.language, clause) else { return };
    match subject_of(cx.language, switch) {
        Some(subject) => {
            // Bash labels are shell patterns, and Bash's own `==` is the pattern comparison
            // (`[[ $v == pat ]]`): they render with `==`, whatever node the label parses to.
            let matches = cx.language != Language::Bash
                && (matches!(
                    cx.language,
                    Language::Rust | Language::Python | Language::Scala | Language::Haskell
                ) || labels
                    .iter()
                    .any(|l| l.kind().ends_with("pattern") && l.kind() != "constant_pattern"));
            out.push(Cond::label(subject, &labels, matches));
        }
        None => {
            // A switch without a subject: the labels are conditions, and no earlier entry
            // held.
            out.push(Cond::any(&labels, true));
            let mut prev = clause.prev_named_sibling();
            for _ in 0..SCAN_LIMIT {
                let Some(p) = prev else { break };
                if is_case(cx.language, p.kind()) {
                    let earlier = case_parts(p, cx.source);
                    if !earlier.default && !earlier.labels.is_empty() {
                        out.push(Cond::any(&earlier.labels, false));
                    }
                }
                prev = p.prev_named_sibling();
            }
        }
    }
}

/// The switch owning `clause` (at most three levels up: clause lists, bodies).
fn switch_of<'t>(language: Language, clause: Node<'t>) -> Option<Node<'t>> {
    let mut up = clause.parent();
    for _ in 0..3 {
        let s = up?;
        if is_switch(language, s.kind()) {
            return Some(s);
        }
        up = s.parent();
    }
    None
}

/// The subject of a switch (`None` for a subject-less switch).
fn subject_of<'t>(language: Language, switch: Node<'t>) -> Option<Node<'t>> {
    for f in SUBJECTS {
        if let Some(n) = switch.child_by_field_name(f) {
            return Some(unwrap_parens(n));
        }
    }
    let children = named_children(switch);
    // C# `x switch { }`, Haskell `case x of`: the first named child unless it is already a
    // clause.
    let first = children.into_iter().next()?;
    let kind = first.kind();
    if is_case(language, kind) || kind == "alternatives" || kind.contains("block") || kind.contains("body") {
        return None;
    }
    Some(unwrap_parens(first))
}

/// Haskell guarded right-hand sides: `f x | c1 = a | c2 = b` (also inside `case`
/// alternatives): the right side of a match runs when its guards hold and every earlier
/// match's single guard did not. `otherwise` / `True` guards add nothing.
pub(super) fn haskell_guards<'t>(
    cx: &Cx<'_>,
    match_node: Node<'t>,
    child: Node<'t>,
    out: &mut Vec<Cond<'t>>,
) {
    let Some(guards) = match_node.child_by_field_name("guards") else { return };
    if encloses(guards, child) {
        return;
    }
    let own = haskell_guard_conditions(guards, cx.source);
    for g in own.iter().rev() {
        out.push(Cond::node(*g, true));
    }
    let mut prev = match_node.prev_named_sibling();
    for _ in 0..SCAN_LIMIT {
        let Some(p) = prev else { break };
        if p.kind() == "match" {
            if let Some(pg) = p.child_by_field_name("guards") {
                let earlier = haskell_guard_conditions(pg, cx.source);
                if let [only] = earlier.as_slice() {
                    out.push(Cond::node(*only, false));
                }
            }
        }
        prev = p.prev_named_sibling();
    }
}

fn haskell_guard_conditions<'t>(guards: Node<'t>, source: &[u8]) -> Vec<Node<'t>> {
    field_children(guards, "guard")
        .into_iter()
        .filter(|g| g.is_named())
        // A boolean guard wraps its expression.
        .map(|g| match (g.kind(), named_children(g).as_slice()) {
            ("boolean", [only]) => *only,
            _ => g,
        })
        .filter(|g| !matches!(text(*g, source).trim(), "otherwise" | "True"))
        .collect()
}
