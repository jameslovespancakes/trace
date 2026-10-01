//! C / C++: C-linkage definitions and prototypes, JNI implementations, CPython method
//! tables; C declarator helpers shared with cgo preambles.

use trace_core::facts::BoundaryRole;
use trace_core::model::{BridgeKind, ByteSpan};
use trace_core::Language;
use tree_sitter::Node;

use super::{
    literals::c_type_name, Cx, CPYTHON_METHOD_FUNCTION, CPYTHON_METHOD_NAME, CPYTHON_METHOD_TABLE,
    CPYTHON_MODULE_DEF, CPYTHON_MODULE_NAME, CPYTHON_MODULE_NAME_POSITION, JNI_PREFIX,
    NAPI_PROPERTY_DESCRIPTOR,
};
use crate::node::{named_children, nth_named, span, text};

impl<'a> Cx<'a> {
    fn c_linkage(&self, node: Node<'_>) -> Option<&'static str> {
        if self.lang == Language::C {
            return Some("c");
        }
        let mut cur = node.parent();
        while let Some(p) = cur {
            if p.kind() == "linkage_specification" {
                let abi = p
                    .child_by_field_name("value")
                    .and_then(|v| self.eval(v, 0).plain())
                    .unwrap_or_default();
                return (abi == "C").then_some("cpp_extern_c");
            }
            cur = p.parent();
        }
        None
    }

    fn at_file_scope(node: Node<'_>) -> bool {
        let mut cur = node.parent();
        while let Some(p) = cur {
            match p.kind() {
                "translation_unit" => return true,
                "compound_statement"
                | "function_definition"
                | "field_declaration_list"
                | "class_specifier"
                | "struct_specifier"
                | "namespace_definition"
                | "template_declaration" => return false,
                _ => {}
            }
            cur = p.parent();
        }
        true
    }

    pub(super) fn c_definition(&self, node: Node<'_>) {
        if !Self::at_file_scope(node) {
            return;
        }
        let Some((name, name_node)) = c_function_name(node, self.src) else { return };
        let decl = self.decl_by_name.get(&(name_node.start_byte() as u32)).copied();
        let is_static = c_is_static(node, self.src);
        if name.starts_with(JNI_PREFIX) {
            self.push(
                BridgeKind::Jni,
                BoundaryRole::Provides,
                name,
                None,
                decl,
                span(node),
                vec![("language".into(), self.lang.as_str().into())],
            );
            return;
        }
        if is_static {
            return;
        }
        let Some(linkage) = self.c_linkage(node) else { return };
        self.push(
            BridgeKind::CAbi,
            BoundaryRole::Provides,
            name,
            None,
            decl,
            span(node),
            vec![("linkage".into(), linkage.into()), ("definition".into(), "true".into())],
        );
    }

    pub(super) fn c_declaration(&self, node: Node<'_>) {
        if !Self::at_file_scope(node) {
            return;
        }
        let ty = c_type_name(node, self.src);
        if ty == CPYTHON_METHOD_TABLE {
            self.cpython_table(node);
            return;
        }
        if ty == NAPI_PROPERTY_DESCRIPTOR {
            self.napi_descriptors(node);
            return;
        }
        if c_is_static(node, self.src) {
            return;
        }
        let Some((name, name_node)) = c_function_name(node, self.src) else { return };
        let Some(linkage) = self.c_linkage(node) else { return };
        let decl = self.decl_by_name.get(&(name_node.start_byte() as u32)).copied();
        self.push(
            BridgeKind::CAbi,
            BoundaryRole::Uses,
            name,
            None,
            decl,
            span(node),
            vec![("linkage".into(), linkage.into()), ("definition".into(), "false".into())],
        );
    }

    fn cpython_table(&self, node: Node<'_>) {
        let Some(init) = crate::node::find_descendant(node, 64, |n| n.kind() == "initializer_list") else {
            return;
        };
        let module = self.cached_module_name(node);
        for row in named_children(init) {
            if row.kind() != "initializer_list" {
                continue;
            }
            let items = named_children(row);
            let mut name = None;
            let mut func = None;
            for item in &items {
                if item.kind() == "initializer_pair" {
                    let field = item
                        .child_by_field_name("designator")
                        .map(|d| self.txt(d))
                        .unwrap_or_default();
                    let field = field.trim_start_matches('.').to_string();
                    let value = item.child_by_field_name("value");
                    if field == CPYTHON_METHOD_NAME {
                        name = value.and_then(|v| self.eval(v, 0).plain());
                    } else if field == CPYTHON_METHOD_FUNCTION {
                        func = value;
                    }
                }
            }
            if name.is_none() {
                name = items.first().and_then(|n| self.eval(*n, 0).plain());
                func = items.get(1).copied();
            }
            let Some(name) = name else { continue };
            let mut detail = vec![("table".to_string(), CPYTHON_METHOD_TABLE.to_string())];
            if let Some(m) = &module {
                detail.push(("module".into(), m.clone()));
            }
            let decl = func.and_then(|f| {
                let f = crate::node::find_descendant(f, 16, |n| n.kind() == "identifier")
                    .or_else(|| (f.kind() == "identifier").then_some(f))?;
                let fname = self.txt(f);
                detail.push(("handler".into(), fname.clone()));
                self.callable_by_name.get(&fname).and_then(|v| v.first().copied())
            });
            self.push(BridgeKind::Cpython, BoundaryRole::Provides, name, None, decl, span(row), detail);
        }
    }

    /// Module name of the file's `PyModuleDef` (`{PyModuleDef_HEAD_INIT, "name", ...}` or
    /// designated `.m_name = "name"`), if any.
    fn cached_module_name(&self, node: Node<'_>) -> Option<String> {
        let mut root = node;
        while let Some(p) = root.parent() {
            root = p;
        }
        for decl in named_children(root) {
            if decl.kind() != "declaration" {
                continue;
            }
            if c_type_name(decl, self.src) != CPYTHON_MODULE_DEF {
                continue;
            }
            let Some(init) = crate::node::find_descendant(decl, 64, |n| n.kind() == "initializer_list")
            else {
                continue;
            };
            let items = named_children(init);
            for item in &items {
                if item.kind() == "initializer_pair" {
                    let field = item
                        .child_by_field_name("designator")
                        .map(|d| self.txt(d))
                        .unwrap_or_default();
                    if field.trim_start_matches('.') == CPYTHON_MODULE_NAME {
                        if let Some(v) = item
                            .child_by_field_name("value")
                            .and_then(|v| self.eval(v, 0).plain())
                        {
                            return Some(v);
                        }
                    }
                }
            }
            if let Some(s) = items
                .get(CPYTHON_MODULE_NAME_POSITION)
                .and_then(|n| self.eval(*n, 0).plain())
            {
                return Some(s);
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------------------
// C helpers (shared by C/C++ files and cgo preambles)
// ---------------------------------------------------------------------------------------

/// Name of the function declared/defined by a C `function_definition` / `declaration`:
/// descends `declarator` fields through pointer/parenthesized/attributed declarators to the
/// function declarator; only plain identifiers (never C++ qualified names / operators).
fn c_function_name<'t>(node: Node<'t>, src: &[u8]) -> Option<(String, Node<'t>)> {
    let mut d = node.child_by_field_name("declarator")?;
    for _ in 0..8 {
        match d.kind() {
            "function_declarator" => {
                let inner = d.child_by_field_name("declarator")?;
                return (inner.kind() == "identifier").then(|| (text(inner, src).into_owned(), inner));
            }
            "pointer_declarator"
            | "parenthesized_declarator"
            | "attributed_declarator"
            | "reference_declarator"
            | "init_declarator" => {
                d = d.child_by_field_name("declarator").or_else(|| nth_named(d, -1))?;
            }
            _ => return None,
        }
    }
    None
}

fn c_is_static(node: Node<'_>, src: &[u8]) -> bool {
    named_children(node)
        .into_iter()
        .any(|c| c.kind() == "storage_class_specifier" && text(c, src).trim() == "static")
}

/// Every file-scope function definition / prototype of a C tree: (name, name span, is
/// definition). Static functions are included (a preamble is a single unit).
pub(super) fn c_functions_in(root: Node<'_>, src: &[u8]) -> Vec<(String, ByteSpan, bool)> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "function_definition" | "declaration" => {
                if let Some((name, id)) = c_function_name(n, src) {
                    out.push((name, span(id), n.kind() == "function_definition"));
                }
            }
            "translation_unit"
            | "linkage_specification"
            | "declaration_list"
            | "preproc_if"
            | "preproc_ifdef"
            | "preproc_else"
            | "preproc_elif" => stack.extend(named_children(n)),
            _ => {}
        }
    }
    out.sort_by_key(|(_, s, _)| s.start);
    out
}
