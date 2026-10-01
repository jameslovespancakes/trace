//! Mapping LSP / compiler results onto syntax declarations by file + position.
//!
//! A location maps to a declaration when, in the same file, the 1-based line of the
//! position equals the declaration's name line or one of its `declaration_lines`, or the
//! byte offset lies inside the declaration span with the declaration's name at that
//! position; exactly one candidate is required (pyright.py `mapped`). The byte offset is
//! computed with `LineIndex::byte_of_utf16` (UTF-16 columns, BOM/CRLF aware).
//!
//! Precisely: candidates are the declarations registered on the position's line (their name
//! line plus `declaration_lines`); a candidate matches when the line is one of its
//! `declaration_lines` (Python overload groups map every `@overload` line to the
//! implementation) or when the position is its name (inside its span). Parameters, locals
//! and other identifiers on a declaration line therefore never map to the declaration.
//!
//! Compiler symbols with their own spans (TypeScript worker) map by exact span first, then
//! by containment + equal name (codepath_v3 `rebind`): the syntax span may include
//! export/decorator wrappers around the compiler's node. Anonymous compiler callables
//! (class-field arrows) map to the innermost declaration whose body starts inside them;
//! anonymous callables that no name binds (callbacks) map onto the synthetic `<lambda>`
//! declaration with the same start or end (trace-syntax spans may include or drop
//! parentheses / `async`), and the TypeScript worker's module owner maps onto `<module>`.
//!
//! The synthetic `<module>` declaration (whole-file span, empty name at byte 0) is never a
//! position target: servers point at byte 0 for module files (`import m`), which is not a
//! declaration of any symbol.
//!
//! URIs (DESIGN §4.2 task 4): [`parse_uri`] normalises what servers send back - percent-encoded
//! Windows paths (`file:///c%3A/...`), lower-case drive letters, `file://localhost/` - into a
//! local path; archive and virtual documents (`jar:file:///x.jar!/p/C.class`, `jar:///..!/`,
//! `jdt://contents/...`, `csharp:/metadata/...`, any non-`file` scheme) are [`UriTarget::Virtual`]
//! and never map into the workspace: the engine hands them unmapped to `external::classify`.
//! [`relative_to`] maps a local path inside a workspace root back to its `/`-separated relative
//! path (component-wise, case-insensitive on Windows, verbatim prefixes stripped).

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use trace_core::assemble::symbol_uid;
use trace_core::facts::{Declaration, FileFacts};
use trace_core::model::ByteSpan;
use trace_core::text::LineIndex;

/// Declarations of all files in a backend partition, indexed for position lookups.
pub struct DeclTable<'a> {
    files: HashMap<&'a str, FileDecls<'a>>,
    /// Lower-cased path -> path (case-insensitive fallback for servers that re-case paths).
    folded: HashMap<String, &'a str>,
    by_uid: HashMap<String, DeclRef<'a>>,
    callable_names: HashSet<&'a str>,
    declared_names: HashSet<&'a str>,
    /// Name -> named (non-synthetic) declarations of the partition, in file/declaration order.
    by_name: HashMap<&'a str, Vec<DeclRef<'a>>>,
    /// Syntax trees of partition files parsed on demand ([`DeclTable::tree`]), one per path.
    trees: Mutex<HashMap<String, Option<Arc<tree_sitter::Tree>>>>,
}

struct FileDecls<'a> {
    source: &'a [u8],
    lines: LineIndex,
    facts: &'a FileFacts,
    /// 1-based line -> declaration indices declared on that line (name line + declaration_lines).
    by_line: HashMap<u32, Vec<u32>>,
    /// Stable uid per declaration.
    uids: Vec<String>,
    /// 1-based line of each declaration's name.
    name_lines: Vec<u32>,
}

/// A resolved declaration reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DeclRef<'a> {
    pub path: &'a str,
    pub decl: u32,
}

impl<'a> DeclTable<'a> {
    pub fn new(files: impl IntoIterator<Item = (&'a str, &'a [u8], &'a FileFacts)>) -> Self {
        let mut table = DeclTable {
            files: HashMap::new(),
            folded: HashMap::new(),
            by_uid: HashMap::new(),
            callable_names: HashSet::new(),
            declared_names: HashSet::new(),
            by_name: HashMap::new(),
            trees: Mutex::new(HashMap::new()),
        };
        for (path, source, facts) in files {
            let lines = LineIndex::new(source);
            let n = facts.declarations.len();
            let mut by_line: HashMap<u32, Vec<u32>> = HashMap::new();
            let mut occurrences: HashMap<&str, u32> = HashMap::new();
            let mut uids = Vec::with_capacity(n);
            let mut name_lines = Vec::with_capacity(n);
            for (i, d) in facts.declarations.iter().enumerate() {
                let synthetic = facts.is_synthetic(i as u32);
                let occurrence = occurrences.entry(d.qualified_name.as_str()).or_insert(0);
                *occurrence += 1;
                let uid = symbol_uid(path, &d.qualified_name, *occurrence);
                table.by_uid.insert(uid.clone(), DeclRef { path, decl: i as u32 });
                uids.push(uid);
                let name_line = lines.line1(d.name_span.start);
                name_lines.push(name_line);
                if facts.module_decl == Some(i as u32) {
                    continue;
                }
                let mut declared_on = Vec::with_capacity(1 + d.declaration_lines.len());
                declared_on.push(name_line);
                declared_on.extend_from_slice(&d.declaration_lines);
                declared_on.sort_unstable();
                declared_on.dedup();
                for line in declared_on {
                    by_line.entry(line).or_default().push(i as u32);
                }
                if synthetic {
                    continue;
                }
                if d.kind.is_callable() {
                    table.callable_names.insert(d.name.as_str());
                }
                table.declared_names.insert(d.name.as_str());
                table
                    .by_name
                    .entry(d.name.as_str())
                    .or_default()
                    .push(DeclRef { path, decl: i as u32 });
            }
            table.folded.insert(path.to_lowercase(), path);
            table.files.insert(
                path,
                FileDecls {
                    source,
                    lines,
                    facts,
                    by_line,
                    uids,
                    name_lines,
                },
            );
        }
        table
    }

    /// Map (relative path, 0-based LSP line, UTF-16 character) to a declaration.
    pub fn at_lsp(&self, path: &str, line0: u32, character: u32) -> Option<DeclRef<'a>> {
        let (path, file) = self.entry(path)?;
        let point = file.lines.byte_of_utf16(file.source, line0, character).ok()?;
        let line1 = line0 + 1;
        let mut found = None;
        for &i in file.by_line.get(&line1)? {
            let d = &file.facts.declarations[i as usize];
            let at_name = (d.name_span.contains(point) || d.name_span.start == point)
                && (d.span.bytes.contains(point) || d.span.bytes.start == point);
            if d.declaration_lines.contains(&line1) || at_name {
                if found.is_some() {
                    return None;
                }
                found = Some(i);
            }
        }
        found.map(|decl| DeclRef { path, decl })
    }

    /// Map a compiler span to a declaration (exact, else containment + same name).
    pub fn at_span(&self, path: &str, span: ByteSpan, name: &str) -> Option<DeclRef<'a>> {
        let (path, file) = self.entry(path)?;
        let decls = &file.facts.declarations;
        let wrap = |i: usize| DeclRef { path, decl: i as u32 };
        // 1. Exact span (name breaks ties between identical spans).
        let exact: Vec<usize> = (0..decls.len()).filter(|&i| decls[i].span.bytes == span).collect();
        match exact.as_slice() {
            [one] => return Some(wrap(*one)),
            [] => {}
            many => {
                let named: Vec<usize> = many.iter().copied().filter(|&i| decls[i].name == name).collect();
                return (named.len() == 1).then(|| wrap(named[0]));
            }
        }
        // 2. The syntax declaration encloses the compiler node (export/decorator wrappers),
        //    or the compiler node encloses the syntax declaration; same name, innermost.
        let related = |d: &Declaration| {
            d.name == name
                && (d.span.bytes.encloses(span)
                    || (span.encloses(d.span.bytes) && span.encloses(d.name_span)))
        };
        if let Some(i) = innermost(decls, related) {
            return Some(wrap(i));
        }
        // 3. Anonymous compiler callable bound by a syntax declaration (class-field arrows):
        //    the declaration whose body starts inside the compiler node.
        if name.starts_with('<') {
            if name == "<module>" {
                return file.facts.module_decl.map(|d| wrap(d as usize));
            }
            let binds = |d: &Declaration| {
                !d.name.starts_with('<')
                    && d.span.bytes.encloses(span)
                    && d.body_start >= span.start
                    && d.body_start < span.end
                    && d.name_span.end <= span.start
            };
            if let Some(i) = innermost(decls, binds) {
                return Some(wrap(i));
            }
            // 4. Unbound anonymous callable: the synthetic `<lambda>` scope with the same
            //    start or end whose span overlaps the compiler node (parentheses, `async`).
            let facts = file.facts;
            let lambda = |d: &Declaration| {
                d.name.starts_with('<')
                    && (d.span.bytes.start == span.start || d.span.bytes.end == span.end)
                    && d.span.bytes.start < span.end
                    && span.start < d.span.bytes.end
            };
            let candidates: Vec<usize> = (0..decls.len())
                .filter(|&i| facts.anonymous_of(i as u32).is_some() && lambda(&decls[i]))
                .collect();
            return (candidates.len() == 1).then(|| wrap(candidates[0]));
        }
        None
    }

    /// A synthetic declaration (`<module>`, `<lambda>`, `<genexpr>`): no name to prepare.
    pub fn is_synthetic(&self, r: DeclRef<'_>) -> bool {
        self.files.get(r.path).is_some_and(|f| f.facts.is_synthetic(r.decl))
    }

    /// Stable uid of a declaration (must match `trace_core::assemble::symbol_uid`, including
    /// the `#k` occurrence suffix computed over the file's declarations in order).
    pub fn uid(&self, decl: DeclRef<'a>) -> String {
        self.files
            .get(decl.path)
            .and_then(|f| f.uids.get(decl.decl as usize))
            .cloned()
            .unwrap_or_else(|| symbol_uid(decl.path, "?", 1))
    }

    /// Byte offset of an LSP position in `path`.
    pub fn byte_of(&self, path: &str, line0: u32, character: u32) -> Option<u32> {
        let (_, file) = self.entry(path)?;
        file.lines.byte_of_utf16(file.source, line0, character).ok()
    }

    /// LSP position of a byte offset in `path`.
    pub fn lsp_of(&self, path: &str, byte: u32) -> Option<(u32, u32)> {
        let (_, file) = self.entry(path)?;
        if byte as usize > file.source.len() {
            return None;
        }
        Some(file.lines.utf16_of_byte(file.source, byte))
    }

    // ---------------------------------------------------------------------------------
    // Additional lookups used by the backends.
    // ---------------------------------------------------------------------------------

    /// The partition's own spelling of `path` (exact, else case-insensitive match).
    pub fn path_key(&self, path: &str) -> Option<&'a str> {
        self.entry(path).map(|(p, _)| p)
    }

    /// Declaration behind a reference.
    pub fn decl(&self, r: DeclRef<'a>) -> &'a Declaration {
        let facts: &'a FileFacts = self.files[r.path].facts;
        &facts.declarations[r.decl as usize]
    }

    /// Checked variant of [`DeclTable::decl`].
    pub fn get(&self, r: DeclRef<'_>) -> Option<&'a Declaration> {
        let facts: &'a FileFacts = self.files.get(r.path)?.facts;
        facts.declarations.get(r.decl as usize)
    }

    /// Syntax facts of a partition file.
    pub fn facts(&self, path: &str) -> Option<&'a FileFacts> {
        self.entry(path).map(|(_, f)| f.facts)
    }

    /// Exact bytes of a partition file.
    pub fn source(&self, path: &str) -> Option<&'a [u8]> {
        self.entry(path).map(|(_, f)| f.source)
    }

    /// Syntax tree of a partition file in its language (syntax facts, else the path's
    /// extension), parsed once per table and path; `None` when it cannot be parsed.
    pub(crate) fn tree(&self, path: &str) -> Option<Arc<tree_sitter::Tree>> {
        let (key, file) = self.entry(path)?;
        let mut trees = self.trees.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        trees
            .entry(key.to_string())
            .or_insert_with(|| {
                let language = file
                    .facts
                    .language
                    .or_else(|| trace_core::languages::from_path(Path::new(key)))?;
                trace_syntax::parse_tree(language, file.source).ok().map(Arc::new)
            })
            .clone()
    }

    /// 1-based display line of a byte offset.
    pub fn line1(&self, path: &str, byte: u32) -> Option<u32> {
        self.entry(path).map(|(_, f)| f.lines.line1(byte))
    }

    /// 1-based line of a declaration's name.
    pub fn name_line(&self, r: DeclRef<'_>) -> Option<u32> {
        self.files.get(r.path)?.name_lines.get(r.decl as usize).copied()
    }

    /// Declaration for a uid.
    pub fn by_uid(&self, uid: &str) -> Option<DeclRef<'a>> {
        self.by_uid.get(uid).copied()
    }

    /// Declaration references of one file, in declaration order.
    pub fn decls_of(&self, path: &str) -> Vec<DeclRef<'a>> {
        match self.entry(path) {
            Some((path, file)) => (0..file.facts.declarations.len() as u32)
                .map(|decl| DeclRef { path, decl })
                .collect(),
            None => Vec::new(),
        }
    }

    /// Unique declaration of `name` whose name starts inside `[start, end]` (flat
    /// `SymbolInformation` results carry full ranges instead of name positions).
    pub fn by_name_within(&self, path: &str, name: &str, start: u32, end: u32) -> Option<DeclRef<'a>> {
        let (path, file) = self.entry(path)?;
        let mut found = None;
        for (i, d) in file.facts.declarations.iter().enumerate() {
            if d.name == name && d.name_span.start >= start && d.name_span.start <= end {
                if found.is_some() {
                    return None;
                }
                found = Some(i as u32);
            }
        }
        found.map(|decl| DeclRef { path, decl })
    }

    /// Some callable (function/method/constructor) in the partition has this name.
    pub fn is_callable_name(&self, name: &str) -> bool {
        self.callable_names.contains(name)
    }

    /// Some declaration (callable or type) in the partition has this name.
    pub fn is_declared_name(&self, name: &str) -> bool {
        self.declared_names.contains(name)
    }

    /// Every named (non-synthetic) declaration of `name` in the partition.
    pub fn named(&self, name: &str) -> &[DeclRef<'a>] {
        self.by_name.get(name).map(Vec::as_slice).unwrap_or_default()
    }

    /// Partition paths (unordered).
    pub fn paths(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.files.keys().copied()
    }

    fn entry(&self, path: &str) -> Option<(&'a str, &FileDecls<'a>)> {
        if let Some((k, f)) = self.files.get_key_value(path) {
            return Some((*k, f));
        }
        let key = *self.folded.get(&path.to_lowercase())?;
        self.files.get(key).map(|f| (key, f))
    }
}

/// Where a server location URI points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UriTarget {
    /// A local file: absolute, percent-decoded path (Windows drive letters upper-cased).
    File(PathBuf),
    /// A virtual or archive document (`jar:`, `jdt:`, `csharp:`, ...): never in the workspace.
    Virtual(String),
}

/// Parse a location URI a server sent. `None` for malformed `file:` URIs and remote hosts.
pub fn parse_uri(uri: &str) -> Option<UriTarget> {
    let trimmed = uri.trim();
    let scheme_end = trimmed.find(':')?;
    let scheme = &trimmed[..scheme_end];
    if !scheme.eq_ignore_ascii_case("file") {
        // A one-letter "scheme" is a bare Windows path (`c:\x`), not a URI.
        if scheme.len() == 1 {
            return None;
        }
        return Some(UriTarget::Virtual(trimmed.to_string()));
    }
    let path = crate::lsp::uri_to_path(trimmed).ok()?;
    Some(UriTarget::File(normalize_drive(path)))
}

/// The canonical form of a location URI: local files re-encoded from their normalised path
/// (`file:///C:/...`), virtual documents unchanged. Used for URIs handed to
/// `external::classify`, so every server's spelling of one file is the same string.
pub fn canonical_uri(uri: &str) -> String {
    match parse_uri(uri) {
        Some(UriTarget::File(path)) => crate::lsp::path_to_uri(&path).unwrap_or_else(|_| uri.to_string()),
        Some(UriTarget::Virtual(v)) => v,
        None => uri.to_string(),
    }
}

/// Upper-case a Windows drive letter (`c:\x` -> `C:\x`); other paths unchanged.
fn normalize_drive(path: PathBuf) -> PathBuf {
    let path = trace_core::inventory::strip_verbatim(path);
    let Some(text) = path.to_str() else { return path };
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_lowercase() {
        let mut fixed = String::with_capacity(text.len());
        fixed.push(bytes[0].to_ascii_uppercase() as char);
        fixed.push_str(&text[1..]);
        return PathBuf::from(fixed);
    }
    path
}

/// `/`-separated path of `abs` relative to `root` (component-wise; case-insensitive on
/// Windows). `None` when `abs` is not strictly inside `root` or has `..` components.
pub fn relative_to(root: &Path, abs: &Path) -> Option<String> {
    let abs = trace_core::inventory::strip_verbatim(abs.to_path_buf());
    let mut components = abs.components();
    for rc in root.components() {
        match components.next() {
            Some(c) if crate::tools::component_eq(c, rc) => {}
            _ => return None,
        }
    }
    let mut parts: Vec<&str> = Vec::new();
    for c in components {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Index of the unique innermost declaration satisfying `pred` (None when tied).
fn innermost(decls: &[Declaration], pred: impl Fn(&Declaration) -> bool) -> Option<usize> {
    let mut best: Option<(usize, u32)> = None;
    let mut tied = false;
    for (i, d) in decls.iter().enumerate().filter(|(_, d)| pred(d)) {
        let len = d.span.bytes.len();
        match best {
            Some((_, b)) if len > b => {}
            Some((_, b)) if len == b => tied = true,
            _ => {
                best = Some((i, len));
                tied = false;
            }
        }
    }
    if tied {
        None
    } else {
        best.map(|(i, _)| i)
    }
}

#[cfg(test)]
#[path = "../tests/unit/mapping.rs"]
pub(crate) mod tests;
