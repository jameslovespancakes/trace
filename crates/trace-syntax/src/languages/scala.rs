//! Scala: the syntax spec (grammar, query, node-kind tables and language rules).

use trace_core::facts::ImportKind;
use trace_core::facts::ParamKind::Positional;
use trace_core::Language;
use tree_sitter::Node;

use crate::interface::ReturnRule;
use crate::language_rules::{LanguageRules, ModulePaths, NONE};
use crate::names::{join_sep, ImportBinding};
use crate::node::{named_children, text};
use crate::scopes::{self, bind, ScopeRules};
use crate::spec::{call, callback_form, fp, member, new_call, param, NameAt, Receiver, SyntaxSpec};
use crate::typefacts::form;

/// Inference rules of the language (`crate::language_rules`).
const RULES: LanguageRules = LanguageRules {
    nested_functions_are_local: true,
    super_receiver: Some(("super", ".")),
    modules: ModulePaths::PackageDirectories,
    locals_shadow_functions: true,
    allocation_receivers: true,
    single_name_imports: true,
    static_types: true,
    implicit_field_receiver: true,
    calls_construct: true,
    package_private_by_directory: true,
    ..NONE
};

pub(crate) static SYNTAX: SyntaxSpec = SyntaxSpec {
    rules: RULES,
    // `x.y` evaluates the parameterless member `y`; `_.name` is a placeholder function.
    member_values_are_calls: true,
    placeholders: &["wildcard"],
    // Eta expansion `f _` / `obj.m _` passes the method as a function value.
    callback_forms: &[
        callback_form("method_value", "#0", &["identifier"], 2, ("#-1", "_")),
        callback_form("method_value", "#0/field", &["identifier"], 2, ("#-1", "_")),
    ],
    lazy_scopes: &["function_definition", "lambda_expression"],
    anonymous_functions: &["lambda_expression"],
    declaring_stores: &["val_definition", "var_definition"],
    type_contexts: &["extends_clause", "type_arguments"],
    type_fields: &["type", "return_type"],
    type_names: &["type_identifier"],
    imports: &["import_declaration"],
    class_bodies: &["template_body"],
    identifiers: &["identifier"],
    name_kinds: &["type_identifier"],
    self_names: &["this"],
    member_access: &[member("field_expression", "value", "field")],
    calls: &[
        call("call_expression", "function", "arguments"),
        new_call("instance_expression", "#0", "arguments"),
    ],
    assignments: &[
        fp("assignment_expression", "left", "right"),
        fp("val_definition", "pattern", "value"),
        fp("var_definition", "pattern", "value"),
    ],
    store_fields: &[
        fp("assignment_expression", "left", ""),
        fp("val_definition", "pattern", ""),
        fp("var_definition", "pattern", ""),
    ],
    binding_kinds: &["import_declaration", "package_clause", "parameters", "bindings"],
    returns: &["return_expression"],
    comments: &["comment", "block_comment"],
    params: &[param("parameter", NameAt::Pick("name"), Positional, "default_value")],
    param_fields: &["parameters"],
    receiver: Receiver::Implicit("this"),
    stub_when_bodiless: true,
    type_forms: &[
        form("parameter", "name", "type", ""),
        form("class_parameter", "name", "type", ""),
        form("val_definition", "pattern", "type", "value"),
        form("var_definition", "pattern", "type", "value"),
        form("assignment_expression", "left", "", "right"),
    ],
    return_types: &[("function_definition", "return_type"), ("function_declaration", "return_type")],
    constructs_by_call: true,
    scopes: &ScopeRules {
        blocks: &["block", "for_expression", "case_clause"],
        binders: &[
            bind("val_definition", "pattern"),
            bind("var_definition", "pattern"),
            bind("enumerator", "#0"),
            bind("case_clause", "pattern"),
        ],
        lowercase_patterns: true,
        class_members_visible: true,
        ..scopes::NONE
    },
    return_rule: ReturnRule::WholeBody,
    import_readers: &[("import_declaration", read_import)],
    library_table: Some("scala"),
    ..SyntaxSpec::empty(Language::Scala, grammar, include_str!("../../queries/scala.scm"))
};

fn grammar() -> tree_sitter::Language {
    tree_sitter::Language::new(tree_sitter_scala::LANGUAGE)
}

/// Scala `import a.b.C`, `import a.b.{C, D => E}`, `import a.b._` / `a.b.*`, `import a.b as c`.
fn read_import<'t>(node: Node<'t>, source: &[u8], out: &mut Vec<ImportBinding<'t>>) {
    let mut cursor = node.walk();
    let paths: Vec<Node<'t>> = node
        .children_by_field_name("path", &mut cursor)
        .filter(|p| p.is_named())
        .collect();
    let prefix: Vec<String> = paths.iter().map(|p| text(*p, source).trim().to_string()).collect();
    let prefix_text = prefix.join(".");
    let path_ids: Vec<usize> = paths.iter().map(|p| p.id()).collect();
    let rest: Vec<Node<'t>> = named_children(node)
        .into_iter()
        .filter(|c| !path_ids.contains(&c.id()))
        .collect();
    let renamed = |item: Node<'t>, out: &mut Vec<ImportBinding<'t>>| {
        let (Some(name), Some(alias)) = (item.child_by_field_name("name"), item.child_by_field_name("alias"))
        else {
            return;
        };
        let local = text(alias, source).trim().to_string();
        if alias.kind() == "wildcard" || local == "_" || local.is_empty() {
            return;
        }
        let target = join_sep(&prefix_text, text(name, source).trim(), ".");
        out.push(ImportBinding::new(local, target, ImportKind::Member, item, Some(name)));
    };
    if rest.is_empty() {
        if let Some(last) = paths.last() {
            let local = text(*last, source).trim().to_string();
            out.push(ImportBinding::new(local, prefix_text.clone(), ImportKind::Member, node, Some(*last)));
        }
        return;
    }
    for item in rest {
        match item.kind() {
            "namespace_wildcard" => {
                out.push(ImportBinding::new(
                    "*".to_string(),
                    prefix_text.clone(),
                    ImportKind::Wildcard,
                    item,
                    None,
                ));
            }
            "as_renamed_identifier" | "arrow_renamed_identifier" => renamed(item, &mut *out),
            "namespace_selectors" => {
                for sel in named_children(item) {
                    match sel.kind() {
                        "identifier" => {
                            let local = text(sel, source).trim().to_string();
                            let target = join_sep(&prefix_text, &local, ".");
                            out.push(ImportBinding::new(local, target, ImportKind::Member, sel, Some(sel)));
                        }
                        "namespace_wildcard" => {
                            out.push(ImportBinding::new(
                                "*".to_string(),
                                prefix_text.clone(),
                                ImportKind::Wildcard,
                                sel,
                                None,
                            ));
                        }
                        "as_renamed_identifier" | "arrow_renamed_identifier" => renamed(sel, &mut *out),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}
