//! PHP: the syntax spec (grammar, query, node-kind tables and language rules).

use trace_core::facts::ImportKind;
use trace_core::facts::ParamKind::{Positional, VarPositional};
use trace_core::Language;
use tree_sitter::Node;

use crate::interface::ReturnRule;
use crate::language_rules::{BareCallBinding, BareCalls, LanguageRules, ModulePaths, NONE};
use crate::names::{join_sep, last_leaf, path_text, ImportBinding};
use crate::node::{named_children, text};
use crate::scopes::{self, hoisted, pair, ScopeRules};
use crate::spec::{
    call, callback_form, choice, for_loop, fp, member, method_call, new_call, param, unwrap, NameAt,
    Receiver, SyntaxSpec,
};
use crate::typefacts::form;

/// Inference rules of the language (`crate::language_rules`).
const RULES: LanguageRules = LanguageRules {
    bare_call_binding: Some(BareCallBinding::FileLevel {
        declared_before: false,
    }),
    interfaces_may_be_mixins: true,
    super_receiver: Some(("parent", "::")),
    modules: ModulePaths::NamespaceDirectories,
    bare_calls: BareCalls::FunctionsOnly,
    global_nested_functions: true,
    allocation_receivers: true,
    ..NONE
};

pub(crate) static SYNTAX: SyntaxSpec = SyntaxSpec {
    rules: RULES,
    // First-class callable syntax `f(...)`.
    callback_forms: &[callback_form(
        "function_call_expression",
        "function",
        &["name"],
        0,
        ("arguments/#0", "..."),
    )],
    lazy_scopes: &[
        "function_definition",
        "method_declaration",
        "anonymous_function",
        "arrow_function",
    ],
    anonymous_functions: &["anonymous_function", "arrow_function"],
    declaring_stores: &["argument"],
    type_contexts: &["base_clause", "class_interface_clause"],
    type_fields: &["type", "return_type"],
    imports: &["namespace_use_declaration"],
    class_bodies: &["declaration_list"],
    identifiers: &["variable_name"],
    name_kinds: &["name"],
    self_names: &["$this"],
    constructor_names: &["__construct"],
    member_access: &[
        member("member_access_expression", "object", "name"),
        member("nullsafe_member_access_expression", "object", "name"),
        member("scoped_property_access_expression", "scope", "name"),
        member("class_constant_access_expression", "#0", "#-1"),
    ],
    calls: &[
        call("function_call_expression", "function", "arguments"),
        method_call("member_call_expression", "name", "arguments", "object"),
        method_call("nullsafe_member_call_expression", "name", "arguments", "object"),
        method_call("scoped_call_expression", "name", "arguments", "scope"),
        new_call("object_creation_expression", "#0", "=arguments"),
    ],
    argument_wrappers: &["argument"],
    choices: &[choice("conditional_expression", &["body", "alternative"])],
    unwrap: &[unwrap("parenthesized_expression", "#0")],
    subscripts: &[fp("subscript_expression", "#0", "")],
    assignments: &[fp("assignment_expression", "left", "right")],
    store_fields: &[
        fp("assignment_expression", "left", ""),
        fp("augmented_assignment_expression", "left", ""),
        fp("argument", "name", ""),
    ],
    binding_kinds: &[
        "namespace_use_declaration",
        "namespace_definition",
        "formal_parameters",
        "attribute_list",
    ],
    load_fields: &[fp("namespace_definition", "body", "")],
    returns: &["return_statement"],
    expression_statements: &["expression_statement"],
    conditions: &[fp("if_statement", "condition", "")],
    binary_ops: &["binary_expression"],
    comments: &["comment"],
    params: &[
        param("simple_parameter", NameAt::Pick("name"), Positional, "default_value"),
        param("property_promotion_parameter", NameAt::Pick("name"), Positional, "default_value"),
        param("variadic_parameter", NameAt::Pick("name"), VarPositional, ""),
    ],
    param_fields: &["parameters"],
    receiver: Receiver::Implicit("$this"),
    stub_when_bodiless: true,
    type_forms: &[
        form("simple_parameter", "name", "type", ""),
        form("property_promotion_parameter", "name", "type", ""),
        form("property_declaration", "=property_element", "type", ""),
        form("assignment_expression", "left", "", "right"),
    ],
    return_types: &[("function_definition", "return_type"), ("method_declaration", "return_type")],
    type_path_separator: "\\",
    scopes: &ScopeRules {
        binders: &[
            hoisted("assignment_expression", "left"),
            hoisted("augmented_assignment_expression", "left"),
            hoisted("foreach_statement", "#1"),
            hoisted("catch_clause", "name"),
            hoisted("anonymous_function_use_clause", ""),
        ],
        closed: &["function_definition", "method_declaration", "anonymous_function"],
        statics: &[
            pair("scoped_call_expression", "scope", "name"),
            pair("scoped_property_access_expression", "scope", "name"),
            pair("class_constant_access_expression", "#0", "#-1"),
        ],
        ..scopes::NONE
    },
    library_loops: &[for_loop("foreach_statement", "#1", "#0")],
    return_rule: ReturnRule::ReturnStatements,
    import_readers: &[("namespace_use_declaration", read_use)],
    library_table: Some("php"),
    cases_fall_through: true,
    ..SyntaxSpec::empty(Language::Php, grammar, include_str!("../../queries/php.scm"))
};

fn grammar() -> tree_sitter::Language {
    tree_sitter::Language::new(tree_sitter_php::LANGUAGE_PHP)
}

/// PHP `use A\B;`, `use A\B as C;`, `use A\{B, C as D};`, `use function A\f;`.
fn read_use<'t>(node: Node<'t>, source: &[u8], out: &mut Vec<ImportBinding<'t>>) {
    let prefix = named_children(node)
        .into_iter()
        .find(|c| c.kind() == "namespace_name")
        .map(|p| path_text(p, source, "\\"))
        .unwrap_or_default();
    let mut clauses: Vec<Node<'t>> = named_children(node)
        .into_iter()
        .filter(|c| c.kind() == "namespace_use_clause")
        .collect();
    if let Some(group) = node.child_by_field_name("body") {
        clauses.extend(
            named_children(group)
                .into_iter()
                .filter(|c| c.kind() == "namespace_use_clause"),
        );
    }
    for clause in clauses {
        let alias = clause.child_by_field_name("alias");
        let Some(path) = named_children(clause)
            .into_iter()
            .find(|c| matches!(c.kind(), "qualified_name" | "name") && Some(c.id()) != alias.map(|a| a.id()))
        else {
            continue;
        };
        let target = join_sep(&prefix, &path_text(path, source, "\\"), "\\");
        let leaf = last_leaf(path);
        let local = match alias {
            Some(a) => text(a, source).trim().to_string(),
            None => text(leaf, source).trim().to_string(),
        };
        if !local.is_empty() {
            out.push(ImportBinding::new(local, target, ImportKind::Member, clause, Some(leaf)));
        }
    }
}
