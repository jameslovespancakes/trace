//! Rust: `extern "C"` exports and imports, binding attributes (`pyo3`, `wasm_bindgen`,
//! `napi` rows), foreign items and exported types.

use trace_core::facts::BoundaryRole;
use trace_core::model::BridgeKind;
use tree_sitter::Node;

use super::{
    literals::camel_case, literals::is_string_kind, Cx, RUST_EXPORT_NAME, RUST_LINK_NAME, RUST_NO_MANGLE,
};
use crate::node::{named_children, nth_named, span};

impl<'a> Cx<'a> {
    fn rust_attrs(&self, item: Node<'_>) -> Vec<RustAttr> {
        let mut out = Vec::new();
        let mut sib = item.prev_sibling();
        while let Some(s) = sib {
            match s.kind() {
                "attribute_item" => {
                    if let Some(attr) = nth_named(s, 0) {
                        out.push(self.rust_attr(attr));
                    }
                }
                "line_comment" | "block_comment" => {}
                _ => break,
            }
            sib = s.prev_sibling();
        }
        out
    }

    fn rust_attr(&self, attr: Node<'_>) -> RustAttr {
        let path = nth_named(attr, 0).map(|p| self.path_of(p)).unwrap_or_default();
        let mut name = path.last().cloned().unwrap_or_default();
        let mut args: Vec<(String, Option<String>)> = Vec::new();
        let tokens = attr.child_by_field_name("arguments");
        if let Some(tt) = tokens {
            let pairs = self.token_pairs(tt);
            if name == "unsafe" {
                // `#[unsafe(no_mangle)]`, `#[unsafe(export_name = "x")]`.
                let mut it = pairs.into_iter();
                if let Some((inner, value)) = it.next() {
                    name = inner;
                    if value.is_some() {
                        args.push(("=".into(), value));
                    }
                }
                args.extend(it);
            } else {
                args = pairs;
            }
        }
        if let Some(v) = attr.child_by_field_name("value") {
            args.push(("=".into(), self.eval(v, 0).plain()));
        }
        RustAttr { name, args }
    }

    /// `key = value` pairs and bare flags of an attribute token tree.
    fn token_pairs(&self, tt: Node<'_>) -> Vec<(String, Option<String>)> {
        let mut out: Vec<(String, Option<String>)> = Vec::new();
        let mut cursor = tt.walk();
        let kids: Vec<Node<'_>> = tt.children(&mut cursor).collect();
        let mut i = 0;
        while i < kids.len() {
            let k = kids[i];
            if k.kind() == "identifier" {
                let key = self.txt(k);
                if kids.get(i + 1).is_some_and(|n| n.kind() == "=") {
                    if let Some(v) = kids.get(i + 2) {
                        let value = if is_string_kind(v.kind()) {
                            self.eval(*v, 0).plain()
                        } else {
                            Some(self.txt(*v))
                        };
                        out.push((key, value));
                        i += 3;
                        continue;
                    }
                }
                out.push((key, None));
            } else if k.kind() == "token_tree" {
                // Nested `pyo3(name = "x")` inside `pyfunction(...)` etc.
                out.extend(self.token_pairs(k));
            }
            i += 1;
        }
        out
    }

    fn rust_extern_c(&self, item: Node<'_>) -> bool {
        let Some(mods) = crate::node::first_of_kind(item, "function_modifiers") else {
            return false;
        };
        let Some(ext) = crate::node::first_of_kind(mods, "extern_modifier") else {
            return false;
        };
        match crate::node::first_of_kind(ext, "string_literal") {
            None => true,
            Some(abi) => {
                let abi = self.eval(abi, 0).plain().unwrap_or_default();
                abi == "C" || abi == "C-unwind" || abi == "system"
            }
        }
    }

    fn rust_impl_of<'t>(&self, item: Node<'t>) -> Option<(Node<'t>, String)> {
        let list = item.parent()?;
        if list.kind() != "declaration_list" {
            return None;
        }
        let imp = list.parent()?;
        if imp.kind() != "impl_item" {
            return None;
        }
        let ty = imp.child_by_field_name("type")?;
        Some((imp, crate::node::type_name(ty, self.src)))
    }

    /// Binding attributes among `attrs` that a row of `rule` names: (bridge, attribute), at
    /// most one per bridge, in table order.
    fn binding_attrs<'r>(&self, rule: &str, attrs: &'r [RustAttr]) -> Vec<(BridgeKind, &'r RustAttr)> {
        let mut out: Vec<(BridgeKind, &'r RustAttr)> = Vec::new();
        for row in self.rows(rule) {
            let (Some(bridge), Some(symbol)) = (row.bridge, row.symbol.as_deref()) else { continue };
            if out.iter().any(|(b, _)| *b == bridge) {
                continue;
            }
            if let Some(a) = attrs.iter().find(|a| a.name == symbol) {
                out.push((bridge, a));
            }
        }
        out
    }

    /// The member's own export attribute of `bridge` (`#[wasm_bindgen(js_name = x)]` on a
    /// method).
    fn own_export_attr<'r>(&self, bridge: BridgeKind, attrs: &'r [RustAttr]) -> Option<&'r RustAttr> {
        let symbols = self.symbols_of("export_function_attribute", bridge);
        attrs.iter().find(|a| symbols.iter().any(|s| *s == a.name))
    }

    /// The name an export gets from a `name_key` of its own attribute, else of a
    /// `rename_attribute` of the item.
    fn exported_name(
        &self,
        bridge: BridgeKind,
        attr: Option<&RustAttr>,
        attrs: &[RustAttr],
    ) -> Option<String> {
        let keys = self.symbols_of("name_key", bridge);
        if let Some(name) = attr.and_then(|a| keys.iter().find_map(|k| a.value(k))) {
            return Some(name);
        }
        let renames = self.symbols_of("rename_attribute", bridge);
        attrs
            .iter()
            .filter(|a| renames.iter().any(|s| *s == a.name))
            .find_map(|a| keys.iter().find_map(|k| a.value(k)))
    }

    /// The default exported name of a function / method (`camel_case_names` rows).
    fn default_name(&self, bridge: BridgeKind, name: &str) -> String {
        if self.rows_of("camel_case_names", bridge).next().is_some() {
            camel_case(name)
        } else {
            name.to_string()
        }
    }

    /// Whether a member is its class's constructor: `Some(member name)` for a
    /// `constructor_attribute` row (the row's `value`, e.g. `__new__`), `Some(None)` for a
    /// `constructor_flag` of the member's own export attribute (the class itself).
    fn constructor_of(
        &self,
        bridge: BridgeKind,
        attr: Option<&RustAttr>,
        attrs: &[RustAttr],
    ) -> Option<Option<String>> {
        for row in self.rows_of("constructor_attribute", bridge) {
            let Some(symbol) = row.symbol.as_deref() else { continue };
            if attrs.iter().any(|a| a.name == symbol) {
                return Some(row.value.clone());
            }
        }
        let own = attr?;
        self.rows_of("constructor_flag", bridge)
            .filter_map(|r| r.symbol.as_deref().map(|s| (s, r)))
            .find(|(s, _)| own.flag(s))
            .map(|(_, r)| r.value.clone())
    }

    pub(super) fn rust_function(&self, item: Node<'_>) {
        let Some(name_node) = item.child_by_field_name("name") else { return };
        let name = self.txt(name_node);
        let decl = self.decl_by_name.get(&(name_node.start_byte() as u32)).copied();
        let attrs = self.rust_attrs(item);
        let at = span(item);
        // C ABI exports (Rust language attributes).
        let export_name = attrs
            .iter()
            .find(|a| a.name == RUST_EXPORT_NAME)
            .and_then(|a| a.value("="));
        if attrs.iter().any(|a| a.name == RUST_NO_MANGLE) || export_name.is_some() {
            let symbol = export_name.unwrap_or_else(|| name.clone());
            let mut detail = vec![
                ("linkage".to_string(), "rust".to_string()),
                ("definition".to_string(), "true".to_string()),
            ];
            if !self.rust_extern_c(item) {
                detail.push(("abi".into(), "rust".into()));
            }
            self.push(BridgeKind::CAbi, BoundaryRole::Provides, symbol, None, decl, at, detail);
        }
        // Members of impl blocks carrying a binding attribute.
        if let Some((imp, ty)) = self.rust_impl_of(item) {
            let impl_attrs = self.rust_attrs(imp);
            let public = crate::node::first_of_kind(item, "visibility_modifier").is_some();
            let mut members: Vec<(BridgeKind, Option<&RustAttr>)> = Vec::new();
            for (bridge, _) in self.binding_attrs("export_members_attribute", &impl_attrs) {
                members.push((bridge, self.own_export_attr(bridge, &attrs)));
            }
            if public {
                for (bridge, _) in self.binding_attrs("export_public_members_attribute", &impl_attrs) {
                    members.push((bridge, self.own_export_attr(bridge, &attrs)));
                }
            }
            for (bridge, _) in self.binding_attrs("export_marked_members_attribute", &impl_attrs) {
                if let Some(own) = self.own_export_attr(bridge, &attrs) {
                    members.push((bridge, Some(own)));
                }
            }
            members.sort_by_key(|(b, _)| *b);
            members.dedup_by_key(|(b, _)| *b);
            // A method returning the exported type itself hands the other language an
            // instance of the class (`Universe.new()` -> `Universe`).
            let returns_self = item.child_by_field_name("return_type").is_some_and(|ret| {
                let r = crate::node::type_name(ret, self.src);
                r == "Self" || r == ty
            });
            for (bridge, attr) in members {
                let constructor = self.constructor_of(bridge, attr, &attrs);
                let key = match &constructor {
                    Some(Some(member)) => format!("{ty}.{member}"),
                    Some(None) => ty.clone(),
                    None => {
                        let exported = self
                            .exported_name(bridge, attr, &attrs)
                            .unwrap_or_else(|| self.default_name(bridge, &name));
                        format!("{ty}.{exported}")
                    }
                };
                let mut detail = vec![
                    ("impl_type".to_string(), ty.clone()),
                    ("rust_name".to_string(), name.clone()),
                    ("item".to_string(), "method".to_string()),
                ];
                if constructor.is_some() {
                    detail.push(("constructor".into(), "true".into()));
                }
                if returns_self {
                    detail.push(("returns_self".into(), "true".into()));
                }
                self.push(bridge, BoundaryRole::Provides, key, None, decl, at, detail);
            }
            return;
        }
        // Exported free functions.
        for (bridge, attr) in self.binding_attrs("export_function_attribute", &attrs) {
            let exported = self
                .exported_name(bridge, Some(attr), &attrs)
                .unwrap_or_else(|| self.default_name(bridge, &name));
            self.push(
                bridge,
                BoundaryRole::Provides,
                exported,
                None,
                decl,
                at,
                vec![("rust_name".into(), name.clone()), ("item".into(), "function".into())],
            );
        }
        // Module init functions and what they register.
        for (bridge, attr) in self.binding_attrs("export_module_attribute", &attrs) {
            let module = self
                .exported_name(bridge, Some(attr), &attrs)
                .unwrap_or_else(|| name.clone());
            let registers = self.registrations(bridge, item);
            self.push(
                bridge,
                BoundaryRole::Provides,
                module.clone(),
                None,
                decl,
                at,
                vec![
                    ("module_init".into(), "true".into()),
                    ("module".into(), module),
                    ("registers".into(), registers.join(",")),
                ],
            );
        }
    }

    /// Rust names registered inside a module init function body: `register_macro` rows
    /// (`wrap_pyfunction!(f, m)`: the first macro argument) and `register_member` rows
    /// (`m.add_class::<T>()`: the type arguments).
    fn registrations(&self, bridge: BridgeKind, item: Node<'_>) -> Vec<String> {
        let Some(body) = item.child_by_field_name("body") else { return Vec::new() };
        let macros = self.symbols_of("register_macro", bridge);
        let members = self.symbols_of("register_member", bridge);
        let mut out = Vec::new();
        let mut stack = vec![body];
        let mut seen = 0;
        while let Some(n) = stack.pop() {
            seen += 1;
            if seen > 20_000 {
                break;
            }
            match n.kind() {
                "macro_invocation" => {
                    let m = n
                        .child_by_field_name("macro")
                        .map(|m| self.path_of(m))
                        .unwrap_or_default();
                    if m.last().is_some_and(|x| macros.iter().any(|s| s == x)) {
                        if let Some(tt) = crate::node::first_of_kind(n, "token_tree") {
                            let first = named_children(tt).into_iter().next();
                            if let Some(f) = first {
                                if let Some(last) = self.path_of(f).last() {
                                    out.push(last.clone());
                                }
                            }
                        }
                    }
                }
                "generic_function" => {
                    let f = n
                        .child_by_field_name("function")
                        .map(|f| self.path_of(f))
                        .unwrap_or_default();
                    if f.last().is_some_and(|x| members.iter().any(|s| s == x)) {
                        if let Some(args) = n.child_by_field_name("type_arguments") {
                            for a in named_children(args) {
                                out.push(crate::node::type_name(a, self.src));
                            }
                        }
                    }
                }
                _ => {}
            }
            stack.extend(named_children(n));
        }
        out.sort();
        out.dedup();
        out
    }

    pub(super) fn rust_foreign_fn(&self, item: Node<'_>) {
        let Some(list) = item.parent() else { return };
        let Some(block) = list.parent().filter(|b| b.kind() == "foreign_mod_item") else {
            return;
        };
        let block_attrs = self.rust_attrs(block);
        if !self.binding_attrs("import_block_attribute", &block_attrs).is_empty() {
            return; // Imports from another language through a binding, not C ABI uses.
        }
        let abi = crate::node::first_of_kind(block, "extern_modifier")
            .and_then(|e| crate::node::first_of_kind(e, "string_literal"))
            .and_then(|s| self.eval(s, 0).plain())
            .unwrap_or_else(|| "C".into());
        if abi != "C" && abi != "C-unwind" && abi != "system" {
            return;
        }
        let Some(name_node) = item.child_by_field_name("name") else { return };
        let link_name = self
            .rust_attrs(item)
            .iter()
            .find(|a| a.name == RUST_LINK_NAME)
            .and_then(|a| a.value("="));
        let symbol = link_name.unwrap_or_else(|| self.txt(name_node));
        let decl = self.decl_by_name.get(&(name_node.start_byte() as u32)).copied();
        self.push(
            BridgeKind::CAbi,
            BoundaryRole::Uses,
            symbol,
            None,
            decl,
            span(item),
            vec![("linkage".into(), "rust".into()), ("definition".into(), "false".into())],
        );
    }

    pub(super) fn rust_type(&self, item: Node<'_>) {
        let Some(name_node) = item.child_by_field_name("name") else { return };
        let name = self.txt(name_node);
        let decl = self.decl_by_name.get(&(name_node.start_byte() as u32)).copied();
        let attrs = self.rust_attrs(item);
        let at = span(item);
        for (bridge, attr) in self.binding_attrs("export_type_attribute", &attrs) {
            let exported = self
                .exported_name(bridge, Some(attr), &attrs)
                .unwrap_or_else(|| name.clone());
            let mut detail = vec![
                ("rust_name".to_string(), name.clone()),
                ("item".to_string(), "class".to_string()),
            ];
            if let Some(module) = self
                .symbols_of("module_key", bridge)
                .iter()
                .find_map(|k| attr.value(k))
            {
                detail.push(("module".into(), module));
            }
            self.push(bridge, BoundaryRole::Provides, exported.clone(), None, decl, at, detail);
            // The variants of an exported C-style enum are properties of the exported object
            // (`Cell.Alive`); each variant is its own export key.
            if item.kind() != "enum_item" || self.rows_of("export_enum_variants", bridge).next().is_none() {
                continue;
            }
            let Some(body) = item.child_by_field_name("body") else { continue };
            for v in named_children(body) {
                if v.kind() != "enum_variant" {
                    continue;
                }
                let Some(vn) = v.child_by_field_name("name") else { continue };
                let variant = self.txt(vn);
                self.push(
                    bridge,
                    BoundaryRole::Provides,
                    format!("{exported}.{variant}"),
                    None,
                    decl,
                    span(v),
                    vec![
                        ("rust_name".into(), format!("{name}::{variant}")),
                        ("item".into(), "variant".into()),
                    ],
                );
            }
        }
    }
}

struct RustAttr {
    name: String,
    args: Vec<(String, Option<String>)>,
}

impl RustAttr {
    pub(super) fn value(&self, key: &str) -> Option<String> {
        self.args.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.clone())
    }
    fn flag(&self, key: &str) -> bool {
        self.args.iter().any(|(k, v)| k == key && v.is_none())
    }
}
