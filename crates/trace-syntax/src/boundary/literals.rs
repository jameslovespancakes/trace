//! Node and text helpers: string literal kinds and contents, escapes, name kinds, JNI
//! mangling and case conversion.

use tree_sitter::Node;

use super::JNI_PREFIX;
use crate::node::text;

pub(super) fn camel_case(snake: &str) -> String {
    let lead = snake.len() - snake.trim_start_matches('_').len();
    let mut out = String::from(&snake[..lead]);
    let mut upper = false;
    for (i, ch) in snake[lead..].chars().enumerate() {
        if ch == '_' {
            upper = i > 0;
            continue;
        }
        if upper {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// JNI short name of a native method (JNI spec "Resolving Native Method Names"):
/// `Java_` + mangled binary class name (`/` -> `_`) + `_` + mangled method name, with
/// `_` -> `_1`, `;` -> `_2`, `[` -> `_3` and other non-alphanumeric characters (incl. the
/// `$` of nested classes) -> `_0xxxx` (lower-case UTF-16 hex).
pub(crate) fn jni_mangle(package: &[&str], classes: &[&str], method: &str) -> String {
    fn mangle(s: &str, out: &mut String) {
        for ch in s.chars() {
            match ch {
                'a'..='z' | 'A'..='Z' | '0'..='9' => out.push(ch),
                '_' => out.push_str("_1"),
                ';' => out.push_str("_2"),
                '[' => out.push_str("_3"),
                other => {
                    let mut units = [0u16; 2];
                    for unit in other.encode_utf16(&mut units) {
                        out.push_str(&format!("_0{:04x}", unit));
                    }
                }
            }
        }
    }
    let mut out = String::from(JNI_PREFIX);
    for p in package {
        mangle(p, &mut out);
        out.push('_');
    }
    mangle(&classes.join("$"), &mut out);
    out.push('_');
    mangle(method, &mut out);
    out
}

// ---------------------------------------------------------------------------------------
// Node helpers
// ---------------------------------------------------------------------------------------

pub(super) fn is_string_kind(kind: &str) -> bool {
    matches!(
        kind,
        "string"
            | "string_literal"
            | "template_string"
            | "interpreted_string_literal"
            | "raw_string_literal"
            | "encapsed_string"
            | "verbatim_string_literal"
            | "concatenated_string"
            | "interpolated_string_expression"
            | "interpolated_verbatim_string_expression"
            | "line_string_literal"
            | "multi_line_string_literal"
            | "heredoc"
            | "nowdoc"
            | "raw_string"
            | "text_block"
    )
}

pub(super) fn is_content_kind(kind: &str) -> bool {
    kind.ends_with("content")
        || kind.ends_with("fragment")
        || kind == "escape_sequence"
        || kind == "string_value"
        || kind == "heredoc_body"
        || kind == "string_text"
}

pub(super) fn is_hole_kind(kind: &str) -> bool {
    matches!(
        kind,
        "interpolation"
            | "template_substitution"
            | "string_interpolation"
            | "interpolated_expression"
            | "interpolated_identifier"
            | "variable_name"
            | "simple_variable"
            | "dynamic_variable_name"
            | "member_access_expression"
            | "subscript_expression"
    )
}

pub(super) fn decode_escape(raw: &str) -> String {
    let mut chars = raw.chars();
    match (chars.next(), chars.next()) {
        (Some('\\'), Some(c)) if chars.as_str().is_empty() => match c {
            'n' => "\n".into(),
            't' => "\t".into(),
            other => other.to_string(),
        },
        _ => raw.to_string(),
    }
}

/// Literal text of a string token without prefixes and delimiters (used only when the
/// grammar exposes no content child).
pub(super) fn strip_delimiters(raw: &str) -> &str {
    let s = raw.trim_start_matches(|c: char| c.is_ascii_alphabetic() || c == '@' || c == '$');
    let s = s.trim_start_matches('#').trim_end_matches('#');
    for q in ["\"\"\"", "'''", "\"", "'", "`"] {
        if s.len() >= 2 * q.len() && s.starts_with(q) && s.ends_with(q) {
            return &s[q.len()..s.len() - q.len()];
        }
    }
    s
}

pub(super) fn is_name_kind(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "property_identifier"
            | "field_identifier"
            | "type_identifier"
            | "constant"
            | "name"
            | "this"
            | "self"
            | "super"
            | "package_identifier"
            | "simple_identifier"
            | "variable_name"
            | "namespace_identifier"
            | "shorthand_property_identifier"
            | "private_property_identifier"
            | "predefined_type"
            | "command_name"
            | "word"
    )
}

pub(super) fn member_fields_fallback(kind: &str) -> Option<(&'static str, &'static str)> {
    Some(match kind {
        "member_expression" => ("object", "property"),
        "attribute" => ("object", "attribute"),
        "selector_expression" => ("operand", "field"),
        "field_expression" => ("value", "field"),
        // Rust labels the qualifier `path`, Java `scope`.
        "scoped_identifier" | "scoped_type_identifier" => ("path|scope", "name"),
        "field_access" => ("object", "field"),
        "member_access_expression" => ("expression", "name"),
        "qualified_name" => ("qualifier", "name"),
        "qualified_identifier" => ("scope", "name"),
        "qualified_type" => ("package", "name"),
        "class_constant_access_expression" => ("#0", "#-1"),
        "scope_resolution" => ("scope", "name"),
        "navigation_expression" => ("#0", "#-1"),
        _ => return None,
    })
}

/// C declaration type name (`struct PyModuleDef` -> `PyModuleDef`).
pub(super) fn c_type_name(decl: Node<'_>, src: &[u8]) -> String {
    let Some(ty) = decl.child_by_field_name("type") else {
        return String::new();
    };
    if ty.kind() == "struct_specifier" {
        if let Some(n) = ty.child_by_field_name("name") {
            return text(n, src).trim().to_string();
        }
    }
    crate::node::type_name(ty, src)
}
