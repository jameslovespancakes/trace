//! Exact byte <-> line and byte <-> LSP (UTF-16) position mapping.
//!
//! All spans in trace are byte offsets into the raw file bytes, *including* a UTF-8 BOM if
//! present. Lines follow LSP rules: terminators are `\r\n`, `\n` and a lone `\r`.
//! Display lines are 1-based; LSP lines are 0-based. The BOM is not part of any LSP column.

use crate::error::{CoreError, Result};
use crate::model::ByteSpan;

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// Precomputed line starts for one source buffer.
#[derive(Clone, Debug)]
pub struct LineIndex {
    /// Byte offset of the first byte of every line. `starts[0] == 0`.
    starts: Vec<u32>,
    /// Total byte length.
    len: u32,
    /// True when the buffer begins with a UTF-8 BOM.
    bom: bool,
}

impl LineIndex {
    pub fn new(src: &[u8]) -> Self {
        let mut starts = Vec::with_capacity(src.len() / 32 + 1);
        starts.push(0u32);
        let mut i = 0usize;
        while i < src.len() {
            match src[i] {
                b'\n' => starts.push((i + 1) as u32),
                b'\r' => {
                    if src.get(i + 1) == Some(&b'\n') {
                        i += 1;
                    }
                    starts.push((i + 1) as u32);
                }
                _ => {}
            }
            i += 1;
        }
        LineIndex {
            starts,
            len: src.len() as u32,
            bom: src.starts_with(BOM),
        }
    }

    /// Number of lines (a trailing terminator starts a final empty line).
    pub fn line_count(&self) -> u32 {
        self.starts.len() as u32
    }

    pub(crate) fn has_bom(&self) -> bool {
        self.bom
    }

    /// 0-based line containing `byte` (bytes past the end map to the last line).
    pub fn line0(&self, byte: u32) -> u32 {
        match self.starts.binary_search(&byte) {
            Ok(i) => i as u32,
            Err(i) => (i - 1) as u32,
        }
    }

    /// 1-based display line containing `byte`.
    pub fn line1(&self, byte: u32) -> u32 {
        self.line0(byte) + 1
    }

    /// Byte span of 0-based `line`, excluding its terminator.
    pub fn line_span(&self, src: &[u8], line0: u32) -> Option<ByteSpan> {
        let start = *self.starts.get(line0 as usize)?;
        let next = self.starts.get(line0 as usize + 1).copied().unwrap_or(self.len);
        let mut end = next;
        if end > start && src.get(end as usize - 1) == Some(&b'\n') {
            end -= 1;
        }
        if end > start && src.get(end as usize - 1) == Some(&b'\r') {
            end -= 1;
        }
        Some(ByteSpan::new(start, end))
    }

    /// Text of 0-based `line` (lossy UTF-8, terminator excluded, BOM stripped on line 0).
    pub fn line_text<'s>(&self, src: &'s [u8], line0: u32) -> std::borrow::Cow<'s, str> {
        match self.line_span(src, line0) {
            Some(span) => {
                let mut bytes = &src[span.range()];
                if line0 == 0 && self.bom {
                    bytes = bytes.strip_prefix(BOM).unwrap_or(bytes);
                }
                String::from_utf8_lossy(bytes)
            }
            None => std::borrow::Cow::Borrowed(""),
        }
    }

    /// LSP position (0-based line, UTF-16 column) -> byte offset.
    ///
    /// Columns beyond the line end clamp to the line end (LSP rule). A column that splits a
    /// surrogate pair is an error. Invalid UTF-8 bytes count as one UTF-16 unit each.
    pub fn byte_of_utf16(&self, src: &[u8], line0: u32, character: u32) -> Result<u32> {
        let span = self
            .line_span(src, line0)
            .ok_or_else(|| CoreError::InvalidPosition(format!("line {line0} outside source")))?;
        let mut offset = span.start;
        let mut bytes = &src[span.range()];
        if line0 == 0 && self.bom && bytes.starts_with(BOM) {
            offset += 3;
            bytes = &bytes[3..];
        }
        let mut units = 0u32;
        for chunk in bytes.utf8_chunks() {
            for ch in chunk.valid().chars() {
                if units == character {
                    return Ok(offset);
                }
                let w = ch.len_utf16() as u32;
                if units + w > character {
                    return Err(CoreError::InvalidPosition("LSP position splits a surrogate pair".into()));
                }
                units += w;
                offset += ch.len_utf8() as u32;
            }
            for _ in chunk.invalid() {
                if units == character {
                    return Ok(offset);
                }
                units += 1;
                offset += 1;
            }
        }
        Ok(offset)
    }

    /// Byte offset -> LSP position (0-based line, UTF-16 column). Inverse of [`byte_of_utf16`].
    pub fn utf16_of_byte(&self, src: &[u8], byte: u32) -> (u32, u32) {
        let line0 = self.line0(byte);
        let start = self.starts[line0 as usize];
        let end = byte.min(self.len).max(start);
        let mut prefix = &src[start as usize..end as usize];
        if line0 == 0 && self.bom {
            prefix = prefix.strip_prefix(BOM).unwrap_or(prefix);
        }
        let mut units = 0u32;
        for chunk in prefix.utf8_chunks() {
            units += chunk.valid().encode_utf16().count() as u32;
            units += chunk.invalid().len() as u32;
        }
        (line0, units)
    }
}

#[cfg(test)]
#[path = "../tests/unit/text.rs"]
mod tests;
