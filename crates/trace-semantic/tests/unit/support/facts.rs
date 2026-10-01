//! Syntax declarations and facts built by hand.

use trace_core::facts::{Declaration, FileFacts, Import, ImportKind, Scope};
use trace_core::model::{ByteSpan, ExecutionModel, Span, SymbolKind};
use trace_core::text::LineIndex;
use trace_core::Language;

use crate::backend::SemanticFile;

/// Test helper: a declaration whose name is found by text search in `src`.
pub(crate) fn decl_at(
    src: &[u8],
    name: &str,
    qualified: &str,
    kind: SymbolKind,
    span: (u32, u32),
    name_at: u32,
) -> Declaration {
    let lines = LineIndex::new(src);
    Declaration {
        name: name.into(),
        qualified_name: qualified.into(),
        kind,
        span: Span {
            bytes: ByteSpan::new(span.0, span.1),
            start_line: lines.line1(span.0),
            end_line: lines.line1(span.1.saturating_sub(1)),
        },
        name_span: ByteSpan::new(name_at, name_at + name.len() as u32),
        body_start: span.1.min(name_at + name.len() as u32 + 3),
        parent: None,
        container: None,
        doc: None,
        decorators: Vec::new(),
        bases: Vec::new(),
        parameters: Vec::new(),
        execution: ExecutionModel::Ordinary,
        is_stub: false,
        is_test: false,
        declaration_lines: Vec::new(),
        identifiers: Vec::new(),
    }
}

pub(crate) fn find(src: &[u8], needle: &str) -> u32 {
    src.windows(needle.len())
        .position(|w| w == needle.as_bytes())
        .expect("needle present") as u32
}

pub(crate) fn facts_with_imports(language: Language, targets: &[&str]) -> FileFacts {
    let mut f = FileFacts {
        language: Some(language),
        ..Default::default()
    };
    for t in targets {
        f.imports.push(Import {
            local: "*".to_string(),
            target: t.to_string(),
            kind: ImportKind::Member,
            scope: Scope::Module,
            span: ByteSpan { start: 0, end: 1 },
            line: 1,
        });
    }
    f
}

pub(crate) fn sfile<'a>(path: &'a str, language: Language, facts: &'a FileFacts) -> SemanticFile<'a> {
    SemanticFile {
        path,
        language,
        hash: trace_core::Hash32::of(b""),
        source: b"",
        facts,
    }
}
