//! Python: module path constants and instances, decorated classes (gRPC servicers, GraphQL
//! resolvers).

use std::collections::{HashMap, HashSet};

use trace_core::facts::BoundaryRole;
use trace_core::model::{BridgeKind, ByteSpan};
use trace_core::Language;
use tree_sitter::Node;

use super::{
    conventions::placeholder_in, conventions::symbol_written, literals::camel_case, Cx, GRAPHQL_ROOT_TYPES,
};
use crate::node::{named_children, nth_named, span};

impl<'a> Cx<'a> {
    /// Python module-level path constants and class-body path attributes of top-level classes
    /// (`API_V1_STR: str = "/api/v1"`; only literals starting with `/`), plus module-level
    /// instances `x = Class(..)` of such classes declared in this file: the values a mount
    /// prefix reference (`prefix=settings.API_V1_STR`) may name. Names bound twice are skipped.
    pub(crate) fn python_constants(&self, root: Node<'_>) {
        if self.lang != Language::Python {
            return;
        }
        let mut classes: Vec<String> = Vec::new();
        let mut pairs: Vec<(String, Node<'_>, Node<'_>)> = Vec::new();
        for stmt in named_children(root) {
            let stmt = if stmt.kind() == "decorated_definition" {
                match stmt.child_by_field_name("definition") {
                    Some(d) => d,
                    None => continue,
                }
            } else {
                stmt
            };
            match stmt.kind() {
                "expression_statement" => {
                    if let Some(a) = nth_named(stmt, 0).filter(|a| a.kind() == "assignment") {
                        if let (Some(l), Some(r)) =
                            (a.child_by_field_name("left"), a.child_by_field_name("right"))
                        {
                            if l.kind() == "identifier" {
                                pairs.push((self.txt(l), r, a));
                            }
                        }
                    }
                }
                "class_definition" => {
                    let (Some(n), Some(body)) =
                        (stmt.child_by_field_name("name"), stmt.child_by_field_name("body"))
                    else {
                        continue;
                    };
                    let class = self.txt(n);
                    classes.push(class.clone());
                    for s in named_children(body) {
                        if s.kind() != "expression_statement" {
                            continue;
                        }
                        let Some(a) = nth_named(s, 0).filter(|a| a.kind() == "assignment") else { continue };
                        if let (Some(l), Some(r)) =
                            (a.child_by_field_name("left"), a.child_by_field_name("right"))
                        {
                            if l.kind() == "identifier" {
                                pairs.push((format!("{class}.{}", self.txt(l)), r, a));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let mut count: HashMap<String, usize> = HashMap::new();
        for (k, _, _) in &pairs {
            *count.entry(k.clone()).or_default() += 1;
        }
        let mut with_paths: HashSet<String> = HashSet::new();
        let mut consts: Vec<(String, String, ByteSpan)> = Vec::new();
        for (key, value, stmt) in &pairs {
            if count.get(key) != Some(&1) || value.kind() == "call" {
                continue;
            }
            let tpl = self.eval(*value, 0);
            if !(tpl.is_literal() && tpl.has_text()) {
                continue;
            }
            let Some(text) = tpl.plain().filter(|t| t.starts_with('/')) else { continue };
            if let Some((class, _)) = key.split_once('.') {
                with_paths.insert(class.to_string());
            }
            consts.push((key.clone(), text, span(*stmt)));
        }
        for (key, text, at) in consts {
            self.push(
                BridgeKind::Http,
                BoundaryRole::Provides,
                format!("CONST {key}"),
                None,
                None,
                at,
                vec![("const".into(), "true".into()), ("value".into(), text)],
            );
        }
        for (key, value, stmt) in pairs {
            if count.get(&key) != Some(&1) {
                continue;
            }
            if value.kind() == "call" {
                let Some(f) = value
                    .child_by_field_name("function")
                    .filter(|f| f.kind() == "identifier")
                else {
                    continue;
                };
                let class = self.txt(f);
                if key.contains('.') || !classes.contains(&class) || !with_paths.contains(&class) {
                    continue;
                }
                self.push(
                    BridgeKind::Http,
                    BoundaryRole::Provides,
                    format!("INSTANCE {key}"),
                    None,
                    None,
                    span(stmt),
                    vec![("class".into(), class), ("instance".into(), "true".into())],
                );
            }
        }
    }
}

impl<'a> Cx<'a> {
    fn python_decorators<'t>(&self, def: Node<'t>) -> Vec<Vec<String>> {
        let Some(holder) = def.parent().filter(|p| p.kind() == "decorated_definition") else {
            return Vec::new();
        };
        named_children(holder)
            .into_iter()
            .filter(|c| c.kind() == "decorator")
            .filter_map(|d| nth_named(d, 0))
            .map(|e| {
                let mut p = self.path_of(e);
                if p.last().is_some_and(|s| s == "()") {
                    p.pop();
                }
                p
            })
            .collect()
    }

    pub(super) fn python_class(&self, node: Node<'_>) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let class_name = self.txt(name_node);
        let decl = self.decl_of_node(node);
        let bases = self.base_types(node);
        // Subclasses of the generated server base (`<Service>Servicer`), written qualified or
        // imported by name.
        for base in &bases {
            let Some(last) = base.last() else { continue };
            let Some(svc) = self.service_of("rpc_server_base", last) else { continue };
            if base.len() >= 2 || self.import_locals.contains(last) {
                self.push(
                    BridgeKind::Grpc,
                    BoundaryRole::Provides,
                    format!("{svc}/*"),
                    None,
                    decl,
                    span(node),
                    vec![("service".into(), svc.clone()), ("rule".into(), "python_servicer".into())],
                );
            }
        }
        // GraphQL root types: a class under a `graphql_type_decorator` row (fields: methods
        // under a `graphql_field_decorator` row), or a subclass of a `graphql_resolver_base`
        // row's type (fields: methods named by the row's `<field>` pattern).
        if !GRAPHQL_ROOT_TYPES.contains(&class_name.as_str()) {
            return;
        }
        let decorators = self.python_decorators(node);
        let typed = self.rows_of("graphql_type_decorator", BridgeKind::Graphql).find(|r| {
            r.symbol
                .as_deref()
                .is_some_and(|s| decorators.iter().any(|d| symbol_written(s, d)))
        });
        let based = self.rows_of("graphql_resolver_base", BridgeKind::Graphql).find(|r| {
            r.symbol
                .as_deref()
                .is_some_and(|s| bases.iter().any(|b| symbol_written(s, b)))
        });
        let Some(row) = typed.or(based) else { return };
        let convention = row.symbol.clone().unwrap_or_default();
        let camel = self
            .rows_of("camel_case_names", BridgeKind::Graphql)
            .any(|r| r.symbol.as_deref() == Some(convention.as_str()));
        let field_decorators = self.symbols_of("graphql_field_decorator", BridgeKind::Graphql);
        let Some(body) = node.child_by_field_name("body") else { return };
        for item in named_children(body) {
            let def = if item.kind() == "decorated_definition" {
                item.child_by_field_name("definition")
            } else {
                Some(item)
            };
            let Some(def) = def.filter(|d| d.kind() == "function_definition") else { continue };
            let Some(fname_node) = def.child_by_field_name("name") else { continue };
            let fname = self.txt(fname_node);
            let field = if typed.is_some() {
                let decos = self.python_decorators(def);
                if !decos
                    .iter()
                    .any(|d| field_decorators.iter().any(|f| symbol_written(f, d)))
                {
                    continue;
                }
                fname
            } else {
                match row
                    .pattern
                    .as_deref()
                    .and_then(|p| placeholder_in(p, "<field>", &fname))
                {
                    Some(f) => f,
                    None => continue,
                }
            };
            let field = if camel { camel_case(&field) } else { field };
            let fdecl = self.decl_by_name.get(&(fname_node.start_byte() as u32)).copied();
            self.push(
                BridgeKind::Graphql,
                BoundaryRole::Provides,
                format!("{class_name}.{field}"),
                None,
                fdecl,
                span(def),
                vec![("convention".into(), convention.clone())],
            );
        }
    }
}
