//! Python: the syntax spec (grammar, query, node-kind tables and language rules).

use trace_core::facts::ImportKind;
use trace_core::facts::ParamKind::{Positional, VarKeyword, VarPositional};
use trace_core::Language;
use tree_sitter::Node;

use crate::interface::ReturnRule;
use crate::language_rules::{BareCalls, LanguageRules, ModulePaths, NameReach, NONE};
use crate::names::{dotted, from_module, join, last_leaf, ImportBinding};
use crate::node::{named_children, text};
use crate::spec::{
    call, choice, for_loop, fp, member, param, unwrap, NameAt, Receiver, SyntaxSpec, TokenShape,
};
use crate::typefacts::form;

/// Python builtin functions and classes (the `builtins` module namespace used as callees).
const PYTHON_BUILTINS: &[&str] = &[
    "__import__",
    "abs",
    "aiter",
    "all",
    "anext",
    "any",
    "ascii",
    "bin",
    "bool",
    "breakpoint",
    "bytearray",
    "bytes",
    "callable",
    "chr",
    "classmethod",
    "compile",
    "complex",
    "delattr",
    "dict",
    "dir",
    "divmod",
    "enumerate",
    "eval",
    "exec",
    "filter",
    "float",
    "format",
    "frozenset",
    "getattr",
    "globals",
    "hasattr",
    "hash",
    "help",
    "hex",
    "id",
    "input",
    "int",
    "isinstance",
    "issubclass",
    "iter",
    "len",
    "list",
    "locals",
    "map",
    "max",
    "memoryview",
    "min",
    "next",
    "object",
    "oct",
    "open",
    "ord",
    "pow",
    "print",
    "property",
    "range",
    "repr",
    "reversed",
    "round",
    "set",
    "setattr",
    "slice",
    "sorted",
    "staticmethod",
    "str",
    "sum",
    "super",
    "tuple",
    "type",
    "vars",
    "zip",
];

/// Inference rules of the language (`crate::language_rules`).
const RULES: LanguageRules = LanguageRules {
    inheritance_rule: "python-mro",
    overrides_follow_mro: true,
    named_constructor: Some("__init__"),
    instance_call_method: Some("__call__"),
    attribute_hook: Some("__getattribute__"),
    lexical_local_reads: true,
    modules: ModulePaths::DottedModules,
    bare_calls: BareCalls::FunctionsOnly,
    bare_reach: NameReach::FileOrImports,
    locals_shadow_functions: true,
    decorators_rewrite_signatures: true,
    structural_typing: true,
    calls_construct: true,
    ..NONE
};

pub(crate) static SYNTAX: SyntaxSpec = SyntaxSpec {
    rules: RULES,
    lazy_scopes: &["lambda", "function_definition"],
    anonymous_functions: &["lambda"],
    generator_expressions: &["generator_expression"],
    generator_clause: fp("for_in_clause", "left", "right"),
    identifiers: &["identifier"],
    self_names: &["self", "cls"],
    constructor_names: &["__init__"],
    member_access: &[member("attribute", "object", "attribute")],
    calls: &[call("call", "function", "arguments")],
    spreads: &["list_splat", "dictionary_splat"],
    keyword_spreads: &["dictionary_splat"],
    keyword_arguments: &[fp("keyword_argument", "name", "value")],
    choices: &[
        choice("boolean_operator", &["left", "right"]),
        choice("conditional_expression", &["#0", "#2"]),
    ],
    awaits: &["await"],
    unwrap: &[unwrap("parenthesized_expression", "#0")],
    subscripts: &[fp("subscript", "value", "")],
    assignments: &[fp("assignment", "left", "right"), fp("named_expression", "name", "value")],
    store_fields: &[
        fp("assignment", "left", ""),
        fp("augmented_assignment", "left", ""),
        fp("for_statement", "left", ""),
        fp("for_in_clause", "left", ""),
        fp("named_expression", "name", ""),
        fp("as_pattern", "alias", ""),
        fp("except_clause", "alias", ""),
        fp("keyword_argument", "name", ""),
    ],
    binding_kinds: &[
        "import_statement",
        "import_from_statement",
        "future_import_statement",
        "global_statement",
        "nonlocal_statement",
        "parameters",
        "lambda_parameters",
        "as_pattern_target",
        "delete_statement",
    ],
    load_fields: &[
        fp("default_parameter", "value", ""),
        fp("typed_default_parameter", "value", ""),
        fp("typed_default_parameter", "type", ""),
        fp("typed_parameter", "type", ""),
    ],
    returns: &["return_statement"],
    for_loops: &[for_loop("for_statement", "left", "right")],
    delegating_yields: &[TokenShape {
        kind: "yield",
        token: "from",
    }],
    // `*x` unpacking (in calls and display literals) iterates its operand.
    iterate_parents: &["list_splat"],
    with_items: &[fp("with_item", "value", "")],
    deletes: &["delete_statement"],
    expression_statements: &["expression_statement"],
    conditions: &[fp("if_statement", "condition", ""), fp("elif_clause", "condition", "")],
    comparisons: &["comparison_operator"],
    arithmetic: &["binary_operator"],
    interpolations: &["interpolation"],
    comments: &["comment"],
    wrappers: &["decorated_definition"],
    imports: &["import_statement", "import_from_statement"],
    builtins: PYTHON_BUILTINS,
    builtins_module: "builtins",
    params: &[
        param("identifier", NameAt::Itself, Positional, ""),
        param("typed_parameter", NameAt::Pick("#0"), Positional, ""),
        param("default_parameter", NameAt::Pick("name"), Positional, "value"),
        param("typed_default_parameter", NameAt::Pick("name"), Positional, "value"),
        param("list_splat_pattern", NameAt::Pick("#0"), VarPositional, ""),
        param("dictionary_splat_pattern", NameAt::Pick("#0"), VarKeyword, ""),
    ],
    param_fields: &["parameters"],
    keyword_separators: &["keyword_separator"],
    receiver: Receiver::FirstParam,
    decorators_wrap: true,
    implicit_ops: true,
    scope_declarations: &["global_statement", "nonlocal_statement"],
    identity_guards: true,
    // Annotations (`x: T`, `-> T`). Class bases stay `read`: in Python they are runtime
    // value expressions (and value references feed the class hierarchy).
    type_fields: &["type", "return_type"],
    type_forms: &[
        form("typed_parameter", "#0", "type", ""),
        form("typed_default_parameter", "name", "type", ""),
        form("assignment", "left", "type", "right"),
    ],
    return_types: &[("function_definition", "return_type")],
    constructs_by_call: true,
    yields: &["yield"],
    return_rule: ReturnRule::ReturnStatements,
    import_readers: &[("import_statement", read_import), ("import_from_statement", read_from_import)],
    library_table: Some("python"),
    ..SyntaxSpec::empty(Language::Python, grammar, include_str!("../../queries/python.scm"))
};

fn grammar() -> tree_sitter::Language {
    tree_sitter::Language::new(tree_sitter_python::LANGUAGE)
}

/// `import a.b`, `import a.b as c`.
fn read_import<'t>(node: Node<'t>, source: &[u8], out: &mut Vec<ImportBinding<'t>>) {
    let mut cursor = node.walk();
    for name in node.children_by_field_name("name", &mut cursor) {
        match name.kind() {
            "dotted_name" => {
                let Some(first) = named_children(name).into_iter().next() else {
                    continue;
                };
                let root = text(first, source).trim().to_string();
                if root.is_empty() {
                    continue;
                }
                out.push(ImportBinding::new(
                    root.clone(),
                    root,
                    ImportKind::Module,
                    name,
                    Some(last_leaf(name)),
                ));
            }
            "aliased_import" => {
                let (Some(original), Some(alias)) =
                    (name.child_by_field_name("name"), name.child_by_field_name("alias"))
                else {
                    continue;
                };
                let target = dotted(original, source);
                let local = text(alias, source).trim().to_string();
                if !target.is_empty() && !local.is_empty() {
                    out.push(ImportBinding::new(
                        local,
                        target,
                        ImportKind::Module,
                        name,
                        Some(last_leaf(original)),
                    ));
                }
            }
            _ => {}
        }
    }
}

/// `from m import a`, `from m import a as b`, `from . import a`, `from m import *`.
fn read_from_import<'t>(node: Node<'t>, source: &[u8], out: &mut Vec<ImportBinding<'t>>) {
    let Some(module) = from_module(node, source) else {
        return;
    };
    for child in named_children(node) {
        if child.kind() == "wildcard_import" {
            out.push(ImportBinding::new("*".to_string(), module.clone(), ImportKind::Wildcard, child, None));
        }
    }
    let mut cursor = node.walk();
    for name in node.children_by_field_name("name", &mut cursor) {
        let (original, local, leaf) = match name.kind() {
            "dotted_name" => {
                let n = dotted(name, source);
                (n.clone(), n, last_leaf(name))
            }
            "aliased_import" => {
                let (Some(o), Some(a)) =
                    (name.child_by_field_name("name"), name.child_by_field_name("alias"))
                else {
                    continue;
                };
                (dotted(o, source), text(a, source).trim().to_string(), last_leaf(o))
            }
            _ => continue,
        };
        if original.is_empty() || local.is_empty() {
            continue;
        }
        out.push(ImportBinding::new(local, join(&module, &original), ImportKind::Member, name, Some(leaf)));
    }
}
