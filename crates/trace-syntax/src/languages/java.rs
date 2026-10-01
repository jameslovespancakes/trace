//! Java: the syntax spec (grammar, query, node-kind tables and language rules).

use trace_core::facts::ImportKind;
use trace_core::facts::ParamKind::{Positional, VarPositional};
use trace_core::Language;
use tree_sitter::Node;

use crate::language_rules::{BareCalls, LanguageRules, ModulePaths, NONE};
use crate::names::{last_leaf, path_text, ImportBinding};
use crate::node::{has_direct_token, named_children, text};
use crate::scopes::{self, bind, ScopeRules};
use crate::spec::{
    callback_form, choice, for_loop, fp, member, method_call, new_call, param, unwrap, NameAt, Receiver,
    SyntaxSpec,
};
use crate::typefacts::form;

/// Inference rules of the language (`crate::language_rules`).
const RULES: LanguageRules = LanguageRules {
    inheritance_rule: "java-inheritance",
    super_receiver: Some(("super", ".")),
    modules: ModulePaths::PackageDirectories,
    bare_calls: BareCalls::ImplicitReceiver,
    allocation_receivers: true,
    static_imports: true,
    single_name_imports: true,
    static_types: true,
    implicit_field_receiver: true,
    package_private_by_directory: true,
    ..NONE
};

pub(crate) static SYNTAX: SyntaxSpec = SyntaxSpec {
    rules: RULES,
    // Method references `X::m`, `this::m`, `super::m` (`X::new` has no method name).
    callback_forms: &[callback_form("method_reference", "#-1", &["identifier"], 2, ("", ""))],
    lazy_scopes: &[
        "method_declaration",
        "constructor_declaration",
        "compact_constructor_declaration",
        "lambda_expression",
    ],
    anonymous_functions: &["lambda_expression"],
    declaring_stores: &[
        "variable_declarator",
        "enhanced_for_statement",
        "catch_formal_parameter",
        "resource",
    ],
    type_contexts: &[
        "superclass",
        "super_interfaces",
        "extends_interfaces",
        "type_arguments",
        "throws",
    ],
    type_fields: &["type"],
    type_names: &["type_identifier"],
    imports: &["import_declaration"],
    class_bodies: &["class_body", "interface_body", "enum_body"],
    identifiers: &["identifier"],
    name_kinds: &["type_identifier"],
    self_kinds: &["this"],
    self_names: &["this"],
    member_access: &[member("field_access", "object", "field")],
    calls: &[
        method_call("method_invocation", "name", "arguments", "object"),
        new_call("object_creation_expression", "type", "arguments"),
    ],
    choices: &[choice("ternary_expression", &["consequence", "alternative"])],
    unwrap: &[unwrap("parenthesized_expression", "#0"), unwrap("cast_expression", "value")],
    subscripts: &[fp("array_access", "array", "")],
    assignments: &[
        fp("assignment_expression", "left", "right"),
        fp("variable_declarator", "name", "value"),
    ],
    store_fields: &[
        fp("variable_declarator", "name", ""),
        fp("assignment_expression", "left", ""),
        fp("enhanced_for_statement", "name", ""),
        fp("catch_formal_parameter", "name", ""),
        fp("resource", "name", ""),
    ],
    binding_kinds: &[
        "import_declaration",
        "package_declaration",
        "formal_parameters",
        "inferred_parameters",
        "modifiers",
        "type_arguments",
    ],
    returns: &["return_statement"],
    for_loops: &[for_loop("enhanced_for_statement", "name", "value")],
    expression_statements: &["expression_statement"],
    conditions: &[fp("if_statement", "condition", "")],
    binary_ops: &["binary_expression"],
    comments: &["line_comment", "block_comment"],
    params: &[
        param("formal_parameter", NameAt::Pick("name"), Positional, ""),
        param("spread_parameter", NameAt::Pick("=variable_declarator/name"), VarPositional, ""),
        param("receiver_parameter", NameAt::Skip, Positional, ""),
        // Single-parameter lambdas (`x -> f(x)`) and inferred lambda parameters.
        param("identifier", NameAt::Itself, Positional, ""),
    ],
    param_fields: &["parameters"],
    receiver: Receiver::Implicit("this"),
    stub_when_bodiless: true,
    type_forms: &[
        form("formal_parameter", "name", "type", ""),
        form("catch_formal_parameter", "name", "=catch_type", ""),
        form("enhanced_for_statement", "name", "type", ""),
        form("resource", "name", "type", "value"),
        form("variable_declarator", "name", "^type", "value"),
        form("assignment_expression", "left", "", "right"),
    ],
    return_types: &[("method_declaration", "type")],
    scopes: &ScopeRules {
        blocks: &[
            "block",
            "for_statement",
            "enhanced_for_statement",
            "catch_clause",
            "try_with_resources_statement",
            "switch_block_statement_group",
            "switch_rule",
        ],
        binders: &[
            bind("local_variable_declaration", ""),
            bind("enhanced_for_statement", "name"),
            bind("catch_formal_parameter", "name"),
            bind("resource", "name"),
            bind("instanceof_expression", "name"),
        ],
        class_members_visible: true,
        ..scopes::NONE
    },
    import_readers: &[("import_declaration", read_import)],
    library_table: Some("java"),
    cases_fall_through: true,
    ..SyntaxSpec::empty(Language::Java, grammar, include_str!("../../queries/java.scm"))
};

fn grammar() -> tree_sitter::Language {
    tree_sitter::Language::new(tree_sitter_java::LANGUAGE)
}

/// `import a.b.C;`, `import static a.B.m;`, `import a.b.*;`.
fn read_import<'t>(node: Node<'t>, source: &[u8], out: &mut Vec<ImportBinding<'t>>) {
    let Some(path) = named_children(node)
        .into_iter()
        .find(|c| matches!(c.kind(), "scoped_identifier" | "identifier"))
    else {
        return;
    };
    let target = path_text(path, source, ".");
    let wildcard = named_children(node).iter().any(|c| c.kind() == "asterisk") || has_direct_token(node, "*");
    if wildcard {
        out.push(ImportBinding::new("*".to_string(), target, ImportKind::Wildcard, node, None));
        return;
    }
    let leaf = last_leaf(path);
    let local = text(leaf, source).trim().to_string();
    if !local.is_empty() {
        out.push(ImportBinding::new(local, target, ImportKind::Member, node, Some(leaf)));
    }
}
