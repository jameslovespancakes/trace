//! A small structured reader for the block-YAML subset used by OpenAPI documents.
//!
//! No YAML crate is available in the local cargo registry cache (checked for this round;
//! see `artifacts/trace-next/notes/P5.md`), so OpenAPI YAML is read here: an indentation-
//! driven recursive parser (not pattern matching) for block mappings and sequences, plain /
//! single- / double-quoted scalars, block scalars (`|`, `>`), single- and multi-line flow
//! collections (`[a, b]`, `{a: b}`), comments, anchors (`&a`, dropped), aliases (`*a`,
//! read as null) and tags (`!!str`, dropped). Only one document is read. Scalars are
//! strings except `null`/`~` and `true`/`false`; numbers stay strings (keys such as `200`
//! and paths are what the bridge needs). Anything unsupported yields `None` for the whole
//! document (reported as `contract_unparsed`).

use serde_json::{Map, Value};

struct Line {
    indent: usize,
    text: String,
}

/// Parse a YAML document into JSON values.
pub fn parse(src: &str) -> Option<Value> {
    let mut lines = Vec::new();
    for raw in src.lines() {
        let raw = raw.trim_end_matches('\r');
        let stripped = strip_comment(raw);
        let trimmed = stripped.trim_end();
        if trimmed.trim().is_empty() {
            continue;
        }
        let t = trimmed.trim_start_matches(' ');
        if t.starts_with('\t') {
            return None; // tabs are not valid YAML indentation
        }
        if t == "---" || t.starts_with("--- ") || t.starts_with("%YAML") || t.starts_with("%TAG") {
            if !lines.is_empty() {
                break; // only the first document is read
            }
            continue;
        }
        if t == "..." {
            break;
        }
        lines.push(Line {
            indent: trimmed.len() - t.len(),
            text: t.to_string(),
        });
    }
    if lines.is_empty() {
        return Some(Value::Null);
    }
    let mut i = 0;
    let indent = lines[0].indent;
    let v = block(&mut lines, &mut i, indent, 0)?;
    Some(v)
}

/// Remove a trailing comment (`#` at line start or after whitespace, outside quotes).
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate() {
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'\'' | b'"' => {
                    // Quotes only open a scalar at a token start.
                    if i == 0 || matches!(bytes[i - 1], b' ' | b':' | b'[' | b'{' | b',' | b'-') {
                        quote = Some(b);
                    }
                }
                b'#' if i == 0 || bytes[i - 1] == b' ' => return &line[..i],
                _ => {}
            },
        }
    }
    line
}

fn block(lines: &mut [Line], i: &mut usize, indent: usize, depth: u32) -> Option<Value> {
    if depth > 128 || *i >= lines.len() {
        return Some(Value::Null);
    }
    if is_seq_item(&lines[*i].text) {
        sequence(lines, i, indent, depth)
    } else {
        mapping(lines, i, indent, depth)
    }
}

fn is_seq_item(text: &str) -> bool {
    text == "-" || text.starts_with("- ")
}

fn sequence(lines: &mut [Line], i: &mut usize, indent: usize, depth: u32) -> Option<Value> {
    let mut items = Vec::new();
    while *i < lines.len() && lines[*i].indent == indent && is_seq_item(&lines[*i].text) {
        let rest = lines[*i].text[1..].trim_start().to_string();
        if rest.is_empty() {
            *i += 1;
            if *i < lines.len() && lines[*i].indent > indent {
                let inner = lines[*i].indent;
                items.push(block(lines, i, inner, depth + 1)?);
            } else {
                items.push(Value::Null);
            }
            continue;
        }
        // `- key: value` starts a mapping whose keys align with `key`.
        let offset = lines[*i].text.len() - rest.len();
        if split_key(&rest).is_some() || is_seq_item(&rest) {
            lines[*i].indent = indent + offset;
            lines[*i].text = rest;
            let inner = lines[*i].indent;
            items.push(block(lines, i, inner, depth + 1)?);
            continue;
        }
        *i += 1;
        items.push(value_text(lines, i, indent, &rest, depth)?);
    }
    Some(Value::Array(items))
}

fn mapping(lines: &mut [Line], i: &mut usize, indent: usize, depth: u32) -> Option<Value> {
    let mut map = Map::new();
    while *i < lines.len() && lines[*i].indent == indent {
        if is_seq_item(&lines[*i].text) {
            break;
        }
        let text = lines[*i].text.clone();
        let Some((key, rest)) = split_key(&text) else {
            // A plain multi-line scalar continuation or unsupported syntax.
            if map.is_empty() {
                *i += 1;
                return Some(Value::String(text));
            }
            return None;
        };
        *i += 1;
        let value = if rest.is_empty() {
            if *i < lines.len() && lines[*i].indent > indent {
                let inner = lines[*i].indent;
                block(lines, i, inner, depth + 1)?
            } else if *i < lines.len() && lines[*i].indent == indent && is_seq_item(&lines[*i].text) {
                // Sequences may sit at the key's indentation.
                sequence(lines, i, indent, depth + 1)?
            } else {
                Value::Null
            }
        } else {
            value_text(lines, i, indent, &rest, depth)?
        };
        map.insert(key, value);
    }
    Some(Value::Object(map))
}

/// Value written after `key:` / `- ` on the same line.
fn value_text(lines: &mut [Line], i: &mut usize, indent: usize, rest: &str, depth: u32) -> Option<Value> {
    let rest = strip_props(rest);
    if rest.is_empty() {
        if *i < lines.len() && lines[*i].indent > indent {
            let inner = lines[*i].indent;
            return block(lines, i, inner, depth + 1);
        }
        return Some(Value::Null);
    }
    if rest.starts_with('|') || rest.starts_with('>') {
        let folded = rest.starts_with('>');
        let mut parts = Vec::new();
        while *i < lines.len() && lines[*i].indent > indent {
            parts.push(lines[*i].text.clone());
            *i += 1;
        }
        return Some(Value::String(parts.join(if folded { " " } else { "\n" })));
    }
    if rest.starts_with('[') || rest.starts_with('{') {
        let mut text = rest.to_string();
        // Multi-line flow collections: append more-indented lines until balanced.
        while !balanced(&text) && *i < lines.len() && lines[*i].indent > indent {
            text.push(' ');
            text.push_str(&lines[*i].text);
            *i += 1;
        }
        let mut pos = 0;
        let v = flow(text.as_bytes(), &mut pos, 0)?;
        return Some(v);
    }
    if rest.starts_with('*') {
        return Some(Value::Null);
    }
    // Plain scalars may continue on more-indented lines.
    let mut text = scalar(rest);
    if !(rest.starts_with('"') || rest.starts_with('\'')) {
        while *i < lines.len() && lines[*i].indent > indent && split_key(&lines[*i].text).is_none() {
            if let Value::String(s) = &mut text {
                s.push(' ');
                s.push_str(&lines[*i].text);
            }
            *i += 1;
        }
    }
    Some(text)
}

/// Drop anchors and tags before a value.
fn strip_props(mut s: &str) -> &str {
    loop {
        let t = s.trim_start();
        if (t.starts_with('&') || t.starts_with('!')) && !t.starts_with("!=") {
            match t.find(' ') {
                Some(sp) => s = &t[sp + 1..],
                None => return "",
            }
        } else {
            return t;
        }
    }
}

fn balanced(s: &str) -> bool {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' => quote = Some(c),
                '[' | '{' => depth += 1,
                ']' | '}' => depth -= 1,
                _ => {}
            },
        }
    }
    depth <= 0
}

/// `key: rest` split at the first `: ` / trailing `:` outside quotes and brackets.
fn split_key(text: &str) -> Option<(String, String)> {
    let bytes = text.as_bytes();
    let first = *bytes.first()?;
    if first == b'[' || first == b'{' || first == b'?' {
        return None;
    }
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate() {
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => {
                if (b == b'\'' || b == b'"') && i == 0 {
                    quote = Some(b);
                } else if b == b':' && (i + 1 == bytes.len() || bytes[i + 1] == b' ') {
                    let key = match scalar(text[..i].trim()) {
                        Value::String(s) => s,
                        Value::Null => "null".into(),
                        Value::Bool(b) => b.to_string(),
                        other => other.to_string(),
                    };
                    return Some((key, text[i + 1..].trim().to_string()));
                }
            }
        }
    }
    None
}

fn scalar(s: &str) -> Value {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')) {
        return Value::String(inner.replace("''", "'"));
    }
    if let Some(inner) = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(o) => out.push(o),
                    None => {}
                }
            } else {
                out.push(c);
            }
        }
        return Value::String(out);
    }
    match s {
        "null" | "Null" | "NULL" | "~" | "" => Value::Null,
        "true" | "True" | "TRUE" => Value::Bool(true),
        "false" | "False" | "FALSE" => Value::Bool(false),
        _ => Value::String(s.to_string()),
    }
}

fn flow(b: &[u8], pos: &mut usize, depth: u32) -> Option<Value> {
    if depth > 64 {
        return None;
    }
    skip_ws(b, pos);
    match b.get(*pos)? {
        b'[' => {
            *pos += 1;
            let mut items = Vec::new();
            loop {
                skip_ws(b, pos);
                match b.get(*pos)? {
                    b']' => {
                        *pos += 1;
                        return Some(Value::Array(items));
                    }
                    b',' => *pos += 1,
                    _ => items.push(flow(b, pos, depth + 1)?),
                }
            }
        }
        b'{' => {
            *pos += 1;
            let mut map = Map::new();
            loop {
                skip_ws(b, pos);
                match b.get(*pos)? {
                    b'}' => {
                        *pos += 1;
                        return Some(Value::Object(map));
                    }
                    b',' => *pos += 1,
                    _ => {
                        let key = match flow_scalar(b, pos, true) {
                            Value::String(s) => s,
                            other => other.to_string(),
                        };
                        skip_ws(b, pos);
                        let value = if b.get(*pos) == Some(&b':') {
                            *pos += 1;
                            flow(b, pos, depth + 1)?
                        } else {
                            Value::Null
                        };
                        map.insert(key, value);
                    }
                }
            }
        }
        _ => Some(flow_scalar(b, pos, false)),
    }
}

fn flow_scalar(b: &[u8], pos: &mut usize, is_key: bool) -> Value {
    skip_ws(b, pos);
    let start = *pos;
    if let Some(&q) = b.get(*pos).filter(|c| **c == b'"' || **c == b'\'') {
        *pos += 1;
        while *pos < b.len() && b[*pos] != q {
            if q == b'"' && b[*pos] == b'\\' {
                *pos += 1;
            }
            *pos += 1;
        }
        *pos = (*pos + 1).min(b.len());
        return scalar(&String::from_utf8_lossy(&b[start..*pos]));
    }
    while *pos < b.len() {
        let c = b[*pos];
        if c == b',' || c == b']' || c == b'}' || (is_key && c == b':') {
            break;
        }
        if c == b':' && b.get(*pos + 1).is_some_and(|n| *n == b' ') {
            break;
        }
        *pos += 1;
    }
    scalar(&String::from_utf8_lossy(&b[start..*pos]))
}

fn skip_ws(b: &[u8], pos: &mut usize) {
    while *pos < b.len() && b[*pos].is_ascii_whitespace() {
        *pos += 1;
    }
}

#[cfg(test)]
#[path = "../../tests/unit/formats/yaml.rs"]
mod tests;
