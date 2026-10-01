//! Lazy argument evaluation for trace-bridge: argument values, call strings, bound names,
//! module exports and declaration annotations of an already parsed file.

use trace_core::facts::FileFacts;
use trace_core::languages::{in_family, Family};
use trace_core::model::ByteSpan;
use trace_core::text::LineIndex;
use trace_core::Language;
use tree_sitter::Node;

use super::{
    literals::is_name_kind, literals::is_string_kind, template::ArgRef, template::Part, template::RawTpl,
    template::Tpl, Cx,
};
use crate::languages::syntax;
use crate::node::{named_children, nth_named, pick, span, text};

/// The value of one call argument: its span, its string template, the callable it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgValue {
    pub span: ByteSpan,
    pub template: Option<Tpl>,
    pub callable: Option<ByteSpan>,
}

/// Evaluate argument `arg` of the call whose callee spans `callee` (lazy, generic; no
/// framework names): the argument's span, its string template (literals, concatenation,
/// template strings, format calls, same-file constants, then `constant` for names this file
/// does not bind; a list / array literal is its elements joined by one space, i.e. a command
/// line) and, when the argument is a function literal or a name / member path, its span as
/// the callable it names. `None`: no call with that callee, or no such argument.
fn eval_argument(
    language: Language,
    tree: &tree_sitter::Tree,
    source: &[u8],
    callee: ByteSpan,
    arg: &ArgRef,
    constant: &dyn Fn(&str) -> Option<Tpl>,
) -> Option<ArgValue> {
    let spec = syntax(language)?;
    let facts = FileFacts::default();
    let lines = LineIndex::new(source);
    let mut cx = Cx::new(language, "", source, &lines, spec, &facts);
    cx.external = Some(constant);
    let root = tree.root_node();
    cx.collect_consts(root);
    let call = cx.call_with_callee(root, callee)?;
    let cv = cx.call_view(call)?;
    let node = match arg {
        ArgRef::Pos(i) => cv.positional(*i as usize),
        ArgRef::Kw(k) => cv.keyword(std::slice::from_ref(k)),
        ArgRef::PosOrKw(i, k) => cv
            .keyword(std::slice::from_ref(k))
            .or_else(|| cv.positional(*i as usize)),
        ArgRef::Receiver => cv.receiver,
        ArgRef::Last => cv.last_positional(),
        ArgRef::Field(i, field) => cv
            .positional(*i as usize)
            .and_then(|o| cx.object_value(o, std::slice::from_ref(field))),
    }?;
    Some(cx.arg_value(node))
}

/// One string value among a call's arguments ([`call_strings`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallString {
    /// The argument's keyword, or the object-literal entry's key (`url` of `{url: "/a"}`).
    pub key: Option<String>,
    pub value: ArgValue,
}

/// The string values among the arguments of the call whose callee spans `callee`: every
/// argument whose value evaluates to a string template, and every entry of an object /
/// dictionary literal argument that does (`client.get({url: "/api/items/"})`). Generic: no
/// names of functions or frameworks.
fn call_strings(
    language: Language,
    tree: &tree_sitter::Tree,
    source: &[u8],
    callee: ByteSpan,
    constant: &dyn Fn(&str) -> Option<Tpl>,
) -> Vec<CallString> {
    let Some(spec) = syntax(language) else {
        return Vec::new();
    };
    let facts = FileFacts::default();
    let lines = LineIndex::new(source);
    let mut cx = Cx::new(language, "", source, &lines, spec, &facts);
    cx.external = Some(constant);
    let root = tree.root_node();
    cx.collect_consts(root);
    let Some(call) = cx.call_with_callee(root, callee) else {
        return Vec::new();
    };
    let Some(cv) = cx.call_view(call) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for a in &cv.args {
        let entries = cx.object_entries(a.value);
        if entries.is_empty() {
            let value = cx.arg_value(a.value);
            if value.template.is_some() {
                out.push(CallString {
                    key: a.key.clone(),
                    value,
                });
            }
            continue;
        }
        for (key, v) in entries {
            let value = cx.arg_value(v);
            if value.template.is_some() {
                out.push(CallString {
                    key: Some(key),
                    value,
                });
            }
        }
    }
    out
}

/// A parsed file for repeated lazy evaluation (the functions below without naming the
/// tree-sitter types).
pub struct ParsedFile<'s> {
    language: Language,
    source: &'s [u8],
    tree: tree_sitter::Tree,
}

impl<'s> ParsedFile<'s> {
    /// Parse `source` with the grammar of `language` (None without a grammar).
    pub fn parse(language: Language, source: &'s [u8]) -> Option<ParsedFile<'s>> {
        let tree = crate::parse_tree(language, source).ok()?;
        Some(ParsedFile {
            language,
            source,
            tree,
        })
    }
    pub fn eval_argument(
        &self,
        callee: ByteSpan,
        arg: &ArgRef,
        constant: &dyn Fn(&str) -> Option<Tpl>,
    ) -> Option<ArgValue> {
        eval_argument(self.language, &self.tree, self.source, callee, arg, constant)
    }
    pub fn annotations(&self, decl: ByteSpan) -> Vec<Annotation> {
        annotations(self.language, &self.tree, self.source, decl)
    }
    pub fn call_span(&self, callee: ByteSpan) -> Option<ByteSpan> {
        call_span(self.language, &self.tree, self.source, callee)
    }
    pub fn applied_to(&self, callee: ByteSpan) -> Option<(ByteSpan, ByteSpan)> {
        applied_to(self.language, &self.tree, self.source, callee)
    }
    pub fn bound_name(&self, callee: ByteSpan) -> Option<String> {
        bound_name(self.language, &self.tree, self.source, callee)
    }
    pub fn exports(&self) -> Vec<Export> {
        exports(self.language, &self.tree, self.source)
    }
    pub fn call_strings(&self, callee: ByteSpan, constant: &dyn Fn(&str) -> Option<Tpl>) -> Vec<CallString> {
        call_strings(self.language, &self.tree, self.source, callee, constant)
    }
    /// Exact text of a span (lossy only for invalid UTF-8).
    pub fn text(&self, at: ByteSpan) -> String {
        let end = (at.end as usize).min(self.source.len());
        let start = (at.start as usize).min(end);
        String::from_utf8_lossy(&self.source[start..end]).into_owned()
    }
}

/// The call expression whose callee spans `callee` (the span of `CallSite::callee_span`).
/// Returns the call's whole span.
fn call_span(
    language: Language,
    tree: &tree_sitter::Tree,
    source: &[u8],
    callee: ByteSpan,
) -> Option<ByteSpan> {
    let spec = syntax(language)?;
    let facts = FileFacts::default();
    let lines = LineIndex::new(source);
    let cx = Cx::new(language, "", source, &lines, spec, &facts);
    cx.call_with_callee(tree.root_node(), callee).map(span)
}

/// The name the result of the call at `callee` is bound to (`x = f()`, `const x = f()`,
/// `x := f()`, `self.x = f()` -> `x`).
fn bound_name(
    language: Language,
    tree: &tree_sitter::Tree,
    source: &[u8],
    callee: ByteSpan,
) -> Option<String> {
    let spec = syntax(language)?;
    let facts = FileFacts::default();
    let lines = LineIndex::new(source);
    let cx = Cx::new(language, "", source, &lines, spec, &facts);
    let call = cx.call_with_callee(tree.root_node(), callee)?;
    cx.binding_name(call)
}

/// Span of the call that applies the call at `callee` to further arguments
/// (`route("/x")(handler)`: the outer call), with the span of its first argument.
fn applied_to(
    language: Language,
    tree: &tree_sitter::Tree,
    source: &[u8],
    callee: ByteSpan,
) -> Option<(ByteSpan, ByteSpan)> {
    let spec = syntax(language)?;
    let facts = FileFacts::default();
    let lines = LineIndex::new(source);
    let cx = Cx::new(language, "", source, &lines, spec, &facts);
    let inner = cx.call_with_callee(tree.root_node(), callee)?;
    let outer = inner.parent()?;
    let shape = spec.call_shape(outer.kind())?;
    let function = pick(outer, shape.function_field)?;
    if cx.unwrap(function).id() != inner.id() {
        return None;
    }
    let cv = cx.call_view(outer)?;
    let first = cv.positional(0)?;
    Some((span(outer), span(cx.unwrap(first))))
}

/// One exported binding of a module (JavaScript / TypeScript `export` statements): the
/// exported name (`default` for the default export) and the span of the exported value or
/// declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export {
    pub name: String,
    pub span: ByteSpan,
    /// Span of the declared name when the export declares a named function / class / const.
    pub name_span: Option<ByteSpan>,
}

/// Exports of an ECMAScript module (language syntax: `export default`, `export function f`,
/// `export const f = ...`, `export { a as b }`). Other languages: empty.
fn exports(language: Language, tree: &tree_sitter::Tree, source: &[u8]) -> Vec<Export> {
    if !in_family(language, Family::JavaScript) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for stmt in named_children(tree.root_node()) {
        if stmt.kind() != "export_statement" {
            continue;
        }
        let is_default = crate::node::has_direct_token(stmt, "default");
        if let Some(decl) = stmt.child_by_field_name("declaration") {
            let names: Vec<(Node<'_>, Node<'_>)> = match decl.kind() {
                "lexical_declaration" | "variable_declaration" => named_children(decl)
                    .into_iter()
                    .filter(|d| d.kind() == "variable_declarator")
                    .filter_map(|d| {
                        let n = d.child_by_field_name("name")?;
                        let v = d.child_by_field_name("value").unwrap_or(d);
                        Some((n, v))
                    })
                    .collect(),
                _ => decl
                    .child_by_field_name("name")
                    .map(|n| vec![(n, decl)])
                    .unwrap_or_default(),
            };
            for (n, v) in names {
                let name = if is_default {
                    "default".to_string()
                } else {
                    text(n, source).into_owned()
                };
                out.push(Export {
                    name,
                    span: span(v),
                    name_span: Some(span(n)),
                });
            }
            if is_default && decl.child_by_field_name("name").is_none() {
                out.push(Export {
                    name: "default".into(),
                    span: span(decl),
                    name_span: None,
                });
            }
            continue;
        }
        if is_default {
            if let Some(value) = stmt.child_by_field_name("value").or_else(|| {
                named_children(stmt)
                    .into_iter()
                    .find(|c| !c.kind().contains("comment") && c.kind() != "decorator")
            }) {
                let name_span = matches!(value.kind(), "identifier").then(|| span(value));
                out.push(Export {
                    name: "default".into(),
                    span: span(value),
                    name_span,
                });
            }
            continue;
        }
        // `export { a, b as c }` (without a `from` source: local bindings).
        if stmt.child_by_field_name("source").is_some() {
            continue;
        }
        for clause in named_children(stmt)
            .into_iter()
            .filter(|c| c.kind() == "export_clause")
        {
            for spec in named_children(clause)
                .into_iter()
                .filter(|c| c.kind() == "export_specifier")
            {
                let Some(local) = spec.child_by_field_name("name") else { continue };
                let alias = spec.child_by_field_name("alias").unwrap_or(local);
                out.push(Export {
                    name: text(alias, source).into_owned(),
                    span: span(local),
                    name_span: Some(span(local)),
                });
            }
        }
    }
    out.sort_by_key(|e| (e.span.start, e.name.clone()));
    out.dedup();
    out
}

/// A decorator / annotation / attribute on a declaration with its element values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Annotation {
    pub name: String,
    pub span: ByteSpan,
    pub elements: Vec<(Option<String>, Tpl)>,
    /// Source text of each element value (parallel to `elements`): the spelling of values
    /// that are not string templates (`RequestMethod.POST`).
    pub texts: Vec<String>,
}

/// Decorators / annotations / attributes on the declaration spanning `decl` (Java / Scala
/// annotations, C# attributes, Python / TypeScript decorators, Rust outer attributes), in
/// source order, with their element values: `(Some(key), value)` for named elements,
/// `(None, value)` for positional ones; an array element value gives one entry per item.
/// `name` is the annotation's path as written (`GetMapping`, `app.get`, `web::get`).
fn annotations(
    language: Language,
    tree: &tree_sitter::Tree,
    source: &[u8],
    decl: ByteSpan,
) -> Vec<Annotation> {
    let Some(spec) = syntax(language) else {
        return Vec::new();
    };
    let facts = FileFacts::default();
    let lines = LineIndex::new(source);
    let mut cx = Cx::new(language, "", source, &lines, spec, &facts);
    let root = tree.root_node();
    cx.collect_consts(root);
    let Some(node) = cx.declaration_node(root, decl) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for a in cx.annotation_nodes(node) {
        if let Some(annotation) = cx.annotation_value(a) {
            out.push(annotation);
        }
    }
    out
}

impl<'a> Cx<'a> {
    /// A same-file constant, else the caller's constant resolver, else unknown.
    pub(super) fn constant(&self, name: &str) -> RawTpl {
        if let Some(t) = self.consts.get(name) {
            return t.clone();
        }
        match self.external.and_then(|f| f(name)) {
            Some(t) => RawTpl::from_public(&t),
            None => RawTpl::unknown(),
        }
    }

    /// The call expression whose function part ends at the end of `callee` and which starts
    /// at or before it (the innermost such call).
    fn call_with_callee<'t>(&self, root: Node<'t>, callee: ByteSpan) -> Option<Node<'t>> {
        let end = (callee.end as usize).max(callee.start as usize);
        let mut node = root.descendant_for_byte_range(callee.start as usize, end)?;
        for _ in 0..12 {
            if let Some(shape) = self.spec.call_shape(node.kind()) {
                let function = pick(node, shape.function_field).or_else(|| {
                    (!shape.receiver_field.is_empty())
                        .then(|| pick(node, shape.receiver_field))
                        .flatten()
                });
                let fits = node.start_byte() as u32 <= callee.start
                    && function.is_some_and(|f| f.end_byte() as u32 == callee.end);
                if fits {
                    return Some(node);
                }
            }
            node = node.parent()?;
        }
        None
    }

    /// The value of one argument node.
    fn arg_value(&self, node: Node<'_>) -> ArgValue {
        let node = self.unwrap(node);
        let kind = node.kind();
        let callable = (self.spec.anonymous_functions.contains(&kind)
            || self.spec.is_lazy(kind)
            || is_name_kind(kind)
            || self.spec.is_identifier(kind)
            || self.member_fields(kind).is_some())
        .then(|| span(node));
        let raw = match self.list_items(node) {
            Some(items) => {
                let mut joined = RawTpl::default();
                for (i, item) in items.into_iter().enumerate() {
                    if i > 0 {
                        joined.push_lit(" ");
                    }
                    joined.append(self.eval(item, 0));
                }
                joined
            }
            None => self.eval(node, 0),
        };
        let template = (!raw.parts.is_empty() && !raw.parts.iter().all(|p| matches!(p, Part::Unknown)))
            .then(|| Tpl::from(&raw));
        ArgValue {
            span: span(node),
            template,
            callable,
        }
    }

    /// Items of a list / array / tuple literal (incl. Go composite literals).
    fn list_items<'t>(&self, node: Node<'t>) -> Option<Vec<Node<'t>>> {
        match node.kind() {
            "list"
            | "array"
            | "tuple"
            | "array_creation_expression"
            | "array_initializer"
            | "collection_expression"
            | "array_literal"
            | "list_literal"
            | "vector" => Some(
                named_children(node)
                    .into_iter()
                    .map(|c| {
                        if c.kind() == "array_element_initializer" {
                            nth_named(c, 0).unwrap_or(c)
                        } else {
                            c
                        }
                    })
                    .filter(|c| !c.kind().contains("comment"))
                    .collect(),
            ),
            "composite_literal" => {
                let body = node.child_by_field_name("body")?;
                Some(
                    named_children(body)
                        .into_iter()
                        .map(|c| {
                            if c.kind() == "literal_element" {
                                nth_named(c, 0).unwrap_or(c)
                            } else {
                                c
                            }
                        })
                        .collect(),
                )
            }
            _ => None,
        }
    }

    /// The declaration node spanning `decl` (exact span, else the innermost node starting at
    /// `decl.start` that encloses it).
    fn declaration_node<'t>(&self, root: Node<'t>, decl: ByteSpan) -> Option<Node<'t>> {
        let end = (decl.end as usize).max(decl.start as usize);
        let mut node = root.descendant_for_byte_range(decl.start as usize, end)?;
        for _ in 0..16 {
            if node.start_byte() as u32 <= decl.start && decl.end <= node.end_byte() as u32 && node.is_named()
            {
                return Some(node);
            }
            node = node.parent()?;
        }
        None
    }

    /// Annotation / attribute / decorator nodes attached to a declaration node.
    fn annotation_nodes<'t>(&self, decl: Node<'t>) -> Vec<Node<'t>> {
        let mut out = self.annotations_of(decl);
        // Scala keeps annotations in `modifiers` / `annotation` children.
        for child in named_children(decl) {
            match child.kind() {
                "annotation" | "marker_annotation" | "decorator" => out.push(child),
                "modifiers" => {
                    for a in named_children(child) {
                        if a.kind() == "annotation" || a.kind() == "decorator" {
                            out.push(a);
                        }
                    }
                }
                _ => {}
            }
        }
        // Python: `decorated_definition` holder.
        if let Some(holder) = decl.parent().filter(|p| p.kind() == "decorated_definition") {
            out.extend(named_children(holder).into_iter().filter(|c| c.kind() == "decorator"));
        }
        if decl.kind() == "decorated_definition" {
            out.extend(named_children(decl).into_iter().filter(|c| c.kind() == "decorator"));
        }
        // TypeScript class members / classes: preceding `decorator` siblings; Rust: preceding
        // `attribute_item`s.
        let mut sib = decl.prev_named_sibling();
        while let Some(s) = sib {
            match s.kind() {
                "decorator" | "attribute_item" => out.push(s),
                k if k.contains("comment") => {}
                _ => break,
            }
            sib = s.prev_named_sibling();
        }
        if let Some(p) = decl.parent().filter(|p| p.kind() == "export_statement") {
            out.extend(named_children(p).into_iter().filter(|c| c.kind() == "decorator"));
        }
        out.sort_by_key(|n| n.start_byte());
        out.dedup_by(|a, b| a.id() == b.id());
        out
    }

    /// One element value (array values: one entry per item).
    fn push_element(
        &self,
        elements: &mut Vec<(Option<String>, Tpl)>,
        texts: &mut Vec<String>,
        key: Option<String>,
        v: Node<'_>,
    ) {
        let v = self.unwrap(v);
        let items = self
            .list_items(v)
            .or_else(|| (v.kind() == "element_value_array_initializer").then(|| named_children(v)));
        match items {
            Some(items) => {
                for item in items {
                    elements.push((key.clone(), Tpl::from(&self.eval(item, 0))));
                    texts.push(self.txt(item).trim().to_string());
                }
            }
            None => {
                elements.push((key, Tpl::from(&self.eval(v, 0))));
                texts.push(self.txt(v).trim().to_string());
            }
        }
    }

    /// Name and element values of one annotation / attribute / decorator node.
    fn annotation_value(&self, a: Node<'_>) -> Option<Annotation> {
        let mut elements: Vec<(Option<String>, Tpl)> = Vec::new();
        let mut texts: Vec<String> = Vec::new();
        let name = match a.kind() {
            "annotation" | "marker_annotation" | "attribute" => {
                let name = a
                    .child_by_field_name("name")
                    .or_else(|| nth_named(a, 0))
                    .map(|n| {
                        self.path_of(n)
                            .into_iter()
                            .filter(|s| s != "?" && s != "()")
                            .collect::<Vec<_>>()
                            .join(".")
                    })
                    .unwrap_or_default();
                // An annotation that wraps a constructor invocation (`@Get("/x")`).
                if let Some(call) = named_children(a)
                    .into_iter()
                    .find(|c| self.spec.call_shape(c.kind()).is_some())
                {
                    if let Some(cv) = self.call_view(call) {
                        for arg in &cv.args {
                            self.push_element(&mut elements, &mut texts, arg.key.clone(), arg.value);
                        }
                        let n = cv
                            .path
                            .iter()
                            .filter(|s| *s != "()" && *s != "new")
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(".");
                        return (!n.is_empty()).then_some(Annotation {
                            name: n,
                            span: span(a),
                            elements,
                            texts,
                        });
                    }
                }
                for (key, v) in self.annotation_args(a) {
                    self.push_element(&mut elements, &mut texts, key, v);
                }
                name
            }
            "decorator" => {
                let expr = nth_named(a, 0)?;
                let expr = self.unwrap(expr);
                match self.call_view(expr) {
                    Some(cv) => {
                        for arg in &cv.args {
                            self.push_element(&mut elements, &mut texts, arg.key.clone(), arg.value);
                        }
                        cv.path
                            .iter()
                            .filter(|s| *s != "()" && *s != "new")
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(".")
                    }
                    None => self.path_of(expr).join("."),
                }
            }
            "attribute_item" => {
                let attr = nth_named(a, 0)?;
                let path = nth_named(attr, 0).map(|p| self.path_of(p)).unwrap_or_default();
                if let Some(tt) = attr.child_by_field_name("arguments") {
                    let mut cursor = tt.walk();
                    let kids: Vec<Node<'_>> = tt.children(&mut cursor).collect();
                    let mut i = 0;
                    while i < kids.len() {
                        let k = kids[i];
                        if k.kind() == "identifier" && kids.get(i + 1).is_some_and(|n| n.kind() == "=") {
                            if let Some(v) = kids.get(i + 2) {
                                elements.push((Some(self.txt(k)), Tpl::from(&self.eval(*v, 0))));
                                texts.push(self.txt(*v));
                                i += 3;
                                continue;
                            }
                        }
                        if is_string_kind(k.kind()) {
                            elements.push((None, Tpl::from(&self.eval(k, 0))));
                            texts.push(self.txt(k));
                        }
                        i += 1;
                    }
                }
                if let Some(v) = attr.child_by_field_name("value") {
                    elements.push((None, Tpl::from(&self.eval(v, 0))));
                    texts.push(self.txt(v));
                }
                path.join("::")
            }
            _ => return None,
        };
        (!name.is_empty()).then_some(Annotation {
            name,
            span: span(a),
            elements,
            texts,
        })
    }
}
