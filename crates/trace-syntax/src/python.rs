//! Python specifics (port of codepath_v3/backends/python/declarations.py and pyright.py
//! `Ownership`), applied after generic extraction.
//!
//! * Docstring: first statement of the body when it is a string literal (<= 1200 bytes),
//!   cleaned like `inspect.cleandoc` (`ast.get_docstring`).
//! * Overload groups: same-name defs in one scope where all but the last are decorated
//!   `@overload` / `@typing.overload` (alias-aware: `from typing import overload as o`,
//!   `import typing as t`; a locally re-bound name disables the rule). The `@overload` defs
//!   are removed; the implementation gets `declaration_lines` = every def line. If the last
//!   def is itself `@overload` (e.g. in a `.pyi`, no implementation), the group collapses onto
//!   the last overload in the same way, which stays marked `is_stub` (one symbol per name,
//!   as in the reference design).
//! * Stubs: every declaration in a `.pyi` file; defs whose body is only `...`/`pass`/docstring
//!   and decorated `@abstractmethod`/`@abc.abstractmethod`, or inside a class whose bases
//!   include `Protocol`/`typing.Protocol`, are `is_stub`.
//! * Execution model: `async def` with `yield` -> AsyncGenerator; `async def` -> Coroutine;
//!   `yield`/`yield from` in own body (not nested defs/lambdas/classes) -> Generator.
//! * Ownership: decorators and default values execute in the enclosing scope; class bodies
//!   run at definition (owner `None`); lambdas are synthetic `<lambda>` declarations owning
//!   their body (their defaults run in the enclosing scope); generator expressions are
//!   synthetic `<genexpr>` declarations (execution Generator / AsyncGenerator) owning
//!   everything except the first iterable, which the enclosing scope evaluates eagerly.
//!   List/set/dict comprehensions run eagerly in the enclosing scope.
//! * Activation: call directly under `await` -> Await; call that is the iterable of `for` /
//!   `async for`, the operand of `yield from` or of `*` unpacking -> Iterate.
//! * Imports / names: see [`crate::names`] (Python scoping for callee paths, builtins).
//! * Callbacks: Name / Attribute call arguments (incl. `*arg`, keyword values).
//! * Value references: Name/Attribute loads (not direct callees, decorators included).
//! * Assignments: Name/Attribute targets of `=`/annotated assignment and keyword argument
//!   names (at most 40 per name are used as evidence).
//! * Data-model operations: see [`crate::lower`].
//!
//! Ownership, activation, callbacks, references and assignments are produced by the generic
//! extractor from the Python [`crate::spec::SyntaxSpec`]; this module applies the rules that
//! need Python semantics.

use std::collections::{HashMap, HashSet};

use trace_core::facts::FileFacts;
use trace_core::model::{ExecutionModel, SymbolKind};
use trace_core::text::LineIndex;
use trace_core::Language;
use tree_sitter::Node;

use crate::node::{bytes, has_direct_token, named_children, nth_named, text, truncate_bytes};
use crate::MAX_DOC_BYTES;

/// Python expression context of an expression node (the `ctx` of the `ast` module).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExprContext {
    Load,
    Store,
    Del,
}

/// Structural `ast` context: targets of assignments / loops / walrus / `as` are stores,
/// operands of `del` are deletes (through tuples, lists and parentheses), all else loads.
pub(crate) fn expr_context(node: Node<'_>) -> ExprContext {
    let mut child = node;
    while let Some(parent) = child.parent() {
        let is_left = || {
            parent
                .child_by_field_name("left")
                .is_some_and(|l| l.id() == child.id())
        };
        match parent.kind() {
            "assignment" | "augmented_assignment" | "for_statement" | "for_in_clause" => {
                return if is_left() {
                    ExprContext::Store
                } else {
                    ExprContext::Load
                };
            }
            "named_expression" => {
                let is_name = parent
                    .child_by_field_name("name")
                    .is_some_and(|n| n.id() == child.id());
                return if is_name {
                    ExprContext::Store
                } else {
                    ExprContext::Load
                };
            }
            "as_pattern_target" => return ExprContext::Store,
            "delete_statement" => return ExprContext::Del,
            "pattern_list"
            | "tuple_pattern"
            | "list_pattern"
            | "list_splat_pattern"
            | "expression_list"
            | "tuple"
            | "list"
            | "parenthesized_expression" => {
                child = parent;
            }
            _ => return ExprContext::Load,
        }
    }
    ExprContext::Load
}

/// The `function_definition` / `class_definition` whose name is at `name_span`.
fn def_node<'t>(root: Node<'t>, start: u32, end: u32) -> Option<Node<'t>> {
    let ident = root.descendant_for_byte_range(start as usize, end as usize)?;
    let parent = ident.parent()?;
    let named = parent
        .child_by_field_name("name")
        .is_some_and(|n| n.id() == ident.id());
    (named && matches!(parent.kind(), "function_definition" | "class_definition")).then_some(parent)
}

/// Apply the Python post-processing rules above. `path` decides `.pyi` handling.
pub(crate) fn postprocess(path: &str, root: tree_sitter::Node<'_>, source: &[u8], facts: &mut FileFacts) {
    let stub_file = Language::is_python_stub_path(path);
    let lines = LineIndex::new(source);
    let nodes: Vec<Option<Node<'_>>> = facts
        .declarations
        .iter()
        .map(|d| def_node(root, d.name_span.start, d.name_span.end))
        .collect();

    // Docstrings and execution models.
    for (decl, node) in facts.declarations.iter_mut().zip(&nodes) {
        let Some(node) = node else {
            continue;
        };
        if let Some(doc) = docstring(*node, source) {
            decl.doc = Some(doc);
        }
        if node.kind() == "function_definition" {
            decl.execution = execution_model(*node);
        }
    }

    // Overload groups.
    let aliases = typing_aliases(root, source);
    let stores = store_names(root, source);
    let is_overload: Vec<bool> = nodes
        .iter()
        .map(|n| n.is_some_and(|n| decorated_overload(n, source, &aliases, &stores)))
        .collect();
    let mut groups: HashMap<(usize, &str), Vec<usize>> = HashMap::new();
    let mut order: Vec<(usize, &str)> = Vec::new();
    for (i, (decl, node)) in facts.declarations.iter().zip(&nodes).enumerate() {
        let Some(node) = node else {
            continue;
        };
        let statement = match node.parent() {
            Some(p) if p.kind() == "decorated_definition" => p,
            _ => *node,
        };
        let Some(block) = statement.parent() else {
            continue;
        };
        let key = (block.id(), decl.name.as_str());
        let entry = groups.entry(key).or_default();
        if entry.is_empty() {
            order.push(key);
        }
        entry.push(i);
    }
    let mut remove = vec![false; facts.declarations.len()];
    let mut collapsed: Vec<(usize, Vec<u32>)> = Vec::new();
    for key in &order {
        let group = &groups[key];
        let Some((&last, rest)) = group.split_last() else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let all_overloads = rest.iter().all(|&i| {
            facts.declarations[i].kind.is_callable()
                && nodes[i].is_some_and(|n| n.kind() == "function_definition")
                && is_overload[i]
        });
        if !all_overloads {
            continue;
        }
        for &i in rest {
            remove[i] = true;
        }
        let def_lines = group
            .iter()
            .map(|&i| lines.line1(facts.declarations[i].name_span.start))
            .collect();
        collapsed.push((last, def_lines));
    }
    for (i, def_lines) in collapsed {
        facts.declarations[i].declaration_lines = def_lines;
    }

    // Stubs.
    let protocol_classes: HashSet<usize> = facts
        .declarations
        .iter()
        .enumerate()
        .filter(|(_, d)| d.kind == SymbolKind::Class && d.bases.iter().any(|b| is_protocol_base(b)))
        .map(|(i, _)| i)
        .collect();
    let synthetic: HashSet<u32> = facts.anonymous.iter().map(|a| a.decl).collect();
    for i in 0..facts.declarations.len() {
        if synthetic.contains(&(i as u32)) {
            continue;
        }
        let stub = {
            let decl = &facts.declarations[i];
            if stub_file || (is_overload[i] && !remove[i]) {
                true
            } else if decl.kind.is_callable() {
                let trivial = nodes[i].is_some_and(|n| trivial_body(n));
                let abstract_method = decl
                    .decorators
                    .iter()
                    .any(|d| d == "abstractmethod" || d == "abc.abstractmethod");
                let in_protocol = decl.parent.is_some_and(|p| protocol_classes.contains(&(p as usize)));
                trivial && (abstract_method || in_protocol)
            } else {
                false
            }
        };
        if stub {
            facts.declarations[i].is_stub = true;
        }
    }

    crate::edit::remove_declarations(facts, &remove);
}

/// `Protocol`, `typing.Protocol`, `Protocol[T]`, `typing_extensions.Protocol`.
fn is_protocol_base(base: &str) -> bool {
    let bare = base.split('[').next().unwrap_or(base).trim();
    bare.rsplit('.').next().unwrap_or(bare) == "Protocol"
}

/// Body consisting only of `...`, `pass` and string statements.
fn trivial_body(def: Node<'_>) -> bool {
    let Some(body) = def.child_by_field_name("body") else {
        return true;
    };
    named_children(body).iter().all(|stmt| match stmt.kind() {
        "pass_statement" => true,
        "expression_statement" => {
            let inner = named_children(*stmt);
            inner.len() == 1 && matches!(inner[0].kind(), "ellipsis" | "string" | "concatenated_string")
        }
        _ => false,
    })
}

/// Execution model from `async` and `yield` in the function's own body.
fn execution_model(def: Node<'_>) -> ExecutionModel {
    let asynchronous = has_direct_token(def, "async");
    let generator = def.child_by_field_name("body").is_some_and(|b| contains_own_yield(b));
    match (asynchronous, generator) {
        (true, true) => ExecutionModel::AsyncGenerator,
        (true, false) => ExecutionModel::Coroutine,
        (false, true) => ExecutionModel::Generator,
        (false, false) => ExecutionModel::Ordinary,
    }
}

/// Whether `body` contains a `yield` of its own function (not of nested defs/lambdas/classes).
pub(crate) fn contains_own_yield(body: Node<'_>) -> bool {
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "yield" => return true,
            "function_definition" | "class_definition" | "lambda" | "decorated_definition" => continue,
            _ => {}
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    false
}

/// The docstring of a def/class body, cleaned like `inspect.cleandoc`.
fn docstring(def: Node<'_>, source: &[u8]) -> Option<String> {
    let body = def.child_by_field_name("body")?;
    let first = nth_named(body, 0)?;
    if first.kind() != "expression_statement" {
        return None;
    }
    let inner = named_children(first);
    if inner.len() != 1 {
        return None;
    }
    let literal = inner[0];
    let strings: Vec<Node<'_>> = match literal.kind() {
        "string" => vec![literal],
        "concatenated_string" => named_children(literal)
            .into_iter()
            .filter(|n| n.kind() == "string")
            .collect(),
        _ => return None,
    };
    let mut raw = String::new();
    for s in strings {
        raw.push_str(&string_content(s, source));
    }
    let cleaned = cleandoc(&raw);
    if cleaned.is_empty() {
        return None;
    }
    Some(truncate_bytes(cleaned, MAX_DOC_BYTES))
}

/// Text between the string start and end delimiters.
fn string_content(string: Node<'_>, source: &[u8]) -> String {
    let children = named_children(string);
    let start = children
        .iter()
        .find(|c| c.kind() == "string_start")
        .map(|c| c.end_byte());
    let end = children
        .iter()
        .rev()
        .find(|c| c.kind() == "string_end")
        .map(|c| c.start_byte());
    match (start, end) {
        (Some(a), Some(b)) if a <= b => crate::node::slice(source, a, b).into_owned(),
        _ => text(string, source).into_owned(),
    }
}

/// `inspect.cleandoc`: strip the first line's indentation, the common indentation of the
/// following lines and leading/trailing blank lines.
fn cleandoc(doc: &str) -> String {
    let expanded = expand_tabs(doc);
    let lines: Vec<&str> = expanded
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let margin = lines
        .iter()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    out.push(lines[0].trim_start());
    for line in lines.iter().skip(1) {
        let mut cut = margin.unwrap_or(0).min(line.len() - line.trim_start().len());
        while !line.is_char_boundary(cut) {
            cut -= 1;
        }
        out.push(&line[cut..]);
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    let first = out.iter().position(|l| !l.trim().is_empty()).unwrap_or(out.len());
    out[first..].join("\n")
}

fn expand_tabs(s: &str) -> String {
    if !s.contains('\t') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 16);
    let mut column = 0usize;
    for c in s.chars() {
        match c {
            '\t' => {
                let spaces = 8 - column % 8;
                out.extend(std::iter::repeat_n(' ', spaces));
                column += spaces;
            }
            '\n' | '\r' => {
                out.push(c);
                column = 0;
            }
            _ => {
                out.push(c);
                column += 1;
            }
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Alias {
    Overload,
    Typing,
}

/// Module-level `from typing import overload [as o]` / `import typing [as t]`.
fn typing_aliases(root: Node<'_>, source: &[u8]) -> HashMap<String, Alias> {
    let mut aliases = HashMap::new();
    let is_typing = |n: Node<'_>| matches!(bytes(n, source), b"typing" | b"typing_extensions");
    for stmt in named_children(root) {
        match stmt.kind() {
            "import_from_statement" => {
                if !stmt.child_by_field_name("module_name").is_some_and(is_typing) {
                    continue;
                }
                let mut cursor = stmt.walk();
                for name in stmt.children_by_field_name("name", &mut cursor) {
                    match name.kind() {
                        "dotted_name" if bytes(name, source) == b"overload" => {
                            aliases.insert("overload".to_string(), Alias::Overload);
                        }
                        "aliased_import" => {
                            let original = name.child_by_field_name("name");
                            let alias = name.child_by_field_name("alias");
                            if let (Some(o), Some(a)) = (original, alias) {
                                if bytes(o, source) == b"overload" {
                                    aliases.insert(text(a, source).into_owned(), Alias::Overload);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "import_statement" => {
                let mut cursor = stmt.walk();
                for name in stmt.children_by_field_name("name", &mut cursor) {
                    match name.kind() {
                        "dotted_name" if is_typing(name) => {
                            aliases.insert(text(name, source).into_owned(), Alias::Typing);
                        }
                        "aliased_import" => {
                            let original = name.child_by_field_name("name");
                            let alias = name.child_by_field_name("alias");
                            if let (Some(o), Some(a)) = (original, alias) {
                                if is_typing(o) {
                                    aliases.insert(text(a, source).into_owned(), Alias::Typing);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    aliases
}

/// Every name bound anywhere in the module (`ast.Name` with `Store` context).
fn store_names(root: Node<'_>, source: &[u8]) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" {
            if expr_context(node) == ExprContext::Store {
                names.insert(text(node, source).into_owned());
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    names
}

/// `@overload` / `@typing.overload` (alias-aware; re-bound names disable the rule).
fn decorated_overload(
    def: Node<'_>,
    source: &[u8],
    aliases: &HashMap<String, Alias>,
    stores: &HashSet<String>,
) -> bool {
    let Some(wrapper) = def.parent().filter(|p| p.kind() == "decorated_definition") else {
        return false;
    };
    let unbound = |n: Node<'_>, want: Alias| {
        let name = text(n, source);
        !stores.contains(name.as_ref()) && aliases.get(name.as_ref()) == Some(&want)
    };
    named_children(wrapper)
        .into_iter()
        .filter(|d| d.kind() == "decorator")
        .filter_map(|d| nth_named(d, 0))
        .any(|expr| match expr.kind() {
            "identifier" => unbound(expr, Alias::Overload),
            "attribute" => {
                let object = expr.child_by_field_name("object");
                let attr = expr.child_by_field_name("attribute");
                matches!((object, attr), (Some(o), Some(a))
                    if o.kind() == "identifier"
                        && bytes(a, source) == b"overload"
                        && unbound(o, Alias::Typing))
            }
            _ => false,
        })
}

#[cfg(test)]
#[path = "../tests/unit/python.rs"]
mod tests;
