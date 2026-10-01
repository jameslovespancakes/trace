//! Live find-references (NEXT.md item 7). Library-level: the CLI answers `uses` from the
//! index and no longer exposes live queries (CLI-FINAL removed `--live`).
//!
//! A live query asks the language server directly instead of the index:
//! LSP `textDocument/references` (`includeDeclaration` per query) at the declaration's name
//! position for Pyright, rust-analyzer and generic servers; for the TypeScript worker a
//! one-shot "references mode" in which the compiler checker compares the symbol of every
//! identifier with the target's name against the target symbol (aliases followed).
//! Results are exact spans in the snapshot mapped back to repository-relative paths; the
//! caller (trace-analysis) classifies each span against syntax facts (call / read / write /
//! import / reexport / override) and reads the exact line text through `SourceStore`.
//!
//! Warm servers: a persistent session (kept by `trace index --watch`) answers with one
//! request; otherwise a one-shot session over the backend's stable workspace is used.
//! Locations are mapped through the workspace's URI resolver (percent-encoded drives,
//! `jar:` / `jdt:` / `csharp:` documents never map: they are dropped and make the answer
//! incomplete). Servers that report the declaration although it was not asked for and
//! servers that repeat locations are normalised here for every backend: the declaration is
//! removed unless asked for, duplicates are removed.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::{json, Value};
use trace_core::model::ByteSpan;
use trace_core::text::LineIndex;

use crate::engine::{position, Session, UriResolver};
use crate::SemanticError;

/// What to look up: the declaration's name position in a repository-relative file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferenceQuery {
    /// Repository-relative path of the declaring file (`/`-separated).
    pub path: String,
    /// Byte offset of the declared identifier (`Symbol::name_span.start`).
    pub byte: u32,
    /// Also return the declaration itself.
    pub include_declaration: bool,
}

/// One location the server reported (already mapped to repository-relative bytes).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LiveReference {
    pub path: String,
    /// Span of the referencing identifier.
    pub at: ByteSpan,
    /// 1-based line of `at.start`.
    pub line: u32,
    /// The location is the queried declaration (or one of its declaration lines).
    pub is_declaration: bool,
}

/// Result of a live query; `complete` is false when the server reported partial results
/// (timeouts, files outside the snapshot, unmapped positions — counted in `dropped`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LiveReferences {
    pub references: Vec<LiveReference>,
    pub dropped: usize,
    pub complete: bool,
    /// Backend id that answered (`pyright`, `typescript`, `rust-analyzer`, `lsp:gopls`).
    pub backend: String,
}

/// One `textDocument/references` (with `context.includeDeclaration`) at `query` over an
/// initialized session whose snapshot holds `sources` (partition path -> exact bytes), mapped
/// back to repository-relative bytes. `Ok(None)` when the queried file is unknown.
pub(crate) fn query_references(
    session: &mut dyn Session,
    uris: &dyn UriResolver,
    sources: &HashMap<&str, &[u8]>,
    query: &ReferenceQuery,
    backend: &str,
) -> Result<Option<LiveReferences>, SemanticError> {
    let Some(source) = sources.get(query.path.as_str()).copied() else {
        return Ok(None);
    };
    if query.byte as usize > source.len() {
        return Ok(None);
    }
    let (line, character) = LineIndex::new(source).utf16_of_byte(source, query.byte);
    let params = json!({
        "textDocument": {"uri": uris.uri_of(&query.path)?},
        "position": {"line": line, "character": character},
        "context": {"includeDeclaration": query.include_declaration}
    });
    let mut results = session.request_many(vec![("textDocument/references".to_string(), params)])?;
    let value = results
        .pop()
        .unwrap_or_else(|| Err(SemanticError::Protocol("missing response".into())))?;
    Ok(Some(map_locations(&value, &|uri: &str| uris.rel_of(uri), sources, query, backend)))
}

/// Map `Location[]` (UTF-16 positions in snapshot URIs) onto repository-relative byte spans.
/// Locations outside the partition, with unmappable positions or malformed entries are
/// counted in `dropped` (and make the answer incomplete). The declaration itself is
/// flagged (`is_declaration`) and removed unless `query.include_declaration`. Sorted by
/// (path, start), duplicates removed.
pub(crate) fn map_locations(
    value: &Value,
    rel_of: &dyn Fn(&str) -> Option<String>,
    sources: &HashMap<&str, &[u8]>,
    query: &ReferenceQuery,
    backend: &str,
) -> LiveReferences {
    let folded: HashMap<String, &str> = sources.keys().map(|p| (p.to_lowercase(), *p)).collect();
    let mut lines: HashMap<&str, LineIndex> = HashMap::new();
    let mut references: Vec<LiveReference> = Vec::new();
    let mut dropped = 0usize;
    let items: &[Value] = value.as_array().map(Vec::as_slice).unwrap_or_default();
    for item in items {
        let mapped = (|| {
            let uri = item.get("uri").or_else(|| item.get("targetUri"))?.as_str()?;
            let range = item.get("range").or_else(|| item.get("targetSelectionRange"))?;
            let rel = rel_of(uri)?;
            let path: &str = match sources.get_key_value(rel.as_str()) {
                Some((p, _)) => p,
                None => folded.get(&rel.to_lowercase()).copied()?,
            };
            let source = sources[path];
            let index = lines.entry(path).or_insert_with(|| LineIndex::new(source));
            let (l0, c0) = position(range.get("start")?)?;
            let (l1, c1) = position(range.get("end")?)?;
            let start = index.byte_of_utf16(source, l0, c0).ok()?;
            let end = index.byte_of_utf16(source, l1, c1).ok()?.max(start);
            Some(LiveReference {
                path: path.to_string(),
                at: ByteSpan::new(start, end),
                line: index.line1(start),
                is_declaration: path == query.path && start == query.byte,
            })
        })();
        match mapped {
            Some(r) => references.push(r),
            None => dropped += 1,
        }
    }
    if !query.include_declaration {
        references.retain(|r| !r.is_declaration);
    }
    references.sort_by(|a, b| (&a.path, a.at.start, a.at.end).cmp(&(&b.path, b.at.start, b.at.end)));
    references.dedup_by(|a, b| a.path == b.path && a.at == b.at);
    LiveReferences {
        references,
        dropped,
        complete: dropped == 0 && (value.is_array() || value.is_null()),
        backend: backend.to_string(),
    }
}

#[cfg(test)]
#[path = "../tests/unit/references.rs"]
mod tests;
