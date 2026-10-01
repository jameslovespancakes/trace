//! Declared contracts read through `SourceStore` with structured parsers (SPEC §2 invariant
//! 8): Protocol Buffers service definitions and GraphQL schemas (small hand-written
//! tokenizers + recursive-descent readers) and OpenAPI / Swagger documents (serde_json, or
//! the block-YAML reader in `yaml.rs`).

use std::collections::HashMap;

use trace_core::model::FileId;
use trace_core::Language;

use crate::ctx::{segments, Ctx};

/// `service S { rpc M(...) returns (...); }` of one `.proto` file.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProtoService {
    pub package: String,
    pub name: String,
    pub rpcs: Vec<String>,
    pub file: FileId,
}

/// Root-operation fields declared by GraphQL schema files: `(root type, field) -> files`.
#[derive(Debug, Default)]
pub(crate) struct GraphqlSchema {
    pub fields: HashMap<(String, String), Vec<FileId>>,
    /// `schema { query: RootQuery }`: declared root type name -> `Query` / `Mutation` /
    /// `Subscription`.
    pub roots: HashMap<String, String>,
}

/// One OpenAPI operation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Operation {
    pub method: String,
    /// Path segments including the server base path (`{}` for parameters).
    pub full: Vec<String>,
    /// Path segments without the server base path.
    pub relative: Vec<String>,
    pub operation_id: Option<String>,
    pub file: FileId,
}

#[derive(Debug, Default)]
pub(crate) struct Contracts {
    pub services: Vec<ProtoService>,
    pub graphql: GraphqlSchema,
    pub operations: Vec<Operation>,
}

pub(crate) fn load(ctx: &mut Ctx<'_>) -> Contracts {
    let mut out = Contracts::default();
    let index = ctx.index;
    for (fi, rec) in index.files.iter().enumerate() {
        if !rec.language.is_contract() {
            continue;
        }
        let file = FileId(fi as u32);
        let bytes = match ctx.sources.file(file) {
            Ok(f) => f.bytes.clone(),
            Err(e) => {
                ctx.diag(
                    "contract_unparsed",
                    Some(rec.path.clone()),
                    format!("contract file could not be read: {e}"),
                );
                continue;
            }
        };
        let text = String::from_utf8_lossy(&bytes);
        let ok = match rec.language {
            Language::Proto => {
                let services = parse_proto(&text, file);
                let ok = !services.is_empty() || !text.contains("service");
                out.services.extend(services);
                ok
            }
            Language::GraphQl => parse_graphql_schema(&text, file, &mut out.graphql),
            Language::OpenApi => {
                let value = if rec.path.to_ascii_lowercase().ends_with(".json") {
                    serde_json::from_str::<serde_json::Value>(&text).ok()
                } else {
                    trace_core::formats::yaml::parse(&text)
                };
                match value {
                    Some(v) => {
                        let ops = openapi_operations(&v, file);
                        let ok = !ops.is_empty() || v.get("paths").is_some();
                        out.operations.extend(ops);
                        ok
                    }
                    None => false,
                }
            }
            _ => true,
        };
        if !ok {
            ctx.diag(
                "contract_unparsed",
                Some(rec.path.clone()),
                "contract file could not be parsed; its bridges are not detected".into(),
            );
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// Tokenizer shared by the proto and GraphQL readers
// ---------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Str,
    Num,
    Punct(u8),
    Spread,
}

/// Tokens with byte offsets. `slash_comments`: `//` and `/* */` (proto); otherwise `#`
/// line comments (GraphQL). Block strings `"""..."""` are one token.
fn tokenize(src: &[u8], slash_comments: bool) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        if c.is_ascii_whitespace() || (c == b',' && !slash_comments) || c == 0xEF || c == 0xBB || c == 0xBF {
            i += 1;
            continue;
        }
        if slash_comments && src[i..].starts_with(b"//") {
            while i < src.len() && src[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if slash_comments && src[i..].starts_with(b"/*") {
            i += 2;
            while i + 1 < src.len() && !src[i..].starts_with(b"*/") {
                i += 1;
            }
            i += 2;
            continue;
        }
        if !slash_comments && c == b'#' {
            while i < src.len() && src[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'"' || c == b'\'' {
            if src[i..].starts_with(b"\"\"\"") {
                i += 3;
                while i < src.len() && !src[i..].starts_with(b"\"\"\"") {
                    i += 1;
                }
                i = (i + 3).min(src.len());
            } else {
                i += 1;
                while i < src.len() && src[i] != c && src[i] != b'\n' {
                    if src[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i = (i + 1).min(src.len());
            }
            out.push(Tok::Str);
            continue;
        }
        if src[i..].starts_with(b"...") {
            out.push(Tok::Spread);
            i += 3;
            continue;
        }
        if c == b'_' || c.is_ascii_alphabetic() {
            let start = i;
            while i < src.len()
                && (src[i] == b'_' || src[i].is_ascii_alphanumeric() || (slash_comments && src[i] == b'.'))
            {
                i += 1;
            }
            out.push(Tok::Ident(String::from_utf8_lossy(&src[start..i]).into_owned()));
            continue;
        }
        if c.is_ascii_digit() || c == b'-' {
            i += 1;
            while i < src.len() && (src[i].is_ascii_alphanumeric() || src[i] == b'.') {
                i += 1;
            }
            out.push(Tok::Num);
            continue;
        }
        out.push(Tok::Punct(c));
        i += 1;
    }
    out
}

fn skip_balanced(toks: &[Tok], mut i: usize, open: u8, close: u8) -> usize {
    let mut depth = 0i32;
    while i < toks.len() {
        match toks[i] {
            Tok::Punct(c) if c == open => depth += 1,
            Tok::Punct(c) if c == close => {
                depth -= 1;
                if depth <= 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    i
}

fn skip_to_semicolon(toks: &[Tok], mut i: usize) -> usize {
    while i < toks.len() && toks[i] != Tok::Punct(b';') {
        if toks[i] == Tok::Punct(b'{') {
            return skip_balanced(toks, i, b'{', b'}');
        }
        i += 1;
    }
    i + 1
}

// ---------------------------------------------------------------------------------------
// Protocol Buffers
// ---------------------------------------------------------------------------------------

pub(crate) fn parse_proto(src: &str, file: FileId) -> Vec<ProtoService> {
    let toks = tokenize(src.as_bytes(), true);
    let mut package = String::new();
    let mut services = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            Tok::Ident(k) if k == "package" => {
                if let Some(Tok::Ident(p)) = toks.get(i + 1) {
                    package = p.clone();
                }
                i = skip_to_semicolon(&toks, i);
            }
            Tok::Ident(k) if matches!(k.as_str(), "syntax" | "edition" | "import" | "option") => {
                i = skip_to_semicolon(&toks, i);
            }
            Tok::Ident(k) if k == "service" => {
                let Some(Tok::Ident(name)) = toks.get(i + 1) else {
                    i += 1;
                    continue;
                };
                let name = name.clone();
                i += 2;
                if toks.get(i) != Some(&Tok::Punct(b'{')) {
                    continue;
                }
                i += 1;
                let mut rpcs = Vec::new();
                while i < toks.len() && toks[i] != Tok::Punct(b'}') {
                    match &toks[i] {
                        Tok::Ident(k) if k == "rpc" => {
                            if let Some(Tok::Ident(m)) = toks.get(i + 1) {
                                rpcs.push(m.clone());
                            }
                            i += 2;
                            // (req) returns (resp) followed by `;` or an options block.
                            while i < toks.len()
                                && toks[i] != Tok::Punct(b';')
                                && toks[i] != Tok::Punct(b'{')
                                && toks[i] != Tok::Punct(b'}')
                            {
                                if toks[i] == Tok::Punct(b'(') {
                                    i = skip_balanced(&toks, i, b'(', b')');
                                } else {
                                    i += 1;
                                }
                            }
                            match toks.get(i) {
                                Some(Tok::Punct(b'{')) => i = skip_balanced(&toks, i, b'{', b'}'),
                                Some(Tok::Punct(b';')) => i += 1,
                                _ => {}
                            }
                        }
                        Tok::Ident(k) if k == "option" => i = skip_to_semicolon(&toks, i),
                        Tok::Punct(b'{') => i = skip_balanced(&toks, i, b'{', b'}'),
                        _ => i += 1,
                    }
                }
                i += 1;
                services.push(ProtoService {
                    package: package.clone(),
                    name,
                    rpcs,
                    file,
                });
            }
            Tok::Ident(k) if matches!(k.as_str(), "message" | "enum" | "extend") => {
                while i < toks.len() && toks[i] != Tok::Punct(b'{') {
                    i += 1;
                }
                i = skip_balanced(&toks, i, b'{', b'}');
            }
            _ => i += 1,
        }
    }
    // `package` may follow services in odd files; re-assign the final package name.
    for s in &mut services {
        if s.package.is_empty() {
            s.package = package.clone();
        }
    }
    services
}

// ---------------------------------------------------------------------------------------
// GraphQL schema (SDL)
// ---------------------------------------------------------------------------------------

const DEFINITION_KEYWORDS: [&str; 10] = [
    "type",
    "extend",
    "schema",
    "input",
    "enum",
    "interface",
    "union",
    "scalar",
    "directive",
    "fragment",
];

/// Read `type`/`extend type` fields and the `schema { ... }` root mapping. Returns false when
/// the document has no recognizable definition.
pub(crate) fn parse_graphql_schema(src: &str, file: FileId, out: &mut GraphqlSchema) -> bool {
    let toks = tokenize(src.as_bytes(), false);
    let mut i = 0;
    let mut any = false;
    while i < toks.len() {
        let Tok::Ident(k) = &toks[i] else {
            if toks[i] == Tok::Punct(b'{') {
                i = skip_balanced(&toks, i, b'{', b'}');
            } else {
                i += 1;
            }
            continue;
        };
        match k.as_str() {
            "schema" => {
                any = true;
                i += 1;
                while i < toks.len() && toks[i] != Tok::Punct(b'{') {
                    i += 1;
                }
                i += 1;
                while i < toks.len() && toks[i] != Tok::Punct(b'}') {
                    if let (Some(Tok::Ident(op)), Some(Tok::Punct(b':')), Some(Tok::Ident(ty))) =
                        (toks.get(i), toks.get(i + 1), toks.get(i + 2))
                    {
                        let root = match op.as_str() {
                            "query" => "Query",
                            "mutation" => "Mutation",
                            "subscription" => "Subscription",
                            _ => "",
                        };
                        if !root.is_empty() {
                            out.roots.insert(ty.clone(), root.to_string());
                        }
                        i += 3;
                    } else {
                        i += 1;
                    }
                }
                i += 1;
            }
            "type" | "extend" => {
                any = true;
                let mut j = i + 1;
                if k == "extend" {
                    if toks.get(j) != Some(&Tok::Ident("type".into())) {
                        i = skip_definition(&toks, i + 1);
                        continue;
                    }
                    j += 1;
                }
                let Some(Tok::Ident(name)) = toks.get(j) else {
                    i = j;
                    continue;
                };
                let name = name.clone();
                j += 1;
                // implements A & B, directives
                while j < toks.len() && toks[j] != Tok::Punct(b'{') {
                    if matches!(&toks[j], Tok::Ident(w) if DEFINITION_KEYWORDS.contains(&w.as_str()) && w != "type")
                    {
                        break;
                    }
                    if toks[j] == Tok::Punct(b'(') {
                        j = skip_balanced(&toks, j, b'(', b')');
                    } else {
                        j += 1;
                    }
                }
                if toks.get(j) != Some(&Tok::Punct(b'{')) {
                    i = j;
                    continue;
                }
                j += 1;
                while j < toks.len() && toks[j] != Tok::Punct(b'}') {
                    match &toks[j] {
                        Tok::Str => j += 1,
                        Tok::Ident(field) => {
                            out.fields
                                .entry((name.clone(), field.clone()))
                                .or_default()
                                .push(file);
                            j += 1;
                            if toks.get(j) == Some(&Tok::Punct(b'(')) {
                                j = skip_balanced(&toks, j, b'(', b')');
                            }
                            if toks.get(j) == Some(&Tok::Punct(b':')) {
                                j += 1;
                                // Type: Name | [Type] followed by `!`s.
                                if toks.get(j) == Some(&Tok::Punct(b'[')) {
                                    j = skip_balanced(&toks, j, b'[', b']');
                                } else {
                                    j += 1;
                                }
                                while toks.get(j) == Some(&Tok::Punct(b'!')) {
                                    j += 1;
                                }
                            }
                            // Directives on the field.
                            while toks.get(j) == Some(&Tok::Punct(b'@')) {
                                j += 2;
                                if toks.get(j) == Some(&Tok::Punct(b'(')) {
                                    j = skip_balanced(&toks, j, b'(', b')');
                                }
                            }
                        }
                        _ => j += 1,
                    }
                }
                i = j + 1;
            }
            _ => {
                i = skip_definition(&toks, i + 1);
            }
        }
    }
    any
}

/// Skip to the next top-level definition keyword, skipping brace groups.
fn skip_definition(toks: &[Tok], mut i: usize) -> usize {
    while i < toks.len() {
        match &toks[i] {
            Tok::Punct(b'{') => {
                i = skip_balanced(toks, i, b'{', b'}');
                return i;
            }
            Tok::Ident(w) if DEFINITION_KEYWORDS.contains(&w.as_str()) => return i,
            _ => i += 1,
        }
    }
    i
}

// ---------------------------------------------------------------------------------------
// OpenAPI / Swagger
// ---------------------------------------------------------------------------------------

const OPENAPI_METHODS: [&str; 8] = ["get", "put", "post", "delete", "options", "head", "patch", "trace"];

/// Path part of a server URL / Swagger `basePath` (`http://h:8000/api/v1` -> `/api/v1`).
fn base_path(v: &serde_json::Value) -> String {
    if let Some(bp) = v.get("basePath").and_then(|b| b.as_str()) {
        return bp.to_string();
    }
    let Some(url) = v
        .get("servers")
        .and_then(|s| s.as_array())
        .and_then(|a| a.first())
        .and_then(|s| s.get("url"))
        .and_then(|u| u.as_str())
    else {
        return String::new();
    };
    let lower = url.to_ascii_lowercase();
    let rest = if let Some(p) = ["http://", "https://"].iter().find(|p| lower.starts_with(**p)) {
        match url[p.len()..].find('/') {
            Some(i) => &url[p.len() + i..],
            None => "",
        }
    } else {
        url
    };
    if rest.contains('{') {
        return String::new(); // templated server URLs: base unknown
    }
    rest.to_string()
}

pub(crate) fn openapi_operations(v: &serde_json::Value, file: FileId) -> Vec<Operation> {
    let base = segments(&base_path(v));
    let mut out = Vec::new();
    let Some(paths) = v.get("paths").and_then(|p| p.as_object()) else {
        return out;
    };
    for (path, item) in paths {
        let Some(item) = item.as_object() else { continue };
        let relative = segments(path);
        let mut full = base.clone();
        full.extend(relative.iter().cloned());
        for method in OPENAPI_METHODS {
            let Some(op) = item.get(method) else { continue };
            out.push(Operation {
                method: method.to_ascii_uppercase(),
                full: full.clone(),
                relative: relative.clone(),
                operation_id: op.get("operationId").and_then(|o| o.as_str()).map(str::to_string),
                file,
            });
        }
    }
    out
}

#[cfg(test)]
#[path = "../tests/unit/contracts.rs"]
mod tests;
