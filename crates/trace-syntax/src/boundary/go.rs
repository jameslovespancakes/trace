//! Go: cgo preambles and `C.name` uses, `//export` functions, gRPC server types.

use trace_core::facts::BoundaryRole;
use trace_core::model::{BridgeKind, ByteSpan};
use trace_core::Language;
use tree_sitter::Node;

use super::{c::c_functions_in, Cx, CGO_PSEUDO};
use crate::node::{named_children, pick, span};

impl<'a> Cx<'a> {
    pub(super) fn go_import(&mut self, node: Node<'_>) {
        let spec = crate::node::find_descendant(node, 16, |n| n.kind() == "import_spec");
        let Some(path) = spec.and_then(|s| s.child_by_field_name("path")) else { return };
        if self.eval(path, 0).plain().as_deref() != Some("C") {
            return;
        }
        // The preamble: comments immediately before `import "C"`.
        let mut comments = Vec::new();
        let mut sib = node.prev_sibling();
        let mut expected_end_line = self.lines.line0(node.start_byte() as u32);
        while let Some(s) = sib {
            if s.kind() != "comment" {
                break;
            }
            let end_line = self.lines.line0(s.end_byte().saturating_sub(1) as u32);
            if end_line + 1 < expected_end_line {
                break;
            }
            expected_end_line = self.lines.line0(s.start_byte() as u32);
            comments.push(s);
            sib = s.prev_sibling();
        }
        if comments.is_empty() {
            return;
        }
        comments.reverse();
        let start = comments[0].start_byte();
        let end = comments[comments.len() - 1].end_byte().min(self.src.len());
        let mut buf = vec![b' '; end - start];
        for c in &comments {
            let (a, b) = (c.start_byte(), c.end_byte().min(end));
            let raw = &self.src[a..b];
            let (skip_head, skip_tail) = if raw.starts_with(b"//") {
                (2, 0)
            } else if raw.starts_with(b"/*") && raw.ends_with(b"*/") && raw.len() >= 4 {
                (2, 2)
            } else {
                (0, 0)
            };
            let body = &raw[skip_head..raw.len() - skip_tail];
            let off = a - start + skip_head;
            buf[off..off + body.len()].copy_from_slice(body);
        }
        // Newlines between comments stay newlines (preprocessor lines need them).
        for (i, b) in self.src[start..end].iter().enumerate() {
            if *b == b'\n' {
                buf[i] = b'\n';
            }
        }
        let Some(grammar) = crate::grammar::grammar(Language::C) else { return };
        let Ok(tree) = crate::parse::parse(grammar, self.path, &buf) else { return };
        let root = tree.root_node();
        let mut includes = Vec::new();
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            if n.kind() == "preproc_include" {
                if let Some(p) = n.child_by_field_name("path") {
                    let raw = String::from_utf8_lossy(&buf[p.start_byte()..p.end_byte()]).to_string();
                    includes.push(raw.trim_matches(|c| c == '"' || c == '<' || c == '>').to_string());
                }
            }
            stack.extend(named_children(n));
        }
        includes.sort();
        includes.dedup();
        self.cgo_includes = includes;
        for (name, name_span, def) in c_functions_in(root, &buf) {
            let at = ByteSpan::new(start as u32 + name_span.start, start as u32 + name_span.end);
            self.push(
                BridgeKind::Cgo,
                BoundaryRole::Provides,
                name,
                None,
                None,
                at,
                vec![("preamble".into(), "true".into()), ("definition".into(), def.to_string())],
            );
        }
    }

    /// cgo `C.name` used as a value, not called (a C function pointer handed to C:
    /// `(*[0]byte)(C.callback)`): a use of the C symbol like `C.name(..)`. Calls are
    /// handled by `on_call`; type positions (`C.int` in declarations) are qualified types,
    /// not selector expressions.
    pub(super) fn go_cgo_value(&self, node: Node<'_>) {
        let (Some(op), Some(field)) =
            (node.child_by_field_name("operand"), node.child_by_field_name("field"))
        else {
            return;
        };
        if op.kind() != "identifier" || self.txt(op) != "C" {
            return;
        }
        if let Some(parent) = node.parent() {
            if let Some(shape) = self.spec.call_shape(parent.kind()) {
                if pick(parent, shape.function_field).is_some_and(|f| f.id() == node.id()) {
                    return;
                }
            }
        }
        let name = self.txt(field);
        if CGO_PSEUDO.contains(&name.as_str()) {
            return;
        }
        let mut detail = vec![
            ("language".to_string(), "go".to_string()),
            ("value".to_string(), "true".to_string()),
        ];
        if !self.cgo_includes.is_empty() {
            detail.push(("includes".into(), self.cgo_includes.join(",")));
        }
        self.push(
            BridgeKind::Cgo,
            BoundaryRole::Uses,
            name,
            self.owner_at(node.start_byte() as u32),
            None,
            span(node),
            detail,
        );
    }

    /// cgo `//export name` in the comment block directly above a Go function: cgo makes the
    /// function callable from C as the global C symbol `name` (C code declares it `extern`).
    pub(super) fn go_export(&self, node: Node<'_>) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let mut sib = node.prev_sibling();
        let mut expected_line = self.lines.line0(node.start_byte() as u32);
        while let Some(s) = sib {
            if s.kind() != "comment" {
                return;
            }
            let end_line = self.lines.line0(s.end_byte().saturating_sub(1) as u32);
            if end_line + 1 < expected_line {
                return;
            }
            expected_line = self.lines.line0(s.start_byte() as u32);
            let raw = &self.src[s.start_byte()..s.end_byte().min(self.src.len())];
            if let Some(rest) = raw.strip_prefix(b"//export ") {
                let symbol = String::from_utf8_lossy(rest).trim().to_string();
                let valid = symbol.starts_with(|c: char| c == '_' || c.is_ascii_alphabetic())
                    && symbol.chars().all(|c| c == '_' || c.is_ascii_alphanumeric());
                if valid {
                    let decl = self.decl_by_name.get(&(name_node.start_byte() as u32)).copied();
                    self.push(
                        BridgeKind::CAbi,
                        BoundaryRole::Provides,
                        symbol,
                        None,
                        decl,
                        span(node),
                        vec![
                            ("linkage".into(), "go".into()),
                            ("definition".into(), "true".into()),
                            ("rule".into(), "cgo_export".into()),
                        ],
                    );
                }
                return;
            }
            sib = s.prev_sibling();
        }
    }

    pub(super) fn go_type(&self, node: Node<'_>) {
        let Some(ty) = node.child_by_field_name("type").filter(|t| t.kind() == "struct_type") else {
            return;
        };
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let Some(list) = crate::node::find_descendant(ty, 4, |n| n.kind() == "field_declaration_list") else {
            return;
        };
        for field in named_children(list) {
            if field.kind() != "field_declaration" || field.child_by_field_name("name").is_some() {
                continue;
            }
            let Some(fty) = field.child_by_field_name("type") else { continue };
            // A struct embedding the generated `Unimplemented<Service>Server`.
            let embedded = crate::node::type_name(fty, self.src);
            let Some(svc) = self.service_of("rpc_server_embed", &embedded) else {
                continue;
            };
            let decl = self.decl_by_name.get(&(name_node.start_byte() as u32)).copied();
            self.push(
                BridgeKind::Grpc,
                BoundaryRole::Provides,
                format!("{svc}/*"),
                None,
                decl,
                span(node),
                vec![
                    ("service".into(), svc.to_string()),
                    ("impl_type".into(), self.txt(name_node)),
                    ("rule".into(), "go_unimplemented_embed".into()),
                ],
            );
        }
    }
}
