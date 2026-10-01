//! The declaration route (Python, Rust, C, C++): `definition` on the callee, the library
//! declaration file parsed with tree-sitter, named types resolved through aliases,
//! typedefs, bounds and protocols (at most `MAX_HOPS` hops).

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use trace_core::facts::CallSite;
use trace_core::semantics::{FnTypeVerdict, SemCallbackParam};
use trace_core::Language;
use trace_library::table::Tables;
use tree_sitter::Node;

use super::*;

/// A location from a `definition` answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Loc {
    pub(super) uri: String,
    pub(super) line: u32,
    pub(super) character: u32,
}

pub(super) fn locations(v: &Value) -> Vec<Loc> {
    let items: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => vec![v],
        _ => Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|l| {
            let uri = l.get("uri").or_else(|| l.get("targetUri"))?.as_str()?;
            let range = l
                .get("targetSelectionRange")
                .or_else(|| l.get("range"))
                .or_else(|| l.get("targetRange"))?;
            let start = range.get("start")?;
            Some(Loc {
                uri: uri.to_string(),
                line: u32::try_from(start.get("line")?.as_u64()?).ok()?,
                character: u32::try_from(start.get("character")?.as_u64()?).ok()?,
            })
        })
        .collect()
}

/// Byte of the callee's last character (inside its last identifier).
pub(super) fn callee_point(call: &CallSite) -> u32 {
    call.callee_span.end.saturating_sub(1).max(call.callee_span.start)
}

pub(super) fn declaration_route(
    q: &FnTypeQuery<'_>,
    session: &mut dyn FnTypeSession,
    cache: &mut FnTypeCache,
) -> Option<SemCallbackParam> {
    let r = ParamRef::of(q.arg)?;
    let uri = session.uri_of(q.path).ok()?;
    let v = session
        .request(
            "textDocument/definition",
            json!({"textDocument": {"uri": uri}, "position": position(q.source, callee_point(q.call))}),
        )
        .ok()?;
    let mut locs = locations(&v);
    // Python: prefer the stub (typeshed .pyi) over the implementation.
    locs.sort_by_key(|l| !l.uri.ends_with(".pyi"));
    let shape = call_shape(q);
    let mut best: Option<Found> = None;
    for loc in locs.iter().take(2) {
        let Some(doc) = cache.doc(session, &loc.uri, q.language) else {
            continue;
        };
        let key = (
            doc.hash,
            loc.line,
            format!("{}|{shape:?}|{}|{}", r.key(), q.call.arg_count, q.call.member.as_deref().unwrap_or("")),
        );
        let found = match cache.verdict(&key) {
            Some(f) => f,
            None => {
                let mut f = declared_param(q, &doc, loc, &r, shape, session, cache);
                if f.symbol.is_none() {
                    f.symbol = trace_library::symbol::library_symbol(
                        doc.language,
                        &doc.path,
                        &doc.source,
                        loc.line,
                        loc.character,
                    );
                }
                cache.remember(key, f.clone());
                f
            }
        };
        best = better(best, found);
        if best
            .as_ref()
            .is_some_and(|b| b.verdict == FnTypeVerdict::FunctionType)
        {
            break;
        }
    }
    Some(answer(q, FnTypeRoute::Declaration, best))
}

/// Call facts the parameter mapping depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Shape {
    /// `receiver.method(..)` syntax: the receiver fills the `self` parameter.
    pub(super) method_syntax: bool,
}

pub(super) fn call_shape(q: &FnTypeQuery<'_>) -> Shape {
    match q.language {
        Language::Rust => Shape {
            method_syntax: rust_method_syntax(&q.call.callee),
        },
        Language::Python => Shape { method_syntax: true },
        _ => Shape { method_syntax: false },
    }
}

/// Whether a Rust callee is a method call (`x.m`), from a syntax parse of the callee text.
pub(super) fn rust_method_syntax(callee: &str) -> bool {
    const PRE: &str = "fn _f() { ";
    let callee = callee.trim();
    let wrapped = format!("{PRE}{callee}; }}");
    let Ok(tree) = trace_syntax::parse_tree(Language::Rust, wrapped.as_bytes()) else {
        return false;
    };
    let Some(n) = exact_node(tree.root_node(), PRE.len(), PRE.len() + callee.len()) else {
        return false;
    };
    match n.kind() {
        "field_expression" => true,
        "generic_function" => n
            .child_by_field_name("function")
            .is_some_and(|f| f.kind() == "field_expression"),
        _ => false,
    }
}

/// Classify the parameter the argument binds to in the declaration at `loc`.
pub(super) fn declared_param(
    q: &FnTypeQuery<'_>,
    doc: &Arc<Doc>,
    loc: &Loc,
    r: &ParamRef,
    shape: Shape,
    session: &mut dyn FnTypeSession,
    cache: &mut FnTypeCache,
) -> Found {
    // `fallbacks`: other readings of the same parameter, resolved only when the declared
    // reading says nothing (C annotation macros before the type, [`c_annotated_type`]).
    let (candidates, fallbacks): (Vec<Candidate>, Vec<Candidate>) = match doc.language {
        Language::Python => (python_params(doc, loc, r, tables()), Vec::new()),
        Language::Rust => (rust_params(doc, loc, r, shape, tables()), Vec::new()),
        Language::C | Language::Cpp => c_params(doc, loc, q.call, r, tables()),
        _ => (Vec::new(), Vec::new()),
    };
    let mut best = best_of(candidates, doc, session, cache, None);
    if best.as_ref().is_none_or(|b| b.verdict == FnTypeVerdict::Unknown) {
        best = best_of(fallbacks, doc, session, cache, best);
    }
    best.unwrap_or_else(Found::unknown)
}

/// The best verdict over `candidates` (named types resolved in the declaration file), from
/// `best`; stops at the first function type (nothing ranks higher).
pub(super) fn best_of(
    candidates: Vec<Candidate>,
    doc: &Arc<Doc>,
    session: &mut dyn FnTypeSession,
    cache: &mut FnTypeCache,
    mut best: Option<Found>,
) -> Option<Found> {
    for (class, param_type, param_name) in candidates {
        if best
            .as_ref()
            .is_some_and(|b| b.verdict == FnTypeVerdict::FunctionType)
        {
            break;
        }
        let class = match class {
            Class::Named(name, at) => resolve_named(doc, &name, at, session, cache, 0),
            other => other,
        };
        best = better(
            best,
            Found {
                verdict: class.verdict(),
                param_type,
                param_name,
                symbol: None,
            },
        );
    }
    best
}

/// Follow a named type with `definition` inside the declaration file.
pub(super) fn resolve_named(
    doc: &Arc<Doc>,
    name: &str,
    at: usize,
    session: &mut dyn FnTypeSession,
    cache: &mut FnTypeCache,
    hops: usize,
) -> Class {
    if hops >= MAX_HOPS || at == usize::MAX {
        return Class::Unknown;
    }
    let Ok(at) = u32::try_from(at) else {
        return Class::Unknown;
    };
    let (line, character) = doc.lines.utf16_of_byte(&doc.source, at);
    let Ok(v) = session.request(
        "textDocument/definition",
        json!({"textDocument": {"uri": doc.uri}, "position": {"line": line, "character": character}}),
    ) else {
        return Class::Unknown;
    };
    let Some(loc) = locations(&v).into_iter().next() else {
        return Class::Unknown;
    };
    let Some(target) = cache.doc(session, &loc.uri, doc.language) else {
        return Class::Unknown;
    };
    match alias_target(&target, loc.line, name, tables()) {
        // Bounded by MAX_HOPS.
        Class::Named(next, at) => resolve_named(&target, &next, at, session, cache, hops + 1),
        other => other,
    }
}

/// What a named type is, at its definition (`line` of the definition in `doc`).
pub(super) fn alias_target(doc: &Doc, line: u32, name: &str, tables: &Tables) -> Class {
    let cx = Cx {
        language: doc.language,
        src: &doc.source,
        tables,
        type_params: HashMap::new(),
    };
    let root = doc.tree.root_node();
    let row = line as usize;
    match doc.language {
        Language::Python => {
            for n in all_of_kind(root, &["assignment", "type_alias_statement", "class_definition"], 200_000) {
                if n.start_position().row != row {
                    continue;
                }
                match n.kind() {
                    "class_definition" => {
                        if n.child_by_field_name("name").map(|x| cx.text(x)) != Some(name) {
                            continue;
                        }
                        let body = n.child_by_field_name("body");
                        let has_call = body.is_some_and(|b| {
                            all_of_kind(b, &["function_definition"], 10_000).into_iter().any(|f| {
                                f.child_by_field_name("name").map(|x| cx.text(x)) == Some("__call__")
                                    && f.parent().is_some_and(|p| {
                                        p.id() == b.id() || p.parent().is_some_and(|pp| pp.id() == b.id())
                                    })
                            })
                        });
                        return if has_call {
                            Class::Function
                        } else {
                            name_class(Language::Python, name, tables).unwrap_or(Class::Not)
                        };
                    }
                    _ => {
                        let left = n
                            .child_by_field_name("left")
                            .or_else(|| n.child_by_field_name("name"));
                        if left.map(|x| cx.text(x)) != Some(name) {
                            continue;
                        }
                        let Some(right) = n
                            .child_by_field_name("right")
                            .or_else(|| n.child_by_field_name("value"))
                        else {
                            return Class::Unknown;
                        };
                        return python_alias_value(&cx, right);
                    }
                }
            }
            Class::Unknown
        }
        Language::Rust => {
            for n in all_of_kind(root, &["type_item", "trait_item", "struct_item", "enum_item"], 200_000) {
                if n.start_position().row != row
                    || n.child_by_field_name("name").map(|x| cx.text(x)) != Some(name)
                {
                    continue;
                }
                return match n.kind() {
                    "type_item" => n
                        .child_by_field_name("type")
                        .map_or(Class::Unknown, |t| class_of(&cx, t, 0)),
                    "trait_item" => {
                        let bounds = n.child_by_field_name("bounds");
                        if bounds.is_some_and(|b| class_of(&cx, b, 0) == Class::Function) {
                            Class::Function
                        } else {
                            Class::Top
                        }
                    }
                    _ => Class::Not,
                };
            }
            Class::Unknown
        }
        Language::C | Language::Cpp => {
            for n in all_of_kind(
                root,
                &["type_definition", "alias_declaration", "class_specifier", "struct_specifier"],
                200_000,
            ) {
                if n.start_position().row != row {
                    continue;
                }
                match n.kind() {
                    "type_definition" => {
                        let Some(d) = n.child_by_field_name("declarator") else {
                            continue;
                        };
                        if decl_name(d).map(|x| cx.text(x)) != Some(name) {
                            continue;
                        }
                        if declarator_is_function(d) {
                            return Class::Function;
                        }
                        return n
                            .child_by_field_name("type")
                            .map_or(Class::Unknown, |t| class_of(&cx, t, 0));
                    }
                    "alias_declaration" => {
                        if n.child_by_field_name("name").map(|x| cx.text(x)) != Some(name) {
                            continue;
                        }
                        return n
                            .child_by_field_name("type")
                            .map_or(Class::Unknown, |t| class_of(&cx, t, 0));
                    }
                    _ => {
                        if n.child_by_field_name("name").map(|x| cx.text(x)) != Some(name) {
                            continue;
                        }
                        // A functor type: its operator() runs when the value is called.
                        let call_operator = all_of_kind(n, &["operator_name"], 10_000)
                            .into_iter()
                            .any(|o| cx.text(o).replace(' ', "") == "operator()");
                        return if call_operator {
                            Class::Function
                        } else {
                            Class::Not
                        };
                    }
                }
            }
            Class::Unknown
        }
        _ => Class::Unknown,
    }
}

/// Right side of a Python type alias / TypeVar assignment.
pub(super) fn python_alias_value(cx: &Cx<'_>, right: Node<'_>) -> Class {
    if right.kind() == "call" {
        let function = right
            .child_by_field_name("function")
            .map(|f| last_segment(cx.text(f)).to_string())
            .unwrap_or_default();
        return match function.as_str() {
            "TypeVar" => {
                let bound = right.child_by_field_name("arguments").and_then(|a| {
                    named_kids(a).into_iter().find(|k| {
                        k.kind() == "keyword_argument"
                            && k.child_by_field_name("name").map(|n| cx.text(n)) == Some("bound")
                    })
                });
                match bound.and_then(|b| b.child_by_field_name("value")) {
                    Some(v) => python_class(cx, v, 1),
                    None => Class::Top,
                }
            }
            "ParamSpec" | "TypeVarTuple" => Class::Top,
            "NewType" => Class::Not,
            _ => Class::Unknown,
        };
    }
    python_class(cx, right, 1)
}

/// The identifier a C/C++ declarator declares.
pub(super) fn decl_name(n: Node<'_>) -> Option<Node<'_>> {
    match n.kind() {
        "identifier" | "field_identifier" | "type_identifier" | "operator_name" | "destructor_name" => {
            Some(n)
        }
        "qualified_identifier" | "template_function" | "template_method" => {
            n.child_by_field_name("name").and_then(decl_name)
        }
        "function_declarator"
        | "pointer_declarator"
        | "reference_declarator"
        | "parenthesized_declarator"
        | "array_declarator"
        | "init_declarator" => n
            .child_by_field_name("declarator")
            // `(__cdecl *name)`: the calling-convention / attribute nodes before the inner
            // declarator declare nothing.
            .or_else(|| {
                named_kids(n).into_iter().find(|c| {
                    !matches!(
                        c.kind(),
                        "ms_call_modifier"
                            | "ms_pointer_modifier"
                            | "ms_based_modifier"
                            | "attribute_specifier"
                            | "attribute_declaration"
                            | "type_qualifier"
                            | "comment"
                    )
                })
            })
            .and_then(decl_name),
        _ => None,
    }
}

/// Names declared by `typedef`s of a C/C++ file: whether each is a function (pointer) type
/// and where its first declaration starts (bounded).
pub(super) fn c_typedefs(root: Node<'_>, src: &[u8]) -> HashMap<String, (bool, usize)> {
    let mut out = HashMap::new();
    for n in all_of_kind(root, &["type_definition"], 100_000) {
        let mut cursor = n.walk();
        for d in n.children_by_field_name("declarator", &mut cursor) {
            if let Some(name) = decl_name(d) {
                out.entry(text(name, src).to_string())
                    .or_insert((declarator_is_function(d), n.start_byte()));
            }
        }
    }
    out
}

/// The typedef `name` declared before `at` in the file: whether it is a function type (C
/// declares a typedef before its uses; a later one is not the one in scope).
pub(super) fn typedef_before(
    typedefs: &HashMap<String, (bool, usize)>,
    name: &str,
    at: usize,
) -> Option<bool> {
    typedefs
        .get(name)
        .filter(|(_, start)| *start < at)
        .map(|(function, _)| *function)
}

/// A parameter whose type is a function-pointer typedef declared earlier in the same file
/// (`CompareFn cmp`): a function type without a definition hop.
pub(super) fn c_local_function_type(
    p: Node<'_>,
    src: &[u8],
    typedefs: &HashMap<String, (bool, usize)>,
) -> bool {
    let Some(ty) = p.child_by_field_name("type") else {
        return false;
    };
    ty.kind() == "type_identifier"
        && typedef_before(typedefs, text(ty, src), p.start_byte()) == Some(true)
        && p.child_by_field_name("declarator")
            .is_none_or(|d| d.kind() == "identifier")
}

/// Annotation macros before a parameter's type (MSVC SAL `_In_`, `_In_opt_`) are unknown
/// identifiers to the grammar: `_In_ CompareFn _Compare` parses with the macro as the type
/// and the real type as the declarator. When the parsed type names no typedef of the file
/// and the declarator is a bare identifier, that identifier is the candidate type: a
/// function-pointer typedef of the file directly, else a name to resolve with `definition`.
/// The grammar may instead recover the real type as an error node holding one identifier
/// between the macro and the declarator (`_In_ CompareFn _Compare` with `CompareFn` in the
/// error node): that identifier is the candidate type then.
pub(super) fn c_annotated_type(
    p: Node<'_>,
    src: &[u8],
    typedefs: &HashMap<String, (bool, usize)>,
) -> Option<Class> {
    let ty = p.child_by_field_name("type")?;
    let declarator = p.child_by_field_name("declarator")?;
    if ty.kind() != "type_identifier" || declarator.kind() != "identifier" {
        return None;
    }
    if typedefs.contains_key(text(ty, src)) {
        return None;
    }
    let recovered = named_kids(p).into_iter().find(|k| {
        k.kind() == "ERROR"
            && k.start_byte() >= ty.end_byte()
            && k.end_byte() <= declarator.start_byte()
            && k.named_child_count() == 1
            && k.named_child(0).is_some_and(|i| i.kind() == "identifier")
    });
    let declarator = recovered.and_then(|k| k.named_child(0)).unwrap_or(declarator);
    let name = text(declarator, src);
    Some(match typedef_before(typedefs, name, p.start_byte()) {
        Some(true) => Class::Function,
        Some(false) => return None,
        None => Class::Named(name.to_string(), declarator.start_byte()),
    })
}

pub(super) type Candidate = (Class, String, Option<String>);

/// Python: the def at `loc` (a class -> its `__init__`), with its `@overload` siblings.
pub(super) fn python_params(doc: &Doc, loc: &Loc, r: &ParamRef, tables: &Tables) -> Vec<Candidate> {
    let root = doc.tree.root_node();
    let cx = Cx {
        language: Language::Python,
        src: &doc.source,
        tables,
        type_params: HashMap::new(),
    };
    let target = all_of_kind(root, &["function_definition", "class_definition"], 200_000)
        .into_iter()
        .find(|n| {
            n.child_by_field_name("name")
                .is_some_and(|x| x.start_position().row == loc.line as usize)
        });
    let Some(target) = target else {
        return Vec::new();
    };
    let defs: Vec<Node<'_>> = if target.kind() == "class_definition" {
        let Some(body) = target.child_by_field_name("body") else {
            return Vec::new();
        };
        let init = python_defs_in(body, "__init__", &cx);
        if init.is_empty() {
            python_defs_in(body, "__new__", &cx)
        } else {
            init
        }
    } else {
        let name = target
            .child_by_field_name("name")
            .map(|n| cx.text(n).to_string())
            .unwrap_or_default();
        let block = python_scope_of(target);
        match block {
            Some(b) => python_defs_in(b, &name, &cx),
            None => vec![target],
        }
    };
    let mut out = Vec::new();
    for def in defs {
        let method = python_is_method(def, &cx);
        let Some(params) = def.child_by_field_name("parameters") else {
            continue;
        };
        if let Some((ty, name)) = python_select(&cx, params, r, method) {
            match ty {
                Some(t) => out.push((class_of(&cx, t, 0), cx.text(t).to_string(), Some(name))),
                None => out.push((Class::Unknown, String::new(), Some(name))),
            }
        }
    }
    out
}

/// The block holding a def (through its decorated_definition).
pub(super) fn python_scope_of(def: Node<'_>) -> Option<Node<'_>> {
    let parent = def.parent()?;
    if parent.kind() == "decorated_definition" {
        parent.parent()
    } else {
        Some(parent)
    }
}

/// Function definitions named `name` directly inside `block` (overloads included).
pub(super) fn python_defs_in<'t>(block: Node<'t>, name: &str, cx: &Cx<'_>) -> Vec<Node<'t>> {
    named_kids(block)
        .into_iter()
        .filter_map(|n| match n.kind() {
            "function_definition" => Some(n),
            "decorated_definition" => n.child_by_field_name("definition"),
            _ => None,
        })
        .filter(|f| f.kind() == "function_definition")
        .filter(|f| f.child_by_field_name("name").map(|x| cx.text(x)) == Some(name))
        .collect()
}

/// A def inside a class body that is not a staticmethod (its first parameter is the
/// receiver).
pub(super) fn python_is_method(def: Node<'_>, cx: &Cx<'_>) -> bool {
    let decorated = def.parent().filter(|p| p.kind() == "decorated_definition");
    let static_method = decorated.is_some_and(|d| {
        named_kids(d)
            .into_iter()
            .filter(|c| c.kind() == "decorator")
            .any(|dec| {
                named_kids(dec)
                    .into_iter()
                    .any(|e| last_segment(cx.text(e)) == "staticmethod")
            })
    });
    let in_class = python_scope_of(def)
        .and_then(|b| b.parent())
        .is_some_and(|c| c.kind() == "class_definition");
    in_class && !static_method
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PyKind {
    Positional,
    PositionalOnlyEnd,
    KeywordOnlyStart,
    VarArgs,
    KwArgs,
}

/// Map the argument to a Python parameter: (type annotation, parameter name).
pub(super) fn python_select<'t>(
    cx: &Cx<'_>,
    params: Node<'t>,
    r: &ParamRef,
    method: bool,
) -> Option<(Option<Node<'t>>, String)> {
    let mut list: Vec<(PyKind, String, Option<Node<'t>>)> = Vec::new();
    for p in named_kids(params) {
        let entry = match p.kind() {
            "identifier" => (PyKind::Positional, cx.text(p).to_string(), None),
            "typed_parameter" => {
                let head = first_named(p)?;
                let ty = p.child_by_field_name("type");
                match head.kind() {
                    "list_splat_pattern" => {
                        (PyKind::VarArgs, cx.text(head).trim_start_matches('*').to_string(), ty)
                    }
                    "dictionary_splat_pattern" => {
                        (PyKind::KwArgs, cx.text(head).trim_start_matches('*').to_string(), ty)
                    }
                    _ => (PyKind::Positional, cx.text(head).to_string(), ty),
                }
            }
            "default_parameter" | "typed_default_parameter" => (
                PyKind::Positional,
                p.child_by_field_name("name")
                    .map(|n| cx.text(n).to_string())
                    .unwrap_or_default(),
                p.child_by_field_name("type"),
            ),
            "list_splat_pattern" => (PyKind::VarArgs, cx.text(p).trim_start_matches('*').to_string(), None),
            "dictionary_splat_pattern" => {
                (PyKind::KwArgs, cx.text(p).trim_start_matches('*').to_string(), None)
            }
            "keyword_separator" => (PyKind::KeywordOnlyStart, String::new(), None),
            "positional_separator" => (PyKind::PositionalOnlyEnd, String::new(), None),
            _ => continue,
        };
        list.push(entry);
    }
    if method {
        if let Some(i) = list.iter().position(|e| e.0 == PyKind::Positional) {
            list.remove(i);
        }
    }
    // Positional-capable parameters (before `*` / `*args`), and whether each may also be
    // passed by keyword (after `/`).
    let mut positional: Vec<usize> = Vec::new();
    let mut by_keyword: Vec<usize> = Vec::new();
    let mut keyword_only = false;
    let slash = list.iter().position(|e| e.0 == PyKind::PositionalOnlyEnd);
    for (i, e) in list.iter().enumerate() {
        match e.0 {
            PyKind::Positional => {
                if !keyword_only {
                    positional.push(i);
                }
                if slash.is_none_or(|s| i > s) {
                    by_keyword.push(i);
                }
            }
            PyKind::VarArgs | PyKind::KeywordOnlyStart => keyword_only = true,
            _ => {}
        }
    }
    let pick = |i: usize| Some((list[i].2, list[i].1.clone()));
    if let Some(k) = &r.keyword {
        if let Some(&i) = by_keyword.iter().find(|&&i| &list[i].1 == k) {
            return pick(i);
        }
        return list.iter().position(|e| e.0 == PyKind::KwArgs).and_then(pick);
    }
    let index = usize::try_from(r.index?).ok()?;
    match positional.get(index) {
        Some(&i) => pick(i),
        None => list.iter().position(|e| e.0 == PyKind::VarArgs).and_then(pick),
    }
}

/// Rust: the fn at `loc`, with type parameters (and their `Fn*` bounds) in scope.
pub(super) fn rust_params(
    doc: &Doc,
    loc: &Loc,
    r: &ParamRef,
    shape: Shape,
    tables: &Tables,
) -> Vec<Candidate> {
    let root = doc.tree.root_node();
    let mut cx = Cx {
        language: Language::Rust,
        src: &doc.source,
        tables,
        type_params: HashMap::new(),
    };
    let Some(item) = all_of_kind(root, &["function_item", "function_signature_item"], 200_000)
        .into_iter()
        .find(|n| {
            n.child_by_field_name("name")
                .is_some_and(|x| x.start_position().row == loc.line as usize)
        })
    else {
        return Vec::new();
    };
    cx.type_params = rust_type_params(&cx, item);
    let Some(params) = item.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut list: Vec<Node<'_>> = named_kids(params)
        .into_iter()
        .filter(|p| matches!(p.kind(), "parameter" | "self_parameter" | "variadic_parameter"))
        .collect();
    if shape.method_syntax && list.first().is_some_and(|p| p.kind() == "self_parameter") {
        list.remove(0);
    }
    let Some(index) = r.index.and_then(|i| usize::try_from(i).ok()) else {
        return Vec::new();
    };
    let Some(p) = list.get(index) else {
        return Vec::new();
    };
    let Some(ty) = p.child_by_field_name("type") else {
        return vec![(Class::Unknown, String::new(), None)];
    };
    let name = p.child_by_field_name("pattern").map(|n| cx.text(n).to_string());
    vec![(class_of(&cx, ty, 0), cx.text(ty).to_string(), name)]
}

/// Type parameters of a Rust fn and its enclosing impl/trait: name -> class of the bounds
/// (a bound on `Fn*` makes it a function type; no bound or other bounds: a top type).
pub(super) fn rust_type_params(cx: &Cx<'_>, item: Node<'_>) -> HashMap<String, Class> {
    let bare = Cx {
        language: cx.language,
        src: cx.src,
        tables: cx.tables,
        type_params: HashMap::new(),
    };
    let mut out: HashMap<String, Class> = HashMap::new();
    let mut scopes = vec![item];
    let mut up = item.parent();
    while let Some(n) = up {
        if matches!(n.kind(), "impl_item" | "trait_item") {
            scopes.push(n);
        }
        up = n.parent();
    }
    for scope in scopes {
        if let Some(tp) = scope.child_by_field_name("type_parameters") {
            for p in named_kids(tp) {
                match p.kind() {
                    "type_parameter" | "constrained_type_parameter" => {
                        let name = p
                            .child_by_field_name("name")
                            .or_else(|| p.child_by_field_name("left"))
                            .map(|n| bare.text(n).to_string());
                        let Some(name) = name else { continue };
                        let class = p
                            .child_by_field_name("bounds")
                            .map_or(Class::Top, |b| class_of(&bare, b, 0));
                        merge_bound(&mut out, name, class);
                    }
                    "type_identifier" => merge_bound(&mut out, bare.text(p).to_string(), Class::Top),
                    _ => {}
                }
            }
        }
        for clause in named_kids(scope).into_iter().filter(|c| c.kind() == "where_clause") {
            for pred in named_kids(clause) {
                let (Some(left), Some(bounds)) =
                    (pred.child_by_field_name("left"), pred.child_by_field_name("bounds"))
                else {
                    continue;
                };
                merge_bound(&mut out, bare.text(left).to_string(), class_of(&bare, bounds, 0));
            }
        }
    }
    out
}

pub(super) fn merge_bound(out: &mut HashMap<String, Class>, name: String, class: Class) {
    let class = if class == Class::Function {
        Class::Function
    } else {
        Class::Top
    };
    let entry = out.entry(name).or_insert(Class::Top);
    if class == Class::Function {
        *entry = Class::Function;
    }
}

/// C/C++: every declaration of the callee's name in the file (overloads), the one at `loc`
/// first, with template parameters in scope.
/// C/C++: the parameter of every declaration of the callee (the one at `loc` first), and the
/// fallback readings of annotated parameters ([`c_annotated_type`]) that need a definition
/// hop.
pub(super) fn c_params(
    doc: &Doc,
    loc: &Loc,
    call: &CallSite,
    r: &ParamRef,
    tables: &Tables,
) -> (Vec<Candidate>, Vec<Candidate>) {
    let root = doc.tree.root_node();
    let src: &[u8] = &doc.source;
    let declarators = all_of_kind(root, &["function_declarator"], 500_000);
    let at_line: Option<String> = declarators
        .iter()
        .filter_map(|d| decl_name(*d))
        .find(|n| n.start_position().row == loc.line as usize)
        .map(|n| text(n, src).to_string());
    let Some(name) = at_line.or_else(|| call.member.clone()) else {
        return (Vec::new(), Vec::new());
    };
    let mut matching: Vec<Node<'_>> = declarators
        .into_iter()
        .filter(|d| decl_name(*d).is_some_and(|n| text(n, src) == name))
        .collect();
    matching.sort_by_key(|d| d.start_position().row != loc.line as usize);
    let Some(index) = r.index.and_then(|i| usize::try_from(i).ok()) else {
        return (Vec::new(), Vec::new());
    };
    let mut out = Vec::new();
    let mut fallbacks = Vec::new();
    let typedefs = c_typedefs(root, src);
    for fd in matching.into_iter().take(16) {
        let cx = Cx {
            language: doc.language,
            src,
            tables,
            type_params: cpp_template_params(fd, src, tables, doc.language),
        };
        let Some(params) = fd.child_by_field_name("parameters") else {
            continue;
        };
        let list: Vec<Node<'_>> = named_kids(params)
            .into_iter()
            .filter(|p| {
                matches!(
                    p.kind(),
                    "parameter_declaration"
                        | "optional_parameter_declaration"
                        | "variadic_parameter_declaration"
                        | "variadic_parameter"
                )
            })
            .collect();
        let p = list.get(index).copied().or_else(|| {
            list.last()
                .copied()
                .filter(|l| matches!(l.kind(), "variadic_parameter" | "variadic_parameter_declaration"))
        });
        let Some(p) = p else { continue };
        let name = p
            .child_by_field_name("declarator")
            .and_then(decl_name)
            .map(|n| text(n, src).to_string());
        let class = if c_local_function_type(p, src, &typedefs) {
            Class::Function
        } else {
            c_param_class(&cx, p, 0)
        };
        match c_annotated_type(p, src, &typedefs) {
            // A function-pointer typedef of this file: the parameter's real type (its name
            // is hidden behind the annotation, so none is claimed).
            Some(Class::Function) => out.push((Class::Function, text(p, src).to_string(), None)),
            Some(named) => fallbacks.push((named, text(p, src).to_string(), None)),
            None => {}
        }
        out.push((class, text(p, src).to_string(), name));
    }
    (out, fallbacks)
}

/// Template parameters around a C++ declaration: unconstrained -> top type; constrained by
/// a callable concept (`std::invocable F`) -> function type.
pub(super) fn cpp_template_params(
    fd: Node<'_>,
    src: &[u8],
    tables: &Tables,
    language: Language,
) -> HashMap<String, Class> {
    let mut out = HashMap::new();
    let mut up = fd.parent();
    while let Some(n) = up {
        if n.kind() == "template_declaration" {
            if let Some(list) = n.child_by_field_name("parameters") {
                for p in named_kids(list) {
                    match p.kind() {
                        "type_parameter_declaration"
                        | "variadic_type_parameter_declaration"
                        | "optional_type_parameter_declaration" => {
                            if let Some(id) = all_of_kind(p, &["type_identifier"], 1).into_iter().next() {
                                out.entry(text(id, src).to_string()).or_insert(Class::Top);
                            }
                        }
                        "parameter_declaration" | "optional_parameter_declaration" => {
                            let concept = p.child_by_field_name("type").map(|t| text(t, src));
                            let id = p.child_by_field_name("declarator").and_then(decl_name);
                            if let (Some(concept), Some(id)) = (concept, id) {
                                if name_class(language, concept, tables) == Some(Class::Function) {
                                    out.insert(text(id, src).to_string(), Class::Function);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        up = n.parent();
    }
    out
}

/// Classify a standalone type text of a declaration-route language (also Python string
/// annotations).
pub(super) fn standalone_type_class(language: Language, type_text: &str, tables: &Tables) -> Class {
    let (pre, post) = match language {
        Language::Python => ("def _f(_x: ", "): ...\n"),
        Language::Rust => ("fn _f(_x: ", ") {}\n"),
        // C/C++: the text is a parameter declaration (`void (*)(int)`, `int x`).
        Language::C | Language::Cpp => ("void _f(", ");\n"),
        _ => return Class::Unknown,
    };
    let source = format!("{pre}{type_text}{post}");
    let src = source.as_bytes();
    let Ok(tree) = trace_syntax::parse_tree(language, src) else {
        return Class::Unknown;
    };
    let cx = Cx {
        language,
        src,
        tables,
        type_params: HashMap::new(),
    };
    let (start, end) = (pre.len(), pre.len() + type_text.len());
    let root = tree.root_node();
    let class = match language {
        Language::Python => find_kind(root, &["typed_parameter"], 0, src.len())
            .and_then(|p| p.child_by_field_name("type"))
            .map_or(Class::Unknown, |t| class_of(&cx, t, 0)),
        Language::Rust => find_kind(root, &["parameter"], 0, src.len())
            .and_then(|p| p.child_by_field_name("type"))
            .map_or(Class::Unknown, |t| class_of(&cx, t, 0)),
        _ => find_kind(root, &["parameter_declaration", "variadic_parameter"], start, end)
            .map_or(Class::Unknown, |p| c_param_class(&cx, p, 0)),
    };
    match class {
        // Synthetic text: a name cannot be followed.
        Class::Named(name, _) => Class::Named(name, usize::MAX),
        other => other,
    }
}
