//! Haskell: the syntax spec (grammar, query, node-kind tables and language rules).

use trace_core::facts::ImportKind;
use trace_core::Language;
use tree_sitter::Node;

use crate::interface::ReturnRule;
use crate::language_rules::{LanguageRules, ModulePaths, NameReach, Signatures, NONE};
use crate::names::{join_sep, path_text, ImportBinding};
use crate::node::{has_direct_token, named_children, text};
use crate::scopes::{self, bind, ScopeRules};
use crate::spec::{call, SyntaxSpec};

/// Inference rules of the language (`crate::language_rules`).
const RULES: LanguageRules = LanguageRules {
    nested_functions_are_local: true,
    signatures: Signatures::EveryEquation,
    modules: ModulePaths::ModuleFiles,
    bare_reach: NameReach::FileOrImports,
    locals_shadow_functions: true,
    static_types: true,
    ..NONE
};

pub(crate) static SYNTAX: SyntaxSpec = SyntaxSpec {
    rules: RULES,
    lazy_scopes: &["lambda", "function", "bind"],
    anonymous_functions: &["lambda"],
    imports: &["import"],
    identifiers: &["variable"],
    name_kinds: &["constructor", "name"],
    // Call facts for the backticked infix form ``x `f` y`` come from queries/haskell.scm
    // (named functions only, never symbolic operators); value-flow lowering keeps `apply`.
    calls: &[call("apply", "function", "")],
    binding_kinds: &["patterns", "imports", "signature"],
    comments: &["comment", "haddock"],
    scopes: &ScopeRules {
        blocks: &["alternative"],
        binders: &[bind("patterns", ""), bind("alternative", "pattern")],
        ordered: false,
        pattern_calls: true,
        ..scopes::NONE
    },
    return_rule: ReturnRule::WholeBody,
    import_readers: &[("import", read_import)],
    library_table: Some("haskell"),
    ..SyntaxSpec::empty(Language::Haskell, grammar, include_str!("../../queries/haskell.scm"))
};

fn grammar() -> tree_sitter::Language {
    tree_sitter::Language::new(tree_sitter_haskell::LANGUAGE)
}

/// Haskell `import M (a, b)`, `import qualified M as N`, `import M hiding (x)`, `import M`.
fn read_import<'t>(node: Node<'t>, source: &[u8], out: &mut Vec<ImportBinding<'t>>) {
    let Some(module) = node.child_by_field_name("module") else {
        return;
    };
    let target = path_text(module, source, ".");
    let names = node.child_by_field_name("names");
    let hiding = has_direct_token(node, "hiding") || names.is_some_and(|n| has_direct_token(n, "hiding"));
    let qualified = has_direct_token(node, "qualified");
    if let Some(alias) = node.child_by_field_name("alias") {
        let local = path_text(alias, source, ".");
        out.push(ImportBinding::new(local, target.clone(), ImportKind::Module, node, Some(alias)));
    } else if qualified {
        out.push(ImportBinding::new(target.clone(), target.clone(), ImportKind::Module, node, Some(module)));
    }
    if qualified {
        return;
    }
    match names {
        Some(list) if !hiding => {
            for item in named_children(list).into_iter().filter(|c| c.kind() == "import_name") {
                let name = item
                    .child_by_field_name("variable")
                    .or_else(|| item.child_by_field_name("type"))
                    .unwrap_or(item);
                let local = text(name, source).trim().to_string();
                if !local.is_empty() {
                    let target = join_sep(&target, &local, ".");
                    out.push(ImportBinding::new(local, target, ImportKind::Member, item, Some(name)));
                }
            }
        }
        _ => out.push(ImportBinding::new("*".to_string(), target, ImportKind::Wildcard, node, None)),
    }
}
