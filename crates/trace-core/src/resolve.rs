//! Symbol selector resolution: the one selector grammar of every command (SPEC §9.4), used
//! identically by text and JSON output.
//!
//! ```text
//! selector   := uid | file_sel | name_sel
//! uid        := <path> ":" <qualified> ["#" <k>]          exact, tried first
//! file_sel   := <path> ":" (<qualified> | <line>)       <path> = an indexed file ("/" or "\")
//! name_sel   := <qualified>                              whole index
//! qualified  := segment (sep segment)*
//! sep        := "." | "::" | ":"                         all equivalent after normalization
//! ```
//!
//! Resolution order:
//! 1. exact uid (`src/auth.py:Session.login`, `src/a.py:helper#2`, `src/a.py:<module>`),
//!    `\` accepted for `/`, a leading `./` ignored;
//! 2. `<file>:<line>`: the innermost *named* symbol whose line range contains the line; a
//!    line covered only by synthetic scopes (`<module>`, `<lambda>`) is an error
//!    ([`CoreError::NoNamedSymbolAt`], `symbol_not_found`, naming the nearest named symbols);
//! 3. `<file>:<qualified>` (the leftmost single `:` whose prefix is an indexed file):
//!    qualified-name or property-path match, else bare-name match, else suffix match
//!    (`Type.method` of `Outer.Type.method`) within that file;
//! 4. whole index: the same matches, then a module-qualified spelling
//!    (`pkg.mod.Type.method` where the file's module path ends with `pkg.mod`).
//!
//! A single `:` separates segments (`Picker:set_selection`) unless the
//! text before it is an indexed file. A prefix that looks like a path (contains `/` or `\`,
//! or ends in an extension of an inventoried language) but is not indexed is
//! `symbol_not_found`: a selector never falls back to another symbol.
//!
//! Spellings are normalized before matching: `::` and `:` -> `.`, generic arguments
//! removed (`Data<'a>::from_bytes` -> `Data.from_bytes`, `Vec<T>.push` -> `Vec.push`), a
//! trailing `()` dropped. The property path `obj.prop` matches a function assigned to a
//! property (`res.redirect = function ...`): `<container>.<name>` or the
//! qualified name the extractor recorded. Synthetic symbols (`<module>`, `<lambda>`,
//! `<genexpr>`) are never matched by name or line: only by exact uid.
//!
//! Never chooses among several matches: ambiguity is an error listing candidate uids
//! (callers number them and add kind/file/line, SPEC §10).

use std::path::Path;

use crate::error::{CoreError, Result};
use crate::model::{FileId, Index, Symbol, SymbolId};

const MAX_LISTED: usize = 20;
/// Named symbols listed by [`CoreError::NoNamedSymbolAt`].
const MAX_NEAREST: usize = 5;

pub fn resolve(
    index: &Index,
    reference: &str,
    by_uid: impl Fn(&str) -> Option<SymbolId>,
) -> Result<SymbolId> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Err(CoreError::SymbolNotFound(String::new()));
    }
    if let Some(id) = by_uid(reference) {
        return Ok(id);
    }
    let normalized = reference.replace('\\', "/");
    let normalized = normalized.trim_start_matches("./");
    if let Some(id) = by_uid(normalized) {
        return Ok(id);
    }
    if let Some((file, rest)) = split_file(index, normalized) {
        let matches = in_file(index, file, rest, reference)?;
        return pick(index, reference, matches);
    }
    if let Some(prefix) = first_single_colon(normalized).map(|i| &normalized[..i]) {
        if looks_like_path(prefix) {
            // `<file>:<name>` whose file is not indexed.
            return Err(CoreError::SymbolNotFound(reference.to_string()));
        }
    }
    let q = normalize_qualified(normalized);
    if q.is_empty() {
        return Err(CoreError::SymbolNotFound(reference.to_string()));
    }
    pick(index, reference, whole_index(index, &q))
}

/// Canonical spelling of a qualified name: `::` and `:` -> `.`, generic arguments removed,
/// trailing `()` dropped, surrounding dots trimmed. Synthetic segments (`<module>`) are kept.
pub(crate) fn normalize_qualified(text: &str) -> String {
    let text = text.trim();
    let text = text.strip_suffix("()").unwrap_or(text);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut prev: Option<char> = None;
    while let Some(c) = chars.next() {
        if c == '<' && prev.is_some_and(|p| p.is_alphanumeric() || p == '_') {
            // Generic arguments: skip the balanced `<...>`.
            let mut depth = 1usize;
            for n in chars.by_ref() {
                match n {
                    '<' => depth += 1,
                    '>' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            continue;
        }
        if c == ':' {
            if chars.peek() == Some(&':') {
                chars.next();
            }
            if prev != Some('.') {
                out.push('.');
            }
            prev = Some('.');
            continue;
        }
        out.push(c);
        prev = Some(c);
    }
    out.trim_matches('.').to_string()
}

/// Byte offset of the first `:` that is not part of `::` (not at either end).
fn first_single_colon(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    (1..bytes.len().saturating_sub(1))
        .find(|&i| bytes[i] == b':' && bytes[i + 1] != b':' && bytes[i - 1] != b':')
}

/// A selector prefix that names a file: contains a path separator or ends in an extension
/// of an inventoried language (`languages` table).
fn looks_like_path(prefix: &str) -> bool {
    if prefix.contains('/') || prefix.contains('\\') {
        return true;
    }
    Path::new(prefix)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| crate::languages::from_extension(e).is_some())
}

/// `<indexed file>:<rest>` (leftmost single colon whose prefix is an indexed file).
fn split_file<'t>(index: &Index, text: &'t str) -> Option<(FileId, &'t str)> {
    let bytes = text.as_bytes();
    for i in 1..bytes.len().saturating_sub(1) {
        if bytes[i] != b':' || bytes[i + 1] == b':' || bytes[i - 1] == b':' {
            continue;
        }
        if let Some(file) = index.file_by_path(&text[..i]) {
            return Some((file, &text[i + 1..]));
        }
    }
    None
}

fn in_file(index: &Index, file: FileId, rest: &str, reference: &str) -> Result<Vec<SymbolId>> {
    let symbols = index.symbols_of(file);
    if let Ok(line) = rest.trim().parse::<u32>() {
        let found = by_line(symbols, line);
        if found.is_empty() {
            return Err(CoreError::NoNamedSymbolAt {
                reference: reference.to_string(),
                nearest: nearest_named(symbols, line),
            });
        }
        return Ok(found);
    }
    let q = normalize_qualified(rest);
    if q.is_empty() {
        return Ok(Vec::new());
    }
    Ok(by_name(symbols.iter().filter(|s| !s.is_synthetic()), &q))
}

/// `path` / `deps` start points: a `<file>:<line>` inside code that belongs to no named symbol
/// (module-level code, an anonymous callback) is the innermost *executing scope* there: the
/// synthetic `<lambda>` enclosing the line, else the file's `<module>`. `None` when the
/// reference is not `<indexed file>:<line>` or nothing contains the line. `uses` never calls
/// this: a use of anonymous code has no meaning, so its selector must name a symbol.
pub fn innermost_scope_at(index: &Index, reference: &str) -> Option<SymbolId> {
    let (file, rest) = split_file(index, reference.trim())?;
    let line = rest.trim().parse::<u32>().ok()?;
    index
        .symbols_of(file)
        .iter()
        .filter(|s| s.span.start_line <= line && line <= s.span.end_line)
        .min_by_key(|s| s.span.bytes.len())
        .map(|s| s.id)
}

/// Innermost named symbol containing `line` (synthetic scopes never).
fn by_line(symbols: &[Symbol], line: u32) -> Vec<SymbolId> {
    let hits: Vec<&Symbol> = symbols
        .iter()
        .filter(|s| !s.is_synthetic() && s.span.start_line <= line && line <= s.span.end_line)
        .collect();
    let Some(width) = hits.iter().map(|s| s.span.bytes.len()).min() else {
        return Vec::new();
    };
    hits.iter()
        .filter(|s| s.span.bytes.len() == width)
        .map(|s| s.id)
        .collect()
}

/// Up to [`MAX_NEAREST`] named symbols of a file nearest to `line`
/// (`Qualified.name (line N)`), nearest first, ties by line.
fn nearest_named(symbols: &[Symbol], line: u32) -> Vec<String> {
    let distance = |s: &Symbol| {
        if line < s.span.start_line {
            s.span.start_line - line
        } else {
            line.saturating_sub(s.span.end_line)
        }
    };
    let mut named: Vec<&Symbol> = symbols.iter().filter(|s| !s.is_synthetic()).collect();
    named.sort_by_key(|s| (distance(s), s.span.start_line, s.id));
    named
        .iter()
        .take(MAX_NEAREST)
        .map(|s| format!("{} (line {})", s.qualified_name, s.span.start_line))
        .collect()
}

/// Whether a stored spelling needs normalization before comparison.
fn needs_normalizing(name: &str) -> bool {
    name.contains('<') || name.contains(':')
}

fn qualified_is(s: &Symbol, q: &str) -> bool {
    if s.qualified_name == q {
        return true;
    }
    needs_normalizing(&s.qualified_name) && normalize_qualified(&s.qualified_name) == q
}

/// The property path of a function assigned to a property: `<container>.<name>`.
fn property_path_is(s: &Symbol, q: &str) -> bool {
    let Some(container) = s.container.as_deref().filter(|c| !c.is_empty()) else {
        return false;
    };
    let container = if needs_normalizing(container) {
        normalize_qualified(container)
    } else {
        container.to_string()
    };
    q.len() == container.len() + 1 + s.name.len()
        && q.starts_with(container.as_str())
        && q.as_bytes()[container.len()] == b'.'
        && q.ends_with(s.name.as_str())
}

fn qualified_ends_with(s: &Symbol, q: &str) -> bool {
    let check = |name: &str| {
        name.len() > q.len() && name.ends_with(q) && name.as_bytes()[name.len() - q.len() - 1] == b'.'
    };
    if check(&s.qualified_name) {
        return true;
    }
    needs_normalizing(&s.qualified_name) && check(&normalize_qualified(&s.qualified_name))
}

/// Qualified-name / property-path matches, else bare-name matches, else suffix matches.
fn by_name<'s>(symbols: impl Iterator<Item = &'s Symbol> + Clone, q: &str) -> Vec<SymbolId> {
    let exact: Vec<SymbolId> = symbols
        .clone()
        .filter(|s| qualified_is(s, q) || property_path_is(s, q))
        .map(|s| s.id)
        .collect();
    if !exact.is_empty() {
        return exact;
    }
    let bare: Vec<SymbolId> = symbols.clone().filter(|s| s.name == q).map(|s| s.id).collect();
    if !bare.is_empty() || !q.contains('.') {
        return bare;
    }
    symbols.filter(|s| qualified_ends_with(s, q)).map(|s| s.id).collect()
}

/// Dotted module path of a file (`src/pkg/mod.py` -> `src.pkg.mod`; `__init__`, `index`
/// and `mod` file stems name their directory).
fn module_path(path: &str) -> String {
    let stem = path.rsplit_once('.').map(|(a, _)| a).unwrap_or(path);
    let stem = ["/__init__", "/index", "/mod"]
        .iter()
        .find_map(|s| stem.strip_suffix(s))
        .unwrap_or(stem);
    stem.replace('/', ".")
}

fn whole_index(index: &Index, q: &str) -> Vec<SymbolId> {
    let named = index.symbols.iter().filter(|s| !s.is_synthetic());
    let found = by_name(named.clone(), q);
    if !found.is_empty() {
        return found;
    }
    // Module-qualified spelling: `pkg.mod.Type.method`.
    let mut out = Vec::new();
    for (i, _) in q.match_indices('.') {
        let (module, rest) = (&q[..i], &q[i + 1..]);
        if rest.is_empty() {
            continue;
        }
        for s in named.clone().filter(|s| qualified_is(s, rest)) {
            let m = module_path(index.file_path(s.file));
            if m == module || m.ends_with(&format!(".{module}")) {
                out.push(s.id);
            }
        }
        if !out.is_empty() {
            break;
        }
    }
    out
}

fn pick(index: &Index, reference: &str, matches: Vec<SymbolId>) -> Result<SymbolId> {
    let mut matches = matches;
    matches.sort_unstable();
    matches.dedup();
    match matches.as_slice() {
        [one] => Ok(*one),
        [] => Err(CoreError::SymbolNotFound(reference.to_string())),
        _ => {
            let mut candidates: Vec<String> =
                matches.iter().map(|&id| index.symbol(id).uid.clone()).collect();
            candidates.sort();
            candidates.dedup();
            candidates.truncate(MAX_LISTED);
            Err(CoreError::AmbiguousSymbol {
                reference: reference.to_string(),
                candidates,
            })
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/resolve.rs"]
mod tests;
