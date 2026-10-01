//! C#: the syntax spec (grammar, query, node-kind tables and language rules).

use trace_core::facts::ImportKind;
use trace_core::facts::ParamKind::Positional;
use trace_core::Language;
use tree_sitter::Node;

use crate::language_rules::{BareCalls, LanguageRules, NONE};
use crate::names::{last_leaf, path_text, ImportBinding};
use crate::node::{named_children, text};
use crate::scopes::{self, bind, ScopeRules};
use crate::spec::{
    call, choice, for_loop, fp, member, new_call, param, unwrap, ChoiceShape, NameAt, Receiver, SyntaxSpec,
};
use crate::typefacts::form;

/// Inference rules of the language (`crate::language_rules`).
const RULES: LanguageRules = LanguageRules {
    super_receiver: Some(("base", ".")),
    bare_calls: BareCalls::ImplicitReceiver,
    allocation_receivers: true,
    static_imports: true,
    static_types: true,
    implicit_field_receiver: true,
    package_private_by_directory: true,
    ..NONE
};

pub(crate) static SYNTAX: SyntaxSpec = SyntaxSpec {
    rules: RULES,
    lazy_scopes: &[
        "method_declaration",
        "constructor_declaration",
        "destructor_declaration",
        "local_function_statement",
        "lambda_expression",
        "anonymous_method_expression",
        "operator_declaration",
        "accessor_declaration",
        "conversion_operator_declaration",
    ],
    // Accessors, operators and destructors have no name a query captures: their bodies are
    // synthetic `<lambda>` scopes nested in the type (never owner-less).
    anonymous_functions: &[
        "lambda_expression",
        "anonymous_method_expression",
        "accessor_declaration",
        "operator_declaration",
        "conversion_operator_declaration",
        "destructor_declaration",
    ],
    declaring_stores: &["variable_declarator", "foreach_statement", "argument", "catch_declaration"],
    type_contexts: &["base_list", "type_argument_list"],
    type_fields: &["type", "returns"],
    imports: &["using_directive"],
    class_bodies: &["declaration_list"],
    identifiers: &["identifier"],
    self_kinds: &["this"],
    self_names: &["this"],
    member_access: &[member("member_access_expression", "expression", "name")],
    calls: &[
        call("invocation_expression", "function", "arguments"),
        new_call("object_creation_expression", "type", "arguments"),
    ],
    argument_wrappers: &["argument"],
    choices: &[
        choice("conditional_expression", &["consequence", "alternative"]),
        ChoiceShape {
            kind: "binary_expression",
            alternatives: &["left", "right"],
            operators: &["??"],
        },
    ],
    awaits: &["await_expression"],
    unwrap: &[unwrap("parenthesized_expression", "#0"), unwrap("cast_expression", "value")],
    subscripts: &[fp("element_access_expression", "expression", "")],
    assignments: &[
        fp("assignment_expression", "left", "right"),
        fp("variable_declarator", "name", "#-1"),
    ],
    store_fields: &[
        fp("assignment_expression", "left", ""),
        fp("variable_declarator", "name", ""),
        fp("foreach_statement", "left", ""),
        fp("argument", "name", ""),
        fp("catch_declaration", "name", ""),
    ],
    binding_kinds: &[
        "using_directive",
        "parameter_list",
        "attribute_list",
        "type_argument_list",
        "namespace_declaration",
        "file_scoped_namespace_declaration",
    ],
    load_fields: &[fp("namespace_declaration", "body", "")],
    returns: &["return_statement"],
    for_loops: &[for_loop("foreach_statement", "left", "right")],
    expression_statements: &["expression_statement"],
    conditions: &[fp("if_statement", "condition", "")],
    binary_ops: &["binary_expression"],
    interpolations: &["interpolation"],
    comments: &["comment"],
    params: &[param("parameter", NameAt::Pick("name"), Positional, "")],
    param_fields: &["parameters"],
    receiver: Receiver::Implicit("this"),
    stub_when_bodiless: true,
    type_forms: &[
        form("parameter", "name", "type", ""),
        form("variable_declarator", "name", "^type", "#-1"),
        form("property_declaration", "name", "type", "value"),
        form("foreach_statement", "left", "type", ""),
        form("catch_declaration", "name", "type", ""),
        form("declaration_pattern", "name", "type", ""),
        form("assignment_expression", "left", "", "right"),
    ],
    return_types: &[("method_declaration", "returns"), ("local_function_statement", "type")],
    scopes: &ScopeRules {
        blocks: &[
            "block",
            "for_statement",
            "foreach_statement",
            "catch_clause",
            "using_statement",
            "switch_section",
        ],
        binders: &[
            bind("variable_declarator", "name"),
            bind("foreach_statement", "left"),
            bind("catch_declaration", "name"),
            bind("declaration_pattern", "name"),
        ],
        pattern_leaves: &["implicit_parameter"],
        class_members_visible: true,
        ..scopes::NONE
    },
    import_readers: &[("using_directive", read_using)],
    library_table: Some("csharp"),
    cases_fall_through: true,
    ..SyntaxSpec::empty(Language::CSharp, grammar, include_str!("../../queries/csharp.scm"))
};

fn grammar() -> tree_sitter::Language {
    tree_sitter::Language::new(tree_sitter_c_sharp::LANGUAGE)
}

/// C# `using A.B;` (namespace: wildcard), `using static A.B;` (wildcard), `using X = A.B;`.
fn read_using<'t>(node: Node<'t>, source: &[u8], out: &mut Vec<ImportBinding<'t>>) {
    let alias = node.child_by_field_name("name");
    let Some(path) = named_children(node)
        .into_iter()
        .rfind(|c| Some(c.id()) != alias.map(|a| a.id()))
    else {
        return;
    };
    let target = path_text(path, source, ".");
    if target.is_empty() {
        return;
    }
    match alias {
        Some(a) => {
            let local = text(a, source).trim().to_string();
            out.push(ImportBinding::new(local, target, ImportKind::Module, node, Some(last_leaf(path))));
        }
        None => out.push(ImportBinding::new("*".to_string(), target, ImportKind::Wildcard, node, None)),
    }
}
