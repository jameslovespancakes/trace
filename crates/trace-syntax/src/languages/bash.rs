//! Bash: the syntax spec (grammar, query, node-kind tables and language rules).

use trace_core::facts::ImportKind;
use trace_core::Language;
use tree_sitter::Node;

use crate::language_rules::{BareCallBinding, BareCalls, LanguageRules, ModulePaths, NONE};
use crate::names::{literal_text, ImportBinding};
use crate::node::{named_children, text};
use crate::scopes::{self, with_tokens, ScopeRules};
use crate::spec::{call, fp, unwrap, SyntaxSpec};

/// Inference rules of the language (`crate::language_rules`).
const RULES: LanguageRules = LanguageRules {
    bare_call_binding: Some(BareCallBinding::Shell),
    modules: ModulePaths::SourcedFiles,
    bare_calls: BareCalls::FunctionsOnly,
    global_nested_functions: true,
    ..NONE
};

pub(crate) static SYNTAX: SyntaxSpec = SyntaxSpec {
    rules: RULES,
    lazy_scopes: &["function_definition"],
    identifiers: &["variable_name"],
    name_kinds: &["word"],
    calls: &[call("command", "name", "")],
    unwrap: &[unwrap("command_name", "#0")],
    assignments: &[fp("variable_assignment", "name", "value")],
    store_fields: &[fp("variable_assignment", "name", "")],
    comments: &["comment"],
    // `source lib.sh` / `. lib.sh`.
    import_calls: &["command"],
    scopes: &ScopeRules {
        binders: &[with_tokens("declaration_command", "", true, &["local", "declare", "typeset"])],
        ..scopes::NONE
    },
    call_import_reader: Some(read_source),
    library_table: Some("bash"),
    ..SyntaxSpec::empty(Language::Bash, grammar, include_str!("../../queries/bash.scm"))
};

fn grammar() -> tree_sitter::Language {
    tree_sitter::Language::new(tree_sitter_bash::LANGUAGE)
}

/// Bash `source file` / `. file` with a literal path.
fn read_source<'t>(node: Node<'t>, source: &[u8], out: &mut Vec<ImportBinding<'t>>) {
    if node.kind() != "command" {
        return;
    }
    let Some(name) = node.child_by_field_name("name") else {
        return;
    };
    let mut cursor = node.walk();
    let args: Vec<Node<'t>> = node
        .children_by_field_name("argument", &mut cursor)
        .filter(|a| a.is_named())
        .collect();
    // `\.` / `\source`: a leading backslash only bypasses aliases.
    let is_source = |n: Node<'_>| matches!(text(n, source).trim().trim_start_matches('\\'), "source" | ".");
    if is_source(name) {
        if let Some(target) = args.first().and_then(|a| source_path(*a, source)) {
            out.push(ImportBinding::new("*".to_string(), target, ImportKind::Wildcard, node, None));
        }
    }
    // The pinned grammar folds a line that starts with `\` into the previous command when no
    // blank line separates them (`. ./a.sh` NEWLINE `\. "$D/b.sh"` parses as one command
    // whose arguments include `\.`). An escaped `\.` / `\source` word is never an ordinary
    // argument of a sourcing command, so its next argument is another sourced path.
    for w in args.windows(2) {
        if w[0].kind() == "word" && text(w[0], source).trim().starts_with('\\') && is_source(w[0]) {
            if let Some(target) = source_path(w[1], source) {
                out.push(ImportBinding::new("*".to_string(), target, ImportKind::Wildcard, node, None));
            }
        }
    }
}

/// The path argument of `source` / `.`. `"$NVM_DIR/nvm.sh"` / `$DIR/lib.sh`: the directory is
/// only known at run time, but the literal tail after the last expansion names the file;
/// module resolution then matches it as a path suffix (a repository file ending in
/// `/nvm.sh`). `None` for a path that is entirely dynamic.
fn source_path(arg: Node<'_>, source: &[u8]) -> Option<String> {
    const EXPANSIONS: [&str; 4] =
        ["expansion", "simple_expansion", "command_substitution", "arithmetic_expansion"];
    let parts = named_children(arg);
    let target = if matches!(arg.kind(), "string" | "concatenation")
        && parts.iter().any(|c| EXPANSIONS.contains(&c.kind()))
    {
        let last = parts
            .iter()
            .rposition(|c| EXPANSIONS.contains(&c.kind()))
            .unwrap_or(0);
        let tail: String = parts[last + 1..]
            .iter()
            .filter(|c| matches!(c.kind(), "string_content" | "word"))
            .map(|c| text(*c, source))
            .collect();
        tail.trim().trim_start_matches('/').to_string()
    } else {
        match arg.kind() {
            "word" => text(arg, source).trim().to_string(),
            "string" | "raw_string" => literal_text(arg, source),
            _ => return None,
        }
    };
    (!target.is_empty() && !target.contains('$')).then_some(target)
}
