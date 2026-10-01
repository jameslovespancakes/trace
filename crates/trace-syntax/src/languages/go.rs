//! Go: the syntax spec (grammar, query, node-kind tables and language rules).

use trace_core::facts::ImportKind;
use trace_core::facts::ParamKind::{Positional, VarPositional};
use trace_core::Language;
use tree_sitter::Node;

use crate::language_rules::{BareCalls, LanguageRules, ModulePaths, NameReach, NONE};
use crate::names::{literal_text, ImportBinding};
use crate::node::{named_children, nth_named, text};
use crate::scopes::{self, bind, with_tokens, ScopeRules};
use crate::spec::{call, for_loop, fp, member, param, unwrap, NameAt, Receiver, SyntaxSpec};
use crate::typefacts::form;

/// Inference rules of the language (`crate::language_rules`).
const RULES: LanguageRules = LanguageRules {
    implicit_conformance: true,
    modules: ModulePaths::ImportPaths,
    bare_calls: BareCalls::FunctionsOnly,
    bare_reach: NameReach::Package,
    locals_shadow_functions: true,
    allocation_receivers: true,
    static_types: true,
    structural_typing: true,
    package_private_by_directory: true,
    ..NONE
};

pub(crate) static SYNTAX: SyntaxSpec = SyntaxSpec {
    rules: RULES,
    // `(*T)(x)` converts to the pointer type `*T` (no function runs).
    pointer_conversions: &[fp("unary_expression", "operand", "*")],
    lazy_scopes: &["func_literal", "function_declaration", "method_declaration"],
    anonymous_functions: &["func_literal"],
    declaring_stores: &[
        "short_var_declaration",
        "var_spec",
        "const_spec",
        "range_clause",
        "labeled_statement",
        "keyed_element",
    ],
    type_contexts: &["type_arguments"],
    type_fields: &["type", "result"],
    type_names: &["type_identifier"],
    imports: &["import_declaration"],
    identifiers: &["identifier"],
    name_kinds: &["field_identifier", "type_identifier", "package_identifier"],
    literal_allocations: &[fp("composite_literal", "type", "")],
    member_access: &[member("selector_expression", "operand", "field")],
    calls: &[call("call_expression", "function", "arguments")],
    unwrap: &[unwrap("parenthesized_expression", "#0")],
    subscripts: &[fp("index_expression", "operand", "")],
    lists: &["expression_list"],
    assignments: &[
        fp("assignment_statement", "left", "right"),
        fp("short_var_declaration", "left", "right"),
        fp("var_spec", "name", "value"),
    ],
    store_fields: &[
        fp("assignment_statement", "left", ""),
        fp("short_var_declaration", "left", ""),
        fp("var_spec", "name", ""),
        fp("const_spec", "name", ""),
        fp("range_clause", "left", ""),
        fp("labeled_statement", "label", ""),
        fp("keyed_element", "key", ""),
    ],
    binding_kinds: &["import_declaration", "parameter_list", "package_clause"],
    returns: &["return_statement"],
    for_loops: &[for_loop("range_clause", "left", "right")],
    expression_statements: &["expression_statement"],
    conditions: &[fp("if_statement", "condition", "")],
    binary_ops: &["binary_expression"],
    comments: &["comment"],
    params: &[
        param("parameter_declaration", NameAt::Pick("name"), Positional, ""),
        param("variadic_parameter_declaration", NameAt::Pick("name"), VarPositional, ""),
    ],
    param_fields: &["parameters"],
    receiver: Receiver::ReceiverField("receiver"),
    stub_when_bodiless: true,
    type_calls_convert: true,
    type_forms: &[
        form("parameter_declaration", "*name", "type", ""),
        form("variadic_parameter_declaration", "*name", "type", ""),
        form("var_spec", "*name", "type", "value"),
        form("short_var_declaration", "left", "", "right"),
        form("assignment_statement", "left", "", "right"),
        form("field_declaration", "*name", "type", ""),
    ],
    return_types: &[
        ("function_declaration", "result"),
        ("method_declaration", "result"),
        ("method_elem", "result"),
    ],
    scopes: &ScopeRules {
        blocks: &[
            "block",
            "if_statement",
            "for_statement",
            "expression_switch_statement",
            "type_switch_statement",
            "select_statement",
            "expression_case",
            "type_case",
            "default_case",
            "communication_case",
        ],
        binders: &[
            bind("short_var_declaration", "left"),
            bind("var_spec", ""),
            bind("const_spec", ""),
            with_tokens("range_clause", "left", false, &[":="]),
            bind("type_switch_statement", "alias"),
        ],
        param_fields: &["receiver", "result"],
        ..scopes::NONE
    },
    import_readers: &[("import_declaration", read_import)],
    library_table: Some("go"),
    ..SyntaxSpec::empty(Language::Go, grammar, include_str!("../../queries/go.scm"))
};

fn grammar() -> tree_sitter::Language {
    tree_sitter::Language::new(tree_sitter_go::LANGUAGE)
}

/// Go package name of an import path: the last element, skipping a major-version suffix
/// (`github.com/x/y/v2` -> `y`) and a `.vN` suffix (`gopkg.in/yaml.v3` -> `yaml`).
fn package_name(path: &str) -> String {
    let mut parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let is_major = |p: &str| p.len() > 1 && p.starts_with('v') && p[1..].bytes().all(|b| b.is_ascii_digit());
    if parts.len() > 1 && parts.last().is_some_and(|p| is_major(p)) {
        parts.pop();
    }
    let last = parts.last().copied().unwrap_or(path);
    let last = match last.rsplit_once('.') {
        Some((stem, suffix)) if is_major(suffix) => stem,
        _ => last,
    };
    last.to_string()
}

/// `import "fmt"`, `import h "net/http"`, `import . "x"` (wildcard); `_` imports bind nothing.
fn read_import<'t>(node: Node<'t>, source: &[u8], out: &mut Vec<ImportBinding<'t>>) {
    let mut specs = Vec::new();
    for child in named_children(node) {
        match child.kind() {
            "import_spec" => specs.push(child),
            "import_spec_list" => specs.extend(
                named_children(child)
                    .into_iter()
                    .filter(|c| c.kind() == "import_spec"),
            ),
            _ => {}
        }
    }
    for spec in specs {
        let Some(path_node) = spec.child_by_field_name("path") else {
            continue;
        };
        let path = literal_text(path_node, source);
        if path.is_empty() {
            continue;
        }
        let content = nth_named(path_node, 0).unwrap_or(path_node);
        match spec.child_by_field_name("name") {
            Some(n) if n.kind() == "dot" => {
                out.push(ImportBinding::new("*".to_string(), path, ImportKind::Wildcard, spec, None));
            }
            Some(n) if n.kind() == "blank_identifier" => {}
            Some(n) => {
                let local = text(n, source).trim().to_string();
                out.push(ImportBinding::new(local, path, ImportKind::Module, spec, Some(n)));
            }
            None => {
                let local = package_name(&path);
                out.push(ImportBinding::new(local, path, ImportKind::Module, spec, Some(content)));
            }
        }
    }
}
