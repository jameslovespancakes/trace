//! The hover route (Haskell): the callee's type signature from `hover`; a function-typed
//! parameter, or a type variable constrained by an action class, is run.

use serde_json::{json, Value};
use std::collections::HashSet;
use trace_core::semantics::SemCallbackParam;
use trace_core::Language;
use trace_library::table::Tables;
use tree_sitter::Node;

use super::*;

/// Type-class constraints whose variable is an action (`m a` runs when sequenced).
pub(super) const ACTION_CLASSES: [&str; 10] = [
    "Monad",
    "Applicative",
    "Functor",
    "MonadIO",
    "MonadUnliftIO",
    "MonadFail",
    "Alternative",
    "MonadPlus",
    "MonadThrow",
    "MonadMask",
];

pub(super) fn hover_route(q: &FnTypeQuery<'_>, session: &mut dyn FnTypeSession) -> Option<SemCallbackParam> {
    let r = ParamRef::of(q.arg)?;
    let index = usize::try_from(r.index?).ok()?;
    let uri = session.uri_of(q.path).ok()?;
    let v = session
        .request(
            "textDocument/hover",
            json!({"textDocument": {"uri": uri}, "position": position(q.source, callee_point(q.call))}),
        )
        .ok()?;
    let found = hover_code(&v)
        .and_then(|code| {
            let (_, ty) = code.split_once("::")?;
            haskell_signature(&format!("_x :: {}", ty.trim()), index, tables())
        })
        .map(|(class, param_type)| Found {
            verdict: class.verdict(),
            param_type,
            param_name: None,
            symbol: None,
        });
    Some(answer(q, FnTypeRoute::Declaration, found))
}

/// The code of a hover answer (first fenced block, else the plain text).
pub(super) fn hover_code(v: &Value) -> Option<String> {
    let contents = v.get("contents")?;
    let raw = match contents {
        Value::String(s) => s.clone(),
        Value::Object(o) => o.get("value")?.as_str()?.to_string(),
        Value::Array(a) => a
            .iter()
            .filter_map(|x| {
                x.as_str()
                    .map(str::to_string)
                    .or_else(|| x.get("value")?.as_str().map(str::to_string))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let mut lines = raw.lines();
    if raw.contains("```") {
        let mut code = Vec::new();
        let mut inside = false;
        for line in lines.by_ref() {
            if line.trim_start().starts_with("```") {
                if inside {
                    break;
                }
                inside = true;
                continue;
            }
            if inside {
                code.push(line);
            }
        }
        return (!code.is_empty()).then(|| code.join("\n"));
    }
    Some(raw)
}

/// Parameter `index` of a Haskell signature `_x :: type`: an arrow type, `IO a`, `ST s a`,
/// `STM a` or `m a` with an action class constraint on `m` is run (function type); a bare
/// type variable is a top type.
pub(super) fn haskell_signature(source: &str, index: usize, tables: &Tables) -> Option<(Class, String)> {
    let tree = trace_syntax::parse_tree(Language::Haskell, source.as_bytes()).ok()?;
    let src = source.as_bytes();
    let signature = all_of_kind(tree.root_node(), &["signature"], 1).into_iter().next()?;
    let mut t = signature.child_by_field_name("type")?;
    let mut action_vars: HashSet<String> = HashSet::new();
    loop {
        match t.kind() {
            "forall" => t = t.child_by_field_name("type")?,
            "context" => {
                for field in ["context", "constraint"] {
                    if let Some(c) = t.child_by_field_name(field) {
                        for apply in all_of_kind(c, &["apply"], 64) {
                            let head = apply.child_by_field_name("constructor").map(|h| text(h, src));
                            let arg = apply.child_by_field_name("argument").map(|a| text(a, src));
                            if let (Some(head), Some(arg)) = (head, arg) {
                                if ACTION_CLASSES.contains(&last_segment(head)) {
                                    action_vars.insert(arg.to_string());
                                }
                            }
                        }
                    }
                }
                t = t.child_by_field_name("type")?;
            }
            "parens" => t = t.child_by_field_name("type").or_else(|| first_named(t))?,
            _ => break,
        }
    }
    let mut params = Vec::new();
    while t.kind() == "function" {
        params.push(t.child_by_field_name("parameter")?);
        t = t.child_by_field_name("result")?;
    }
    let param = *params.get(index)?;
    Some((haskell_class(param, src, &action_vars, tables), text(param, src).to_string()))
}

pub(super) fn haskell_class(
    n: Node<'_>,
    src: &[u8],
    action_vars: &HashSet<String>,
    tables: &Tables,
) -> Class {
    match n.kind() {
        "parens" => n
            .child_by_field_name("type")
            .or_else(|| first_named(n))
            .map_or(Class::Unknown, |t| haskell_class(t, src, action_vars, tables)),
        "function" => Class::Function,
        "apply" => {
            let mut head = n;
            while head.kind() == "apply" {
                let Some(c) = head.child_by_field_name("constructor") else {
                    return Class::Unknown;
                };
                head = c;
            }
            let name = text(head, src);
            if head.kind() == "variable" {
                return if action_vars.contains(name) {
                    Class::Function
                } else {
                    Class::Not
                };
            }
            match name_class(Language::Haskell, name, tables) {
                Some(Class::Function) => Class::Function,
                _ => Class::Not,
            }
        }
        "variable" => {
            if action_vars.contains(text(n, src)) {
                Class::Function
            } else {
                Class::Top
            }
        }
        "forall" | "context" => n
            .child_by_field_name("type")
            .map_or(Class::Unknown, |t| haskell_class(t, src, action_vars, tables)),
        _ => Class::Not,
    }
}
