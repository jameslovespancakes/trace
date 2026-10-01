//! Library-qualified symbol of a declaration at (line, column) in a library file, from its
//! syntax tree: `<module prefix><separator><qualified name>` as the adapter
//! names modules (`asyncio.events.Handle.__init__`, `net/http.HandleFunc`, `std::thread::spawn`,
//! `Enum.map/2`); languages without an adapter use the qualified name alone.

use std::path::Path;

use trace_core::facts::FileFacts;
use trace_core::text::LineIndex;
use trace_core::Language;

use crate::languages;

/// The declaration (callable or type) whose name is on `line` (0-based), nearest `column`
/// (byte column): its index.
pub(crate) fn declaration_at(facts: &FileFacts, source: &[u8], line: u32, column: u32) -> Option<usize> {
    let lines = LineIndex::new(source);
    facts
        .declarations
        .iter()
        .enumerate()
        .filter(|(i, d)| {
            facts.module_decl != Some(*i as u32)
                && (d.kind.is_callable() || d.kind.is_type())
                && lines.line0(d.name_span.start) == line
        })
        .min_by_key(|(_, d)| {
            let start = lines.line_span(source, line).map(|s| s.start).unwrap_or(0);
            let col = d.name_span.start.saturating_sub(start);
            (i64::from(col) - i64::from(column)).abs()
        })
        .map(|(i, _)| i)
}

/// `builtins.map`, `threading.Thread.__init__`, `Enum.map/2`, ...
pub fn library_symbol(
    language: Language,
    path: &Path,
    source: &[u8],
    line: u32,
    column: u32,
) -> Option<String> {
    let path_text = path.to_string_lossy();
    let facts = trace_syntax::extract(trace_syntax::SourceInput {
        path: &path_text,
        language,
        source,
    })
    .ok()?;
    let index = declaration_at(&facts, source, line, column)?;
    let decl = &facts.declarations[index];
    let adapter = languages::adapter(language);
    let sep = adapter.map(|s| s.symbol_separator).unwrap_or(".");
    let qualified = if sep == "." {
        decl.qualified_name.clone()
    } else {
        decl.qualified_name.replace('.', sep)
    };
    let base = match adapter.and_then(|a| (a.module_name)(path, &[])) {
        Some(m) => format!("{m}{sep}{qualified}"),
        None => qualified,
    };
    Some(base)
}

#[cfg(test)]
#[path = "../tests/unit/symbol.rs"]
mod tests;
