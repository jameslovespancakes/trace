//! The label route (Go, PHP, C#, Scala): `signatureHelp` labels (or the callee's `hover`
//! declaration) parsed as a synthetic declaration; anonymous function arguments.

use crate::SemanticError;
use serde_json::{json, Value};
use std::collections::HashMap;
use trace_core::facts::{ArgSlot, CallbackArg, Consumer, FileFacts};
use trace_core::semantics::{FnTypeVerdict, SemCallbackParam};
use trace_core::text::LineIndex;
use trace_core::Language;
use trace_library::table::Tables;
use tree_sitter::Node;

use super::*;

/// One parameter parsed from a signature label.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct LabelParam {
    pub(super) name: Option<String>,
    pub(super) variadic: bool,
    pub(super) type_text: String,
    pub(super) class: Class,
}

/// Synthetic declaration around ONE parameter label.
pub(super) fn label_wrapper(language: Language) -> Option<(&'static str, &'static str)> {
    Some(match language {
        Language::Go => ("package p\nfunc _(", ") {}\n"),
        Language::Php => ("<?php function _(", ") {}\n"),
        Language::CSharp => ("class _T { void _M(", ") {} }\n"),
        Language::Java => ("class _T { void _m(", ") {} }\n"),
        Language::Scala => ("def _f(", ") = ()\n"),
        _ => return None,
    })
}

pub(super) fn param_kinds(language: Language) -> &'static [&'static str] {
    match language {
        Language::Go => &["parameter_declaration", "variadic_parameter_declaration"],
        Language::Php => &["simple_parameter", "variadic_parameter", "property_promotion_parameter"],
        Language::CSharp => &["parameter"],
        Language::Java => &["formal_parameter", "spread_parameter"],
        Language::Scala => &["parameter", "class_parameter"],
        _ => &[],
    }
}

/// Parse one parameter label of a signature (`less func(i int, j int) bool`,
/// `Comparison<int> comparison`, `f: A => B`).
pub(super) fn parse_label_param(language: Language, label: &str, tables: &Tables) -> Option<LabelParam> {
    let label = label.trim();
    if label.is_empty() {
        return None;
    }
    let (pre, post) = label_wrapper(language)?;
    let wrapped = format!("{pre}{label}{post}");
    let src = wrapped.as_bytes();
    let tree = trace_syntax::parse_tree(language, src).ok()?;
    let root = tree.root_node();
    let (start, end) = (pre.len(), pre.len() + label.len());
    let cx = Cx {
        language,
        src,
        tables,
        type_params: HashMap::new(),
    };
    let holder = find_kind(root, param_kinds(language), start, end)
        .or_else(|| find_kind(root, &["parameter_list", "formal_parameters"], 0, src.len()));
    let Some(p) = holder else {
        return Some(LabelParam {
            name: None,
            variadic: false,
            type_text: label.to_string(),
            class: Class::Unknown,
        });
    };
    Some(label_param_of(&cx, p, label))
}

/// One parsed parameter node of `language` (a label wrapped in a synthetic declaration, or a
/// parameter of a declaration the server showed): name, variadic flag, type text and class.
/// `fallback` is the type text when the parameter has no type node.
pub(super) fn label_param_of(cx: &Cx<'_>, p: Node<'_>, fallback: &str) -> LabelParam {
    let (language, src) = (cx.language, cx.src);
    let name = label_name(p, src);
    let type_node = label_type_node(language, p);
    let variadic = label_variadic(language, p, type_node);
    let class = match type_node {
        Some(t) => class_of(cx, t, 0),
        // Untyped PHP parameters accept anything.
        None if language == Language::Php => Class::Top,
        None => Class::Unknown,
    };
    let type_text = type_node
        .map(|t| text(t, src).to_string())
        .unwrap_or_else(|| fallback.to_string());
    LabelParam {
        name,
        variadic,
        type_text,
        class: settle(language, class),
    }
}

/// How a hover answer of a label-route server shows a callee's declaration in its own
/// language: the prefix that makes it a source file and the parameter-list node kinds of a
/// function declaration (PHP: `function array_map(?callable $callback, array $array, array
/// ...$arrays): array`).
pub(super) fn hover_declaration(language: Language) -> Option<(&'static str, &'static [&'static str])> {
    const PHP_LISTS: &[&str] = &["formal_parameters"];
    match language {
        Language::Php => Some(("<?php\n", PHP_LISTS)),
        _ => None,
    }
}

/// The parameters of the first function declaration in a hover `code` block of `language`
/// (parsed with the language's grammar; nothing when the language has no hover form).
pub(super) fn hover_params(language: Language, code: &str, tables: &Tables) -> Vec<LabelParam> {
    let Some((prefix, lists)) = hover_declaration(language) else {
        return Vec::new();
    };
    // A PHP hover block usually starts with its own `<?php` tag.
    let prefix = if code.trim_start().starts_with("<?") {
        ""
    } else {
        prefix
    };
    let source = format!("{prefix}{code}\n");
    let src = source.as_bytes();
    let Ok(tree) = trace_syntax::parse_tree(language, src) else {
        return Vec::new();
    };
    let Some(list) = all_of_kind(tree.root_node(), lists, 1).into_iter().next() else {
        return Vec::new();
    };
    let cx = Cx {
        language,
        src,
        tables,
        type_params: HashMap::new(),
    };
    named_kids(list)
        .into_iter()
        .filter(|p| param_kinds(language).contains(&p.kind()))
        .map(|p| label_param_of(&cx, p, text(p, src)))
        .collect()
}

/// Whether the parameter itself is variadic (tokens of the parameter, never of its type:
/// `f func(xs ...int)` is not variadic).
pub(super) fn label_variadic(language: Language, p: Node<'_>, type_node: Option<Node<'_>>) -> bool {
    let direct = |kind: &str| kids(p).into_iter().any(|c| c.kind() == kind);
    match language {
        Language::Go => p.kind() == "variadic_parameter_declaration",
        Language::Php => p.kind() == "variadic_parameter" || direct("..."),
        Language::CSharp => direct("params"),
        Language::Java => p.kind() == "spread_parameter",
        Language::Scala => type_node.is_some_and(|t| t.kind() == "repeated_parameter_type"),
        _ => false,
    }
}

pub(super) fn label_name(p: Node<'_>, src: &[u8]) -> Option<String> {
    let found = p.child_by_field_name("name")?;
    let name = text(found, src).trim().trim_start_matches('$').to_string();
    (!name.is_empty()).then_some(name)
}

pub(super) fn label_type_node(language: Language, p: Node<'_>) -> Option<Node<'_>> {
    match language {
        Language::Go | Language::Php | Language::CSharp | Language::Java | Language::Scala => {
            p.child_by_field_name("type")
        }
        _ => None,
    }
}

/// UTF-16 offsets of a parameter label inside the signature label.
pub(super) fn param_label(signature: &str, label: &Value) -> Option<String> {
    match label {
        Value::String(s) => Some(s.clone()),
        Value::Array(range) => {
            let start = usize::try_from(range.first()?.as_u64()?).ok()?;
            let end = usize::try_from(range.get(1)?.as_u64()?).ok()?;
            let mut out = String::new();
            let mut unit = 0usize;
            for c in signature.chars() {
                if unit >= start && unit < end {
                    out.push(c);
                }
                unit += c.len_utf16();
            }
            Some(out)
        }
        _ => None,
    }
}

/// The parameter an argument binds to among parsed labels.
pub(super) fn select<'p>(params: &'p [LabelParam], r: &ParamRef) -> Option<&'p LabelParam> {
    if let Some(k) = &r.keyword {
        if let Some(p) = params.iter().find(|p| p.name.as_deref() == Some(k)) {
            return Some(p);
        }
    }
    let i = usize::try_from(r.index?).ok()?;
    params
        .get(i)
        .filter(|p| !p.variadic || i + 1 >= params.len())
        .or_else(|| params.iter().find(|p| p.variadic))
        .or_else(|| params.get(i))
}

/// LSP position of a byte offset.
pub(super) fn position(source: &[u8], byte: u32) -> Value {
    let (line, character) = LineIndex::new(source).utf16_of_byte(source, byte);
    json!({"line": line, "character": character})
}

/// Whether the argument is an anonymous function (`FileFacts::anonymous` passed as this
/// argument: [`anonymous_arguments`]), not a name.
pub(super) fn is_anonymous_argument(q: &FnTypeQuery<'_>) -> bool {
    q.facts.anonymous.iter().any(|a| {
        matches!(a.consumer, Consumer::Argument { .. })
            && q.facts
                .declarations
                .get(a.decl as usize)
                .is_some_and(|d| d.span.bytes == q.arg.arg_span)
    })
}

/// Label route: `signatureHelp` at the argument (its END for a name - inside an identifier
/// some servers answer the argument's own signature -, its START for an anonymous function,
/// whose end may sit right after a call of its body). When the server answers without a
/// parameter for the argument (or does not support the request), the callee's `hover`
/// declaration is parsed instead where the language has a hover form ([`hover_declaration`]).
pub(super) fn label_route(q: &FnTypeQuery<'_>, session: &mut dyn FnTypeSession) -> Option<SemCallbackParam> {
    let r = ParamRef::of(q.arg)?;
    let uri = session.uri_of(q.path).ok()?;
    let at = if is_anonymous_argument(q) {
        q.arg.arg_span.start
    } else {
        q.arg.arg_span.end
    };
    let help = session.request(
        "textDocument/signatureHelp",
        json!({
            "textDocument": {"uri": uri},
            "position": position(q.source, at),
            "context": {"triggerKind": 1, "isRetrigger": false},
        }),
    );
    let tables = tables();
    let mut best = help
        .as_ref()
        .ok()
        .and_then(|help| signature_param(q, help, &r, tables));
    let usable = best.as_ref().is_some_and(|b| b.verdict != FnTypeVerdict::Unknown);
    // A real answer (or a server error) - never the recording pass of the pipelining, whose
    // requests are answered with `Capability` - may fall back to the hover declaration.
    let answered = matches!(help, Ok(_) | Err(SemanticError::Rpc { .. }));
    if !usable && answered && hover_declaration(q.language).is_some() {
        if let Some(found) = hover_param(q, &uri, &r, session, tables) {
            best = better(best, found);
        }
    }
    if help.is_err() && best.is_none() {
        return None;
    }
    Some(answer(q, FnTypeRoute::Label, best))
}

/// The best parameter for the argument over every signature of a `signatureHelp` answer.
pub(super) fn signature_param(
    q: &FnTypeQuery<'_>,
    help: &Value,
    r: &ParamRef,
    tables: &Tables,
) -> Option<Found> {
    let mut best: Option<Found> = None;
    for signature in help.get("signatures").and_then(Value::as_array).into_iter().flatten() {
        let Some(label) = signature.get("label").and_then(Value::as_str) else {
            continue;
        };
        let params: Vec<LabelParam> = signature
            .get("parameters")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|p| param_label(label, p.get("label")?))
            .filter_map(|l| parse_label_param(q.language, &l, tables))
            .collect();
        let Some(p) = select(&params, r) else {
            continue;
        };
        best = better(
            best,
            Found {
                verdict: p.class.verdict(),
                param_type: p.type_text.clone(),
                param_name: p.name.clone(),
                symbol: None,
            },
        );
    }
    best
}

/// The argument's parameter in the callee's `hover` declaration (label-route fallback).
pub(super) fn hover_param(
    q: &FnTypeQuery<'_>,
    uri: &str,
    r: &ParamRef,
    session: &mut dyn FnTypeSession,
    tables: &Tables,
) -> Option<Found> {
    let v = session
        .request(
            "textDocument/hover",
            json!({"textDocument": {"uri": uri}, "position": position(q.source, callee_point(q.call))}),
        )
        .ok()?;
    let code = hover_code(&v)?;
    let params = hover_params(q.language, &code, tables);
    let p = select(&params, r)?;
    Some(Found {
        verdict: p.class.verdict(),
        param_type: p.type_text.clone(),
        param_name: p.name.clone(),
        symbol: None,
    })
}

/// Anonymous functions passed as call arguments (`FileFacts::anonymous` with an
/// `Argument` consumer at an exact position or a keyword) as callback arguments for the
/// function-type rule: the argument span is the anonymous function's declaration span (as
/// in library behaviour requests), the name is empty, the argument text is the function's
/// source text (the Java language rule reads it). Sorted by argument span.
pub fn anonymous_arguments(facts: &FileFacts, source: &[u8]) -> Vec<CallbackArg> {
    let mut out: Vec<CallbackArg> = Vec::new();
    for anon in &facts.anonymous {
        let Consumer::Argument { call, slot } = &anon.consumer else {
            continue;
        };
        let (index, keyword) = match slot {
            ArgSlot::Positional { index, exact: true } => (Some(*index), None),
            ArgSlot::Keyword(k) if !k.is_empty() => (None, Some(k.clone())),
            _ => continue,
        };
        let (Some(c), Some(decl)) =
            (facts.calls.get(*call as usize), facts.declarations.get(anon.decl as usize))
        else {
            continue;
        };
        let span = decl.span.bytes;
        let argument = source
            .get(span.start as usize..span.end as usize)
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default();
        out.push(CallbackArg {
            call_callee_span: c.callee_span,
            callee: c.callee.clone(),
            arg_span: span,
            argument,
            name: String::new(),
            owner: anon.created_in,
            index,
            keyword,
        });
    }
    out.sort_by_key(|a| (a.arg_span.start, a.arg_span.end));
    out.dedup_by_key(|a| (a.arg_span.start, a.arg_span.end));
    out
}
