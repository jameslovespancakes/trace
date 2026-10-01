//! C++ template-dependent calls: an unanswered call whose callee depends on a template
//! parameter is `template_dependent` (syntax proof over the tree-sitter tree).

use std::collections::HashSet;
use trace_core::facts::FileFacts;
use trace_core::model::ByteSpan;
use trace_core::Language;

/// Nesting bound of the template-dependence walk (receiver chains, declarators).
pub(in crate::engine) const MAX_DEPENDENT_DEPTH: usize = 16;

/// Template parameters visible at a node: type parameters (`typename T`, `class... Ts`,
/// template template parameters) and value parameters (`int N`, `F f` in a template head).
#[derive(Default)]
pub(in crate::engine) struct TemplateScope {
    pub(in crate::engine) types: HashSet<String>,
    pub(in crate::engine) values: HashSet<String>,
}

/// C++ dependent names ([temp.dep]): the calls (indices into `facts.calls`, a subset of
/// `calls`) whose callee depends on a template parameter of an enclosing template, so the
/// server can resolve them only per instantiation: a member call whose receiver is a name
/// declared with a type that spells a template type parameter (`T&`, `std::vector<T>`,
/// `typename T::X`) or with `auto` as a parameter (abbreviated templates, generic lambdas),
/// a call of such a name or of a template value parameter, a qualified call whose scope is a
/// template type parameter (`T::make()`), a template call with dependent template arguments,
/// and an unqualified call with such an argument (argument-dependent lookup at
/// instantiation). Syntax tree only; never a guess about the target.
pub(in crate::engine) fn template_dependent_calls(
    source: &[u8],
    facts: &FileFacts,
    calls: &[usize],
) -> HashSet<usize> {
    let mut found = HashSet::new();
    if calls.is_empty() {
        return found;
    }
    let Ok(tree) = trace_syntax::parse_tree(Language::Cpp, source) else {
        return found;
    };
    let root = tree.root_node();
    for &ci in calls {
        let Some(c) = facts.calls.get(ci) else { continue };
        let Some(function) = exact_node(root, c.callee_span) else { continue };
        let scope = template_scope(function, source);
        if callee_is_dependent(function, &scope, source, 0) {
            found.insert(ci);
        }
    }
    found
}

/// The largest node spanning exactly `span`.
pub(in crate::engine) fn exact_node(
    root: tree_sitter::Node<'_>,
    span: ByteSpan,
) -> Option<tree_sitter::Node<'_>> {
    let (start, end) = (span.start as usize, span.end as usize);
    let mut n = root.descendant_for_byte_range(start, end)?;
    if n.start_byte() != start || n.end_byte() != end {
        return None;
    }
    while let Some(p) = n.parent() {
        if p.start_byte() == start && p.end_byte() == end {
            n = p;
        } else {
            break;
        }
    }
    Some(n)
}

pub(in crate::engine) fn node_text<'s>(n: tree_sitter::Node<'_>, source: &'s [u8]) -> &'s str {
    n.utf8_text(source).unwrap_or("")
}

pub(in crate::engine) fn named_kids(n: tree_sitter::Node<'_>) -> Vec<tree_sitter::Node<'_>> {
    let mut cursor = n.walk();
    n.named_children(&mut cursor).collect()
}

/// Template parameters of every enclosing `template <...>` head and templated lambda.
pub(in crate::engine) fn template_scope(node: tree_sitter::Node<'_>, source: &[u8]) -> TemplateScope {
    let mut scope = TemplateScope::default();
    let mut current = node.parent();
    while let Some(n) = current {
        let list = match n.kind() {
            "template_declaration" => n.child_by_field_name("parameters"),
            "lambda_expression" => n.child_by_field_name("template_parameters"),
            _ => None,
        };
        if let Some(list) = list {
            for p in named_kids(list) {
                template_parameter(p, source, &mut scope, 0);
            }
        }
        current = n.parent();
    }
    scope
}

/// One entry of a `template_parameter_list`.
pub(in crate::engine) fn template_parameter(
    p: tree_sitter::Node<'_>,
    source: &[u8],
    scope: &mut TemplateScope,
    depth: usize,
) {
    if depth > MAX_DEPENDENT_DEPTH {
        return;
    }
    match p.kind() {
        "type_parameter_declaration" | "variadic_type_parameter_declaration" => {
            for k in named_kids(p) {
                if k.kind() == "type_identifier" {
                    scope.types.insert(node_text(k, source).to_string());
                }
            }
        }
        "optional_type_parameter_declaration" => {
            if let Some(name) = p.child_by_field_name("name") {
                scope.types.insert(node_text(name, source).to_string());
            }
        }
        "template_template_parameter_declaration" => {
            for k in named_kids(p) {
                if k.kind() != "template_parameter_list" {
                    template_parameter(k, source, scope, depth + 1);
                }
            }
        }
        "parameter_declaration" | "optional_parameter_declaration" | "variadic_parameter_declaration" => {
            if let Some(name) = p
                .child_by_field_name("declarator")
                .and_then(|d| declarator_name(d, 0))
            {
                scope.values.insert(node_text(name, source).to_string());
            }
        }
        _ => {}
    }
}

/// The identifier a declarator declares (through pointer / reference / array / init /
/// function declarators).
pub(in crate::engine) fn declarator_name(
    d: tree_sitter::Node<'_>,
    depth: usize,
) -> Option<tree_sitter::Node<'_>> {
    if depth > MAX_DEPENDENT_DEPTH {
        return None;
    }
    match d.kind() {
        "identifier" | "field_identifier" => Some(d),
        _ => match d.child_by_field_name("declarator") {
            Some(inner) => declarator_name(inner, depth + 1),
            // `reference_declarator` / `variadic_declarator` have no field names.
            None => named_kids(d).into_iter().find_map(|k| declarator_name(k, depth + 1)),
        },
    }
}

/// Whether the callee expression of a call depends on a template parameter.
pub(in crate::engine) fn callee_is_dependent(
    function: tree_sitter::Node<'_>,
    scope: &TemplateScope,
    source: &[u8],
    depth: usize,
) -> bool {
    if depth > MAX_DEPENDENT_DEPTH {
        return false;
    }
    match function.kind() {
        "field_expression" => function
            .child_by_field_name("argument")
            .is_some_and(|receiver| expression_is_dependent(receiver, scope, source, depth + 1)),
        "identifier" => {
            let name = node_text(function, source);
            scope.values.contains(name)
                || name_is_dependent(name, function, scope, source)
                || arguments_are_dependent(function, scope, source, depth)
        }
        "template_function" => {
            function
                .child_by_field_name("arguments")
                .is_some_and(|args| type_is_dependent(args, scope, source, false))
                || arguments_are_dependent(function, scope, source, depth)
        }
        "qualified_identifier" => function
            .child_by_field_name("scope")
            .is_some_and(|s| type_is_dependent(s, scope, source, false)),
        "parenthesized_expression" => named_kids(function)
            .into_iter()
            .any(|k| callee_is_dependent(k, scope, source, depth + 1)),
        _ => false,
    }
}

/// An unqualified call with an argument that depends on a template parameter (its lookup
/// happens at instantiation).
pub(in crate::engine) fn arguments_are_dependent(
    function: tree_sitter::Node<'_>,
    scope: &TemplateScope,
    source: &[u8],
    depth: usize,
) -> bool {
    let Some(call) = function.parent().filter(|p| p.kind() == "call_expression") else {
        return false;
    };
    call.child_by_field_name("arguments").is_some_and(|args| {
        named_kids(args)
            .into_iter()
            .any(|a| expression_is_dependent(a, scope, source, depth + 1))
    })
}

/// Whether a value expression's type depends on a template parameter.
pub(in crate::engine) fn expression_is_dependent(
    expr: tree_sitter::Node<'_>,
    scope: &TemplateScope,
    source: &[u8],
    depth: usize,
) -> bool {
    if depth > MAX_DEPENDENT_DEPTH {
        return false;
    }
    match expr.kind() {
        "identifier" => {
            let name = node_text(expr, source);
            scope.values.contains(name) || name_is_dependent(name, expr, scope, source)
        }
        "field_expression" | "pointer_expression" | "subscript_expression" => expr
            .child_by_field_name("argument")
            .is_some_and(|inner| expression_is_dependent(inner, scope, source, depth + 1)),
        "call_expression" => expr
            .child_by_field_name("function")
            .is_some_and(|f| callee_is_dependent(f, scope, source, depth + 1)),
        "parenthesized_expression" => named_kids(expr)
            .into_iter()
            .any(|k| expression_is_dependent(k, scope, source, depth + 1)),
        _ => false,
    }
}

/// Whether the nearest declaration of `name` visible at `at` (a parameter of an enclosing
/// function or lambda, a local declared before `at` in an enclosing block or `for`, a field
/// of an enclosing class) has a type that depends on a template parameter.
pub(in crate::engine) fn name_is_dependent(
    name: &str,
    at: tree_sitter::Node<'_>,
    scope: &TemplateScope,
    source: &[u8],
) -> bool {
    let before = at.start_byte();
    let declares = |d: tree_sitter::Node<'_>| {
        let mut cursor = d.walk();
        let found = d
            .children_by_field_name("declarator", &mut cursor)
            .any(|decl| declarator_name(decl, 0).is_some_and(|n| node_text(n, source) == name));
        found
    };
    let dependent = |d: tree_sitter::Node<'_>, parameter: bool| {
        d.child_by_field_name("type")
            .is_some_and(|t| type_is_dependent(t, scope, source, parameter))
    };
    let mut current = at.parent();
    while let Some(n) = current {
        match n.kind() {
            "compound_statement" => {
                // The last declaration of the name before `at` in this block.
                let local = named_kids(n)
                    .into_iter()
                    .rev()
                    .find(|k| k.kind() == "declaration" && k.end_byte() <= before && declares(*k));
                if let Some(d) = local {
                    return dependent(d, false);
                }
            }
            "for_range_loop" => {
                if declares(n) {
                    return dependent(n, false);
                }
            }
            "for_statement" => {
                if let Some(init) = n
                    .child_by_field_name("initializer")
                    .filter(|i| i.kind() == "declaration" && declares(*i))
                {
                    return dependent(init, false);
                }
            }
            "function_definition" | "lambda_expression" => {
                let parameters = n
                    .child_by_field_name("declarator")
                    .and_then(|d| function_parameters(d, 0));
                if let Some(list) = parameters {
                    let param = named_kids(list).into_iter().find(|p| {
                        p.child_by_field_name("declarator")
                            .and_then(|d| declarator_name(d, 0))
                            .is_some_and(|id| node_text(id, source) == name)
                    });
                    if let Some(p) = param {
                        return dependent(p, true);
                    }
                }
            }
            "field_declaration_list" => {
                let field = named_kids(n)
                    .into_iter()
                    .find(|k| k.kind() == "field_declaration" && declares(*k));
                if let Some(f) = field {
                    return dependent(f, false);
                }
            }
            _ => {}
        }
        current = n.parent();
    }
    false
}

/// The parameter list of a function / lambda declarator (through pointer / reference
/// declarators around the function declarator).
pub(in crate::engine) fn function_parameters(
    d: tree_sitter::Node<'_>,
    depth: usize,
) -> Option<tree_sitter::Node<'_>> {
    if depth > MAX_DEPENDENT_DEPTH {
        return None;
    }
    if matches!(d.kind(), "function_declarator" | "abstract_function_declarator") {
        return d.child_by_field_name("parameters");
    }
    match d.child_by_field_name("declarator") {
        Some(inner) => function_parameters(inner, depth + 1),
        None => named_kids(d)
            .into_iter()
            .find_map(|k| function_parameters(k, depth + 1)),
    }
}

/// Whether a type spelling mentions a template type parameter (anywhere, template
/// arguments and qualifier scopes included); a parameter declared `auto` is an abbreviated
/// template parameter.
pub(in crate::engine) fn type_is_dependent(
    t: tree_sitter::Node<'_>,
    scope: &TemplateScope,
    source: &[u8],
    parameter: bool,
) -> bool {
    let mut stack = vec![(t, 0usize)];
    while let Some((n, depth)) = stack.pop() {
        match n.kind() {
            "type_identifier" | "namespace_identifier" => {
                if scope.types.contains(node_text(n, source)) {
                    return true;
                }
            }
            "placeholder_type_specifier" | "auto" if parameter => return true,
            _ => {}
        }
        if depth < MAX_DEPENDENT_DEPTH {
            stack.extend(named_kids(n).into_iter().map(|k| (k, depth + 1)));
        }
    }
    false
}
