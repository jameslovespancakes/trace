//! GraphQL: client documents (tokenizer and top-level field reader) and resolver maps.

use trace_core::facts::BoundaryRole;
use trace_core::model::{BridgeKind, ByteSpan};
use tree_sitter::Node;

use super::{
    conventions::last_segment, literals::is_content_kind, literals::is_hole_kind, literals::is_string_kind,
    literals::strip_delimiters, Bind, CallView, Cx, GRAPHQL_ROOT_TYPES,
};
use crate::node::{named_children, span};

impl<'a> Cx<'a> {
    pub(super) fn graphql_call(&self, cv: &CallView<'_>) {
        // Client documents under a `graphql_document_tag` row: gql`...`, gql("...").
        if cv.path.len() == 1 && self.names("graphql_document_tag", BridgeKind::Graphql, &cv.member) {
            if let Some(doc) = cv.positional(0) {
                let doc = self.unwrap(doc);
                if is_string_kind(doc.kind()) {
                    self.graphql_document(cv, doc);
                }
            }
            return;
        }
        // `@query.field("name")` on a GraphQL type object (`graphql_field_member` rows).
        let Some(row) = self
            .rows_of("graphql_field_member", BridgeKind::Graphql)
            .find(|r| r.symbol.as_deref().is_some_and(|s| last_segment(s) == cv.member))
        else {
            return;
        };
        if let (Some(Some(decl)), Some((_, Bind::GraphqlType { type_name }))) =
            (self.decorated_decl(cv.node), self.bind_of_receiver(cv))
        {
            if let Some(field) = cv.positional(0).and_then(|n| self.eval(n, 0).plain()) {
                let convention = row.symbol.clone().unwrap_or_default();
                self.push(
                    BridgeKind::Graphql,
                    BoundaryRole::Provides,
                    format!("{type_name}.{field}"),
                    None,
                    Some(decl),
                    span(cv.node),
                    vec![("convention".into(), convention)],
                );
            }
        }
    }

    fn graphql_document(&self, cv: &CallView<'_>, doc: Node<'_>) {
        // Blank everything but string content so offsets map 1:1 to the file.
        let start = doc.start_byte();
        let end = doc.end_byte().min(self.src.len());
        let mut buf = vec![b' '; end.saturating_sub(start)];
        let mut stack = vec![doc];
        while let Some(n) = stack.pop() {
            if is_content_kind(n.kind()) {
                let (a, b) = (n.start_byte().max(start), n.end_byte().min(end));
                if a < b {
                    buf[a - start..b - start].copy_from_slice(&self.src[a..b]);
                }
                continue;
            }
            if is_hole_kind(n.kind()) {
                continue;
            }
            let kids = named_children(n);
            if kids.is_empty() && n.id() == doc.id() {
                // Grammar without content children: take the text inside the delimiters.
                let raw = &self.src[start..end];
                let inner = strip_delimiters(std::str::from_utf8(raw).unwrap_or(""));
                if let Some(off) = std::str::from_utf8(raw).ok().and_then(|r| r.find(inner)) {
                    buf[off..off + inner.len()].copy_from_slice(inner.as_bytes());
                }
            }
            stack.extend(kids);
        }
        for (root, field, offset) in graphql_top_fields(&buf) {
            let at = (start + offset) as u32;
            self.push(
                BridgeKind::Graphql,
                BoundaryRole::Uses,
                format!("{root}.{field}"),
                cv.owner,
                None,
                ByteSpan::new(at, at + field.len() as u32),
                vec![("operation".into(), root.to_ascii_lowercase())],
            );
        }
    }

    pub(super) fn js_resolver_pair(&self, pair: Node<'_>) {
        let Some(key) = pair.child_by_field_name("key") else { return };
        let root = self.key_text(key);
        let Some(row) = self.rows_of("graphql_resolver_map", BridgeKind::Graphql).next() else {
            return;
        };
        if !GRAPHQL_ROOT_TYPES.contains(&root.as_str()) {
            return;
        }
        let convention = row.symbol.clone().unwrap_or_default();
        let Some(value) = pair.child_by_field_name("value") else { return };
        let value = self.unwrap(value);
        if value.kind() != "object" {
            return;
        }
        for entry in named_children(value) {
            let (k, v) = match entry.kind() {
                "pair" => (entry.child_by_field_name("key"), entry.child_by_field_name("value")),
                "method_definition" => (entry.child_by_field_name("name"), Some(entry)),
                "shorthand_property_identifier" => (Some(entry), Some(entry)),
                _ => (None, None),
            };
            let (Some(k), Some(v)) = (k, v) else { continue };
            let field = self.key_text(k);
            let mut detail = vec![("convention".to_string(), convention.clone())];
            let decl = if entry.kind() == "method_definition" {
                self.decl_of_node(entry)
            } else {
                self.handler_info(v, &mut detail)
            };
            self.push(
                BridgeKind::Graphql,
                BoundaryRole::Provides,
                format!("{root}.{field}"),
                self.owner_at(entry.start_byte() as u32),
                decl,
                span(entry),
                detail,
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// GraphQL executable documents (client side): tokenizer + top-level field reader
// ---------------------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum GTok {
    Name(String),
    Punct(u8),
    Spread,
    Str,
}

fn graphql_tokens(doc: &[u8]) -> Vec<(GTok, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < doc.len() {
        let c = doc[i];
        match c {
            b' ' | b'\t' | b'\n' | b'\r' | b',' | 0xEF | 0xBB | 0xBF => i += 1,
            b'#' => {
                while i < doc.len() && doc[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' => {
                let start = i;
                if doc[i..].starts_with(b"\"\"\"") {
                    i += 3;
                    while i < doc.len() && !doc[i..].starts_with(b"\"\"\"") {
                        i += 1;
                    }
                    i = (i + 3).min(doc.len());
                } else {
                    i += 1;
                    while i < doc.len() && doc[i] != b'"' && doc[i] != b'\n' {
                        if doc[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                    i = (i + 1).min(doc.len());
                }
                out.push((GTok::Str, start));
            }
            b'.' if doc[i..].starts_with(b"...") => {
                out.push((GTok::Spread, i));
                i += 3;
            }
            b'_' | b'a'..=b'z' | b'A'..=b'Z' => {
                let start = i;
                while i < doc.len() && (doc[i] == b'_' || doc[i].is_ascii_alphanumeric()) {
                    i += 1;
                }
                out.push((GTok::Name(String::from_utf8_lossy(&doc[start..i]).into_owned()), start));
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                i += 1;
                while i < doc.len()
                    && (doc[i].is_ascii_alphanumeric() || doc[i] == b'.' || doc[i] == b'-' || doc[i] == b'+')
                {
                    i += 1;
                }
                out.push((GTok::Str, start));
            }
            other => {
                out.push((GTok::Punct(other), i));
                i += 1;
            }
        }
    }
    out
}

/// Skip a balanced `open ... close` group starting at `i` (which must be `open`).
fn skip_group(toks: &[(GTok, usize)], mut i: usize, open: u8, close: u8) -> usize {
    let mut depth = 0i32;
    while i < toks.len() {
        match toks[i].0 {
            GTok::Punct(c) if c == open => depth += 1,
            GTok::Punct(c) if c == close => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    i
}

/// Top-level selected fields of every operation: (root type, field name, byte offset).
fn graphql_top_fields(doc: &[u8]) -> Vec<(String, String, usize)> {
    let toks = graphql_tokens(doc);
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let root = match &toks[i].0 {
            GTok::Name(n) if n == "query" => "Query",
            GTok::Name(n) if n == "mutation" => "Mutation",
            GTok::Name(n) if n == "subscription" => "Subscription",
            GTok::Name(n) if n == "fragment" => {
                // fragment F on T { ... }
                while i < toks.len() && toks[i].0 != GTok::Punct(b'{') {
                    i += 1;
                }
                i = skip_group(&toks, i, b'{', b'}');
                continue;
            }
            GTok::Punct(b'{') => "Query",
            _ => {
                i += 1;
                continue;
            }
        };
        // Header: name, variables, directives.
        if toks[i].0 != GTok::Punct(b'{') {
            i += 1;
            while i < toks.len() && toks[i].0 != GTok::Punct(b'{') {
                if toks[i].0 == GTok::Punct(b'(') {
                    i = skip_group(&toks, i, b'(', b')');
                } else {
                    i += 1;
                }
            }
        }
        if i >= toks.len() {
            break;
        }
        // Selection set at depth 1.
        i += 1;
        while i < toks.len() && toks[i].0 != GTok::Punct(b'}') {
            match &toks[i].0 {
                GTok::Spread => {
                    // `...Fragment [@dir]` or inline `... on T { }` / `... @include(..) { }`.
                    i += 1;
                    let named_spread = matches!(toks.get(i), Some((GTok::Name(n), _)) if n != "on");
                    if named_spread {
                        i += 1;
                        while toks.get(i).is_some_and(|t| t.0 == GTok::Punct(b'@')) {
                            i += 2;
                            if toks.get(i).is_some_and(|t| t.0 == GTok::Punct(b'(')) {
                                i = skip_group(&toks, i, b'(', b')');
                            }
                        }
                    } else {
                        while i < toks.len()
                            && toks[i].0 != GTok::Punct(b'{')
                            && toks[i].0 != GTok::Punct(b'}')
                        {
                            if toks[i].0 == GTok::Punct(b'(') {
                                i = skip_group(&toks, i, b'(', b')');
                            } else {
                                i += 1;
                            }
                        }
                        if toks.get(i).is_some_and(|t| t.0 == GTok::Punct(b'{')) {
                            i = skip_group(&toks, i, b'{', b'}');
                        }
                    }
                }
                GTok::Name(first) => {
                    let (mut field, mut at) = (first.clone(), toks[i].1);
                    i += 1;
                    if toks.get(i).is_some_and(|t| t.0 == GTok::Punct(b':')) {
                        // alias: field
                        if let Some((GTok::Name(real), pos)) = toks.get(i + 1) {
                            field = real.clone();
                            at = *pos;
                            i += 2;
                        }
                    }
                    if toks.get(i).is_some_and(|t| t.0 == GTok::Punct(b'(')) {
                        i = skip_group(&toks, i, b'(', b')');
                    }
                    while toks.get(i).is_some_and(|t| t.0 == GTok::Punct(b'@')) {
                        i += 2;
                        if toks.get(i).is_some_and(|t| t.0 == GTok::Punct(b'(')) {
                            i = skip_group(&toks, i, b'(', b')');
                        }
                    }
                    if toks.get(i).is_some_and(|t| t.0 == GTok::Punct(b'{')) {
                        i = skip_group(&toks, i, b'{', b'}');
                    }
                    if !field.starts_with("__") {
                        out.push((root.to_string(), field, at));
                    }
                }
                _ => i += 1,
            }
        }
        i += 1;
    }
    out
}
