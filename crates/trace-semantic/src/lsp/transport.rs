//! The LSP wire: `Content-Length` framing ([`read_frame`], [`write_frame`]), the reader thread
//! that parses and filters incoming frames, the outgoing frame shapes, bounded kept lists
//! ([`Kept`]) and file path <-> URI conversion.

use crate::SemanticError;
use serde::Serialize;
use serde_json::Value;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ChildStdout;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use super::client::*;

/// Largest accepted frame body.
pub const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;

/// Longest accepted header line (bytes, including the terminator).
pub(super) const MAX_HEADER_LINE: u64 = 8 * 1024;

/// A server-fed list kept for the load checks, bounded by count and bytes (module docs).
/// `rolling`: the first entries (up to half of each bound) stay for good and later ones roll
/// (the oldest of them are dropped first); otherwise the list stops growing when full.
#[derive(Debug)]
pub(crate) struct Kept<T> {
    pub(super) items: Vec<T>,
    pub(super) sizes: Vec<usize>,
    pub(super) bytes: usize,
    /// Entries kept for good (the first ones).
    pub(super) head: usize,
    /// The head is complete (it reached half of a bound, or something was dropped).
    pub(super) frozen: bool,
    pub(super) max_count: usize,
    pub(super) max_bytes: usize,
    pub(super) rolling: bool,
}

impl<T> Kept<T> {
    pub(crate) fn new(max_count: usize, max_bytes: usize, rolling: bool) -> Kept<T> {
        Kept {
            items: Vec::new(),
            sizes: Vec::new(),
            bytes: 0,
            head: 0,
            frozen: false,
            max_count: max_count.max(2),
            max_bytes: max_bytes.max(2),
            rolling,
        }
    }

    /// Keep `item` of (estimated) `size` bytes; an item larger than half the byte bound is
    /// never kept (callers cut long texts first).
    pub(crate) fn push(&mut self, item: T, size: usize) {
        if size > self.max_bytes / 2 {
            return;
        }
        let full = self.items.len() >= self.max_count || self.bytes + size > self.max_bytes;
        if full && !self.rolling {
            self.frozen = true;
            return;
        }
        self.items.push(item);
        self.sizes.push(size);
        self.bytes += size;
        if !self.frozen {
            if self.items.len() <= self.max_count / 2 && self.bytes <= self.max_bytes / 2 {
                self.head = self.items.len();
            } else {
                self.frozen = true;
            }
        }
        if self.items.len() > self.max_count || self.bytes > self.max_bytes {
            // Drop the oldest rolling entries: over the count, a quarter of the count bound at
            // a time (amortised; never the newest); over the bytes, as many as needed (the
            // head holds at most half the bytes and an item at most half, so the newest stays).
            let rolling_len = self.items.len() - self.head;
            let mut n = if self.items.len() > self.max_count {
                (self.max_count / 4).min(rolling_len.saturating_sub(1)).max(1)
            } else {
                0
            };
            let mut removed: usize = self.sizes[self.head..self.head + n].iter().sum();
            while self.bytes - removed > self.max_bytes && n < rolling_len {
                removed += self.sizes[self.head + n];
                n += 1;
            }
            self.items.drain(self.head..self.head + n);
            self.sizes.drain(self.head..self.head + n);
            self.bytes -= removed;
        }
    }

    pub(crate) fn as_slice(&self) -> &[T] {
        &self.items
    }
}

/// Estimated heap bytes of a JSON value (strings + a fixed cost per node).
pub(crate) fn value_size(value: &Value) -> usize {
    const NODE: usize = 16;
    match value {
        Value::String(s) => NODE + s.len(),
        Value::Array(items) => NODE + items.iter().map(value_size).sum::<usize>(),
        Value::Object(map) => NODE + map.iter().map(|(k, v)| NODE + k.len() + value_size(v)).sum::<usize>(),
        _ => NODE,
    }
}

/// `text` cut to at most `max` bytes at a character boundary.
pub(crate) fn cut_text(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Messages from the reader thread.
pub(super) enum Incoming {
    Message(Value),
    /// The server closed its output (exited).
    Eof,
    /// Framing or JSON error; the stream cannot be trusted any more.
    Broken(String),
}

/// Read one frame body. `Ok(None)` at a clean end of stream (before any header byte).
pub(crate) fn read_frame<R: BufRead>(reader: &mut R) -> Result<Option<Vec<u8>>, String> {
    let mut length: Option<usize> = None;
    let mut line = Vec::with_capacity(64);
    let mut first = true;
    loop {
        line.clear();
        let n = reader
            .by_ref()
            .take(MAX_HEADER_LINE)
            .read_until(b'\n', &mut line)
            .map_err(|e| format!("LSP read failed: {e}"))?;
        if n == 0 {
            return if first {
                Ok(None)
            } else {
                Err("truncated LSP header".into())
            };
        }
        first = false;
        if line.last() != Some(&b'\n') {
            return Err("LSP header line too long or truncated".into());
        }
        let text = line
            .strip_suffix(b"\n")
            .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
            .unwrap_or(&line[..]);
        if text.is_empty() {
            break;
        }
        let text = std::str::from_utf8(text).map_err(|_| "non-ASCII LSP header".to_string())?;
        let (key, value) = text
            .split_once(':')
            .ok_or_else(|| format!("malformed LSP header: {text:?}"))?;
        if key.trim().eq_ignore_ascii_case("content-length") {
            let size: usize = value
                .trim()
                .parse()
                .map_err(|_| format!("invalid Content-Length: {:?}", value.trim()))?;
            length = Some(size);
        }
    }
    let size = length.ok_or_else(|| "LSP frame without Content-Length".to_string())?;
    if size > MAX_FRAME_BYTES {
        return Err(format!("LSP frame of {size} bytes exceeds the 32 MB limit"));
    }
    let mut body = vec![0u8; size];
    reader
        .read_exact(&mut body)
        .map_err(|_| "truncated LSP frame".to_string())?;
    Ok(Some(body))
}

/// Write one frame (`Content-Length` header + body) and flush.
pub(crate) fn write_frame<W: Write>(writer: &mut W, body: &[u8]) -> io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body)?;
    writer.flush()
}

/// Reader thread: frames -> messages. What the client would not keep is dropped or cut here,
/// before it is queued (module docs): `publishDiagnostics` once `keep_diagnostics` is off, log
/// texts longer than [`MAX_MESSAGE_BYTES`].
pub(super) fn read_loop(stdout: ChildStdout, tx: Sender<Incoming>, keep_diagnostics: &AtomicBool) {
    let mut reader = BufReader::with_capacity(64 * 1024, stdout);
    loop {
        let incoming = match read_frame(&mut reader) {
            Ok(Some(body)) => match serde_json::from_slice::<Value>(&body) {
                Ok(mut value) => {
                    if !keep_queued(&mut value, keep_diagnostics.load(Ordering::Relaxed)) {
                        continue;
                    }
                    Incoming::Message(value)
                }
                Err(e) => Incoming::Broken(format!("invalid JSON in LSP frame: {e}")),
            },
            Ok(None) => Incoming::Eof,
            Err(reason) => Incoming::Broken(reason),
        };
        let stop = !matches!(incoming, Incoming::Message(_));
        if tx.send(incoming).is_err() || stop {
            return;
        }
    }
}

/// Whether a received message is queued for the client (`false`: a notification the client
/// would drop anyway); over-long log / show message texts are cut in place.
pub(super) fn keep_queued(message: &mut Value, keep_diagnostics: bool) -> bool {
    if message.get("id").is_some() {
        return true;
    }
    let (diagnostics, log) = match message.get("method").and_then(Value::as_str) {
        Some("textDocument/publishDiagnostics") => (true, false),
        Some("window/logMessage" | "window/showMessage") => (false, true),
        _ => (false, false),
    };
    if diagnostics {
        return keep_diagnostics;
    }
    if log {
        if let Some(Value::String(text)) = message.pointer_mut("/params/message") {
            if text.len() > MAX_MESSAGE_BYTES {
                let end = cut_text(text, MAX_MESSAGE_BYTES).len();
                text.truncate(end);
            }
        }
    }
    true
}

#[derive(Serialize)]
pub(super) struct RequestFrame<'a> {
    pub(super) jsonrpc: &'static str,
    pub(super) id: i64,
    pub(super) method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) params: Option<&'a Value>,
}

#[derive(Serialize)]
pub(super) struct NotificationFrame<'a, P> {
    pub(super) jsonrpc: &'static str,
    pub(super) method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) params: Option<&'a P>,
}

#[derive(Serialize)]
pub(super) struct ResponseFrame<'a> {
    pub(super) jsonrpc: &'static str,
    pub(super) id: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) result: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<Value>,
}

#[derive(Serialize)]
pub(super) struct DidOpen<'a> {
    #[serde(rename = "textDocument")]
    pub(super) text_document: TextDocumentItem<'a>,
}

#[derive(Serialize)]
pub(super) struct TextDocumentItem<'a> {
    pub(super) uri: &'a str,
    #[serde(rename = "languageId")]
    pub(super) language_id: &'a str,
    pub(super) version: i32,
    pub(super) text: &'a str,
}

/// `file:` URI for an absolute path (via `url::Url::from_file_path`).
pub fn path_to_uri(path: &Path) -> Result<String, SemanticError> {
    url::Url::from_file_path(path)
        .map(|u| u.to_string())
        .map_err(|_| SemanticError::Protocol(format!("not an absolute path: {}", path.display())))
}

/// Absolute path for a `file:` URI; other schemes and remote hosts are rejected.
pub fn uri_to_path(uri: &str) -> Result<PathBuf, SemanticError> {
    let url = url::Url::parse(uri).map_err(|e| SemanticError::Protocol(e.to_string()))?;
    if url.scheme() != "file" || url.host_str().is_some_and(|h| !h.is_empty() && h != "localhost") {
        return Err(SemanticError::Protocol(format!("unsupported URI: {uri}")));
    }
    url.to_file_path()
        .map_err(|_| SemanticError::Protocol(format!("unsupported URI: {uri}")))
}
