//! TypeScript and TSX: the syntax spec (grammar, query, node-kind tables and language rules).

use trace_core::facts::ParamKind::{Positional, VarPositional};
use trace_core::Language;

use super::javascript::{
    read_import, read_require, JS_ANONYMOUS, JS_CALLS, JS_CHOICES, JS_DECLARING, JS_EXPORTS, JS_FAMILY,
    JS_FOR, JS_GLOBALS, JS_LAZY, JS_LOADS, JS_MEMBER, JS_SCOPES, JS_STORE, JS_YIELD,
};
use crate::interface::ReturnRule;
use crate::language_rules::{LanguageRules, Signatures};
use crate::spec::{fp, param, unwrap, NameAt, ParamRule, Receiver, SyntaxSpec, Unwrap};
use crate::typefacts::form;

const TS_UNWRAP: &[Unwrap] = &[
    unwrap("parenthesized_expression", "#0"),
    unwrap("non_null_expression", "#0"),
    unwrap("as_expression", "#0"),
    unwrap("satisfies_expression", "#0"),
    unwrap("type_assertion", "#-1"),
    unwrap("instantiation_expression", "#0"),
];

const TS_PARAMS: &[ParamRule] = &[
    param("identifier", NameAt::Itself, Positional, ""),
    param("this", NameAt::Skip, Positional, ""),
    param("required_parameter", NameAt::Pick("pattern|name"), Positional, "value"),
    param("optional_parameter", NameAt::Pick("pattern|name"), Positional, "value"),
    param("assignment_pattern", NameAt::Pick("left"), Positional, "right"),
    param("rest_pattern", NameAt::Pick("#0"), VarPositional, ""),
];

/// Inference rules of the language (`crate::language_rules`).
const RULES: LanguageRules = LanguageRules {
    signatures: Signatures::UniqueImplementation,
    declaration_file_suffix: ".d",
    static_types: true,
    ..JS_FAMILY
};

pub(crate) static SYNTAX: SyntaxSpec = TYPESCRIPT_FIELDS;

/// TSX shares the TypeScript tables (the grammar adds JSX only).
pub(crate) static TSX_SYNTAX: SyntaxSpec = SyntaxSpec {
    language: Language::Tsx,
    grammar: tsx_grammar,
    query: include_str!("../../queries/tsx.scm"),
    ..TYPESCRIPT_FIELDS
};

/// `SYNTAX` as a const so `TSX_SYNTAX` can reuse it with struct-update syntax.
const TYPESCRIPT_FIELDS: SyntaxSpec = SyntaxSpec {
    rules: RULES,
    lazy_scopes: JS_LAZY,
    anonymous_functions: JS_ANONYMOUS,
    import_calls: &["variable_declarator"],
    declaring_stores: JS_DECLARING,
    type_contexts: &[
        "type_annotation",
        "type_arguments",
        "class_heritage",
        "extends_type_clause",
        "implements_clause",
    ],
    type_names: &["type_identifier"],
    export_specifiers: JS_EXPORTS,
    class_bodies: &["class_body"],
    identifiers: &["identifier", "shorthand_property_identifier"],
    name_kinds: &["property_identifier", "private_property_identifier", "type_identifier"],
    self_kinds: &["this"],
    self_names: &["this"],
    constructor_names: &["constructor"],
    member_access: JS_MEMBER,
    calls: JS_CALLS,
    spreads: &["spread_element"],
    choices: JS_CHOICES,
    awaits: &["await_expression"],
    unwrap: TS_UNWRAP,
    subscripts: &[fp("subscript_expression", "object", "")],
    assignments: &[
        fp("assignment_expression", "left", "right"),
        fp("variable_declarator", "name", "value"),
        fp("public_field_definition", "name", "value"),
    ],
    store_fields: JS_STORE,
    binding_kinds: &[
        "import_statement",
        "formal_parameters",
        "object_pattern",
        "array_pattern",
        "type_annotation",
        "type_arguments",
        "type_parameters",
    ],
    load_fields: JS_LOADS,
    returns: &["return_statement"],
    for_loops: JS_FOR,
    delegating_yields: JS_YIELD,
    iterate_parents: &["spread_element"],
    expression_statements: &["expression_statement"],
    conditions: &[fp("if_statement", "condition", "")],
    binary_ops: &["binary_expression"],
    interpolations: &["template_substitution"],
    comments: &["comment", "html_comment"],
    wrappers: &[
        "export_statement",
        "lexical_declaration",
        "variable_declaration",
        "expression_statement",
        "ambient_declaration",
    ],
    leading_attributes: &["decorator"],
    namespaces: &[fp("internal_module", "name", ""), fp("module", "name", "")],
    imports: &["import_statement"],
    builtins: JS_GLOBALS,
    builtins_module: "globalThis",
    params: TS_PARAMS,
    param_fields: &["parameters", "parameter"],
    receiver: Receiver::Implicit("this"),
    stub_when_bodiless: true,
    type_forms: &[
        form("required_parameter", "pattern", "type", ""),
        form("optional_parameter", "pattern", "type", ""),
        form("variable_declarator", "name", "type", "value"),
        form("public_field_definition", "name", "type", "value"),
        form("property_signature", "name", "type", ""),
        form("assignment_expression", "left", "", "right"),
    ],
    return_types: &[
        ("function_declaration", "return_type"),
        ("generator_function_declaration", "return_type"),
        ("function_signature", "return_type"),
        ("method_definition", "return_type"),
        ("method_signature", "return_type"),
        ("abstract_method_signature", "return_type"),
        ("arrow_function", "return_type"),
        ("function_expression", "return_type"),
    ],
    scopes: &JS_SCOPES,
    object_literals: &["object"],
    yields: &["yield_expression"],
    dynamic_members: true,
    return_rule: ReturnRule::ReturnStatements,
    import_readers: &[("import_statement", read_import)],
    call_import_reader: Some(read_require),
    library_table: Some("javascript"),
    cases_fall_through: true,
    ..SyntaxSpec::empty(Language::TypeScript, grammar, include_str!("../../queries/typescript.scm"))
};

fn grammar() -> tree_sitter::Language {
    tree_sitter::Language::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT)
}

fn tsx_grammar() -> tree_sitter::Language {
    tree_sitter::Language::new(tree_sitter_typescript::LANGUAGE_TSX)
}
