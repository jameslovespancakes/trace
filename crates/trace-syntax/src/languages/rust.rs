//! Rust: the syntax spec (grammar, query, node-kind tables and language rules).

use trace_core::facts::ImportKind;
use trace_core::facts::ParamKind::{Positional, VarPositional};
use trace_core::Language;
use tree_sitter::Node;

use crate::language_rules::{BareCalls, LanguageRules, ModulePaths, NameReach, NONE};
use crate::names::{join_sep, last_leaf, last_segment, path_text, ImportBinding};
use crate::node::{named_children, nth_named, text};
use crate::scopes::{self, bind, pair, ScopeRules};
use crate::spec::{call, callback_form, for_loop, fp, member, param, unwrap, NameAt, Receiver, SyntaxSpec};
use crate::typefacts::form;

/// Inference rules of the language (`crate::language_rules`).
const RULES: LanguageRules = LanguageRules {
    inheritance_rule: "rust-trait-impl",
    trait_impls: true,
    deref_wrappers: &["Box", "Rc", "Arc", "Pin"],
    modules: ModulePaths::CratePaths,
    bare_calls: BareCalls::FunctionsOnly,
    bare_reach: NameReach::FileOrImports,
    locals_shadow_functions: true,
    decorators_rewrite_signatures: true,
    foreign_prototypes: true,
    static_types: true,
    deref_forwarding: true,
    ..NONE
};

pub(crate) static SYNTAX: SyntaxSpec = SyntaxSpec {
    rules: RULES,
    // Paths naming a function: `module::f`, `Self::f`, `Type::method`.
    callback_forms: &[callback_form("scoped_identifier", "name", &["identifier"], 0, ("", ""))],
    lazy_scopes: &["function_item", "closure_expression", "async_block"],
    anonymous_functions: &["closure_expression", "async_block"],
    declaring_stores: &["let_declaration", "for_expression", "mod_item", "match_arm", "let_condition"],
    type_contexts: &["type_arguments"],
    type_fields: &["type", "return_type", "trait"],
    type_names: &["type_identifier"],
    imports: &["use_declaration"],
    identifiers: &["identifier"],
    name_kinds: &["field_identifier", "type_identifier", "shorthand_field_identifier"],
    self_kinds: &["self"],
    self_names: &["self", "Self"],
    member_access: &[
        member("field_expression", "value", "field"),
        member("scoped_identifier", "path", "name"),
    ],
    calls: &[call("call_expression", "function", "arguments")],
    awaits: &["await_expression"],
    unwrap: &[
        unwrap("parenthesized_expression", "#0"),
        unwrap("generic_function", "function"),
        unwrap("try_expression", "#0"),
        unwrap("reference_expression", "value"),
    ],
    subscripts: &[fp("index_expression", "#0", "")],
    assignments: &[
        fp("let_declaration", "pattern", "value"),
        fp("assignment_expression", "left", "right"),
    ],
    store_fields: &[
        fp("let_declaration", "pattern", ""),
        fp("assignment_expression", "left", ""),
        fp("compound_assignment_expr", "left", ""),
        fp("for_expression", "pattern", ""),
        fp("mod_item", "name", ""),
        fp("match_arm", "pattern", ""),
        fp("if_let_expression", "pattern", ""),
        fp("let_condition", "pattern", ""),
    ],
    binding_kinds: &[
        "use_declaration",
        "extern_crate_declaration",
        "closure_parameters",
        "parameters",
        "attribute_item",
        "inner_attribute_item",
        "macro_definition",
        "type_arguments",
    ],
    returns: &["return_expression"],
    for_loops: &[for_loop("for_expression", "pattern", "value")],
    expression_statements: &["expression_statement"],
    conditions: &[fp("if_expression", "condition", "")],
    binary_ops: &["binary_expression"],
    comments: &["line_comment", "block_comment"],
    leading_attributes: &["attribute_item"],
    namespaces: &[fp("mod_item", "name", "")],
    params: &[
        param("parameter", NameAt::Pick("pattern"), Positional, ""),
        param("self_parameter", NameAt::Pick("#-1"), Positional, ""),
        param("identifier", NameAt::Itself, Positional, ""),
        param("mut_pattern", NameAt::Pick("#-1"), Positional, ""),
        param("variadic_parameter", NameAt::Skip, VarPositional, ""),
    ],
    param_fields: &["parameters"],
    receiver: Receiver::SelfParam("self"),
    stub_when_bodiless: true,
    type_forms: &[
        form("parameter", "pattern", "type", ""),
        form("let_declaration", "pattern", "type", "value"),
        form("field_declaration", "name", "type", ""),
        form("assignment_expression", "left", "", "right"),
    ],
    return_types: &[("function_item", "return_type"), ("function_signature_item", "return_type")],
    type_path_separator: "::",
    scopes: &ScopeRules {
        blocks: &["block", "for_expression", "match_arm", "if_expression", "while_expression"],
        binders: &[
            bind("let_declaration", "pattern"),
            bind("for_expression", "pattern"),
            bind("match_arm", "pattern"),
            bind("let_condition", "pattern"),
        ],
        closed: &["function_item"],
        pattern_leaves: &["shorthand_field_identifier"],
        statics: &[
            pair("scoped_identifier", "path", "name"),
            pair("scoped_type_identifier", "path", "name"),
        ],
        ..scopes::NONE
    },
    import_readers: &[("use_declaration", read_use)],
    library_table: Some("rust"),
    ..SyntaxSpec::empty(Language::Rust, grammar, include_str!("../../queries/rust.scm"))
};

fn grammar() -> tree_sitter::Language {
    tree_sitter::Language::new(tree_sitter_rust::LANGUAGE)
}

/// `use a::b::{c, d as e, f::*}`; `pub use` re-exports.
fn read_use<'t>(node: Node<'t>, source: &[u8], out: &mut Vec<ImportBinding<'t>>) {
    let export = named_children(node).iter().any(|c| c.kind() == "visibility_modifier");
    let Some(argument) = node.child_by_field_name("argument") else {
        return;
    };
    let start = out.len();
    use_tree(argument, "", source, out, 0);
    for b in &mut out[start..] {
        b.export = export;
    }
}

fn use_tree<'t>(tree: Node<'t>, prefix: &str, source: &[u8], out: &mut Vec<ImportBinding<'t>>, depth: usize) {
    if depth > 16 {
        return;
    }
    match tree.kind() {
        "use_as_clause" => {
            let (Some(path), Some(alias)) =
                (tree.child_by_field_name("path"), tree.child_by_field_name("alias"))
            else {
                return;
            };
            let target = join_sep(prefix, &path_text(path, source, "::"), "::");
            let local = text(alias, source).trim().to_string();
            if !local.is_empty() && local != "_" {
                out.push(ImportBinding::new(local, target, ImportKind::Member, tree, Some(last_leaf(path))));
            }
        }
        "use_wildcard" => {
            let inner = nth_named(tree, 0)
                .map(|p| path_text(p, source, "::"))
                .unwrap_or_default();
            let target = join_sep(prefix, &inner, "::");
            if !target.is_empty() {
                out.push(ImportBinding::new("*".to_string(), target, ImportKind::Wildcard, tree, None));
            }
        }
        "scoped_use_list" => {
            let path = tree
                .child_by_field_name("path")
                .map(|p| path_text(p, source, "::"))
                .unwrap_or_default();
            let prefix = join_sep(prefix, &path, "::");
            if let Some(list) = tree.child_by_field_name("list") {
                use_tree(list, &prefix, source, out, depth + 1);
            }
        }
        "use_list" => {
            for item in named_children(tree) {
                use_tree(item, prefix, source, out, depth + 1);
            }
        }
        "self" if !prefix.is_empty() => {
            let local = last_segment(prefix, "::").to_string();
            out.push(ImportBinding::new(local, prefix.to_string(), ImportKind::Module, tree, Some(tree)));
        }
        "identifier" | "scoped_identifier" | "crate" | "super" | "self" => {
            let path = path_text(tree, source, "::");
            let target = join_sep(prefix, &path, "::");
            let leaf = last_leaf(tree);
            let local = text(leaf, source).trim().to_string();
            if !local.is_empty() {
                let kind = if matches!(leaf.kind(), "crate" | "super" | "self") {
                    ImportKind::Module
                } else {
                    ImportKind::Member
                };
                out.push(ImportBinding::new(local, target, kind, tree, Some(leaf)));
            }
        }
        _ => {}
    }
}
