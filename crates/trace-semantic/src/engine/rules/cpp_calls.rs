//! C / C++ calls answered with several in-index targets (engine 12).
//!
//! clangd reports every function a call range refers to: for a call the compiler resolved,
//! the chosen function plus the implicit calls at the same range (a converting or copying
//! constructor of the result, a conversion operator); for a dependent call inside a
//! template, every declaration its name lookup found (the overload set); for a function-like
//! macro invocation, every function its expansion calls (all reported at the macro name).
//! Three language rules turn these answers into the call's own target, from syntax trees
//! only ([`DeclTable::tree`]):
//!
//! 1. Macro invocation ([`macro_invocation`]): a call whose callee is a plain name that the
//!    partition declares only as function-like macros (`#define F(x) ...`), answered with
//!    targets none of which carries that name, runs the macro's expansion: it calls the
//!    in-index targets the server reports at the name (each a call edge; targets sharing a
//!    name are an overload set the expansion's call chooses from and stay unrecorded,
//!    [`expansion_callees`]), and its own target
//!    is the macro, which `textDocument/definition` at the name proves (the engine leaves the
//!    call to the definition fallback instead of recording the expansion's callees as
//!    alternatives).
//! 2. Implicit calls ([`narrow`]): when some targets carry the called name and others do
//!    not, the others are the compiler's implicit calls at the same range; only the named
//!    targets are candidates of the call (the definition path's name rule).
//! 3. Overloads by argument count ([`narrow`]): a candidate whose parameter list cannot bind
//!    the call's arguments is dropped. Parameters come from the declaration's
//!    `parameter_list` (defaults, `...`, parameter packs; `(void)` is empty; a C declaration
//!    `f()` declares nothing about its parameters and is never dropped); arguments from the
//!    call's `argument_list` (a pack expansion `args...` is zero or more arguments). Too
//!    many arguments drop a candidate without a variadic parameter; too few drop it only
//!    when no other declaration of the name with as many parameters could supply defaults
//!    (a redeclaration pair always contains a bodiless declaration). Nothing is dropped
//!    without positive evidence, and when no candidate would be left the list stays.
//!
//! One candidate left is the proven target only when the candidate set is complete by the
//! language's lookup rules: a qualified callee (`ns::f`, `W::f`; not `T::f` with `T` a type
//! parameter of an enclosing template: no argument-dependent lookup), a class member (of a
//! class body parsed without syntax error) called by an unqualified name, through `this`, or
//! as a constructor (class member lookup), a call without arguments (no
//! argument-dependent lookup), or a call whose implicit calls prove the compiler resolved
//! it (a dependent call has no implicit conversions) and named exactly one target. An
//! unqualified dependent call with arguments may reach functions argument-dependent lookup
//! finds at instantiation, and a dependent receiver's members were found in the primary
//! template only: those stay candidates (possible), narrowed.

use trace_core::facts::CallSite;
use trace_core::model::SymbolKind;
use trace_core::Language;

use super::cpp_templates::{exact_node, named_kids, node_text, template_scope, MAX_DEPENDENT_DEPTH};
use crate::mapping::{DeclRef, DeclTable};

/// C and C++ (the languages these rules speak about).
pub(crate) fn is_c_family(language: Option<Language>) -> bool {
    language.is_some_and(|l| trace_core::languages::info(l).family == Some(trace_core::languages::Family::C))
}

/// Rule 1: the call invokes a function-like macro, so the call-hierarchy targets at its
/// range are what the expansion calls, not the call's target.
pub(crate) fn macro_invocation(c: &CallSite, targets: &[DeclRef<'_>], decls: &DeclTable<'_>) -> bool {
    let Some(member) = c.member.as_deref() else {
        return false;
    };
    // A macro is invoked by its bare name.
    if c.callee != member || targets.iter().any(|t| decls.decl(*t).name == member) {
        return false;
    }
    let named = decls.named(member);
    !named.is_empty() && named.iter().all(|r| is_function_macro(decls, *r))
}

/// Rule 1: the targets a macro invocation calls. A target sharing its name with another
/// target at the range belongs to the overload set of a call in the expansion (inside a
/// template: every overload its lookup found), which the invocation's syntax cannot narrow:
/// only uniquely named targets are called.
pub(crate) fn expansion_callees<'a>(targets: &[DeclRef<'a>], decls: &DeclTable<'a>) -> Vec<DeclRef<'a>> {
    targets
        .iter()
        .copied()
        .filter(|t| {
            let name = &decls.decl(*t).name;
            targets.iter().filter(|o| decls.decl(**o).name == *name).count() == 1
        })
        .collect()
}

/// Whether a declaration is a function-like macro definition.
fn is_function_macro(decls: &DeclTable<'_>, r: DeclRef<'_>) -> bool {
    let Some(tree) = decls.tree(r.path) else {
        return false;
    };
    let d = decls.decl(r);
    let root = tree.root_node();
    let Some(name) = root.descendant_for_byte_range(d.name_span.start as usize, d.name_span.end as usize)
    else {
        return false;
    };
    name.parent().is_some_and(|p| {
        p.kind() == "preproc_function_def"
            && p.child_by_field_name("name").is_some_and(|n| n.id() == name.id())
    })
}

/// Result of [`narrow`].
pub(crate) struct Narrowed<'a> {
    pub targets: Vec<DeclRef<'a>>,
    /// Exactly one target is left and the lookup rules make the candidate set complete.
    pub proven: bool,
}

/// Rules 2 and 3 over the in-index targets of call `c` in file `path` (module docs).
pub(crate) fn narrow<'a>(
    targets: Vec<DeclRef<'a>>,
    c: &CallSite,
    decls: &DeclTable<'a>,
    path: &str,
) -> Narrowed<'a> {
    let original = targets.len();
    let named: Vec<DeclRef<'a>> = match c.member.as_deref() {
        Some(m) => targets.iter().copied().filter(|t| decls.decl(*t).name == m).collect(),
        None => Vec::new(),
    };
    // Implicit calls dropped: constructors / conversion operators beside the named target.
    let implicit_dropped = !named.is_empty()
        && named.len() < original
        && targets
            .iter()
            .filter(|t| !named.contains(t))
            .all(|t| is_implicit_callee(decls, *t));
    let named_one = named.len() == 1;
    let set = if named.is_empty() { targets } else { named };
    let Some(tree) = decls.tree(path) else {
        return Narrowed {
            targets: set,
            proven: false,
        };
    };
    let root = tree.root_node();
    let callee = exact_node(root, c.callee_span);
    let args = callee.and_then(call_arguments);
    let kept = match args {
        Some(args) => {
            let kept: Vec<DeclRef<'a>> = set
                .iter()
                .copied()
                .filter(|t| accepts(decls, *t, args) != Some(false))
                .collect();
            if kept.is_empty() {
                set
            } else {
                kept
            }
        }
        None => set,
    };
    let proven = match (kept.as_slice(), callee) {
        ([only], Some(callee)) => {
            (named_one && implicit_dropped)
                || args.is_some_and(|a| a.exact && a.count == 0)
                || complete_lookup(callee, decls, path, *only)
        }
        _ => false,
    };
    Narrowed {
        targets: kept,
        proven,
    }
}

/// A target the compiler calls implicitly at a call's range: a constructor (converting /
/// copying the result) or a conversion operator.
fn is_implicit_callee(decls: &DeclTable<'_>, t: DeclRef<'_>) -> bool {
    let d = decls.decl(t);
    d.kind == SymbolKind::Constructor || d.name.starts_with("operator")
}

/// Whether the lookup of the callee finds every candidate without argument-dependent
/// lookup: a name qualified by a scope that is no template parameter of an enclosing
/// template, or a class member (unqualified, through `this`, or a constructor of the named
/// type).
fn complete_lookup(
    callee: tree_sitter::Node<'_>,
    decls: &DeclTable<'_>,
    path: &str,
    target: DeclRef<'_>,
) -> bool {
    match callee.kind() {
        "qualified_identifier" => {
            let source = decls.source(path).unwrap_or_default();
            callee.child_by_field_name("scope").is_some_and(|scope| {
                scope.kind() == "namespace_identifier"
                    && !template_scope(callee, source)
                        .types
                        .contains(node_text(scope, source))
            })
        }
        "identifier" | "template_function" => is_member(decls, target),
        "field_expression" => {
            is_member(decls, target)
                && callee
                    .child_by_field_name("argument")
                    .is_some_and(|r| r.kind() == "this")
        }
        _ => false,
    }
}

/// A member function: declared in the body of a class the syntax tree parses without error
/// (a class specifier holding a syntax error may have swallowed the declarations after it),
/// or defined out of line with a qualified name whose scope the partition declares as a type
/// (`void W::f() {}`; a namespace is no type declaration).
fn is_member(decls: &DeclTable<'_>, t: DeclRef<'_>) -> bool {
    let Some(tree) = decls.tree(t.path) else {
        return false;
    };
    let d = decls.decl(t);
    let Some(name) = tree
        .root_node()
        .descendant_for_byte_range(d.name_span.start as usize, d.name_span.end as usize)
    else {
        return false;
    };
    let mut current = name.parent();
    for _ in 0..MAX_DEPENDENT_DEPTH {
        let Some(n) = current else { return false };
        match n.kind() {
            "field_declaration_list" => return n.parent().is_some_and(|class| !class.has_error()),
            "qualified_identifier" if n.child_by_field_name("name").is_some_and(|m| m.id() == name.id()) => {
                return n.child_by_field_name("scope").is_some_and(|scope| {
                    let source = decls.source(t.path).unwrap_or_default();
                    let scope = node_text(scope, source);
                    decls.named(scope).iter().any(|r| decls.decl(*r).kind.is_type())
                });
            }
            "compound_statement"
            | "namespace_definition"
            | "declaration_list"
            | "translation_unit"
            | "ERROR" => {
                return false;
            }
            _ => current = n.parent(),
        }
    }
    false
}

/// Explicit arguments of a call: `count` arguments, `exact` without a pack expansion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Args {
    count: u32,
    exact: bool,
}

/// Arguments of the call whose callee is `callee` (`None` without a plain argument list).
fn call_arguments(callee: tree_sitter::Node<'_>) -> Option<Args> {
    let call = callee.parent()?;
    let list = match call.kind() {
        "call_expression"
            if call
                .child_by_field_name("function")
                .is_some_and(|f| f.id() == callee.id()) =>
        {
            call.child_by_field_name("arguments")?
        }
        _ => return None,
    };
    if list.kind() != "argument_list" || list.has_error() {
        return None;
    }
    let mut args = Args {
        count: 0,
        exact: true,
    };
    for a in named_kids(list) {
        match a.kind() {
            "comment" => {}
            "parameter_pack_expansion" => args.exact = false,
            _ => args.count += 1,
        }
    }
    Some(args)
}

/// Parameters of a declaration: `positional` declared (`required` of them without a
/// default), `variadic` with `...` or a parameter pack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Arity {
    required: u32,
    positional: u32,
    variadic: bool,
}

/// Whether a candidate's parameter list binds the arguments: `Some(false)` only with
/// positive evidence (rule 3).
fn accepts(decls: &DeclTable<'_>, t: DeclRef<'_>, args: Args) -> Option<bool> {
    let arity = arity(decls, t)?;
    if !arity.variadic && args.count > arity.positional {
        return Some(false);
    }
    if args.exact && args.count < arity.required && !may_gain_defaults(decls, t, arity) {
        return Some(false);
    }
    Some(true)
}

/// Another callable declaration of the name with as many parameters (or an unknown list),
/// one of the two bodiless: a possible redeclaration that may supply default arguments.
fn may_gain_defaults(decls: &DeclTable<'_>, t: DeclRef<'_>, own: Arity) -> bool {
    let d = decls.decl(t);
    decls.named(&d.name).iter().any(|other| {
        let o = decls.decl(*other);
        *other != t
            && o.kind.is_callable()
            && (d.is_stub || o.is_stub)
            && arity(decls, *other)
                .is_none_or(|a| a.positional == own.positional && a.variadic == own.variadic)
    })
}

/// Arity of a function declaration from its `parameter_list` (`None` when unknown: a macro,
/// no function declarator, an unexpected parameter form, a C `f()`).
fn arity(decls: &DeclTable<'_>, t: DeclRef<'_>) -> Option<Arity> {
    let tree = decls.tree(t.path)?;
    let d = decls.decl(t);
    let source = decls.source(t.path)?;
    let name = tree
        .root_node()
        .descendant_for_byte_range(d.name_span.start as usize, d.name_span.end as usize)?;
    // The nearest function declarator around the name.
    let mut current = name.parent();
    let mut depth = 0usize;
    let list = loop {
        let n = current?;
        depth += 1;
        if depth > MAX_DEPENDENT_DEPTH {
            return None;
        }
        match n.kind() {
            "function_declarator" => break n.child_by_field_name("parameters")?,
            "function_definition"
            | "declaration"
            | "field_declaration"
            | "template_declaration"
            | "preproc_function_def"
            | "translation_unit" => return None,
            _ => current = n.parent(),
        }
    };
    if list.kind() != "parameter_list" || list.has_error() {
        return None;
    }
    let mut arity = Arity {
        required: 0,
        positional: 0,
        variadic: false,
    };
    let mut cursor = list.walk();
    if list.children(&mut cursor).any(|k| k.kind() == "...") {
        arity.variadic = true;
    }
    let declared: Vec<tree_sitter::Node<'_>> = named_kids(list)
        .into_iter()
        .filter(|p| p.kind() != "comment")
        .collect();
    // `(void)`: no parameters.
    if let [only] = declared.as_slice() {
        let void = only.kind() == "parameter_declaration"
            && only.child_by_field_name("declarator").is_none()
            && only
                .child_by_field_name("type")
                .is_some_and(|ty| ty.kind() == "primitive_type" && node_text(ty, source) == "void");
        if void {
            return Some(arity);
        }
    }
    // C `f()`: nothing is declared about the parameters.
    let c_language = decls.facts(t.path).and_then(|f| f.language) == Some(Language::C);
    if declared.is_empty() && c_language {
        return None;
    }
    for p in &declared {
        match p.kind() {
            "parameter_declaration" => {
                arity.positional += 1;
                arity.required += 1;
            }
            "optional_parameter_declaration" => arity.positional += 1,
            "variadic_parameter_declaration" | "variadic_parameter" => arity.variadic = true,
            _ => return None,
        }
    }
    Some(arity)
}

#[cfg(test)]
#[path = "../../../tests/unit/engine/rules/cpp_calls.rs"]
mod tests;
