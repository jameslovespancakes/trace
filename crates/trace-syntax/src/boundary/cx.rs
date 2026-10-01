//! The per-file context: construction, convention rows, fact recording, structure
//! (unwrapping, paths, call views, owners) and expression evaluation.

use std::cell::RefCell;
use std::collections::HashMap;

use trace_core::facts::{BoundaryFact, BoundaryRole, FileFacts};
use trace_core::languages::{in_family, Family};
use trace_core::model::{BridgeKind, ByteSpan};
use trace_core::text::LineIndex;
use trace_core::Language;
use tree_sitter::Node;

use super::{
    conventions::conventions, conventions::last_segment, conventions::placeholder_in,
    conventions::Convention, literals::decode_escape, literals::is_content_kind, literals::is_hole_kind,
    literals::is_name_kind, literals::is_string_kind, literals::member_fields_fallback,
    literals::strip_delimiters, template::Part, template::RawTpl, ArgView, Bind, CallView, Cx, MAX_DEPTH,
    MAX_FACTS,
};
use crate::node::{named_children, nth_named, pick, text};
use crate::spec::SyntaxSpec;

impl<'a> Cx<'a> {
    pub(super) fn new(
        lang: Language,
        path: &'a str,
        src: &'a [u8],
        lines: &'a LineIndex,
        spec: &'static SyntaxSpec,
        facts: &'a FileFacts,
    ) -> Self {
        let mut decl_by_name = HashMap::new();
        let mut decl_by_span = HashMap::new();
        let mut decl_by_start = HashMap::new();
        let mut callable_by_name: HashMap<String, Vec<u32>> = HashMap::new();
        for (i, d) in facts.declarations.iter().enumerate() {
            let i = i as u32;
            if facts.module_decl == Some(i) {
                continue;
            }
            decl_by_name.entry(d.name_span.start).or_insert(i);
            decl_by_span
                .entry((d.span.bytes.start, d.span.bytes.end))
                .or_insert(i);
            decl_by_start.entry(d.span.bytes.start).or_insert(i);
            if d.kind.is_callable() && !d.name.starts_with('<') {
                callable_by_name.entry(d.name.clone()).or_default().push(i);
            }
        }
        let call_at = facts
            .calls
            .iter()
            .enumerate()
            .map(|(i, c)| ((c.span.start, c.span.end), i))
            .collect();
        let import_locals = facts.imports.iter().map(|i| i.local.clone()).collect();
        Cx {
            lang,
            path,
            src,
            lines,
            spec,
            facts,
            conv: conventions(lang),
            call_at,
            decl_by_name,
            decl_by_span,
            decl_by_start,
            callable_by_name,
            import_locals,
            consts: HashMap::new(),
            binds: HashMap::new(),
            cgo_includes: Vec::new(),
            out: RefCell::new(Vec::new()),
            external: None,
        }
    }

    pub(super) fn is_js(&self) -> bool {
        in_family(self.lang, Family::JavaScript)
    }

    // ---- syntax convention rows ---------------------------------------------------------

    /// Rows of `rule` (any bridge).
    pub(super) fn rows<'s>(&'s self, rule: &'s str) -> impl Iterator<Item = &'static Convention> + 's {
        self.conv.iter().copied().filter(move |r| r.rule == rule)
    }

    /// Rows of `rule` for `bridge`.
    pub(super) fn rows_of<'s>(
        &'s self,
        rule: &'s str,
        bridge: BridgeKind,
    ) -> impl Iterator<Item = &'static Convention> + 's {
        self.rows(rule).filter(move |r| r.bridge == Some(bridge))
    }

    /// Symbols of the rows of `rule` for `bridge`, in table order.
    pub(super) fn symbols_of(&self, rule: &str, bridge: BridgeKind) -> Vec<&'static str> {
        self.rows_of(rule, bridge)
            .filter_map(|r| r.symbol.as_deref())
            .collect()
    }

    /// Whether a row of `rule` for `bridge` names `name` by its symbol's last segment.
    pub(super) fn names(&self, rule: &str, bridge: BridgeKind, name: &str) -> bool {
        self.rows_of(rule, bridge)
            .filter_map(|r| r.symbol.as_deref())
            .any(|s| last_segment(s) == name)
    }

    /// The service a generated name stands for under the `<Service>` patterns of `rule`.
    pub(super) fn service_of(&self, rule: &str, name: &str) -> Option<String> {
        self.rows_of(rule, BridgeKind::Grpc)
            .filter_map(|r| r.pattern.as_deref())
            .find_map(|p| placeholder_in(p, "<Service>", name))
    }

    pub(super) fn txt(&self, node: Node<'_>) -> String {
        text(node, self.src).into_owned()
    }

    pub(super) fn name_text(&self, node: Node<'_>) -> String {
        let t = text(node, self.src);
        t.trim().trim_start_matches(['$', '@', '#']).to_string()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn push(
        &self,
        kind: BridgeKind,
        role: BoundaryRole,
        name: String,
        owner: Option<u32>,
        decl: Option<u32>,
        at: ByteSpan,
        detail: Vec<(String, String)>,
    ) {
        let mut out = self.out.borrow_mut();
        if out.len() >= MAX_FACTS || name.len() > 512 {
            return;
        }
        out.push(BoundaryFact {
            kind,
            role,
            name,
            owner,
            decl,
            span: at,
            line: self.lines.line1(at.start),
            detail,
        });
    }

    // ---- structure --------------------------------------------------------------------

    pub(super) fn unwrap<'t>(&self, mut node: Node<'t>) -> Node<'t> {
        for _ in 0..8 {
            let kind = node.kind();
            let next = if let Some(p) = self.spec.unwrap_pick(kind) {
                pick(node, p)
            } else {
                match kind {
                    "parenthesized_expression"
                    | "non_null_expression"
                    | "as_expression"
                    | "satisfies_expression"
                    | "type_assertion"
                    | "expression_statement" => nth_named(node, 0),
                    "argument" | "value_argument" | "attribute_argument" => nth_named(node, -1),
                    _ => None,
                }
            };
            match next {
                Some(n) if n.id() != node.id() => node = n,
                _ => break,
            }
        }
        node
    }

    pub(super) fn member_fields(&self, kind: &str) -> Option<(&'static str, &'static str)> {
        self.spec
            .member(kind)
            .map(|m| (m.object_field, m.property_field))
            .or_else(|| member_fields_fallback(kind))
    }

    /// Dotted segments of an expression (names, member accesses, calls as `()`).
    pub(super) fn segs(&self, node: Node<'_>, depth: u32, out: &mut Vec<String>) {
        if depth > 14 || out.len() > 16 {
            out.push("?".into());
            return;
        }
        let node = self.unwrap(node);
        let kind = node.kind();
        if let Some((o, p)) = self.member_fields(kind) {
            match (pick(node, o), pick(node, p)) {
                (Some(obj), Some(prop)) if obj.id() != prop.id() => {
                    self.segs(obj, depth + 1, out);
                    out.push(self.name_text(prop));
                    return;
                }
                (None, Some(prop)) => {
                    out.push(self.name_text(prop));
                    return;
                }
                _ => {}
            }
        }
        if kind.contains("scoped") || kind.contains("qualified") {
            // Scoped names without field labels (Java `A.B` types): every named part.
            let parts = named_children(node);
            if !parts.is_empty() {
                for p in parts {
                    self.segs(p, depth + 1, out);
                }
                return;
            }
        }
        if let Some(shape) = self.spec.call_shape(kind) {
            if shape.is_new {
                out.push("new".into());
                if let Some(t) = pick(node, shape.function_field) {
                    self.segs(t, depth + 1, out);
                }
            } else {
                if !shape.receiver_field.is_empty() {
                    if let Some(r) = pick(node, shape.receiver_field) {
                        self.segs(r, depth + 1, out);
                    }
                }
                if let Some(f) = pick(node, shape.function_field) {
                    self.segs(f, depth + 1, out);
                }
            }
            out.push("()".into());
            return;
        }
        match kind {
            "generic_function" | "generic_type" => {
                match node
                    .child_by_field_name("function")
                    .or_else(|| node.child_by_field_name("type"))
                {
                    Some(f) => self.segs(f, depth + 1, out),
                    None => out.push("?".into()),
                }
            }
            "generic_name" => match nth_named(node, 0) {
                Some(n) => out.push(self.name_text(n)),
                None => out.push("?".into()),
            },
            "macro_invocation" => match node.child_by_field_name("macro") {
                Some(m) => {
                    self.segs(m, depth + 1, out);
                    out.push("!".into());
                }
                None => out.push("?".into()),
            },
            _ if is_name_kind(kind) || self.spec.is_name_like(kind) => out.push(self.name_text(node)),
            _ => out.push("?".into()),
        }
    }

    pub(super) fn path_of(&self, node: Node<'_>) -> Vec<String> {
        let mut out = Vec::new();
        self.segs(node, 0, &mut out);
        out
    }

    pub(super) fn call_view<'t>(&self, node: Node<'t>) -> Option<CallView<'t>> {
        let shape = self.spec.call_shape(node.kind())?;
        let callee = pick(node, shape.function_field);
        let receiver_field = (!shape.receiver_field.is_empty())
            .then(|| pick(node, shape.receiver_field))
            .flatten();
        let mut path = Vec::new();
        if shape.is_new {
            path.push("new".to_string());
        }
        if let Some(r) = receiver_field {
            self.segs(r, 0, &mut path);
        }
        match callee {
            Some(c) => self.segs(c, 0, &mut path),
            None if receiver_field.is_none() => return None,
            None => {}
        }
        let member = path.last().cloned().unwrap_or_default();
        let receiver = receiver_field.or_else(|| {
            let c = self.unwrap(callee?);
            let (o, _) = self.member_fields(c.kind())?;
            pick(c, o)
        });
        let mut args = Vec::new();
        let args_node = (!shape.arguments_field.is_empty())
            .then(|| pick(node, shape.arguments_field))
            .flatten();
        if let Some(a) = args_node {
            if is_string_kind(a.kind()) {
                args.push(ArgView { key: None, value: a });
            } else {
                for child in named_children(a) {
                    let kind = child.kind();
                    if self.spec.is_comment(kind) || kind.contains("comment") {
                        continue;
                    }
                    if let Some(kw) = self.spec.keyword_arguments.iter().find(|k| k.kind == kind) {
                        let key = pick(child, kw.first).map(|k| self.key_text(k));
                        match pick(child, kw.second) {
                            Some(v) => args.push(ArgView { key, value: v }),
                            None => continue,
                        }
                        continue;
                    }
                    if matches!(kind, "argument" | "value_argument")
                        || self.spec.argument_wrappers.contains(&kind)
                    {
                        let key = child.child_by_field_name("name").map(|n| self.name_text(n));
                        if let Some(v) = nth_named(child, -1) {
                            let v = if key.is_some()
                                && Some(v.id()) == child.child_by_field_name("name").map(|n| n.id())
                            {
                                continue;
                            } else {
                                v
                            };
                            args.push(ArgView { key, value: v });
                        }
                        continue;
                    }
                    args.push(ArgView {
                        key: None,
                        value: child,
                    });
                }
            }
        }
        let idx = self
            .call_at
            .get(&(node.start_byte() as u32, node.end_byte() as u32))
            .copied();
        let owner = match idx {
            Some(i) => self.facts.calls[i].owner,
            None => self.owner_at(node.start_byte() as u32),
        };
        Some(CallView {
            node,
            receiver,
            path,
            member,
            args,
            is_new: shape.is_new,
            owner,
        })
    }

    pub(super) fn key_text(&self, node: Node<'_>) -> String {
        let t = self.eval(node, 0);
        match t.plain() {
            Some(s) if is_string_kind(node.kind()) || node.kind().contains("symbol") => {
                s.trim_start_matches(':').to_string()
            }
            _ => self
                .name_text(node)
                .trim_start_matches(':')
                .trim_end_matches(':')
                .to_string(),
        }
    }

    /// Innermost executable (callable, incl. synthetic lambda) declaration whose body
    /// contains `byte`; `None` = module / class level.
    pub(super) fn owner_at(&self, byte: u32) -> Option<u32> {
        let mut best: Option<(u32, u32)> = None;
        for (i, d) in self.facts.declarations.iter().enumerate() {
            let i = i as u32;
            if self.facts.module_decl == Some(i) || !d.kind.is_callable() {
                continue;
            }
            if d.span.bytes.contains(byte) && byte >= d.body_start {
                let len = d.span.bytes.len();
                if best.is_none_or(|(_, l)| len < l) {
                    best = Some((i, len));
                }
            }
        }
        best.map(|(i, _)| i)
    }

    /// Declaration of a function-literal / declaration node.
    pub(super) fn decl_of_node(&self, node: Node<'_>) -> Option<u32> {
        if let Some(name) = node.child_by_field_name("name") {
            if let Some(&d) = self.decl_by_name.get(&(name.start_byte() as u32)) {
                return Some(d);
            }
        }
        let key = (node.start_byte() as u32, node.end_byte() as u32);
        self.decl_by_span
            .get(&key)
            .or_else(|| self.decl_by_start.get(&key.0))
            .copied()
    }

    /// Handler argument of a registration: declaration (function literal / same-file
    /// declaration) plus `handler*` details for trace-bridge.
    pub(super) fn handler_info(&self, node: Node<'_>, detail: &mut Vec<(String, String)>) -> Option<u32> {
        let node = self.unwrap(node);
        let kind = node.kind();
        detail.push(("handler_span".into(), format!("{}:{}", node.start_byte(), node.end_byte())));
        if self.spec.anonymous_functions.contains(&kind) || self.spec.is_lazy(kind) {
            return self.decl_of_node(node);
        }
        if is_string_kind(kind) {
            return None;
        }
        let mut path = self.path_of(node);
        // A method called on the handler (`handler.bind(this)`) -> the handler itself.
        if path.last().is_some_and(|s| s == "()") {
            path.pop();
            path.pop();
        }
        path.retain(|s| s != "new");
        if path.is_empty() || path.iter().any(|s| s == "?" || s == "()") {
            return None;
        }
        let joined = path.join(".");
        detail.push(("handler".into(), joined));
        if path.len() == 1 {
            if let Some(ds) = self.callable_by_name.get(&path[0]) {
                if ds.len() == 1 {
                    return Some(ds[0]);
                }
            }
        }
        None
    }

    /// Name a call result is bound to (`x = f()`, `const x = f()`, `x := f()`,
    /// `with f() as x`, `self.x = f()`).
    pub(super) fn binding_name(&self, node: Node<'_>) -> Option<String> {
        let mut cur = node;
        for _ in 0..6 {
            let parent = cur.parent()?;
            let kind = parent.kind();
            let target = match kind {
                "await_expression"
                | "await"
                | "parenthesized_expression"
                | "try_expression"
                | "unary_expression"
                | "expression_list"
                | "non_null_expression"
                | "as_expression"
                | "equals_value_clause"
                | "cast_expression" => {
                    cur = parent;
                    continue;
                }
                "assignment" | "assignment_expression" | "assignment_statement" | "short_var_declaration" => {
                    parent.child_by_field_name("left")
                }
                "variable_declarator"
                | "public_field_definition"
                | "field_definition"
                | "var_spec"
                | "const_spec"
                | "property_declaration" => {
                    parent.child_by_field_name("name").or_else(|| nth_named(parent, 0))
                }
                "let_declaration" => parent.child_by_field_name("pattern"),
                "as_pattern" => parent.child_by_field_name("alias"),
                "init_declarator" => parent.child_by_field_name("declarator"),
                _ => return None,
            }?;
            // The call must be the bound value, not part of the target.
            if target.start_byte() <= cur.start_byte() && cur.end_byte() <= target.end_byte() {
                return None;
            }
            let target = if matches!(target.kind(), "expression_list" | "pattern_list" | "as_pattern_target")
            {
                nth_named(target, 0)?
            } else {
                target
            };
            let segs = self.path_of(target);
            return segs
                .into_iter()
                .rev()
                .find(|s| s != "?" && s != "()")
                .filter(|s| !s.is_empty());
        }
        None
    }

    pub(super) fn bind_of_receiver(&self, cv: &CallView<'_>) -> Option<(&String, &Bind)> {
        let recv = cv.receiver_path();
        let last = recv.iter().rev().find(|s| *s != "?")?;
        if recv.last().is_some_and(|s| s == "()") {
            return None;
        }
        self.binds.get_key_value(last)
    }

    // ---- expression evaluation ---------------------------------------------------------

    pub(super) fn eval(&self, node: Node<'_>, depth: u32) -> RawTpl {
        if depth > MAX_DEPTH {
            return RawTpl::unknown();
        }
        let node = self.unwrap(node);
        let kind = node.kind();
        if is_string_kind(kind) {
            return self.string_tpl(node, depth);
        }
        if kind == "simple_symbol" || kind == "hash_key_symbol" || kind == "symbol" {
            return RawTpl::lit(self.txt(node).trim_start_matches(':').trim_end_matches(':'));
        }
        if matches!(kind, "binary_expression" | "binary_operator" | "additive_expression" | "string_concat") {
            let op = node
                .child_by_field_name("operator")
                .map(|o| self.txt(o))
                .unwrap_or_else(|| {
                    let mut cursor = node.walk();
                    let found = node
                        .children(&mut cursor)
                        .find(|c| !c.is_named())
                        .map(|c| self.txt(c))
                        .unwrap_or_default();
                    found
                });
            let left = node.child_by_field_name("left").or_else(|| nth_named(node, 0));
            let right = node.child_by_field_name("right").or_else(|| nth_named(node, -1));
            return match (op.trim(), left, right) {
                ("+" | "." | "..", Some(l), Some(r)) => {
                    self.eval(l, depth + 1).concat(self.eval(r, depth + 1))
                }
                ("%", Some(l), _) => self.eval(l, depth + 1),
                _ => RawTpl::unknown(),
            };
        }
        if self.spec.call_shape(kind).is_some() {
            if let Some(cv) = self.call_view(node) {
                let joined = cv.path.join(".");
                if cv.member == "format" {
                    if let Some(r) = cv.receiver {
                        let r = self.unwrap(r);
                        if is_string_kind(r.kind()) {
                            return self.eval(r, depth + 1);
                        }
                    }
                }
                if matches!(
                    joined.as_str(),
                    "fmt.Sprintf" | "String.format" | "string.Format" | "sprintf" | "format"
                ) {
                    if let Some(first) = cv.positional(0) {
                        return self.eval(first, depth + 1);
                    }
                }
            }
            return RawTpl::unknown();
        }
        if kind == "macro_invocation" {
            let is_format = node
                .child_by_field_name("macro")
                .is_some_and(|m| matches!(self.txt(m).as_str(), "format" | "format_args" | "concat"));
            if is_format {
                if let Some(tt) = crate::node::find_descendant(node, 64, |n| is_string_kind(n.kind())) {
                    return self.eval(tt, depth + 1);
                }
            }
            return RawTpl::unknown();
        }
        if is_name_kind(kind) || self.spec.is_identifier(kind) {
            let name = self.name_text(node);
            return self.constant(&name);
        }
        if self.member_fields(kind).is_some() {
            let joined = self.path_of(node).join(".");
            return self.constant(&joined);
        }
        RawTpl::unknown()
    }

    fn string_tpl(&self, node: Node<'_>, depth: u32) -> RawTpl {
        let mut tpl = RawTpl::default();
        if node.kind() == "concatenated_string" {
            for child in named_children(node) {
                tpl.append(self.eval(child, depth + 1));
            }
            return tpl;
        }
        let mut recognized = false;
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
        for child in children {
            let kind = child.kind();
            if is_content_kind(kind) {
                recognized = true;
                let raw = self.txt(child);
                if kind == "escape_sequence" {
                    tpl.push_lit(&decode_escape(&raw));
                } else {
                    tpl.push_lit(&raw);
                }
            } else if is_hole_kind(kind) {
                recognized = true;
                let inner = named_children(child)
                    .into_iter()
                    .find(|c| !c.kind().contains("format") && !c.kind().contains("specifier"));
                let value = inner.map(|n| self.eval(n, depth + 1));
                match value {
                    Some(v) if v.is_literal() && v.has_text() => tpl.append(v),
                    _ if tpl.parts.is_empty() => tpl.parts.push(Part::Unknown),
                    _ => tpl.parts.push(Part::Hole),
                }
            } else if child.is_named() && is_string_kind(kind) {
                recognized = true;
                tpl.append(self.string_tpl(child, depth + 1));
            }
        }
        if !recognized {
            let raw = self.txt(node);
            let inner = strip_delimiters(&raw);
            if !inner.is_empty() {
                tpl.push_lit(inner);
            } else {
                tpl.push_lit("");
            }
        }
        if tpl.parts.is_empty() {
            tpl.push_lit("");
        }
        tpl
    }

    /// Value of `key` inside an object / dict / hash literal.
    /// The (key, value) entries of an object / dictionary literal (empty for other nodes).
    pub(super) fn object_entries<'t>(&self, node: Node<'t>) -> Vec<(String, Node<'t>)> {
        let node = self.unwrap(node);
        if !matches!(node.kind(), "object" | "dictionary" | "hash" | "object_literal") {
            return Vec::new();
        }
        let mut out = Vec::new();
        for child in named_children(node) {
            if child.kind() != "pair" {
                continue;
            }
            let k = child.child_by_field_name("key").or_else(|| nth_named(child, 0));
            let v = child.child_by_field_name("value").or_else(|| nth_named(child, -1));
            if let (Some(k), Some(v)) = (k, v) {
                if k.id() != v.id() {
                    out.push((self.key_text(k), v));
                }
            }
        }
        out
    }

    pub(super) fn object_value<'t>(&self, node: Node<'t>, keys: &[String]) -> Option<Node<'t>> {
        let node = self.unwrap(node);
        if !matches!(
            node.kind(),
            "object"
                | "dictionary"
                | "hash"
                | "object_literal"
                | "anonymous_object_creation_expression"
                | "array_creation_expression"
                | "initializer_expression"
                | "object_creation_expression"
        ) {
            return None;
        }
        let mut stack = named_children(node);
        stack.reverse();
        while let Some(child) = stack.pop() {
            match child.kind() {
                "pair"
                | "array_element_initializer"
                | "assignment_expression"
                | "anonymous_object_member_declarator"
                | "initializer_pair" => {
                    let k = child
                        .child_by_field_name("key")
                        .or_else(|| child.child_by_field_name("left"))
                        .or_else(|| nth_named(child, 0));
                    let v = child
                        .child_by_field_name("value")
                        .or_else(|| child.child_by_field_name("right"))
                        .or_else(|| nth_named(child, -1));
                    if let (Some(k), Some(v)) = (k, v) {
                        if k.id() != v.id() && keys.iter().any(|x| *x == self.key_text(k)) {
                            return Some(v);
                        }
                    }
                }
                "shorthand_property_identifier" => {}
                "initializer_list" | "object_initializer" => {
                    let mut inner = named_children(child);
                    inner.reverse();
                    stack.extend(inner);
                }
                _ => {}
            }
        }
        None
    }
}
