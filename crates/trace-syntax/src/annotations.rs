//! Type annotations in documentation comments (SPEC §6.3, general fixes rule 11):
//! `FileFacts::types` with source `comment`.
//!
//! Only comment nodes of the syntax tree are read (an annotation spelled inside a string
//! literal is never seen), and only with the small hand-written grammar below — never a
//! regular expression over source code.
//!
//! | flavour  | language   | tags                                                          |
//! |----------|------------|---------------------------------------------------------------|
//! | JSDoc    | JS, TS     | `@type {T}`, `@returns {T}` / `@return {T}`, `@param {T} name`, `@typedef {T} Name` |
//! | PHPDoc   | PHP        | `@var T [$name]`, `@return T`, `@param T $name`               |
//!
//! Attachment is by tree adjacency: consecutive comment siblings (at most one line break
//! between them) form a block; the block attaches to the next named sibling when at most one
//! line break separates them. A block followed by a blank line, or by a statement that is
//! neither a declaration nor a binding, attaches nothing. Targets: `@return` / `@param` a
//! callable declaration starting at the sibling (`Return { decl }`, `Var { Decl(decl), name
//! }`); `@type` / `@var` the binding of the sibling (`Var` / `Field`, like declared types) or
//! a declaration's own name. JSDoc `@typedef` names a type alias without a declaration and
//! records nothing. `self` / `static` / `$this` return types name the enclosing class (PHP)
//! or the method's container.
//!
//! Type expressions: alternatives split at top-level `|`, optional markers
//! (`?T`, `T?`, `T=`, `!T`) and `nil` / `null` / `undefined` / `void` / `mixed` dropped,
//! generic arguments removed (`Array<Foo>` -> `Array`), array
//! (`Foo[]`), function (`function(...)`), record (`{...}`) and literal types
//! skipped; a leading `\` of PHP names removed.

use trace_core::facts::{Scope, TypeFact, TypeSource, TypeSubject};
use trace_core::model::ByteSpan;
use trace_core::Language;
use tree_sitter::Node;

use crate::typefacts::Context;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flavour {
    JsDoc,
    PhpDoc,
}

fn flavour(language: Language) -> Option<Flavour> {
    match language {
        Language::JavaScript | Language::TypeScript | Language::Tsx => Some(Flavour::JsDoc),
        Language::Php => Some(Flavour::PhpDoc),
        _ => None,
    }
}

/// One parsed tag.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Tag {
    /// `@type` / `@var`: types, optional bound name (PHPDoc `$x`).
    Type {
        types: Vec<Spelled>,
        name: Option<String>,
    },
    Return {
        types: Vec<Spelled>,
    },
    Param {
        name: String,
        types: Vec<Spelled>,
    },
}

/// A type name with its absolute byte span.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Spelled {
    name: String,
    span: ByteSpan,
}

/// Comment-annotated types of a file.
pub(crate) fn extract<'t>(ctx: &Context<'_, 't>, root: Node<'t>) -> Vec<TypeFact> {
    let mut types = Vec::new();
    let Some(flavour) = flavour(ctx.language) else {
        return types;
    };
    for block in comment_blocks(ctx, root) {
        let mut tags: Vec<Tag> = Vec::new();
        for comment in &block {
            tags.extend(parse_comment(ctx.source, *comment, flavour));
        }
        if tags.is_empty() {
            continue;
        }
        let last = *block.last().expect("non-empty block");
        let target = next_target(ctx.source, last);
        attach(ctx, &tags, target, &mut types);
    }
    types
}

/// Blocks of adjacent comment siblings, in source order.
fn comment_blocks<'t>(ctx: &Context<'_, 't>, root: Node<'t>) -> Vec<Vec<Node<'t>>> {
    let mut comments: Vec<Node<'t>> = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if ctx.spec.is_comment(node.kind()) {
            comments.push(node);
            continue;
        }
        let mut cursor = node.walk();
        let children: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    comments.sort_by_key(|c| c.start_byte());
    let mut blocks: Vec<Vec<Node<'t>>> = Vec::new();
    for c in comments {
        let joined = blocks.last().and_then(|b| b.last()).is_some_and(|prev| {
            prev.next_named_sibling().is_some_and(|n| n.id() == c.id())
                && line_breaks(ctx.source, prev.end_byte(), c.start_byte()) <= 1
        });
        if joined {
            if let Some(b) = blocks.last_mut() {
                b.push(c);
            }
        } else {
            blocks.push(vec![c]);
        }
    }
    blocks
}

/// Number of line breaks between two byte offsets, or `usize::MAX` when anything but
/// whitespace separates them.
fn line_breaks(source: &[u8], start: usize, end: usize) -> usize {
    if start > end || end > source.len() {
        return usize::MAX;
    }
    let mut breaks = 0usize;
    for &b in &source[start..end] {
        match b {
            b'\n' => breaks += 1,
            b' ' | b'\t' | b'\r' | b'\x0c' => {}
            _ => return usize::MAX,
        }
    }
    breaks
}

/// The node a comment block documents: its next named sibling, when adjacent.
fn next_target<'t>(source: &[u8], last: Node<'t>) -> Option<Node<'t>> {
    let next = last.next_named_sibling()?;
    (line_breaks(source, last.end_byte(), next.start_byte()) <= 1).then_some(next)
}

/// Lines of a comment with their absolute start offsets, comment markers removed:
/// `(offset of the first content byte, content)`.
fn comment_lines(source: &[u8], comment: Node<'_>, flavour: Flavour) -> Vec<(usize, Vec<u8>)> {
    let start = comment.start_byte();
    let end = comment.end_byte().min(source.len());
    let bytes = &source[start..end];
    let doc_block = bytes.starts_with(b"/**");
    let mut out = Vec::new();
    let mut line_start = 0usize;
    for i in 0..=bytes.len() {
        if i < bytes.len() && bytes[i] != b'\n' {
            continue;
        }
        let line = &bytes[line_start..i];
        let mut j = 0usize;
        let skip_ws = |j: &mut usize| {
            while *j < line.len() && matches!(line[*j], b' ' | b'\t' | b'\r') {
                *j += 1;
            }
        };
        skip_ws(&mut j);
        let content = match flavour {
            Flavour::JsDoc | Flavour::PhpDoc => {
                if !doc_block {
                    None
                } else {
                    if line[j..].starts_with(b"/**") {
                        j += 3;
                    } else if line[j..].starts_with(b"*") && !line[j..].starts_with(b"*/") {
                        j += 1;
                    }
                    skip_ws(&mut j);
                    Some(j)
                }
            }
        };
        if let Some(j) = content {
            let mut text = line[j..].to_vec();
            // A one-line doc block ends with `*/`.
            if let Some(pos) = find(&text, b"*/") {
                text.truncate(pos);
            }
            while text.last().is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r')) {
                text.pop();
            }
            out.push((start + line_start + j, text));
        }
        line_start = i + 1;
    }
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A small cursor over one annotation line.
struct Cursor<'s> {
    s: &'s [u8],
    i: usize,
    /// Absolute offset of `s[0]`.
    base: usize,
}

impl<'s> Cursor<'s> {
    fn skip_ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t') {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    /// An identifier-like word (letters, digits, `_`, `.`, `$`, `\`).
    fn word(&mut self) -> Option<(String, usize, usize)> {
        let start = self.i;
        while self.i < self.s.len() {
            let b = self.s[self.i];
            if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'$' | b'\\') || b >= 0x80 {
                self.i += 1;
            } else {
                break;
            }
        }
        (self.i > start).then(|| {
            let w = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
            (w, start, self.i)
        })
    }

    /// A type expression region: balanced brackets; ends at top-level whitespace (unless a
    /// `|` continues the union), or at `:` / `#`.
    fn type_region(&mut self) -> Option<(usize, usize)> {
        self.skip_ws();
        let start = self.i;
        let mut depth = 0i32;
        while self.i < self.s.len() {
            let b = self.s[self.i];
            match b {
                b'(' | b'[' | b'{' | b'<' => depth += 1,
                b')' | b']' | b'}' | b'>' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                b' ' | b'\t' if depth == 0 => {
                    // `A | B`: continue across spaces around a union bar.
                    let mut k = self.i;
                    while k < self.s.len() && matches!(self.s[k], b' ' | b'\t') {
                        k += 1;
                    }
                    let prev_bar = self.i > start && self.s[self.i - 1] == b'|';
                    if k < self.s.len() && (self.s[k] == b'|' || prev_bar) {
                        self.i = k;
                        continue;
                    }
                    break;
                }
                b':' | b'#' if depth == 0 => break,
                _ => {}
            }
            self.i += 1;
        }
        (self.i > start).then_some((start, self.i))
    }

    /// The balanced content of a `{...}` / `[...]` group starting at the cursor.
    fn group(&mut self, open: u8, close: u8) -> Option<(usize, usize)> {
        self.skip_ws();
        if self.peek() != Some(open) {
            return None;
        }
        let inner = self.i + 1;
        let mut depth = 0i32;
        while self.i < self.s.len() {
            let b = self.s[self.i];
            if b == open {
                depth += 1;
            } else if b == close {
                depth -= 1;
                if depth == 0 {
                    let end = self.i;
                    self.i += 1;
                    return Some((inner, end));
                }
            }
            self.i += 1;
        }
        None
    }
}

/// Split a type expression `s[start..end]` into its nominal alternatives (module docs).
fn type_names(s: &[u8], start: usize, end: usize, base: usize) -> Vec<Spelled> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut alt_start = start;
    for i in start..=end {
        let at_end = i == end;
        let b = if at_end { b'|' } else { s[i] };
        match b {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b')' | b']' | b'}' | b'>' => depth -= 1,
            _ => {}
        }
        let split = depth == 0 && b == b'|';
        if split || at_end {
            if let Some(name) = alternative(s, alt_start, i, base) {
                out.push(name);
            }
            alt_start = i + 1;
        }
    }
    out
}

/// One alternative of a type expression -> its nominal type name.
fn alternative(s: &[u8], mut start: usize, mut end: usize, base: usize) -> Option<Spelled> {
    let trim = |b: u8| matches!(b, b' ' | b'\t');
    while start < end && (trim(s[start]) || matches!(s[start], b'?' | b'!')) {
        start += 1;
    }
    while start < end && s[start] == b'.' {
        start += 1; // `...T` rest parameters
    }
    while end > start && (trim(s[end - 1]) || matches!(s[end - 1], b'?' | b'!' | b'=')) {
        end -= 1;
    }
    if start >= end {
        return None;
    }
    // Parenthesized alternative: `(A|B)`.
    if s[start] == b'(' && s[end - 1] == b')' {
        return type_names(s, start + 1, end - 1, base).into_iter().next();
    }
    if matches!(s[start], b'{' | b'"' | b'\'' | b'`' | b'[') || s[start].is_ascii_digit() {
        return None;
    }
    // Array types: `Foo[]`.
    if end - start >= 2 && &s[end - 2..end] == b"[]" {
        return None;
    }
    let mut i = start;
    if s[i] == b'\\' {
        i += 1;
    }
    let name_start = i;
    while i < end {
        let b = s[i];
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'$' | b'\\') || b >= 0x80 {
            i += 1;
        } else {
            break;
        }
    }
    if i == name_start {
        return None;
    }
    // Function types: `function(...)`.
    if i < end && s[i] == b'(' {
        return None;
    }
    let name = String::from_utf8_lossy(&s[name_start..i]).into_owned();
    let name = name.trim_end_matches('.').to_string();
    const NULLS: &[&str] = &["nil", "null", "undefined", "void", "mixed", "never", "None"];
    if name.is_empty() || NULLS.contains(&name.as_str()) {
        return None;
    }
    Some(Spelled {
        name,
        span: ByteSpan::new((base + name_start) as u32, (base + i) as u32),
    })
}

/// Parse the tags of one comment node.
fn parse_comment(source: &[u8], comment: Node<'_>, flavour: Flavour) -> Vec<Tag> {
    let mut tags = Vec::new();
    for (offset, line) in comment_lines(source, comment, flavour) {
        let mut c = Cursor {
            s: &line,
            i: 0,
            base: offset,
        };
        if c.peek() != Some(b'@') {
            continue;
        }
        c.i += 1;
        let Some((tag, _, _)) = c.word() else {
            continue;
        };
        if let Some(t) = parse_tag(&mut c, &tag, flavour) {
            tags.push(t);
        }
    }
    tags
}

fn spelled(c: &Cursor<'_>, region: Option<(usize, usize)>) -> Vec<Spelled> {
    match region {
        Some((a, b)) => type_names(c.s, a, b, c.base),
        None => Vec::new(),
    }
}

fn parse_tag(c: &mut Cursor<'_>, tag: &str, flavour: Flavour) -> Option<Tag> {
    match flavour {
        Flavour::JsDoc => match tag {
            "type" => {
                let region = c.group(b'{', b'}');
                Some(Tag::Type {
                    types: spelled(c, region),
                    name: None,
                })
            }
            "return" | "returns" => {
                let region = c.group(b'{', b'}');
                Some(Tag::Return {
                    types: spelled(c, region),
                })
            }
            "param" | "arg" | "argument" => {
                let region = c.group(b'{', b'}');
                let types = spelled(c, region);
                c.skip_ws();
                // `[name=default]` optional parameters.
                let optional = c.peek() == Some(b'[');
                if optional {
                    c.i += 1;
                }
                let (name, _, _) = c.word()?;
                if name.contains('.') {
                    return None; // `opts.x`: a property of a parameter
                }
                Some(Tag::Param { name, types })
            }
            _ => None,
        },
        Flavour::PhpDoc => match tag {
            "var" => {
                let region = c.type_region();
                let types = spelled(c, region);
                c.skip_ws();
                let name = match c.peek() {
                    Some(b'$') => c.word().map(|w| w.0),
                    _ => None,
                };
                Some(Tag::Type { types, name })
            }
            "return" => {
                let region = c.type_region();
                Some(Tag::Return {
                    types: spelled(c, region),
                })
            }
            "param" => {
                c.skip_ws();
                if c.peek() == Some(b'$') {
                    return None; // untyped `@param $x`
                }
                let region = c.type_region();
                let types = spelled(c, region);
                c.skip_ws();
                if c.s[c.i..].starts_with(b"...") {
                    c.i += 3;
                }
                let (name, _, _) = c.word()?;
                name.starts_with('$').then_some(Tag::Param { name, types })
            }
            _ => None,
        },
    }
}

/// Replace self types (`self`, `static`, `$this`) by the enclosing class / container of
/// declaration `decl`.
fn resolve_self(ctx: &Context<'_, '_>, types: &[Spelled], decl: Option<u32>) -> Vec<Spelled> {
    types
        .iter()
        .filter_map(|t| {
            if !matches!(t.name.as_str(), "self" | "static" | "$this" | "this") {
                return Some(t.clone());
            }
            let d = decl?;
            let own = ctx.facts.declarations[d as usize].container.clone().or_else(|| {
                ctx.enclosing_type(d)
                    .map(|c| ctx.facts.declarations[c as usize].name.clone())
            })?;
            Some(Spelled {
                name: own,
                span: t.span,
            })
        })
        .collect()
}

fn push(types: &mut Vec<TypeFact>, subject: &TypeSubject, spelled: &[Spelled]) {
    for t in spelled {
        types.push(TypeFact {
            subject: subject.clone(),
            type_name: t.name.clone(),
            span: t.span,
            source: TypeSource::Comment,
        });
    }
}

/// The binding of declaration `d`'s own name: a variable of the enclosing callable or the
/// module, or a field of the enclosing type.
fn declaring_scope(ctx: &Context<'_, '_>, d: u32) -> Option<TypeSubject> {
    let decl = ctx.facts.declarations.get(d as usize)?;
    if decl.container.is_some() {
        return None;
    }
    let name = decl.name.clone();
    Some(match decl.parent {
        Some(p) if ctx.facts.declarations[p as usize].kind.is_type() => TypeSubject::Field { class: p, name },
        Some(p) => TypeSubject::Var {
            scope: Scope::Decl(p),
            name,
        },
        None => TypeSubject::Var {
            scope: Scope::Module,
            name,
        },
    })
}

fn attach<'t>(ctx: &Context<'_, 't>, tags: &[Tag], target: Option<Node<'t>>, types: &mut Vec<TypeFact>) {
    let Some(target) = target else {
        return;
    };
    let decl = ctx.decl_starting_at(target.start_byte() as u32);
    let callable = decl.filter(|&d| ctx.facts.declarations[d as usize].kind.is_callable());
    // Binding subjects of the target statement (computed lazily).
    let bindings =
        || -> Vec<TypeSubject> { ctx.binding_subjects(target).into_iter().map(|(s, _)| s).collect() };
    for tag in tags {
        match tag {
            Tag::Return { types: t } => {
                if let Some(d) = callable {
                    let t = resolve_self(ctx, t, Some(d));
                    push(types, &TypeSubject::Return { decl: d }, &t);
                }
            }
            Tag::Param { name, types: t } => {
                if let Some(d) = callable {
                    let subject = TypeSubject::Var {
                        scope: Scope::Decl(d),
                        name: name.clone(),
                    };
                    push(types, &subject, &resolve_self(ctx, t, Some(d)));
                }
            }
            Tag::Type { types: t, name } => {
                if let Some(d) = decl.filter(|_| name.is_none()) {
                    // A declaration's own binding (`/** @type {H} */ const h = () => ...`).
                    if let Some(subject) = declaring_scope(ctx, d) {
                        push(types, &subject, t);
                    }
                    continue;
                }
                for subject in bindings() {
                    let matches = match (name, &subject) {
                        (None, _) => true,
                        (Some(n), TypeSubject::Var { name: v, .. }) => v == n,
                        (Some(n), TypeSubject::Field { name: f, .. }) => {
                            f == n || f == n.trim_start_matches('$')
                        }
                        _ => false,
                    };
                    if matches {
                        push(types, &subject, t);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/annotations.rs"]
mod tests;
