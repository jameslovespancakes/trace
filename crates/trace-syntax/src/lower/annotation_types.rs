//! Annotation types and their meta-annotations (Java `@interface` declarations).

use trace_core::Language;
use tree_sitter::Node;

use super::library::string_content;
use crate::node::{named_children, text};

/// One annotation use (`@Mapped(method = Verb.GET)`): its name and element
/// values (`None` key = the single unnamed value; arrays are comma-joined).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnnotationUse {
    pub name: String,
    pub values: Vec<(Option<String>, String)>,
}

/// One element of an annotation type (`String[] value() default {}` with its own
/// annotations, e.g. `@Alias(annotation = Mapped.class)`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnnotationElement {
    pub name: String,
    pub default: Option<String>,
    pub annotations: Vec<AnnotationUse>,
}

/// An annotation type declaration (Java `@interface`) with its meta-annotations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnnotationType {
    pub name: String,
    pub annotations: Vec<AnnotationUse>,
    pub elements: Vec<AnnotationElement>,
}

/// Annotation type declarations of a library file (Java; other languages declare
/// annotations as ordinary classes whose bases the index facts already carry).
pub fn annotation_types(language: Language, source: &[u8]) -> Vec<AnnotationType> {
    if language != Language::Java {
        return Vec::new();
    }
    let Ok(tree) = crate::parse_tree(language, source) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.kind() == "annotation_type_declaration" {
            let name = n
                .child_by_field_name("name")
                .map(|x| text(x, source).trim().to_string())
                .unwrap_or_default();
            let mut elements = Vec::new();
            if let Some(body) = n.child_by_field_name("body") {
                for el in named_children(body) {
                    if el.kind() == "annotation_type_element_declaration" {
                        elements.push(AnnotationElement {
                            name: el
                                .child_by_field_name("name")
                                .map(|x| text(x, source).trim().to_string())
                                .unwrap_or_default(),
                            default: el.child_by_field_name("value").map(|v| annotation_value(v, source)),
                            annotations: annotation_uses(el, source),
                        });
                    }
                }
            }
            out.push(AnnotationType {
                name,
                annotations: annotation_uses(n, source),
                elements,
            });
        }
        let mut children = named_children(n);
        children.reverse();
        stack.extend(children);
    }
    out
}

/// Annotations in the `modifiers` of a declaration node.
fn annotation_uses(node: Node<'_>, source: &[u8]) -> Vec<AnnotationUse> {
    let Some(modifiers) = crate::node::first_of_kind(node, "modifiers") else {
        return Vec::new();
    };
    named_children(modifiers)
        .into_iter()
        .filter(|m| m.kind() == "annotation" || m.kind() == "marker_annotation")
        .map(|m| {
            let name = m
                .child_by_field_name("name")
                .map(|x| text(x, source).trim().to_string())
                .unwrap_or_default();
            let values = m
                .child_by_field_name("arguments")
                .map(|args| {
                    named_children(args)
                        .into_iter()
                        .map(|a| {
                            if a.kind() == "element_value_pair" {
                                (
                                    a.child_by_field_name("key")
                                        .map(|k| text(k, source).trim().to_string()),
                                    a.child_by_field_name("value")
                                        .map(|v| annotation_value(v, source))
                                        .unwrap_or_default(),
                                )
                            } else {
                                (None, annotation_value(a, source))
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            AnnotationUse { name, values }
        })
        .collect()
}

/// Structural value of an annotation element: string literal content, comma-joined arrays,
/// else the expression text (`Verb.GET`, `Mapped.class`).
fn annotation_value(node: Node<'_>, source: &[u8]) -> String {
    match node.kind() {
        "string_literal" => string_content(node, source),
        "element_value_array_initializer" => named_children(node)
            .into_iter()
            .map(|v| annotation_value(v, source))
            .collect::<Vec<_>>()
            .join(","),
        _ => text(node, source).trim().to_string(),
    }
}
