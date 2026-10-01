//! The prepass (same-file constants and bindings) and the main walk dispatching calls and
//! declarations to the per-concern readers.

use std::collections::HashSet;

use trace_core::facts::BoundaryRole;
use trace_core::model::BridgeKind;
use trace_core::Language;
use tree_sitter::Node;

use super::{
    conventions::last_segment, literals::is_name_kind, Bind, CallView, Cx, CGO_PSEUDO, MAX_NODES,
    NODE_ADDON_EXTENSION,
};
use crate::node::{named_children, nth_named, pick, span};

impl<'a> Cx<'a> {
    pub(super) fn prepare(&mut self, root: Node<'_>) {
        self.collect_consts(root);
        let mut stack = vec![root];
        let mut seen = 0usize;
        let mut calls = Vec::new();
        while let Some(node) = stack.pop() {
            seen += 1;
            if seen > MAX_NODES {
                break;
            }
            if self.spec.call_shape(node.kind()).is_some() {
                calls.push(node);
            }
            let mut cursor = node.walk();
            let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
            stack.extend(children.into_iter().rev());
        }
        // Source order, so a binding is recorded before the calls that use it.
        calls.sort_by_key(|n| n.start_byte());
        for node in calls {
            if let Some(cv) = self.call_view(node) {
                self.binding_call(&cv);
            }
        }
    }

    /// Module-level (and Java/C# static field) string constants, evaluated in source order.
    pub(super) fn collect_consts(&mut self, root: Node<'_>) {
        let mut rebound: HashSet<String> = HashSet::new();
        let mut stack: Vec<(Node<'_>, u32)> =
            named_children(root).into_iter().rev().map(|n| (n, 0)).collect();
        let mut pairs: Vec<(String, Node<'_>)> = Vec::new();
        while let Some((node, depth)) = stack.pop() {
            let kind = node.kind();
            if depth > 5 || self.spec.is_lazy(kind) || self.spec.anonymous_functions.contains(&kind) {
                continue;
            }
            let pair = match kind {
                "assignment" | "assignment_expression" => {
                    match (node.child_by_field_name("left"), node.child_by_field_name("right")) {
                        (Some(l), Some(r)) if is_name_kind(l.kind()) => Some((self.name_text(l), r)),
                        _ => None,
                    }
                }
                "variable_declarator"
                | "const_spec"
                | "var_spec"
                | "const_item"
                | "static_item"
                | "const_declaration_item" => {
                    let name = node.child_by_field_name("name").or_else(|| nth_named(node, 0));
                    let value = node.child_by_field_name("value").or_else(|| {
                        let kids = named_children(node);
                        (kids.len() >= 2).then(|| kids[kids.len() - 1])
                    });
                    match (name, value) {
                        (Some(n), Some(v)) if n.id() != v.id() => Some((self.name_text(n), v)),
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some((name, value)) = pair {
                let value = if value.kind() == "expression_list" {
                    match nth_named(value, 0) {
                        Some(v) => v,
                        None => continue,
                    }
                } else {
                    value
                };
                pairs.push((name, value));
                continue;
            }
            let descend = matches!(
                kind,
                "expression_statement"
                    | "lexical_declaration"
                    | "variable_declaration"
                    | "export_statement"
                    | "const_declaration"
                    | "var_declaration"
                    | "field_declaration"
                    | "class_body"
                    | "declaration_list"
                    | "class_declaration"
                    | "program"
                    | "module"
                    | "source_file"
                    | "compilation_unit"
                    | "namespace_declaration"
                    | "file_scoped_namespace_declaration"
                    | "local_declaration_statement"
                    | "property_declaration"
                    | "const_declaration_list"
                    | "body_statement"
                    | "class_definition"
                    | "block"
                    | "statement_list"
            ) && !(kind == "block" && self.lang != Language::Python);
            if descend {
                for child in named_children(node).into_iter().rev() {
                    stack.push((child, depth + 1));
                }
            }
        }
        let mut seen_names: HashSet<String> = HashSet::new();
        for (name, _) in &pairs {
            if !seen_names.insert(name.clone()) {
                rebound.insert(name.clone());
            }
        }
        pairs.sort_by_key(|(_, v)| v.start_byte());
        for (name, value) in pairs {
            if rebound.contains(&name) {
                continue;
            }
            let tpl = self.eval(value, 0);
            if tpl.is_literal() && tpl.has_text() {
                self.consts.insert(name, tpl);
            }
        }
    }

    /// Record instance bindings created by calls (generated RPC stubs, loaded addons,
    /// GraphQL type objects).
    fn binding_call(&mut self, cv: &CallView<'_>) {
        let Some(name) = self.binding_name(cv.node) else { return };
        if let Some(service) = self.grpc_stub_service(cv) {
            self.binds.insert(name, Bind::Stub { service });
            return;
        }
        if self.is_js() {
            if let Some(module) = self.addon_module(cv) {
                self.binds.insert(name, Bind::Addon { module });
                return;
            }
        }
        if let Some(type_name) = self.graphql_type_object(cv) {
            self.binds.insert(name, Bind::GraphqlType { type_name });
        }
    }

    /// The GraphQL type a constructed type object holds resolvers for: a
    /// `graphql_root_type_object` row (its `value`), or a `graphql_type_object` row (the type
    /// named by the first argument).
    fn graphql_type_object(&self, cv: &CallView<'_>) -> Option<String> {
        let root = self
            .rows_of("graphql_root_type_object", BridgeKind::Graphql)
            .find(|r| r.symbol.as_deref().is_some_and(|s| last_segment(s) == cv.member))
            .and_then(|r| r.value.clone());
        if root.is_some() {
            return root;
        }
        if self.names("graphql_type_object", BridgeKind::Graphql, &cv.member) {
            return cv.positional(0).and_then(|n| self.eval(n, 0).plain());
        }
        None
    }

    /// The module a JavaScript `require` loads as a compiled Node-API addon: a `.node` file
    /// (Node's own rule), or the addon an `addon_loader` row's package loads
    /// (`require(<loader>)(<name>)`; the name may be absent).
    fn addon_module(&self, cv: &CallView<'_>) -> Option<String> {
        if cv.path == ["require"] {
            let spec = cv.positional(0).and_then(|n| self.eval(n, 0).plain())?;
            if spec.ends_with(NODE_ADDON_EXTENSION) {
                let stem = spec.rsplit('/').next().unwrap_or(&spec);
                return Some(stem.trim_end_matches(NODE_ADDON_EXTENSION).to_string());
            }
            return None;
        }
        let callee = pick(cv.node, "function")?;
        let inner = self.call_view(self.unwrap(callee))?;
        if inner.path == ["require"] {
            let spec = inner.positional(0).and_then(|n| self.eval(n, 0).plain())?;
            if self
                .symbols_of("addon_loader", BridgeKind::Napi)
                .iter()
                .any(|l| *l == spec)
            {
                return cv
                    .positional(0)
                    .and_then(|n| self.eval(n, 0).plain())
                    .or(Some(String::new()));
            }
        }
        None
    }

    /// The service of a generated RPC client created by this call, per the language's
    /// generated-code shape; the names come from the `rpc_*` rows.
    pub(super) fn grpc_stub_service(&self, cv: &CallView<'_>) -> Option<String> {
        let segs: Vec<&str> = cv
            .path
            .iter()
            .map(String::as_str)
            .filter(|s| *s != "()" && *s != "new")
            .collect();
        let last = *segs.last()?;
        match self.lang {
            Language::Python => {
                // `pb2_grpc.<Service>Stub(channel)`.
                let service = self.service_of("rpc_client_type", last)?;
                (!cv.is_new).then_some(service)
            }
            // `pb.New<Service>Client(conn)`.
            Language::Go => self.service_of("rpc_client_constructor", last),
            Language::Java => {
                // `<Service>Grpc.newBlockingStub(channel)`.
                if segs.len() < 2 || !self.names("rpc_stub_factory", BridgeKind::Grpc, last) {
                    return None;
                }
                self.service_of("rpc_stub_class", segs[segs.len() - 2])
            }
            Language::CSharp => {
                if !cv.is_new {
                    return None;
                }
                let service = self.service_of("rpc_client_type", last)?;
                // `new Svc.SvcClient(channel)`, or the nested client class imported with
                // `using static Svc;` (`new SvcClient(channel)`): the generated client takes
                // the channel (or call invoker) as its only argument.
                let qualified = segs.len() >= 2 && segs[segs.len() - 2] == service;
                let unqualified = segs.len() == 1 && cv.positional_count() == 1;
                (qualified || unqualified).then_some(service)
            }
            Language::JavaScript | Language::TypeScript | Language::Tsx => {
                // `new <Service>Client(address, credentials)`.
                if !cv.is_new || cv.positional_count() < 2 {
                    return None;
                }
                let creds = cv.positional(1)?;
                let spelled = self.path_of(creds);
                if !spelled
                    .iter()
                    .any(|s| self.names("rpc_credentials", BridgeKind::Grpc, s))
                {
                    return None;
                }
                let service = self
                    .service_of("rpc_client_type", last)
                    .unwrap_or_else(|| last.to_string());
                (!service.is_empty()).then_some(service)
            }
            _ => None,
        }
    }

    // ---- main walk ----------------------------------------------------------------------

    pub(super) fn walk(&mut self, root: Node<'_>) {
        let mut stack = vec![root];
        let mut seen = 0usize;
        while let Some(node) = stack.pop() {
            seen += 1;
            if seen > MAX_NODES {
                break;
            }
            self.visit(node);
            let mut cursor = node.walk();
            let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
            stack.extend(children.into_iter().rev());
        }
    }

    fn visit(&mut self, node: Node<'_>) {
        let kind = node.kind();
        if self.spec.call_shape(kind).is_some() {
            if let Some(cv) = self.call_view(node) {
                self.on_call(&cv);
            }
        }
        match self.lang {
            Language::Rust => match kind {
                "function_item" => self.rust_function(node),
                "function_signature_item" => self.rust_foreign_fn(node),
                "struct_item" | "enum_item" => self.rust_type(node),
                _ => {}
            },
            Language::C | Language::Cpp => match kind {
                "function_definition" => self.c_definition(node),
                "declaration" => self.c_declaration(node),
                _ => {}
            },
            Language::Java => match kind {
                "method_declaration" => self.java_method(node),
                "class_declaration" => self.java_class(node),
                _ => {}
            },
            Language::CSharp => {
                if kind == "class_declaration" {
                    self.csharp_class(node);
                }
            }
            Language::Go => match kind {
                "import_declaration" => self.go_import(node),
                "type_spec" => self.go_type(node),
                "function_declaration" => self.go_export(node),
                "selector_expression" => self.go_cgo_value(node),
                _ => {}
            },
            Language::Python if kind == "class_definition" => self.python_class(node),
            Language::JavaScript | Language::TypeScript | Language::Tsx if kind == "pair" => {
                self.js_resolver_pair(node);
            }
            _ => {}
        }
    }

    fn on_call(&mut self, cv: &CallView<'_>) {
        // cgo `C.name(...)`.
        if self.lang == Language::Go && cv.path.len() == 2 && cv.path[0] == "C" {
            if !CGO_PSEUDO.contains(&cv.path[1].as_str()) {
                let mut detail = vec![("language".into(), "go".into())];
                if !self.cgo_includes.is_empty() {
                    detail.push(("includes".into(), self.cgo_includes.join(",")));
                }
                self.push(
                    BridgeKind::Cgo,
                    BoundaryRole::Uses,
                    cv.path[1].clone(),
                    cv.owner,
                    None,
                    span(cv.node),
                    detail,
                );
            }
            return;
        }
        self.grpc_call(cv);
        self.graphql_call(cv);
        self.napi_call(cv);
    }

    pub(super) fn decorated_decl(&self, call: Node<'_>) -> Option<Option<u32>> {
        let parent = call.parent()?;
        if parent.kind() != "decorator" {
            return None;
        }
        let holder = parent.parent()?;
        let def = holder
            .child_by_field_name("definition")
            .or_else(|| (holder.kind() != "decorated_definition").then_some(holder))?;
        Some(self.decl_of_node(def))
    }
}
