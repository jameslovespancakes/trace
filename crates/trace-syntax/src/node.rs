//! Small structural helpers over tree-sitter nodes (no source-text matching).

use std::borrow::Cow;

use trace_core::model::ByteSpan;
use tree_sitter::Node;

/// Byte span of a node.
#[inline]
pub(crate) fn span(node: Node<'_>) -> ByteSpan {
    ByteSpan::new(node.start_byte() as u32, node.end_byte() as u32)
}

/// Exact source text of a node (lossy for invalid UTF-8).
#[inline]
pub(crate) fn text<'s>(node: Node<'_>, source: &'s [u8]) -> Cow<'s, str> {
    slice(source, node.start_byte(), node.end_byte())
}

/// Lossy text of a byte range, clamped to the buffer.
#[inline]
pub(crate) fn slice(source: &[u8], start: usize, end: usize) -> Cow<'_, str> {
    let end = end.min(source.len());
    let start = start.min(end);
    String::from_utf8_lossy(&source[start..end])
}

/// Raw bytes of a node.
#[inline]
pub(crate) fn bytes<'s>(node: Node<'_>, source: &'s [u8]) -> &'s [u8] {
    let end = node.end_byte().min(source.len());
    let start = node.start_byte().min(end);
    &source[start..end]
}

/// Named children that are not extras (comments).
pub(crate) fn named_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    let children: Vec<Node<'t>> = node.named_children(&mut cursor).filter(|c| !c.is_extra()).collect();
    children
}

/// All children (named and anonymous) with their field names.
pub(crate) fn children_with_fields<'t>(node: Node<'t>, out: &mut Vec<(Node<'t>, Option<&'static str>)>) {
    out.clear();
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            out.push((cursor.node(), cursor.field_name()));
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// The n-th named non-extra child; negative indexes count from the end.
pub(crate) fn nth_named<'t>(node: Node<'t>, index: i64) -> Option<Node<'t>> {
    if index >= 0 {
        let mut cursor = node.walk();
        let found = node
            .named_children(&mut cursor)
            .filter(|c| !c.is_extra())
            .nth(index as usize);
        found
    } else {
        let all = named_children(node);
        let back = index.unsigned_abs() as usize;
        all.len().checked_sub(back).and_then(|i| all.get(i).copied())
    }
}

/// First named child of `kind`.
pub(crate) fn first_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    let found = node.named_children(&mut cursor).find(|c| c.kind() == kind);
    found
}

/// Apply a spec selector (see `spec` module docs): `field`, `#n`, `=kind`, `a/b`, and
/// alternatives `a|b` (first that exists).
pub(crate) fn pick<'t>(node: Node<'t>, selector: &str) -> Option<Node<'t>> {
    if selector.is_empty() {
        return None;
    }
    selector
        .split('|')
        .find_map(|alternative| pick_path(node, alternative))
}

fn pick_path<'t>(node: Node<'t>, path: &str) -> Option<Node<'t>> {
    let mut current = node;
    for step in path.split('/') {
        current = pick_one(current, step)?;
    }
    Some(current)
}

fn pick_one<'t>(node: Node<'t>, step: &str) -> Option<Node<'t>> {
    if let Some(index) = step.strip_prefix('#') {
        let index: i64 = index.parse().ok()?;
        nth_named(node, index)
    } else if let Some(kind) = step.strip_prefix('=') {
        first_of_kind(node, kind)
    } else {
        node.child_by_field_name(step)
    }
}

/// Whether `child` is the node selected by `selector` on `parent`.
#[inline]
pub(crate) fn is_pick(parent: Node<'_>, selector: &str, child: Node<'_>) -> bool {
    pick(parent, selector).is_some_and(|c| c.id() == child.id())
}

/// Whether the node has a direct anonymous child token of this kind (e.g. `async`, `*`),
/// also looking one level into modifier containers.
pub(crate) fn has_token(node: Node<'_>, token: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if !child.is_named() {
            if child.kind() == token {
                return true;
            }
        } else if child.kind().contains("modifier") {
            let mut inner = child.walk();
            let found = child.children(&mut inner).any(|c| c.kind() == token);
            if found {
                return true;
            }
        }
    }
    false
}

/// Whether the node has a direct anonymous child token of this kind.
pub(crate) fn has_direct_token(node: Node<'_>, token: &str) -> bool {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|c| !c.is_named() && c.kind() == token);
    found
}

/// First descendant (pre-order, bounded) whose kind satisfies `accept`.
pub(crate) fn find_descendant<'t>(
    node: Node<'t>,
    max_nodes: usize,
    accept: impl Fn(Node<'t>) -> bool,
) -> Option<Node<'t>> {
    let mut stack = vec![node];
    let mut seen = 0usize;
    while let Some(n) = stack.pop() {
        seen += 1;
        if seen > max_nodes {
            break;
        }
        if n.id() != node.id() && accept(n) {
            return Some(n);
        }
        let mut cursor = n.walk();
        let children: Vec<Node<'t>> = n.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    None
}

/// Cut a string to at most `max` bytes on a char boundary.
pub(crate) fn truncate_bytes(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut cut = max;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

/// Reduce a type spelling node to its bare type name, structurally:
/// `pkg::Foo<T>` -> `Foo`, `*Server` -> `Server`, `Foo[T]` -> `Foo`.
pub(crate) fn type_name(node: Node<'_>, source: &[u8]) -> String {
    let mut current = node;
    for _ in 0..8 {
        let kind = current.kind();
        let next = if kind.starts_with("generic") || kind == "template_type" {
            current
                .child_by_field_name("type")
                .or_else(|| current.child_by_field_name("name"))
                .or_else(|| nth_named(current, 0))
        } else if kind.starts_with("scoped")
            || kind.starts_with("qualified")
            || kind == "nested_type_identifier"
        {
            current.child_by_field_name("name").or_else(|| nth_named(current, -1))
        } else if kind.contains("pointer") || kind.contains("reference") || kind == "parenthesized_type" {
            current.child_by_field_name("type").or_else(|| nth_named(current, -1))
        } else {
            None
        };
        match next {
            Some(n) if n.id() != current.id() => current = n,
            _ => break,
        }
    }
    text(current, source).trim().to_string()
}

#[cfg(test)]
#[path = "../tests/unit/node.rs"]
mod tests;
