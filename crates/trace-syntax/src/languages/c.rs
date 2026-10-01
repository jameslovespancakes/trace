//! C: the syntax spec (grammar, query, node-kind tables and language rules).

use trace_core::facts::ImportKind;
use trace_core::facts::ParamKind::{Positional, VarPositional};
use trace_core::Language;
use tree_sitter::Node;

use crate::language_rules::{BareCalls, LanguageRules, NONE};
use crate::names::{literal_text, ImportBinding};
use crate::scopes::{self, bind, ScopeRules};
use crate::spec::{call, callback_form, choice, fp, member, param, unwrap, NameAt, ParamRule, SyntaxSpec};
use crate::typefacts::form;

pub(super) const C_PARAMS: &[ParamRule] = &[
    param("parameter_declaration", NameAt::Pick("declarator"), Positional, ""),
    param("optional_parameter_declaration", NameAt::Pick("declarator"), Positional, "default_value"),
    // C `...`: recorded (named `...`) so the parameter list says it accepts more arguments.
    param("variadic_parameter", NameAt::Itself, VarPositional, ""),
    param("variadic_parameter_declaration", NameAt::Pick("declarator"), VarPositional, ""),
    param("identifier", NameAt::Itself, Positional, ""),
    param("pointer_declarator", NameAt::Pick("declarator"), Positional, ""),
    param("reference_declarator", NameAt::Pick("#-1"), Positional, ""),
    param("array_declarator", NameAt::Pick("declarator"), Positional, ""),
];

/// Inference rules of the language (`crate::language_rules`).
const RULES: LanguageRules = LanguageRules {
    bare_calls: BareCalls::FunctionsOnly,
    locals_shadow_functions: true,
    static_types: true,
    ..NONE
};

pub(crate) static SYNTAX: SyntaxSpec = SyntaxSpec {
    rules: RULES,
    macro_definitions: &["preproc_function_def"],
    // Address of a function: `&f`.
    callback_forms: &[callback_form(
        "pointer_expression",
        "argument",
        &["identifier"],
        0,
        ("operator", "&"),
    )],
    lazy_scopes: &["function_definition"],
    declaring_stores: &["init_declarator", "declaration"],
    type_fields: &["type"],
    type_names: &["type_identifier"],
    imports: &["preproc_include"],
    identifiers: &["identifier"],
    name_kinds: &["field_identifier", "type_identifier"],
    member_access: &[member("field_expression", "argument", "field")],
    calls: &[call("call_expression", "function", "arguments")],
    choices: &[choice("conditional_expression", &["consequence", "alternative"])],
    unwrap: &[unwrap("parenthesized_expression", "#0"), unwrap("cast_expression", "value")],
    subscripts: &[fp("subscript_expression", "argument", "")],
    assignments: &[
        fp("assignment_expression", "left", "right"),
        fp("init_declarator", "declarator", "value"),
    ],
    store_fields: &[
        fp("assignment_expression", "left", ""),
        fp("init_declarator", "declarator", ""),
        fp("declaration", "declarator", ""),
    ],
    binding_kinds: &[
        "parameter_list",
        "preproc_include",
        "preproc_def",
        "preproc_function_def",
        "preproc_params",
    ],
    returns: &["return_statement"],
    expression_statements: &["expression_statement"],
    conditions: &[fp("if_statement", "condition", "")],
    binary_ops: &["binary_expression"],
    comments: &["comment"],
    params: C_PARAMS,
    param_fields: &["parameters"],
    param_lists: &["parameter_list"],
    stub_when_bodiless: true,
    type_forms: &[
        form("parameter_declaration", "declarator", "type", ""),
        form("declaration", "declarator", "type", "declarator/value"),
        form("field_declaration", "declarator", "type", ""),
    ],
    return_types: &[
        ("function_definition", "type"),
        ("declaration", "type"),
        ("field_declaration", "type"),
    ],
    scopes: &ScopeRules {
        blocks: &["compound_statement", "for_statement"],
        binders: &[bind("declaration", "")],
        ..scopes::NONE
    },
    import_readers: &[("preproc_include", read_include)],
    library_table: Some("c"),
    cases_fall_through: true,
    ..SyntaxSpec::empty(Language::C, grammar, include_str!("../../queries/c.scm"))
};

fn grammar() -> tree_sitter::Language {
    tree_sitter::Language::new(tree_sitter_c::LANGUAGE)
}

/// `#include "a.h"` / `#include <stdio.h>`: every declaration of the header is visible.
pub(super) fn read_include<'t>(node: Node<'t>, source: &[u8], out: &mut Vec<ImportBinding<'t>>) {
    let Some(path) = node.child_by_field_name("path") else {
        return;
    };
    let target = literal_text(path, source);
    if !target.is_empty() {
        out.push(ImportBinding::new("*".to_string(), target, ImportKind::Wildcard, node, None));
    }
}
