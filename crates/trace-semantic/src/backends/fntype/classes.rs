//! Per-language classification of a declared parameter type (function type, top type,
//! data) over the language's tree-sitter nodes and function-type table.

use trace_core::Language;
use tree_sitter::Node;

use super::*;

/// Classify one type node.
pub(super) fn class_of(cx: &Cx<'_>, n: Node<'_>, depth: u8) -> Class {
    if depth > MAX_DEPTH {
        return Class::Unknown;
    }
    let d = depth + 1;
    match cx.language {
        Language::Python => python_class(cx, n, d),
        Language::Rust => rust_class(cx, n, d),
        Language::C | Language::Cpp => c_type_class(cx, n, d),
        Language::Go => go_class(cx, n, d),
        Language::Php => php_class(cx, n, d),
        Language::CSharp => csharp_class(cx, n, d),
        Language::Java => java_class(cx, n, d),
        Language::Scala => scala_class(cx, n, d),
        _ => Class::Unknown,
    }
}

pub(super) fn first_class(cx: &Cx<'_>, n: Node<'_>, d: u8) -> Class {
    first_named(n).map_or(Class::Unknown, |c| class_of(cx, c, d))
}

pub(super) fn python_class(cx: &Cx<'_>, n: Node<'_>, d: u8) -> Class {
    match n.kind() {
        "type" | "parenthesized_expression" | "constrained_type" => first_class(cx, n, d),
        "identifier" | "attribute" | "member_type" => cx.named(cx.text(n), n.start_byte()),
        "none" => Class::Not,
        "ellipsis" => Class::Top,
        "union_type" => union(named_kids(n).into_iter().map(|c| class_of(cx, c, d))),
        "binary_operator" => {
            let op = n.child_by_field_name("operator").map(|o| cx.text(o));
            if op != Some("|") {
                return Class::Unknown;
            }
            let sides = [n.child_by_field_name("left"), n.child_by_field_name("right")];
            union(sides.into_iter().flatten().map(|c| class_of(cx, c, d)))
        }
        "generic_type" => {
            let parts = named_kids(n);
            let Some(base) = parts.first().copied() else {
                return Class::Unknown;
            };
            let args: Vec<Node<'_>> = parts
                .iter()
                .filter(|p| p.kind() == "type_parameter")
                .flat_map(|p| named_kids(*p))
                .collect();
            python_generic(cx, base, &args, d)
        }
        "subscript" => {
            let Some(base) = n.child_by_field_name("value") else {
                return Class::Unknown;
            };
            let mut cursor = n.walk();
            let args: Vec<Node<'_>> = n.children_by_field_name("subscript", &mut cursor).collect();
            python_generic(cx, base, &args, d)
        }
        "string" => {
            // A forward reference: the annotation is the string's content.
            let content: String = named_kids(n)
                .into_iter()
                .filter(|c| c.kind() == "string_content")
                .map(|c| cx.text(c).to_string())
                .collect();
            match standalone_type_class(Language::Python, &content, cx.tables) {
                Class::Named(name, _) => Class::Named(name, usize::MAX),
                other => other,
            }
        }
        "list" | "integer" | "string_content" => Class::Not,
        _ => Class::Unknown,
    }
}

pub(super) fn python_generic(cx: &Cx<'_>, base: Node<'_>, args: &[Node<'_>], d: u8) -> Class {
    let name = cx.text(base);
    if is_wrapper(cx.language, name, cx.tables) {
        return if last_segment(name) == "Union" {
            union(args.iter().map(|a| class_of(cx, *a, d)))
        } else {
            args.first().map_or(Class::Unknown, |a| class_of(cx, *a, d))
        };
    }
    cx.named(name, base.start_byte())
}

pub(super) fn rust_class(cx: &Cx<'_>, n: Node<'_>, d: u8) -> Class {
    match n.kind() {
        "function_type" => Class::Function,
        "abstract_type" | "dynamic_type" => {
            let t = n.child_by_field_name("trait").or_else(|| first_named(n));
            match t.map(|t| class_of(cx, t, d)) {
                Some(Class::Function) => Class::Function,
                Some(Class::Data) => Class::Data,
                None | Some(Class::Unknown) => Class::Unknown,
                // `impl Debug`, `dyn Any`: the function is accepted as a value.
                Some(_) => Class::Top,
            }
        }
        "bounded_type" => {
            let classes: Vec<Class> = named_kids(n)
                .into_iter()
                .filter(|c| c.kind() != "lifetime")
                .map(|c| class_of(cx, c, d))
                .collect();
            if classes.contains(&Class::Function) {
                Class::Function
            } else {
                Class::Top
            }
        }
        "reference_type" | "pointer_type" => n
            .child_by_field_name("type")
            .map_or(Class::Unknown, |t| class_of(cx, t, d)),
        "generic_type" => {
            let Some(base) = n.child_by_field_name("type") else {
                return Class::Unknown;
            };
            let name = cx.text(base);
            if is_wrapper(cx.language, name, cx.tables) {
                let first = n.child_by_field_name("type_arguments").and_then(|a| {
                    named_kids(a)
                        .into_iter()
                        .find(|c| c.kind() != "lifetime" && c.kind() != "type_binding")
                });
                return first.map_or(Class::Unknown, |t| class_of(cx, t, d));
            }
            cx.named(name, base.start_byte())
        }
        "type_identifier" | "scoped_type_identifier" | "identifier" | "scoped_identifier" => {
            cx.named(cx.text(n), n.start_byte())
        }
        "trait_bounds" => {
            let classes: Vec<Class> = named_kids(n)
                .into_iter()
                .filter(|c| c.kind() != "lifetime")
                .map(|c| class_of(cx, c, d))
                .collect();
            if classes.contains(&Class::Function) {
                Class::Function
            } else {
                Class::Top
            }
        }
        "primitive_type" | "tuple_type" | "array_type" | "unit_type" | "never_type" => Class::Not,
        _ => Class::Unknown,
    }
}

/// Whether a C/C++ declarator declares a function (pointer, reference or plain function
/// declarator anywhere in its chain).
pub(super) fn declarator_is_function(d: Node<'_>) -> bool {
    if matches!(d.kind(), "function_declarator" | "abstract_function_declarator") {
        return true;
    }
    if matches!(
        d.kind(),
        "pointer_declarator"
            | "abstract_pointer_declarator"
            | "parenthesized_declarator"
            | "abstract_parenthesized_declarator"
            | "reference_declarator"
            | "abstract_reference_declarator"
    ) {
        return named_kids(d).into_iter().any(declarator_is_function);
    }
    false
}

pub(super) fn declarator_is_pointer(d: Node<'_>) -> bool {
    matches!(d.kind(), "pointer_declarator" | "abstract_pointer_declarator")
        || named_kids(d).into_iter().any(declarator_is_pointer)
}

/// Class of a C/C++ parameter declaration (type specifier + declarator).
pub(super) fn c_param_class(cx: &Cx<'_>, p: Node<'_>, d: u8) -> Class {
    if p.kind() == "variadic_parameter" {
        return Class::Top;
    }
    let declarator = p.child_by_field_name("declarator");
    if declarator.is_some_and(declarator_is_function) {
        return Class::Function;
    }
    let Some(ty) = p.child_by_field_name("type") else {
        return Class::Unknown;
    };
    if declarator.is_some_and(declarator_is_pointer) && ty.kind() == "primitive_type" && cx.text(ty) == "void"
    {
        return Class::Top;
    }
    class_of(cx, ty, d)
}

pub(super) fn c_type_class(cx: &Cx<'_>, n: Node<'_>, d: u8) -> Class {
    match n.kind() {
        "primitive_type"
        | "sized_type_specifier"
        | "struct_specifier"
        | "union_specifier"
        | "enum_specifier"
        | "class_specifier" => Class::Not,
        "type_identifier" => cx.named(cx.text(n), n.start_byte()),
        "qualified_identifier" | "template_type" => {
            let name = base_name(cx.text(n)).to_string();
            if is_wrapper(cx.language, &name, cx.tables) {
                let arg = all_of_kind(n, &["template_argument_list"], 1)
                    .into_iter()
                    .next()
                    .and_then(first_named);
                return arg.map_or(Class::Unknown, |a| class_of(cx, a, d));
            }
            cx.named(&name, n.start_byte())
        }
        "placeholder_type_specifier" => {
            match n.child_by_field_name("constraint") {
                Some(c) => match name_class(cx.language, cx.text(c), cx.tables) {
                    Some(Class::Function) => Class::Function,
                    _ => Class::Top,
                },
                // Plain `auto`: an unconstrained template parameter.
                None => Class::Top,
            }
        }
        "type_descriptor" => {
            if n.child_by_field_name("declarator")
                .is_some_and(declarator_is_function)
            {
                return Class::Function;
            }
            n.child_by_field_name("type")
                .map_or(Class::Unknown, |t| class_of(cx, t, d))
        }
        "auto" => Class::Top,
        _ => Class::Unknown,
    }
}

pub(super) fn go_class(cx: &Cx<'_>, n: Node<'_>, d: u8) -> Class {
    match n.kind() {
        "function_type" => Class::Function,
        "type_identifier" | "qualified_type" => cx.named(cx.text(n), n.start_byte()),
        "generic_type" => {
            let base = n.child_by_field_name("type").unwrap_or(n);
            cx.named(cx.text(base), base.start_byte())
        }
        "interface_type" => {
            if n.named_child_count() == 0 {
                Class::Top
            } else {
                Class::Named("interface".into(), usize::MAX)
            }
        }
        "parenthesized_type" | "pointer_type" => first_class(cx, n, d),
        "slice_type" | "array_type" | "map_type" | "channel_type" | "struct_type" => Class::Not,
        _ => Class::Unknown,
    }
}

pub(super) fn php_class(cx: &Cx<'_>, n: Node<'_>, d: u8) -> Class {
    match n.kind() {
        "primitive_type" => match cx.text(n) {
            "callable" => Class::Function,
            "mixed" | "object" => Class::Top,
            _ => Class::Not,
        },
        "named_type" | "qualified_name" | "name" => cx.named(cx.text(n), n.start_byte()),
        "optional_type" => first_class(cx, n, d),
        "union_type" | "disjunctive_normal_form_type" => {
            union(named_kids(n).into_iter().map(|c| class_of(cx, c, d)))
        }
        "intersection_type" | "bottom_type" => Class::Not,
        _ => Class::Unknown,
    }
}

pub(super) fn csharp_class(cx: &Cx<'_>, n: Node<'_>, d: u8) -> Class {
    match n.kind() {
        "predefined_type" => match cx.text(n) {
            "object" | "dynamic" => Class::Top,
            _ => Class::Not,
        },
        "nullable_type" => n
            .child_by_field_name("type")
            .map_or_else(|| first_class(cx, n, d), |t| class_of(cx, t, d)),
        "array_type" | "tuple_type" | "pointer_type" | "function_pointer_type" => Class::Not,
        "generic_name" => {
            let base = first_named(n).unwrap_or(n);
            cx.named(cx.text(base), base.start_byte())
        }
        "qualified_name" | "identifier" | "alias_qualified_name" => cx.named(cx.text(n), n.start_byte()),
        _ => Class::Unknown,
    }
}

pub(super) fn java_class(cx: &Cx<'_>, n: Node<'_>, _d: u8) -> Class {
    match n.kind() {
        "type_identifier" | "scoped_type_identifier" => cx.named(cx.text(n), n.start_byte()),
        "generic_type" => {
            let base = first_named(n).unwrap_or(n);
            cx.named(cx.text(base), base.start_byte())
        }
        "integral_type" | "floating_point_type" | "boolean_type" | "array_type" | "void_type" => Class::Not,
        _ => Class::Unknown,
    }
}

pub(super) fn scala_class(cx: &Cx<'_>, n: Node<'_>, d: u8) -> Class {
    match n.kind() {
        // `A => B`, `(A, B) ?=> C`, and by-name `=> A` (the argument runs on each read).
        "function_type" | "lazy_parameter_type" => Class::Function,
        "repeated_parameter_type" | "annotated_type" => first_class(cx, n, d),
        "type_identifier" | "stable_type_identifier" => cx.named(cx.text(n), n.start_byte()),
        "generic_type" => {
            let base = n.child_by_field_name("type").or_else(|| first_named(n)).unwrap_or(n);
            cx.named(cx.text(base), base.start_byte())
        }
        "compound_type" | "infix_type" => Class::Named(cx.text(n).to_string(), usize::MAX),
        "tuple_type" => Class::Not,
        _ => Class::Unknown,
    }
}
