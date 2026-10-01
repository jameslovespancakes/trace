//! JavaScript (JSX included): the syntax spec (grammar, query, node-kind tables and language rules).

use trace_core::facts::ImportKind;
use trace_core::facts::ParamKind::{Positional, VarPositional};
use trace_core::Language;
use tree_sitter::Node;

use crate::interface::ReturnRule;
use crate::language_rules::{BareCalls, LanguageRules, ModulePaths, NameReach, NONE};
use crate::names::{join, literal_text, loader_literal, string_value, ImportBinding};
use crate::node::{named_children, text};
use crate::scopes::{self, bind, hoisted, with_tokens, ScopeRules};
use crate::spec::{
    call, choice, fp, member, new_call, param, unwrap, CallShape, ChoiceShape, FieldPair, ForLoop,
    MemberAccess, NameAt, Receiver, SyntaxSpec, TokenShape,
};
use crate::typefacts::form;

pub(super) const JS_LAZY: &[&str] = &[
    "function_declaration",
    "function_expression",
    "generator_function",
    "generator_function_declaration",
    "arrow_function",
    "method_definition",
];

pub(super) const JS_MEMBER: &[MemberAccess] = &[member("member_expression", "object", "property")];

pub(super) const JS_CALLS: &[CallShape] = &[
    call("call_expression", "function", "arguments"),
    new_call("new_expression", "constructor", "arguments"),
];

pub(super) const JS_CHOICES: &[ChoiceShape] = &[
    choice("ternary_expression", &["consequence", "alternative"]),
    ChoiceShape {
        kind: "binary_expression",
        alternatives: &["left", "right"],
        operators: &["||", "??"],
    },
];

pub(super) const JS_STORE: &[FieldPair] = &[
    fp("variable_declarator", "name", ""),
    fp("assignment_expression", "left", ""),
    fp("augmented_assignment_expression", "left", ""),
    fp("for_in_statement", "left", ""),
    fp("catch_clause", "parameter", ""),
    fp("arrow_function", "parameter", ""),
    fp("assignment_pattern", "left", ""),
];

pub(super) const JS_LOADS: &[FieldPair] = &[
    fp("assignment_pattern", "right", ""),
    fp("object_assignment_pattern", "right", ""),
    fp("required_parameter", "value", ""),
    fp("optional_parameter", "value", ""),
];

pub(super) const JS_FOR: &[ForLoop] = &[ForLoop {
    kind: "for_in_statement",
    target: "left",
    iterable: "right",
    token: "of",
}];

pub(super) const JS_YIELD: &[TokenShape] = &[TokenShape {
    kind: "yield_expression",
    token: "*",
}];

/// Anonymous callables of JS/TS (named function expressions and callables bound by
/// `const f = ...`, `obj.f = ...` or class fields are query definitions and stay named).
pub(super) const JS_ANONYMOUS: &[&str] = &["arrow_function", "function_expression", "generator_function"];

/// JS/TS store parents that declare names (no `write` reference for bare identifiers).
pub(super) const JS_DECLARING: &[&str] = &[
    "variable_declarator",
    "for_in_statement",
    "catch_clause",
    "arrow_function",
    "assignment_pattern",
];

pub(super) const JS_EXPORTS: &[FieldPair] = &[fp("export_specifier", "name", "alias")];

/// Global objects/functions whose callback behaviour is language semantics.
pub(super) const JS_GLOBALS: &[&str] = &[
    "Array",
    "JSON",
    "Object",
    "Promise",
    "Reflect",
    "queueMicrotask",
    "requestAnimationFrame",
    "requestIdleCallback",
    "setImmediate",
    "setInterval",
    "setTimeout",
    "structuredClone",
];

pub(super) const JS_SCOPES: ScopeRules = ScopeRules {
    blocks: &[
        "statement_block",
        "for_statement",
        "for_in_statement",
        "catch_clause",
        "switch_body",
        "class_static_block",
    ],
    binders: &[
        bind("lexical_declaration", ""),
        hoisted("variable_declaration", ""),
        with_tokens("for_in_statement", "left", false, &["let", "const", "var"]),
        bind("catch_clause", "parameter"),
    ],
    ordered: false,
    pattern_leaves: &["shorthand_property_identifier_pattern"],
    ..scopes::NONE
};

/// Inference rules shared by JavaScript, TypeScript and TSX.
pub(super) const JS_FAMILY: LanguageRules = LanguageRules {
    inheritance_rule: "ts-heritage",
    named_constructor: Some("constructor"),
    super_receiver: Some(("super", ".")),
    modules: ModulePaths::RelativeSpecifiers,
    bare_calls: BareCalls::FunctionsOnly,
    bare_reach: NameReach::FileOrImports,
    locals_shadow_functions: true,
    structural_typing: true,
    ..NONE
};

/// Inference rules of the language (`crate::language_rules`).
const RULES: LanguageRules = LanguageRules {
    commonjs_modules: true,
    ..JS_FAMILY
};

pub(crate) static SYNTAX: SyntaxSpec = SyntaxSpec {
    rules: RULES,
    lazy_scopes: JS_LAZY,
    anonymous_functions: JS_ANONYMOUS,
    // CommonJS `const m = require('m')`.
    import_calls: &["variable_declarator"],
    declaring_stores: JS_DECLARING,
    type_contexts: &["class_heritage"],
    export_specifiers: JS_EXPORTS,
    class_bodies: &["class_body"],
    identifiers: &["identifier", "shorthand_property_identifier"],
    name_kinds: &["property_identifier", "private_property_identifier"],
    self_kinds: &["this"],
    self_names: &["this"],
    constructor_names: &["constructor"],
    member_access: JS_MEMBER,
    calls: JS_CALLS,
    spreads: &["spread_element"],
    choices: JS_CHOICES,
    awaits: &["await_expression"],
    unwrap: &[unwrap("parenthesized_expression", "#0")],
    subscripts: &[fp("subscript_expression", "object", "")],
    assignments: &[
        fp("assignment_expression", "left", "right"),
        fp("variable_declarator", "name", "value"),
        fp("field_definition", "property", "value"),
    ],
    store_fields: JS_STORE,
    binding_kinds: &["import_statement", "formal_parameters", "object_pattern", "array_pattern"],
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
    ],
    leading_attributes: &["decorator"],
    imports: &["import_statement"],
    builtins: JS_GLOBALS,
    builtins_module: "globalThis",
    params: &[
        param("identifier", NameAt::Itself, Positional, ""),
        param("assignment_pattern", NameAt::Pick("left"), Positional, "right"),
        param("rest_pattern", NameAt::Pick("#0"), VarPositional, ""),
    ],
    param_fields: &["parameters", "parameter"],
    receiver: Receiver::Implicit("this"),
    type_forms: &[
        form("variable_declarator", "name", "", "value"),
        form("assignment_expression", "left", "", "right"),
        form("field_definition", "property", "", "value"),
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
    ..SyntaxSpec::empty(Language::JavaScript, grammar, include_str!("../../queries/javascript.scm"))
};

fn grammar() -> tree_sitter::Language {
    tree_sitter::Language::new(tree_sitter_javascript::LANGUAGE)
}

/// ES imports: default, namespace, named (with aliases), TS `import x = require("m")`.
pub(super) fn read_import<'t>(node: Node<'t>, source: &[u8], out: &mut Vec<ImportBinding<'t>>) {
    let children = named_children(node);
    let specifier = node
        .child_by_field_name("source")
        .map(|s| string_value(s, source))
        .unwrap_or_default();
    for child in children {
        match child.kind() {
            "import_clause" if !specifier.is_empty() => {
                for part in named_children(child) {
                    clause_part(part, &specifier, source, out);
                }
            }
            "import_require_clause" => {
                let Some(module) = child
                    .child_by_field_name("source")
                    .map(|s| string_value(s, source))
                    .filter(|m| !m.is_empty())
                else {
                    continue;
                };
                if let Some(id) = named_children(child).into_iter().find(|c| c.kind() == "identifier") {
                    out.push(ImportBinding::new(
                        text(id, source).trim().to_string(),
                        module,
                        ImportKind::Module,
                        child,
                        Some(id),
                    ));
                }
            }
            _ => {}
        }
    }
}

fn clause_part<'t>(part: Node<'t>, specifier: &str, source: &[u8], out: &mut Vec<ImportBinding<'t>>) {
    match part.kind() {
        "identifier" => out.push(ImportBinding::new(
            text(part, source).trim().to_string(),
            join(specifier, "default"),
            ImportKind::Member,
            part,
            Some(part),
        )),
        "namespace_import" => {
            if let Some(id) = named_children(part).into_iter().next() {
                out.push(ImportBinding::new(
                    text(id, source).trim().to_string(),
                    specifier.to_string(),
                    ImportKind::Module,
                    part,
                    Some(id),
                ));
            }
        }
        "named_imports" => {
            for spec in named_children(part) {
                if spec.kind() != "import_specifier" {
                    continue;
                }
                let Some(name) = spec.child_by_field_name("name") else {
                    continue;
                };
                let exported = if name.kind() == "string" {
                    string_value(name, source)
                } else {
                    text(name, source).trim().to_string()
                };
                let local = spec
                    .child_by_field_name("alias")
                    .map(|a| text(a, source).trim().to_string())
                    .unwrap_or_else(|| exported.clone());
                if exported.is_empty() || local.is_empty() {
                    continue;
                }
                out.push(ImportBinding::new(
                    local,
                    join(specifier, &exported),
                    ImportKind::Member,
                    spec,
                    Some(name),
                ));
            }
        }
        _ => {}
    }
}

/// CommonJS `const m = require('m')` / `const { a, b: c } = require('m')`.
pub(super) fn read_require<'t>(node: Node<'t>, source: &[u8], out: &mut Vec<ImportBinding<'t>>) {
    if node.kind() != "variable_declarator" {
        return;
    }
    let (Some(name), Some(value)) = (node.child_by_field_name("name"), node.child_by_field_name("value"))
    else {
        return;
    };
    if value.kind() != "call_expression" {
        return;
    }
    let Some(literal) = loader_literal(value, "function", "arguments", &["require"], source) else {
        return;
    };
    let specifier = literal_text(literal, source);
    if specifier.is_empty() {
        return;
    }
    match name.kind() {
        "identifier" => {
            let local = text(name, source).trim().to_string();
            out.push(ImportBinding::new(local, specifier, ImportKind::Module, node, Some(name)));
        }
        "object_pattern" => {
            for part in named_children(name) {
                let (key, local) = match part.kind() {
                    "shorthand_property_identifier_pattern" => (part, part),
                    "pair_pattern" => {
                        match (part.child_by_field_name("key"), part.child_by_field_name("value")) {
                            (Some(k), Some(v)) if v.kind() == "identifier" => (k, v),
                            _ => continue,
                        }
                    }
                    _ => continue,
                };
                let exported = text(key, source).trim().to_string();
                let local_name = text(local, source).trim().to_string();
                if !exported.is_empty() && !local_name.is_empty() {
                    out.push(ImportBinding::new(
                        local_name,
                        join(&specifier, &exported),
                        ImportKind::Member,
                        part,
                        Some(key),
                    ));
                }
            }
        }
        _ => {}
    }
}
