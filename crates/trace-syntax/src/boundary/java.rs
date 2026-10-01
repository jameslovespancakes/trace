//! Java and C#: JNI `native` methods, annotations, generated gRPC server bases.

use trace_core::facts::BoundaryRole;
use trace_core::model::BridgeKind;
use tree_sitter::Node;

use super::{conventions::last_segment, literals::jni_mangle, Cx};
use crate::node::{named_children, nth_named, span};

impl<'a> Cx<'a> {
    fn java_package(&self, node: Node<'_>) -> Vec<String> {
        let mut root = node;
        while let Some(p) = root.parent() {
            root = p;
        }
        for child in named_children(root) {
            if child.kind() == "package_declaration" {
                if let Some(n) = nth_named(child, 0) {
                    return self.path_of(n);
                }
            }
        }
        Vec::new()
    }

    fn java_classes(&self, node: Node<'_>) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = node.parent();
        while let Some(p) = cur {
            if matches!(
                p.kind(),
                "class_declaration" | "interface_declaration" | "enum_declaration" | "record_declaration"
            ) {
                if let Some(n) = p.child_by_field_name("name") {
                    out.push(self.txt(n));
                }
            }
            cur = p.parent();
        }
        out.reverse();
        out
    }

    pub(super) fn annotations_of<'t>(&self, decl: Node<'t>) -> Vec<Node<'t>> {
        let mut out = Vec::new();
        for child in named_children(decl) {
            if matches!(child.kind(), "modifiers" | "attribute_list") {
                for a in named_children(child) {
                    if matches!(a.kind(), "annotation" | "marker_annotation" | "attribute") {
                        out.push(a);
                    } else if a.kind() == "attribute_list" {
                        out.extend(named_children(a).into_iter().filter(|x| x.kind() == "attribute"));
                    }
                }
            }
        }
        out.sort_by_key(|n| n.start_byte());
        out.dedup_by(|a, b| a.id() == b.id());
        out
    }

    fn annotation_name(&self, a: Node<'_>) -> String {
        a.child_by_field_name("name")
            .map(|n| self.path_of(n).last().cloned().unwrap_or_default())
            .unwrap_or_default()
    }

    /// Annotation arguments: (key or None, value node).
    pub(super) fn annotation_args<'t>(&self, a: Node<'t>) -> Vec<(Option<String>, Node<'t>)> {
        let Some(list) = a
            .child_by_field_name("arguments")
            .or_else(|| crate::node::first_of_kind(a, "attribute_argument_list"))
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for arg in named_children(list) {
            match arg.kind() {
                "element_value_pair" => {
                    if let (Some(k), Some(v)) =
                        (arg.child_by_field_name("key"), arg.child_by_field_name("value"))
                    {
                        out.push((Some(self.txt(k)), v));
                    }
                }
                "attribute_argument" => {
                    let key = arg.child_by_field_name("name").map(|n| self.txt(n)).or_else(|| {
                        crate::node::first_of_kind(arg, "name_equals")
                            .and_then(|ne| nth_named(ne, 0))
                            .map(|n| self.txt(n))
                    });
                    if let Some(v) = nth_named(arg, -1) {
                        out.push((key, v));
                    }
                }
                _ => out.push((None, arg)),
            }
        }
        out
    }

    pub(super) fn java_method(&self, node: Node<'_>) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let name = self.txt(name_node);
        let is_native = named_children(node)
            .into_iter()
            .find(|c| c.kind() == "modifiers")
            .is_some_and(|m| {
                let mut cursor = m.walk();
                let found = m.children(&mut cursor).any(|c| c.kind() == "native");
                found
            });
        let decl = self.decl_by_name.get(&(name_node.start_byte() as u32)).copied();
        if is_native {
            let package = self.java_package(node);
            let classes = self.java_classes(node);
            let pkg: Vec<&str> = package.iter().map(String::as_str).collect();
            let cls: Vec<&str> = classes.iter().map(String::as_str).collect();
            if !cls.is_empty() {
                self.push(
                    BridgeKind::Jni,
                    BoundaryRole::Uses,
                    jni_mangle(&pkg, &cls, &name),
                    None,
                    decl,
                    span(node),
                    vec![("method".into(), name.clone()), ("class".into(), classes.join("$"))],
                );
            }
        }
        // GraphQL field resolvers bound by annotation (`graphql_field_annotation` rows: the
        // root type from the row's `value` or from the element its `key` names; the field
        // from a `graphql_field_name_element`, else the method name).
        let name_elements = self.symbols_of("graphql_field_name_element", BridgeKind::Graphql);
        for a in self.annotations_of(node) {
            let aname = self.annotation_name(a);
            let Some(row) = self
                .rows_of("graphql_field_annotation", BridgeKind::Graphql)
                .find(|r| r.symbol.as_deref().is_some_and(|s| last_segment(s) == aname))
            else {
                continue;
            };
            let args = self.annotation_args(a);
            let arg = |k: &str| {
                args.iter()
                    .find(|(key, _)| key.as_deref() == Some(k))
                    .and_then(|(_, v)| self.eval(*v, 0).plain())
            };
            let root = match (&row.value, &row.key_kw) {
                (Some(v), _) => v.clone(),
                (None, Some(k)) => match arg(k) {
                    Some(t) => t,
                    None => continue,
                },
                (None, None) => continue,
            };
            let field = name_elements
                .iter()
                .find_map(|k| arg(k))
                .unwrap_or_else(|| name.clone());
            let convention = row.symbol.clone().unwrap_or_default();
            self.push(
                BridgeKind::Graphql,
                BoundaryRole::Provides,
                format!("{root}.{field}"),
                None,
                decl,
                span(a),
                vec![("convention".into(), convention)],
            );
        }
    }

    pub(super) fn base_types(&self, class: Node<'_>) -> Vec<Vec<String>> {
        let mut out = Vec::new();
        for field in ["superclass", "bases", "superclasses"] {
            if let Some(b) = class.child_by_field_name(field) {
                for n in std::iter::once(b).chain(named_children(b)) {
                    let p = self.path_of(n);
                    if !p.iter().any(|s| s == "?") {
                        out.push(p);
                    }
                }
            }
        }
        for child in named_children(class) {
            if matches!(child.kind(), "base_list" | "superclass" | "argument_list" | "class_heritage") {
                for n in named_children(child) {
                    let n = if n.kind() == "primary_constructor_base_type" {
                        nth_named(n, 0).unwrap_or(n)
                    } else {
                        n
                    };
                    let p = self.path_of(n);
                    if !p.iter().any(|s| s == "?") {
                        out.push(p);
                    }
                }
            }
        }
        out
    }

    /// A subclass of the generated server base (`<Service>Grpc.<Service>ImplBase`).
    pub(super) fn java_class(&self, node: Node<'_>) {
        for base in self.base_types(node) {
            let Some(last) = base.last() else { continue };
            let Some(svc) = self.service_of("rpc_server_base", last) else { continue };
            if base.len() >= 2
                && self.service_of("rpc_stub_class", &base[base.len() - 2]).as_deref() != Some(svc.as_str())
            {
                continue;
            }
            let decl = self.decl_of_node(node);
            self.push(
                BridgeKind::Grpc,
                BoundaryRole::Provides,
                format!("{svc}/*"),
                None,
                decl,
                span(node),
                vec![("service".into(), svc.to_string()), ("rule".into(), "java_impl_base".into())],
            );
        }
    }

    // ---- C# -----------------------------------------------------------------------------

    /// A subclass of the generated server base nested in the service class
    /// (`<Service>.<Service>Base`).
    pub(super) fn csharp_class(&self, node: Node<'_>) {
        for base in self.base_types(node) {
            if base.len() < 2 {
                continue;
            }
            let last = &base[base.len() - 1];
            let outer = &base[base.len() - 2];
            if self.service_of("rpc_server_base", last).as_deref() == Some(outer.as_str()) {
                let decl = self.decl_of_node(node);
                self.push(
                    BridgeKind::Grpc,
                    BoundaryRole::Provides,
                    format!("{outer}/*"),
                    None,
                    decl,
                    span(node),
                    vec![("service".into(), outer.clone()), ("rule".into(), "csharp_base".into())],
                );
            }
        }
    }
}
