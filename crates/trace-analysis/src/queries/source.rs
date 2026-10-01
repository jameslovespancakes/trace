//! Explicit, bounded, literal source fallback over admitted, hash-verified indexed files.
//! This is discovery, not a replacement for complete `show` definitions or graph evidence.
use crate::report::Envelope;
use crate::{AnalysisError, Result, Workspace};
use serde::Serialize;
use trace_core::{FileId, Index};

const MAX_SOURCE_BYTES: usize = 48_000;
const PREVIEW_CHARS: usize = 160;

#[derive(Debug, Serialize)]
pub struct SourceMatch {
    pub file: String,
    pub line: u32,
    /// One-based Unicode character column of the first literal match on the line.
    pub column: usize,
    /// Actual enclosing named declaration (or module), never an invented callback name.
    pub owner: Option<String>,
    pub preview: String,
    pub preview_column: usize,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct SearchReport {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub query: String,
    pub matches: Vec<SourceMatch>,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub eligible_files: usize,
    pub notice: &'static str,
}

#[derive(Debug, Serialize)]
pub struct SourceReport {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    pub file_lines: u32,
    pub source: String,
    pub next_line: Option<u32>,
    pub notice: &'static str,
}

fn invalid(message: &str) -> AnalysisError {
    AnalysisError::InvalidArgument(message.into())
}

fn owner(index: &Index, file: FileId, byte: u32) -> Option<String> {
    index
        .symbols_of(file)
        .iter()
        .filter(|s| {
            (!s.is_synthetic() || s.kind == trace_core::SymbolKind::Module) && s.span.bytes.contains(byte)
        })
        .min_by_key(|s| s.span.bytes.len())
        .map(|s| s.uid.clone())
}

/// Literal, case-sensitive, one result per matching line, deterministic file/line order.
/// Pages skip matching lines, not source bytes. Preview truncation is always explicit.
pub fn search(
    ws: &Workspace,
    query: &str,
    file: Option<&str>,
    limit: usize,
    offset: usize,
) -> Result<SearchReport> {
    if query.is_empty() || query.contains(['\n', '\r']) || !(1..=100).contains(&limit) {
        return Err(invalid("Give a nonempty single-line literal and a limit in 1..100."));
    }
    let index = ws.index()?;
    let sources = ws.sources()?;
    let mut files: Vec<_> = index
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| file.is_none_or(|filter| f.path.contains(filter)))
        .collect();
    files.sort_by_key(|(_, f)| &f.path);
    let eligible_files = files.len();
    let mut matches = Vec::new();
    let mut skipped = 0usize;
    let mut bytes = 0usize;
    let mut next_offset = None;
    'files: for (fi, record) in files {
        let fid = FileId(fi as u32);
        let loaded = sources.file(fid)?;
        let text = std::str::from_utf8(&loaded.bytes)
            .map_err(|_| invalid("Source fallback requires UTF-8 text; no bytes were silently replaced."))?;
        let mut byte_start = 0usize;
        for (number, raw) in text.split_inclusive('\n').enumerate() {
            let base = byte_start;
            byte_start += raw.len();
            let line = raw
                .strip_suffix("\r\n")
                .or_else(|| raw.strip_suffix('\n'))
                .unwrap_or(raw);
            let Some(at) = line.find(query) else { continue };
            if skipped < offset {
                skipped += 1;
                continue;
            }
            if matches.len() == limit {
                next_offset = Some(offset + matches.len());
                break 'files;
            }
            let column = line[..at].chars().count();
            let start = column.saturating_sub(30);
            let preview: String = line.chars().skip(start).take(PREVIEW_CHARS).collect();
            let truncated = start != 0 || line.chars().count() > start + PREVIEW_CHARS;
            let hit = SourceMatch {
                file: record.path.clone(),
                line: number as u32 + 1,
                column: column + 1,
                owner: owner(index, fid, (base + at) as u32),
                preview,
                preview_column: start + 1,
                truncated,
            };
            let size = serde_json::to_vec(&hit).expect("source match serializes").len();
            if bytes + size > MAX_SOURCE_BYTES {
                if matches.is_empty() {
                    return Err(invalid("Match identity exceeds the source-search page budget."));
                }
                next_offset = Some(offset + matches.len());
                break 'files;
            }
            bytes += size;
            matches.push(hit);
        }
    }
    Ok(SearchReport { envelope: ws.envelope("search"), query: query.into(), matches, offset,
        next_offset, eligible_files,
        notice: "Literal, case-sensitive source excerpts, including strings and anonymous scopes; not semantic matches or full definitions. Each matching line occurs once. Follow next_offset with the same query/filters; use source for exact lines or show for complete definitions." })
}

/// An explicit exact file-line window: never silently shorten a requested definition.
pub fn source(ws: &Workspace, file: &str, start: u32, count: u32) -> Result<SourceReport> {
    if start == 0 || !(1..=200).contains(&count) {
        return Err(invalid("Source requires start >= 1 and lines in 1..200."));
    }
    let normalized = file.replace('\\', "/");
    let normalized = normalized.strip_prefix("./").unwrap_or(&normalized);
    let index = ws.index()?;
    let fid = index.file_by_path(normalized).ok_or_else(|| {
        invalid("Use an exact indexed repo-relative file path; no paths outside the index are readable.")
    })?;
    let sources = ws.sources()?;
    let loaded = sources.file(fid)?;
    let text = std::str::from_utf8(&loaded.bytes)
        .map_err(|_| invalid("Source fallback requires UTF-8 text; no bytes were silently replaced."))?;
    let total = text.split_inclusive('\n').count() as u32;
    if start > total && !(start == 1 && total == 0) {
        return Err(invalid("Source start is beyond the end of the file."));
    }
    let end = start.saturating_add(count - 1).min(total);
    let source: String = text
        .split_inclusive('\n')
        .skip(start as usize - 1)
        .take(count as usize)
        .collect();
    if source.len() > MAX_SOURCE_BYTES {
        return Err(invalid("Requested source window exceeds 48000 bytes; request fewer lines, or use show for the complete definition/file. No requested bytes were discarded."));
    }
    Ok(SourceReport { envelope: ws.envelope("source"), file: index.file_path(fid).into(),
        start_line: start, end_line: end, file_lines: total, source,
        next_line: (end < total).then_some(end + 1),
        notice: "Exact hash-verified file-line window, not a complete definition. EOF may shorten the requested window; next_line continues. Show always returns full requested definitions." })
}

#[cfg(test)]
#[path = "../../tests/unit/source.rs"]
mod tests;
