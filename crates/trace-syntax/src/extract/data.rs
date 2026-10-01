//! Non-callable source definitions, deliberately separate from executing declarations.
use trace_core::facts::{DataDefinition, FileFacts};
use trace_core::{Language, Span};
use tree_sitter::Node;

use super::{Ctx, Walker};
use crate::node::{named_children, span, text};

impl<'a, 't> Walker<'a, 't> {
    pub(super) fn module_data(&self, node: Node<'t>, ctx: &Ctx, facts: &mut FileFacts) {
        if self.language == Language::Go {
            self.go_package_data(node, ctx, facts);
            return;
        }
        if self.language != Language::Python
            || node.kind() != "assignment"
            || ctx.lexical.is_some()
            || ctx.scope.is_some()
            || ctx.in_import
            || node.has_error()
            || node.child_by_field_name("right").is_none()
        {
            return;
        }
        let Some(left) = node.child_by_field_name("left") else {
            return;
        };
        // a = b = value: both bindings retrieve the entire original statement.
        let mut statement = node;
        while let Some(parent) = statement.parent().filter(|p| p.kind() == "assignment") {
            statement = parent;
        }
        let bytes = span(statement);
        let range = Span {
            bytes,
            start_line: self.lines.line1(bytes.start),
            end_line: self.lines.line1(bytes.end.saturating_sub(1)),
        };
        let mut parent = statement.parent();
        if parent.is_some_and(|p| p.kind() == "expression_statement") {
            parent = parent.and_then(|p| p.parent());
        }
        let conditional = parent.is_some_and(|p| p.kind() != "module");
        let mut stack = vec![left];
        while let Some(target) = stack.pop() {
            match target.kind() {
                "identifier" => facts.data_definitions.push(DataDefinition {
                    name: text(target, self.source).into_owned(),
                    name_span: span(target),
                    span: range,
                    conditional,
                }),
                "pattern_list" | "tuple_pattern" | "list_pattern" | "list_splat_pattern" => {
                    stack.extend(named_children(target).into_iter().rev());
                }
                // obj.attr, obj[index] and type syntax do not define module bindings.
                _ => {}
            }
        }
    }

    // Grammar-derived package bindings, never a table of project-specific names/values.
    // Preserve the whole declaration group: omitted const expressions and iota depend on
    // preceding specs and their ordinal. Do not evaluate or invent their runtime values.
    fn go_package_data(&self, node: Node<'t>, ctx: &Ctx, facts: &mut FileFacts) {
        if !matches!(node.kind(), "const_spec" | "var_spec")
            || ctx.lexical.is_some()
            || ctx.scope.is_some()
            || ctx.in_import
            || node.has_error()
            || (node.kind() == "var_spec" && node.child_by_field_name("value").is_none())
        {
            return;
        }
        let mut statement = node;
        while let Some(parent) = statement.parent() {
            if matches!(parent.kind(), "const_declaration" | "var_declaration") {
                statement = parent;
                break;
            }
            if parent.kind() == "source_file" {
                break;
            }
            statement = parent;
        }
        if statement.has_error() {
            return;
        }
        let bytes = span(statement);
        let range = Span {
            bytes,
            start_line: self.lines.line1(bytes.start),
            end_line: self.lines.line1(bytes.end.saturating_sub(1)),
        };
        let mut cursor = node.walk();
        for name in node.children_by_field_name("name", &mut cursor) {
            let spelling = text(name, self.source);
            if spelling == "_" || !self.spec.is_identifier(name.kind()) {
                continue;
            }
            facts.data_definitions.push(DataDefinition {
                name: spelling.into_owned(),
                name_span: span(name),
                span: range,
                conditional: false,
            });
        }
    }
}
