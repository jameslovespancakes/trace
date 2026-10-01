//! Result-use analysis for `impact` RESULT USES (port of codepath_v4/analysis.py `uses`),
//! language-generic over syntax trees.
//!
//! Given a caller file and the byte position of a call (edge evidence), find the smallest
//! call whose callee span contains the point, climb through `await`, and classify the parent:
//!
//! * expression statement -> `IgnoresResult`;
//! * assignment -> for each later load of the assigned names inside the enclosing function
//!   (same scope, line >= assignment line) classify that load's parent by [`UseKind`];
//!   none found -> `StoredIn(names)`;
//! * anything else -> classify the direct parent.
//!
//! Only parents with a known [`UseKind`] count as uses of an assigned name (analysis.py
//! `USE_KIND`); uses found through an assignment carry the trimmed line text (<= 90 chars);
//! direct uses and `IgnoresResult` carry the call line and an empty `code`.

use serde::Serialize;
use trace_core::text::LineIndex;
use trace_core::Language;
use tree_sitter::Node;

use crate::extract::unwrap_node;
use crate::grammar::grammar;
use crate::node::{is_pick, named_children, pick, text};
use crate::spec::SyntaxSpec;

/// How a returned value is used.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UseKind {
    IgnoresResult,
    ReadsAttribute,
    IndexesOrSlices,
    PassesToCall,
    CallsIt,
    ReturnsIt,
    ComparesIt,
    IteratesIt,
    FormatsIntoText,
    Arithmetic,
    TestsTruthiness,
    StoredIn,
    UsesInExpression,
}

impl UseKind {
    /// Human phrase (analysis.py `USE_KIND` wording).
    pub fn phrase(&self) -> &'static str {
        match self {
            UseKind::IgnoresResult => "ignores the result",
            UseKind::ReadsAttribute => "reads attribute",
            UseKind::IndexesOrSlices => "indexes or slices",
            UseKind::PassesToCall => "passes to call",
            UseKind::CallsIt => "calls it",
            UseKind::ReturnsIt => "returns it",
            UseKind::ComparesIt => "compares it",
            UseKind::IteratesIt => "iterates it",
            UseKind::FormatsIntoText => "formats it into text",
            UseKind::Arithmetic => "uses in arithmetic/concatenation",
            UseKind::TestsTruthiness => "tests truthiness",
            UseKind::StoredIn => "stores it in",
            UseKind::UsesInExpression => "uses it in an expression",
        }
    }
}

/// One observed use (line + trimmed line text <= 90 chars).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UseSite {
    pub kind: UseKind,
    pub line: u32,
    pub code: String,
}

/// Uses of one call's result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResultUse {
    /// Trimmed text of the call line (<= 120 chars).
    pub call: String,
    pub line: u32,
    /// Assigned names when the result is stored in variables.
    pub stored_in: Vec<String>,
    pub uses: Vec<UseSite>,
}

const COMPARISON_OPERATORS: &[&str] = &[
    "==",
    "!=",
    "===",
    "!==",
    "<",
    ">",
    "<=",
    ">=",
    "<>",
    "in",
    "is",
    "instanceof",
    "not in",
    "is not",
];
const ARITHMETIC_OPERATORS: &[&str] = &[
    "+", "-", "*", "/", "%", "**", "//", "<<", ">>", ">>>", "&", "|", "^", "@", "..", "<>", "++",
];

/// Analyze how the call at byte `point` in `source` uses its result.
/// `None` when no call contains the point or the language has no grammar.
pub fn result_use(language: Language, source: &[u8], point: u32) -> Option<ResultUse> {
    let grammar = grammar(language)?;
    let tree = crate::parse::parse(grammar, "", source).ok()?;
    let spec = grammar.spec;
    let root = tree.root_node();
    let call = smallest_call(spec, root, point as usize)?;
    let lines = LineIndex::new(source);
    let call_line0 = lines.line0(call.start_byte() as u32);
    let call_text: String = lines.line_text(source, call_line0).trim().chars().take(120).collect();

    // Climb through await (and transparent wrappers).
    let mut node = call;
    let mut parent = call.parent();
    while let Some(p) = parent {
        if spec.awaits.contains(&p.kind()) || spec.unwrap_pick(p.kind()).is_some() {
            node = p;
            parent = p.parent();
        } else {
            break;
        }
    }
    let line = call_line0 + 1;
    let mut result = ResultUse {
        call: call_text,
        line,
        stored_in: Vec::new(),
        uses: Vec::new(),
    };
    let Some(parent) = parent else {
        return Some(result);
    };

    if spec.expression_statements.contains(&parent.kind()) {
        result.uses.push(UseSite {
            kind: UseKind::IgnoresResult,
            line,
            code: String::new(),
        });
        return Some(result);
    }
    if let Some(a) = spec
        .assignment(parent.kind())
        .filter(|a| is_pick(parent, a.second, node))
    {
        let targets: Vec<String> = pick(parent, a.first)
            .map(|left| {
                let items = if spec.lists.contains(&left.kind()) {
                    named_children(left)
                } else {
                    vec![left]
                };
                items
                    .into_iter()
                    .map(|t| unwrap_node(spec, t))
                    .filter(|t| spec.is_identifier(t.kind()))
                    .map(|t| text(t, source).into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let assignment_line = lines.line1(parent.start_byte() as u32);
        let scope = enclosing_function(spec, parent).unwrap_or(root);
        if !targets.is_empty() {
            collect_uses(spec, source, &lines, scope, &targets, parent, assignment_line, &mut result.uses);
        }
        if result.uses.is_empty() {
            result.uses.push(UseSite {
                kind: UseKind::StoredIn,
                line,
                code: if targets.is_empty() {
                    "a structure".to_string()
                } else {
                    targets.join(", ")
                },
            });
        }
        result.stored_in = targets;
        return Some(result);
    }
    let kind = classify(spec, parent, node).unwrap_or(UseKind::UsesInExpression);
    result.uses.push(UseSite {
        kind,
        line,
        code: String::new(),
    });
    Some(result)
}

/// The smallest call whose callee span contains `point`.
fn smallest_call<'t>(spec: &SyntaxSpec, root: Node<'t>, point: usize) -> Option<Node<'t>> {
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

fn enclosing_function<'t>(spec: &SyntaxSpec, node: Node<'t>) -> Option<Node<'t>> {
    let mut current = node.parent();
    while let Some(n) = current {
        if spec.is_lazy(n.kind()) {
            return Some(n);
        }
        current = n.parent();
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn collect_uses(
    spec: &SyntaxSpec,
    source: &[u8],
    lines: &LineIndex,
    scope: Node<'_>,
    targets: &[String],
    assignment: Node<'_>,
    assignment_line: u32,
    out: &mut Vec<UseSite>,
) {
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        if spec.is_identifier(node.kind()) {
            let line = lines.line1(node.start_byte() as u32);
            let name = text(node, source);
            let inside_target = pick(assignment, spec.assignment(assignment.kind()).map_or("", |a| a.first))
                .is_some_and(|t| t.start_byte() <= node.start_byte() && node.end_byte() <= t.end_byte());
            if line >= assignment_line
                && !inside_target
                && targets.iter().any(|t| t.as_str() == name.as_ref())
                && is_load(spec, node)
            {
                if let Some(parent) = node.parent() {
                    if let Some(kind) = classify(spec, parent, node) {
                        let code: String =
                            lines.line_text(source, line - 1).trim().chars().take(90).collect();
                        out.push(UseSite { kind, line, code });
                    }
                }
            }
            continue;
        }
        let mut cursor = node.walk();
        let mut children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        children.reverse();
        stack.extend(children);
    }
}

/// Not a binding position (assignment target, loop variable, keyword name, parameter).
fn is_load(spec: &SyntaxSpec, node: Node<'_>) -> bool {
    let mut child = node;
    while let Some(parent) = child.parent() {
        let kind = parent.kind();
        if spec.binding_kinds.contains(&kind) {
            return false;
        }
        if spec
            .store_fields
            .iter()
            .any(|p| p.kind == kind && is_pick(parent, p.first, child))
        {
            return false;
        }
        if spec
            .member(kind)
            .is_some_and(|m| is_pick(parent, m.property_field, child))
        {
            return false;
        }
        // Only direct targets and patterns bind; stop at the first expression parent.
        if kind.ends_with("pattern") || kind.ends_with("_list") || spec.lists.contains(&kind) {
            child = parent;
            continue;
        }
        return true;
    }
    true
}

/// analysis.py `USE_KIND` over syntax: the kind of use `parent` makes of its child `node`.
fn classify(spec: &SyntaxSpec, parent: Node<'_>, node: Node<'_>) -> Option<UseKind> {
    let kind = parent.kind();
    if let Some(m) = spec.member(kind) {
        return is_pick(parent, m.object_field, node).then_some(UseKind::ReadsAttribute);
    }
    if spec.subscript(kind).is_some() {
        return Some(UseKind::IndexesOrSlices);
    }
    if let Some(shape) = spec.call_shape(kind) {
        if is_pick(parent, shape.function_field, node)
            || (!shape.receiver_field.is_empty() && is_pick(parent, shape.receiver_field, node))
        {
            return Some(UseKind::CallsIt);
        }
        return Some(UseKind::PassesToCall);
    }
    // Argument lists: the grandparent is the call.
    if let Some(call) = parent.parent() {
        if let Some(shape) = spec.call_shape(call.kind()) {
            if is_pick(call, shape.arguments_field, parent) {
                return Some(UseKind::PassesToCall);
            }
        }
    }
    if spec.returns.contains(&kind) {
        return Some(UseKind::ReturnsIt);
    }
    if spec.comparisons.contains(&kind) {
        return Some(UseKind::ComparesIt);
    }
    if spec.arithmetic.contains(&kind) {
        return Some(UseKind::Arithmetic);
    }
    if spec.binary_ops.contains(&kind) {
        let mut cursor = parent.walk();
        let operator = parent.children(&mut cursor).find(|c| !c.is_named()).map(|c| c.kind());
        return match operator {
            Some(op) if COMPARISON_OPERATORS.contains(&op) => Some(UseKind::ComparesIt),
            Some(op) if ARITHMETIC_OPERATORS.contains(&op) => Some(UseKind::Arithmetic),
            _ => None,
        };
    }
    if spec
        .for_loops
        .iter()
        .any(|f| f.kind == kind && is_pick(parent, f.iterable, node))
    {
        return Some(UseKind::IteratesIt);
    }
    if spec.interpolations.contains(&kind) {
        return Some(UseKind::FormatsIntoText);
    }
    if spec
        .conditions
        .iter()
        .any(|c| c.kind == kind && is_pick(parent, c.first, node))
    {
        return Some(UseKind::TestsTruthiness);
    }
    // Parenthesized conditions (`if (x)`): look through the wrapper.
    if spec.unwrap_pick(kind).is_some() {
        if let Some(grand) = parent.parent() {
            if spec
                .conditions
                .iter()
                .any(|c| c.kind == grand.kind() && is_pick(grand, c.first, parent))
            {
                return Some(UseKind::TestsTruthiness);
            }
        }
    }
    None
}

#[cfg(test)]
#[path = "../tests/unit/uses.rs"]
mod tests;
