//! C++: the syntax spec (grammar, query, node-kind tables and language rules).

use trace_core::facts::ImportKind;
use trace_core::Language;
use tree_sitter::Node;

use super::c::{read_include, C_PARAMS};
use crate::interface::ReturnRule;
use crate::language_rules::{BareCalls, LanguageRules, NONE};
use crate::names::{last_leaf, path_text, ImportBinding};
use crate::node::{has_direct_token, named_children, text};
use crate::scopes::{self, bind, pair, ScopeRules};
use crate::spec::{
    call, callback_form, choice, for_loop, fp, member, new_call, unwrap, Receiver, SyntaxSpec,
};
use crate::typefacts::form;

/// Inference rules of the language (`crate::language_rules`).
const RULES: LanguageRules = LanguageRules {
    template_dependent_calls: true,
    bare_calls: BareCalls::ImplicitReceiver,
    locals_shadow_functions: true,
    allocation_receivers: true,
    static_types: true,
    deref_forwarding: true,
    implicit_field_receiver: true,
    ..NONE
};

pub(crate) static SYNTAX: SyntaxSpec = SyntaxSpec {
    rules: RULES,
    macro_definitions: &["preproc_function_def"],
    // Functional cast of a built-in type: `int(x)`.
    type_callees: &["primitive_type"],
    // `&f`, `&A::f`, `A::f`.
    callback_forms: &[
        callback_form("pointer_expression", "argument", &["identifier"], 0, ("operator", "&")),
        callback_form("pointer_expression", "argument/name", &["identifier"], 0, ("operator", "&")),
        callback_form("qualified_identifier", "name", &["identifier"], 0, ("", "")),
    ],
    lazy_scopes: &["function_definition", "lambda_expression"],
    anonymous_functions: &["lambda_expression"],
    declaring_stores: &["init_declarator", "declaration", "for_range_loop"],
    type_contexts: &["base_class_clause", "template_argument_list"],
    type_fields: &["type"],
    type_names: &["type_identifier"],
    imports: &["preproc_include", "using_declaration"],
    class_bodies: &["field_declaration_list"],
    identifiers: &["identifier"],
    name_kinds: &["field_identifier", "type_identifier", "namespace_identifier"],
    self_kinds: &["this"],
    self_names: &["this"],
    member_access: &[
        member("field_expression", "argument", "field"),
        member("qualified_identifier", "scope", "name"),
    ],
    calls: &[
        call("call_expression", "function", "arguments"),
        new_call("new_expression", "type", "arguments"),
    ],
    choices: &[choice("conditional_expression", &["consequence", "alternative"])],
    awaits: &["co_await_expression"],
    unwrap: &[
        unwrap("parenthesized_expression", "#0"),
        unwrap("cast_expression", "value"),
        unwrap("template_function", "name"),
    ],
    subscripts: &[fp("subscript_expression", "argument", "")],
    assignments: &[
        fp("assignment_expression", "left", "right"),
        fp("init_declarator", "declarator", "value"),
    ],
    store_fields: &[
        fp("assignment_expression", "left", ""),
        fp("init_declarator", "declarator", ""),
        fp("declaration", "declarator", ""),
        fp("for_range_loop", "declarator", ""),
    ],
    binding_kinds: &[
        "parameter_list",
        "preproc_include",
        "preproc_def",
        "preproc_function_def",
        "preproc_params",
        "using_declaration",
        "template_parameter_list",
        "template_argument_list",
        "lambda_capture_specifier",
    ],
    returns: &["return_statement", "co_return_statement"],
    for_loops: &[for_loop("for_range_loop", "declarator", "right")],
    expression_statements: &["expression_statement"],
    conditions: &[fp("if_statement", "condition", "")],
    binary_ops: &["binary_expression"],
    comments: &["comment"],
    wrappers: &["template_declaration"],
    // `namespace a { ... }` qualifies its declarations (`a.B.f`), like the out-of-line
    // definitions `a::B::f` (general fixes rule 9: prototypes are qualified like their
    // definition).
    namespaces: &[fp("namespace_definition", "name", "")],
    params: C_PARAMS,
    param_fields: &["parameters"],
    param_lists: &["parameter_list"],
    receiver: Receiver::Implicit("this"),
    stub_when_bodiless: true,
    type_forms: &[
        form("parameter_declaration", "declarator", "type", ""),
        form("optional_parameter_declaration", "declarator", "type", ""),
        form("declaration", "declarator", "type", "declarator/value"),
        form("field_declaration", "declarator", "type", ""),
        form("for_range_loop", "declarator", "type", ""),
        form("assignment_expression", "left", "", "right"),
    ],
    return_types: &[
        ("function_definition", "type"),
        ("declaration", "type"),
        ("field_declaration", "type"),
    ],
    type_path_separator: "::",
    scopes: &ScopeRules {
        blocks: &["compound_statement", "for_statement", "for_range_loop", "catch_clause"],
        binders: &[
            bind("declaration", ""),
            bind("for_range_loop", "declarator"),
            bind("catch_clause", "parameters"),
        ],
        statics: &[pair("qualified_identifier", "scope", "name")],
        class_members_visible: true,
        ..scopes::NONE
    },
    return_rule: ReturnRule::ReturnStatements,
    import_readers: &[("preproc_include", read_include), ("using_declaration", read_using)],
    library_table: Some("cpp"),
    cases_fall_through: true,
    ..SyntaxSpec::empty(Language::Cpp, grammar, include_str!("../../queries/cpp.scm"))
};

fn grammar() -> tree_sitter::Language {
    tree_sitter::Language::new(tree_sitter_cpp::LANGUAGE)
}

/// C++ `using std::cout;` (member) and `using namespace std;` (wildcard).
fn read_using<'t>(node: Node<'t>, source: &[u8], out: &mut Vec<ImportBinding<'t>>) {
    let Some(path) = named_children(node)
        .into_iter()
        .find(|c| matches!(c.kind(), "identifier" | "qualified_identifier"))
    else {
        return;
    };
    let target = path_text(path, source, "::");
    if has_direct_token(node, "namespace") {
        out.push(ImportBinding::new("*".to_string(), target, ImportKind::Wildcard, node, None));
        return;
    }
    let leaf = last_leaf(path);
    let local = text(leaf, source).trim().to_string();
    if !local.is_empty() {
        out.push(ImportBinding::new(local, target, ImportKind::Member, node, Some(leaf)));
    }
}
