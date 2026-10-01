//! Name bindings, imports and qualified paths of dotted names (structural, per file).
//!
//! * Imports are read from the syntax tree (Python `import` / `from ... import`, JS/TS ES
//!   `import` incl. TS `import x = require(...)`, Rust `use`, Go / Java / Scala / Haskell
//!   imports, C# `using`, PHP `use`, C/C++ `#include` and `using`) into [`ImportBinding`]s;
//!   loader calls that bind a module (`const m = require('m')`, R `library(x)`, Bash
//!   `source`) are read by [`read_call_imports`]. Path targets keep the language's separator
//!   (`::` for Rust/C++, `\` for PHP, `/` paths for Go/C includes, `.` otherwise).
//!   Bindings that bring every name of a module into scope (Java `.*`, Go `.`, C# namespace
//!   `using`, C `#include`, Haskell unqualified import, R `library`) are `Wildcard`: unbound
//!   names become unknown, never guessed.
//! * [`Names`] records every binding per lexical scope while the extractor walks the tree:
//!   imports, declaration names, parameters and identifiers in store positions.
//! * [`Names::resolve`] follows the Python scoping rule (innermost function scope outwards,
//!   class scopes visible only directly in their own body, then the module, then builtins):
//!   a dotted name whose root is bound in the nearest binding scope only by imports of one
//!   target resolves to `<target>.<rest>`; a root with no binding anywhere on the chain that
//!   is a language builtin resolves to `<builtins_module>.<root>.<rest>`. Any other binding
//!   (assignment, parameter, def) shadows; a wildcard import on the chain makes unbound
//!   names unknown. Unknown is never guessed.

use std::collections::HashMap;

use trace_core::facts::{Declaration, ImportKind};
use tree_sitter::Node;

use crate::extract::unwrap_node;
use crate::node::{has_direct_token, named_children, nth_named, pick, slice, text};
use crate::spec::SyntaxSpec;

/// Maximum dotted-name length followed by [`chain`].
const MAX_CHAIN: usize = 16;

/// One name bound by an import statement.
#[derive(Clone, Debug)]
pub(crate) struct ImportBinding<'t> {
    pub local: String,
    pub target: String,
    pub kind: ImportKind,
    /// Node the binding is reported at (the imported name / alias clause).
    pub node: Node<'t>,
    /// Identifier of the imported name (`redirect` in `from .helpers import redirect as r`,
    /// `C` in `import a.b.C`): the `import` / `export` reference position. `None` when the
    /// statement spells no name (wildcards).
    pub name: Option<Node<'t>>,
    /// The binding is also a re-export (Rust `pub use`).
    pub export: bool,
}

impl<'t> ImportBinding<'t> {
    pub(crate) fn new(
        local: String,
        target: String,
        kind: ImportKind,
        node: Node<'t>,
        name: Option<Node<'t>>,
    ) -> Self {
        ImportBinding {
            local,
            target,
            kind,
            node,
            name,
            export: false,
        }
    }
}

/// A reader of the bindings of one import statement / loader call node.
pub(crate) type ImportReader = for<'t> fn(Node<'t>, &[u8], &mut Vec<ImportBinding<'t>>);

/// Read the bindings of one import statement node (`SyntaxSpec::imports` kinds, read by the
/// language's `SyntaxSpec::import_readers`).
pub(crate) fn read_imports<'t>(spec: &SyntaxSpec, node: Node<'t>, source: &[u8]) -> Vec<ImportBinding<'t>> {
    let mut out = Vec::new();
    if let Some((_, read)) = spec.import_readers.iter().find(|(kind, _)| *kind == node.kind()) {
        read(node, source, &mut out);
    }
    out
}

/// Innermost last leaf of a (qualified) name node: `C` of `a.b.C`, `Entry` of
/// `hash_map::Entry`.
pub(crate) fn last_leaf(node: Node<'_>) -> Node<'_> {
    let mut current = node;
    for _ in 0..16 {
        match current.child_by_field_name("name").or_else(|| nth_named(current, -1)) {
            Some(next) if next.id() != current.id() => current = next,
            _ => break,
        }
    }
    current
}

/// A qualified name spelled by a node, its leaves joined with `sep` (structural: the named
/// leaves in source order; type arguments are skipped).
pub(crate) fn path_text(node: Node<'_>, source: &[u8], sep: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut stack = vec![node];
    let mut seen = 0usize;
    while let Some(n) = stack.pop() {
        seen += 1;
        if seen > 256 {
            break;
        }
        let kind = n.kind();
        if kind.contains("type_arguments") || kind.contains("argument_list") {
            continue;
        }
        let children = named_children(n);
        if children.is_empty() {
            let t = text(n, source).trim().to_string();
            if !t.is_empty() {
                parts.push(t);
            }
            continue;
        }
        stack.extend(children.into_iter().rev());
    }
    parts.join(sep)
}

/// Contents of a string literal node: its content/fragment children, else the text between
/// the literal's delimiters (`"a.h"`, `<stdio.h>`, `'x.js'`).
pub(crate) fn literal_text(node: Node<'_>, source: &[u8]) -> String {
    let mut stack = vec![node];
    let mut parts: Vec<Node<'_>> = Vec::new();
    while let Some(n) = stack.pop() {
        if n.kind().contains("content") || n.kind().contains("fragment") {
            parts.push(n);
            continue;
        }
        if parts.len() > 64 {
            break;
        }
        let mut kids = named_children(n);
        kids.reverse();
        stack.extend(kids);
    }
    if let (Some(a), Some(b)) = (parts.first(), parts.last()) {
        return slice(source, a.start_byte(), b.end_byte()).trim().to_string();
    }
    let raw = text(node, source);
    let raw = raw.trim();
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        let paired = matches!((first, last), (b'"', b'"') | (b'\'', b'\'') | (b'<', b'>') | (b'`', b'`'));
        if paired {
            return raw[1..raw.len() - 1].to_string();
        }
    }
    raw.to_string()
}

/// First string-like descendant (bounded).
fn first_string<'t>(node: Node<'t>) -> Option<Node<'t>> {
    crate::node::find_descendant(node, 64, |n| {
        let k = n.kind();
        k.contains("string") && !k.contains("content") && !k.contains("fragment")
    })
}

/// Join a module prefix and a member with `sep`.
pub(crate) fn join_sep(prefix: &str, name: &str, sep: &str) -> String {
    match (prefix.is_empty(), name.is_empty()) {
        (true, _) => name.to_string(),
        (_, true) => prefix.to_string(),
        _ => format!("{prefix}{sep}{name}"),
    }
}

/// Last segment of a path spelled with `sep`.
pub(crate) fn last_segment<'s>(path: &'s str, sep: &str) -> &'s str {
    path.rsplit(sep).next().unwrap_or(path)
}

// ---- Rust -----------------------------------------------------------------------------

// ---- Go, Java, Scala, C#, PHP, Haskell, C/C++ -------------------------------------------

// ---- loader calls (`SyntaxSpec::import_calls`) -------------------------------------

/// Bindings made by a loader call inside `node` (a `SyntaxSpec::import_calls` kind). Only
/// fixed library loaders of the language are recognised (JS `require`, R `library`/`require`,
/// Bash `source`/`.`), with a literal argument.
pub(crate) fn read_call_imports<'t>(
    spec: &SyntaxSpec,
    node: Node<'t>,
    source: &[u8],
) -> Vec<ImportBinding<'t>> {
    let mut out = Vec::new();
    if let Some(read) = spec.call_import_reader {
        read(node, source, &mut out);
    }
    out
}

/// `callee("literal")` with a bare-identifier callee in `names`: the literal node.
pub(crate) fn loader_literal<'t>(
    call: Node<'t>,
    callee_field: &str,
    args_field: &str,
    names: &[&str],
    source: &[u8],
) -> Option<Node<'t>> {
    let callee = pick(call, callee_field)?;
    if callee.named_child_count() != 0 || !names.contains(&text(callee, source).trim()) {
        return None;
    }
    let args = pick(call, args_field)?;
    let first = nth_named(args, 0)?;
    let first = if first.kind().contains("string") {
        first
    } else {
        first_string(first)?
    };
    Some(first)
}

// ---- exports -------------------------------------------------------------------------

/// One entry of a JS/TS `export` statement.
pub(crate) struct EsExport<'t> {
    /// Exported name (`*` for `export * from`).
    pub exported: String,
    /// Name in the source module / local scope (`a` of `a as b`); empty for `*`.
    pub name: String,
    /// Module specifier of `export ... from '<spec>'`.
    pub specifier: Option<String>,
    pub node: Node<'t>,
}

/// Entries of a JS/TS `export_statement` that re-export (`export {a as b} from`, `export *
/// from`, `export * as ns from`, and `export { x }` lists whose names the caller resolves).
pub(crate) fn es_exports<'t>(node: Node<'t>, source: &[u8]) -> Vec<EsExport<'t>> {
    let mut out = Vec::new();
    if node.kind() == "assignment_expression" {
        out.extend(commonjs_reexport(node, source));
        return out;
    }
    if node.kind() != "export_statement" {
        return out;
    }
    let specifier = node
        .child_by_field_name("source")
        .map(|s| string_value(s, source))
        .filter(|s| !s.is_empty());
    let mut clause = false;
    for child in named_children(node) {
        match child.kind() {
            "export_clause" => {
                clause = true;
                for spec in named_children(child)
                    .into_iter()
                    .filter(|c| c.kind() == "export_specifier")
                {
                    let Some(name) = spec.child_by_field_name("name") else {
                        continue;
                    };
                    let name_text = if name.kind() == "string" {
                        string_value(name, source)
                    } else {
                        text(name, source).trim().to_string()
                    };
                    let exported = spec
                        .child_by_field_name("alias")
                        .map(|a| {
                            if a.kind() == "string" {
                                string_value(a, source)
                            } else {
                                text(a, source).trim().to_string()
                            }
                        })
                        .unwrap_or_else(|| name_text.clone());
                    if !name_text.is_empty() && !exported.is_empty() {
                        out.push(EsExport {
                            exported,
                            name: name_text,
                            specifier: specifier.clone(),
                            node: spec,
                        });
                    }
                }
            }
            "namespace_export" => {
                clause = true;
                if let (Some(spec), Some(id)) = (specifier.clone(), nth_named(child, 0)) {
                    out.push(EsExport {
                        exported: text(id, source).trim().to_string(),
                        name: String::new(),
                        specifier: Some(spec),
                        node: child,
                    });
                }
            }
            _ => {}
        }
    }
    if !clause && has_direct_token(node, "*") {
        if let Some(spec) = specifier {
            out.push(EsExport {
                exported: "*".to_string(),
                name: String::new(),
                specifier: Some(spec),
                node,
            });
        }
    }
    out
}

/// CommonJS whole-module re-export: a module-level `module.exports = require('<spec>')` (or
/// `exports = require('<spec>')`) makes the module's value the loaded module's, like the
/// wildcard re-export `export * from '<spec>'` (exported `*`, node: the loader call). A
/// chain (`exports = module.exports = require(..)`) is read at its inner assignment.
fn commonjs_reexport<'t>(node: Node<'t>, source: &[u8]) -> Option<EsExport<'t>> {
    let left = node.child_by_field_name("left")?;
    let whole_module = match left.kind() {
        "identifier" => text(left, source).trim() == "exports",
        "member_expression" => {
            let object = left.child_by_field_name("object")?;
            let property = left.child_by_field_name("property")?;
            object.kind() == "identifier"
                && text(object, source).trim() == "module"
                && text(property, source).trim() == "exports"
        }
        _ => false,
    };
    let right = node.child_by_field_name("right")?;
    if !whole_module || right.kind() != "call_expression" {
        return None;
    }
    let literal = loader_literal(right, "function", "arguments", &["require"], source)?;
    let args = pick(right, "arguments")?;
    if args.named_child_count() != 1 || nth_named(args, 0) != Some(literal) {
        return None;
    }
    let specifier = literal_text(literal, source);
    (!specifier.is_empty()).then_some(EsExport {
        exported: "*".to_string(),
        name: String::new(),
        specifier: Some(specifier),
        node: right,
    })
}

/// `a.b.c` from a `dotted_name` node (identifier children joined structurally).
pub(crate) fn dotted(node: Node<'_>, source: &[u8]) -> String {
    if node.kind() != "dotted_name" {
        return text(node, source).trim().to_string();
    }
    named_children(node)
        .iter()
        .map(|c| text(*c, source).trim().to_string())
        .collect::<Vec<_>>()
        .join(".")
}

/// Module path of `from <module> import ...` (relative imports keep their dots).
pub(crate) fn from_module(node: Node<'_>, source: &[u8]) -> Option<String> {
    let module = node.child_by_field_name("module_name")?;
    Some(match module.kind() {
        "relative_import" => {
            let mut path = String::new();
            for part in named_children(module) {
                match part.kind() {
                    "import_prefix" => path.push_str(text(part, source).trim()),
                    "dotted_name" => path.push_str(&dotted(part, source)),
                    _ => {}
                }
            }
            path
        }
        _ => dotted(module, source),
    })
    .filter(|m| !m.is_empty())
}

/// `module` + `.` + `name`, without doubling the dot of a bare relative prefix.
pub(crate) fn join(module: &str, name: &str) -> String {
    if module.ends_with('.') {
        format!("{module}{name}")
    } else {
        format!("{module}.{name}")
    }
}

/// Text of a JS string literal without its quotes (fragments are structural children).
pub(crate) fn string_value(node: Node<'_>, source: &[u8]) -> String {
    let fragments: Vec<Node<'_>> = named_children(node)
        .into_iter()
        .filter(|c| c.kind() == "string_fragment")
        .collect();
    match (fragments.first(), fragments.last()) {
        (Some(a), Some(b)) => crate::node::slice(source, a.start_byte(), b.end_byte()).into_owned(),
        _ => String::new(),
    }
}

/// A dotted name: root identifier and the attribute names after it (`a.b.c` -> `a`, `[b, c]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Chain {
    pub root: String,
    pub rest: Vec<String>,
}

/// The dotted name spelled by `node` (identifiers and member accesses only; parentheses and
/// other transparent wrappers are skipped). `None` for anything else (`f().x`, `a[0].b`).
pub(crate) fn chain(spec: &SyntaxSpec, node: Node<'_>, source: &[u8]) -> Option<Chain> {
    let mut rest: Vec<String> = Vec::new();
    let mut current = unwrap_node(spec, node);
    for _ in 0..MAX_CHAIN {
        let kind = current.kind();
        if spec.is_identifier(kind) && current.named_child_count() == 0 {
            let root = text(current, source).trim().to_string();
            if root.is_empty() {
                return None;
            }
            rest.reverse();
            return Some(Chain { root, rest });
        }
        let m = spec.member(kind)?;
        let property = pick(current, m.property_field)?;
        if property.named_child_count() != 0 {
            return None;
        }
        let name = text(property, source).trim().to_string();
        if name.is_empty() {
            return None;
        }
        rest.push(name);
        current = unwrap_node(spec, pick(current, m.object_field)?);
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Bound {
    Import(String),
    /// `def` / `class` name.
    Declared,
    Parameter,
    /// Assignment, loop, `with`/`except` target and other store positions.
    Variable,
}

/// What the nearest binding of a bare name is (see [`Names::binding`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Binding {
    /// Bound in a function scope only as that function's parameter.
    Parameter,
    /// Bound in a function scope only as parameters/variables (at least one variable).
    Variable,
    /// Anything else: module or class scope, a declaration, an import, unbound, unknown.
    Other,
}

/// Bindings per lexical scope (`None` = module) and the resolution parent of every
/// declaration scope.
#[derive(Debug, Default)]
pub(crate) struct Names {
    bindings: HashMap<String, Vec<(Option<u32>, Bound)>>,
    wildcards: Vec<Option<u32>>,
    /// Scope in which each declaration's definition is evaluated (index = declaration).
    parents: Vec<Option<u32>>,
}

impl Names {
    /// Register declaration `decl` (called in declaration order) defined in `scope`.
    pub fn declare_scope(&mut self, decl: u32, scope: Option<u32>) {
        let i = decl as usize;
        if self.parents.len() <= i {
            self.parents.resize(i + 1, None);
        }
        self.parents[i] = scope;
    }

    fn push(&mut self, scope: Option<u32>, name: &str, bound: Bound) {
        if name.is_empty() {
            return;
        }
        let entry = self.bindings.entry(name.to_string()).or_default();
        if !entry.iter().any(|(s, b)| *s == scope && *b == bound) {
            entry.push((scope, bound));
        }
    }

    /// A `def` / `class` name bound in `scope`.
    pub fn bind_declared(&mut self, scope: Option<u32>, name: &str) {
        self.push(scope, name, Bound::Declared);
    }

    /// `global name` / `nonlocal name` in `scope`: the name is bound outside `scope`, so it
    /// never counts as one of its plain variables.
    pub fn bind_outer(&mut self, scope: Option<u32>, name: &str) {
        self.push(scope, name, Bound::Declared);
    }

    /// A parameter of declaration `decl`.
    pub fn bind_parameter(&mut self, decl: u32, name: &str) {
        self.push(Some(decl), name, Bound::Parameter);
    }

    /// A variable binding of `name` in `scope` (store positions).
    pub fn bind_local(&mut self, scope: Option<u32>, name: &str) {
        self.push(scope, name, Bound::Variable);
    }

    /// Kind of the nearest binding of the bare name `root` used in `scope` (same scope
    /// chain as [`Names::resolve`]).
    pub fn binding(&self, root: &str, scope: Option<u32>, decls: &[Declaration]) -> Binding {
        let Some(bindings) = self.bindings.get(root) else {
            return Binding::Other;
        };
        let mut current = scope;
        let mut first = true;
        for _ in 0..=self.parents.len() {
            let decl = current.and_then(|d| decls.get(d as usize));
            let is_class = decl.is_some_and(|d| d.kind.is_type());
            if first || !is_class {
                let found: Vec<&Bound> = bindings
                    .iter()
                    .filter(|(s, _)| *s == current)
                    .map(|(_, b)| b)
                    .collect();
                if !found.is_empty() {
                    if !decl.is_some_and(|d| d.kind.is_callable()) {
                        return Binding::Other;
                    }
                    if found.iter().all(|b| **b == Bound::Parameter) {
                        return Binding::Parameter;
                    }
                    if found.iter().all(|b| matches!(b, Bound::Parameter | Bound::Variable)) {
                        return Binding::Variable;
                    }
                    return Binding::Other;
                }
                if self.wildcards.contains(&current) {
                    return Binding::Other;
                }
            }
            match current {
                Some(d) => {
                    current = self.parents.get(d as usize).copied().flatten();
                    first = false;
                }
                None => break,
            }
        }
        Binding::Other
    }

    /// An import binding (`Wildcard` imports make unbound names unknown in `scope`).
    pub fn bind_import(&mut self, scope: Option<u32>, binding: &ImportBinding<'_>) {
        if binding.kind == ImportKind::Wildcard {
            if !self.wildcards.contains(&scope) {
                self.wildcards.push(scope);
            }
            return;
        }
        self.push(scope, &binding.local, Bound::Import(binding.target.clone()));
    }

    /// Qualified path of `chain` used in `scope` (see module docs).
    pub fn resolve(
        &self,
        spec: &SyntaxSpec,
        chain: &Chain,
        scope: Option<u32>,
        decls: &[Declaration],
    ) -> Option<String> {
        let base = self.resolve_root(spec, &chain.root, scope, decls)?;
        let mut path = base;
        for part in &chain.rest {
            path.push('.');
            path.push_str(part);
        }
        Some(path)
    }

    fn resolve_root(
        &self,
        spec: &SyntaxSpec,
        root: &str,
        scope: Option<u32>,
        decls: &[Declaration],
    ) -> Option<String> {
        let bindings = self.bindings.get(root);
        let mut current = scope;
        let mut first = true;
        for _ in 0..=self.parents.len() {
            let is_class = current
                .and_then(|d| decls.get(d as usize))
                .is_some_and(|d| d.kind.is_type());
            if first || !is_class {
                if let Some(found) = bindings.map(|all| {
                    all.iter()
                        .filter(|(s, _)| *s == current)
                        .map(|(_, b)| b)
                        .collect::<Vec<&Bound>>()
                }) {
                    if !found.is_empty() {
                        return single_import(&found);
                    }
                }
                if self.wildcards.contains(&current) {
                    return None;
                }
            }
            match current {
                Some(d) => {
                    current = self.parents.get(d as usize).copied().flatten();
                    first = false;
                }
                None => break,
            }
        }
        (!spec.builtins_module.is_empty() && spec.builtins.contains(&root))
            .then(|| format!("{}.{root}", spec.builtins_module))
    }
}

/// The target when every binding in the scope is an import of the same target.
fn single_import(found: &[&Bound]) -> Option<String> {
    let mut target: Option<&str> = None;
    for b in found {
        match b {
            Bound::Import(t) => match target {
                None => target = Some(t),
                Some(existing) if existing == t => {}
                Some(_) => return None,
            },
            Bound::Declared | Bound::Parameter | Bound::Variable => return None,
        }
    }
    target.map(str::to_string)
}

#[cfg(test)]
#[path = "../tests/unit/names.rs"]
mod tests;
