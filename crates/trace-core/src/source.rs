//! Exact, hash-verified source access.
//!
//! Every read re-validates the path (no symlinks, no escape) and the blake3 hash recorded in
//! the index. A mismatch is [`CoreError::SourceChanged`]: output never shows bytes the index
//! did not analyze. Model-provided text is never trusted as source.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::error::{CoreError, Result};
use crate::fingerprint::Hash32;
use crate::inventory::safe_source_path;
use crate::model::{ByteSpan, FileId, Index, SymbolId};
use crate::text::LineIndex;

/// Read `rel` under `root` and verify it hashes to `expected`.
pub fn read_verified(root: &Path, rel: &str, expected: &Hash32) -> Result<Vec<u8>> {
    let path = safe_source_path(root, rel)?;
    let bytes = fs::read(&path).map_err(|e| CoreError::io(&path, e))?;
    if Hash32::of(&bytes) != *expected {
        return Err(CoreError::SourceChanged(rel.to_string()));
    }
    Ok(bytes)
}

/// A loaded, verified file with its line index.
#[derive(Debug)]
pub struct SourceFile {
    pub bytes: Vec<u8>,
    pub lines: LineIndex,
}

impl SourceFile {
    /// Exact slice (lossy only if the file is not valid UTF-8).
    pub fn slice(&self, span: ByteSpan) -> std::borrow::Cow<'_, str> {
        let end = (span.end as usize).min(self.bytes.len());
        let start = (span.start as usize).min(end);
        String::from_utf8_lossy(&self.bytes[start..end])
    }
}

/// Thread-safe cache of verified sources for one index.
pub struct SourceStore<'a> {
    index: &'a Index,
    root: PathBuf,
    files: Mutex<HashMap<FileId, Arc<SourceFile>>>,
}

impl<'a> SourceStore<'a> {
    pub fn new(index: &'a Index) -> Self {
        Self::with_root(index, PathBuf::from(&index.header.root))
    }

    /// Store reading from an explicit canonical root (e.g. the workspace root, which equals
    /// `index.header.root` for loaded indexes).
    pub fn with_root(index: &'a Index, root: PathBuf) -> Self {
        SourceStore {
            index,
            root,
            files: Mutex::new(HashMap::new()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Load (once) and verify a file.
    pub fn file(&self, id: FileId) -> Result<Arc<SourceFile>> {
        if let Some(f) = self.cache().get(&id) {
            return Ok(Arc::clone(f));
        }
        let rec = self
            .index
            .files
            .get(id.idx())
            .ok_or_else(|| CoreError::UnknownFile(format!("#{}", id.0)))?;
        let bytes = read_verified(&self.root, &rec.path, &rec.hash)?;
        let lines = LineIndex::new(&bytes);
        let loaded = Arc::new(SourceFile { bytes, lines });
        // Another thread may have loaded the same file meanwhile; keep the first copy.
        let kept = self.cache().entry(id).or_insert(loaded).clone();
        Ok(kept)
    }

    /// The cache map; a poisoned lock only means another reader panicked mid-insert of an
    /// immutable, fully verified value, so the data is still sound.
    fn cache(&self) -> std::sync::MutexGuard<'_, HashMap<FileId, Arc<SourceFile>>> {
        self.files.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Exact text of a span.
    pub fn text(&self, file: FileId, span: ByteSpan) -> Result<String> {
        Ok(self.file(file)?.slice(span).into_owned())
    }

    /// Exact source of a symbol's full declaration span.
    pub fn symbol_source(&self, id: SymbolId) -> Result<String> {
        let s = self.index.symbol(id);
        self.text(s.file, s.span.bytes)
    }

    /// Signature/header text: declaration start up to the body (trimmed), else first line.
    pub fn signature(&self, id: SymbolId) -> Result<String> {
        let s = self.index.symbol(id);
        let f = self.file(s.file)?;
        let end = if s.body_start > s.span.bytes.start {
            s.body_start
        } else {
            f.lines
                .line_span(&f.bytes, f.lines.line0(s.span.bytes.start))
                .map(|l| l.end)
                .unwrap_or(s.span.bytes.end)
        };
        let text = f.slice(ByteSpan::new(s.span.bytes.start, end.min(s.span.bytes.end)));
        Ok(text.trim_end().to_string())
    }

    /// The line containing `byte`: (1-based line, 1-based character column of `byte`, exact
    /// line text without terminator; BOM stripped on the first line).
    pub fn line_at(&self, file: FileId, byte: u32) -> Result<(u32, u32, String)> {
        let f = self.file(file)?;
        let line0 = f.lines.line0(byte);
        let start = f.lines.line_span(&f.bytes, line0).map(|s| s.start).unwrap_or(0);
        let mut from = start as usize;
        if line0 == 0 && f.lines.has_bom() {
            from = (from + 3).min(f.bytes.len());
        }
        let to = (byte as usize).clamp(from, f.bytes.len());
        let column = String::from_utf8_lossy(&f.bytes[from..to]).chars().count() as u32 + 1;
        Ok((line0 + 1, column, f.lines.line_text(&f.bytes, line0).into_owned()))
    }

    /// Text of a 1-based line (terminator excluded).
    pub fn line(&self, file: FileId, line1: u32) -> Result<String> {
        let f = self.file(file)?;
        Ok(f.lines.line_text(&f.bytes, line1.saturating_sub(1)).into_owned())
    }
}

#[cfg(test)]
#[path = "../tests/unit/source.rs"]
mod tests;
