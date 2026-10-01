//! R: the syntax spec (grammar, query, node-kind tables and language rules).

use trace_core::facts::ImportKind;
use trace_core::facts::ParamKind::Positional;
use trace_core::Language;
use tree_sitter::Node;

use crate::interface::ReturnRule;
use crate::language_rules::{BareCallBinding, LanguageRules, NONE};
use crate::names::{literal_text, ImportBinding};
use crate::node::{nth_named, text};
use crate::scopes::{self, hoisted, pair, with_tokens, ScopeRules};
use crate::spec::{call, member, param, NameAt, NameCall, OperatorStore, SyntaxSpec};

/// Inference rules of the language (`crate::language_rules`).
const RULES: LanguageRules = LanguageRules {
    bare_call_binding: Some(BareCallBinding::FileLevel {
        declared_before: true,
    }),
    generic_method_rule: "r-s3",
    locals_shadow_functions: true,
    ..NONE
};

pub(crate) static SYNTAX: SyntaxSpec = SyntaxSpec {
    rules: RULES,
    // `do.call("f", args)` / `do.call(f, args)` / `do.call(what = "f", ...)` call `f`.
    name_calls: &[NameCall {
        member: "do.call",
        position: 0,
        keyword: "what",
        kinds: &["string", "identifier"],
    }],
    separators: &["comma"],
    // S3 generics dispatch with `UseMethod("g")`.
    generic_dispatch_calls: &["UseMethod"],
    lazy_scopes: &["function_definition"],
    // `function(x) ...` values not bound by `name <- function(...)` (a query definition).
    anonymous_functions: &["function_definition"],
    operator_stores: &[
        OperatorStore {
            kind: "binary_operator",
            pick: "lhs",
            operators: &["<-", "<<-", "=", ":="],
        },
        OperatorStore {
            kind: "binary_operator",
            pick: "rhs",
            operators: &["->", "->>"],
        },
    ],
    import_calls: &["call"],
    identifiers: &["identifier"],
    member_access: &[
        member("extract_operator", "lhs", "rhs"),
        member("namespace_operator", "lhs", "rhs"),
    ],
    calls: &[call("call", "function", "arguments")],
    argument_wrappers: &["argument"],
    binding_kinds: &["parameters"],
    comments: &["comment"],
    params: &[param("parameter", NameAt::Pick("name"), Positional, "default")],
    param_fields: &["parameters"],
    scopes: &ScopeRules {
        binders: &[
            with_tokens("binary_operator", "lhs", true, &["<-", "=", ":="]),
            with_tokens("binary_operator", "rhs", true, &["->"]),
            hoisted("for_statement", "variable"),
        ],
        statics: &[pair("namespace_operator", "lhs", "rhs")],
        ..scopes::NONE
    },
    return_rule: ReturnRule::WholeBody,
    call_import_reader: Some(read_library),
    library_table: Some("r"),
    ..SyntaxSpec::empty(Language::R, grammar, include_str!("../../queries/r.scm"))
};

fn grammar() -> tree_sitter::Language {
    tree_sitter::Language::new(tree_sitter_r::LANGUAGE)
}

/// R `library(pkg)` / `require(pkg)` (identifier or string): every export is attached.
fn read_library<'t>(node: Node<'t>, source: &[u8], out: &mut Vec<ImportBinding<'t>>) {
    if node.kind() != "call" {
        return;
    }
    let Some(function) = node.child_by_field_name("function") else {
        return;
    };
    if function.kind() != "identifier" || !matches!(text(function, source).trim(), "library" | "require") {
        return;
    }
    let Some(first) = node
        .child_by_field_name("arguments")
        .and_then(|a| nth_named(a, 0))
        .and_then(|arg| arg.child_by_field_name("value").or(Some(arg)))
    else {
        return;
    };
    let package = match first.kind() {
        "identifier" => text(first, source).trim().to_string(),
        k if k.contains("string") => literal_text(first, source),
        _ => return,
    };
    if !package.is_empty() {
        out.push(ImportBinding::new("*".to_string(), package, ImportKind::Wildcard, node, None));
    }
}
