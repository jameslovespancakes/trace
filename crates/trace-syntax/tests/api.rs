//! Public API: extraction of single files and batches.

use trace_core::Language;
use trace_syntax::{extract, extract_many, SourceInput, SyntaxError};

#[test]
fn extract_many_preserves_order_and_reports_unsupported() {
    let inputs = [
        SourceInput {
            path: "a.py",
            language: Language::Python,
            source: b"def a():\n    pass\n",
        },
        SourceInput {
            path: "b.sql",
            language: Language::Sql,
            source: b"select 1;",
        },
        SourceInput {
            path: "c.js",
            language: Language::JavaScript,
            source: b"function c() {}\n",
        },
    ];
    let out = extract_many(&inputs);
    assert_eq!(out.len(), 3);
    assert_eq!(out[0].as_ref().unwrap().declarations[0].name, "a");
    assert!(matches!(out[1], Err(SyntaxError::NoGrammar(Language::Sql))));
    assert_eq!(out[2].as_ref().unwrap().declarations[0].name, "c");
    assert_eq!(out[2].as_ref().unwrap().language, Some(Language::JavaScript));
}

#[test]
fn empty_and_binary_like_sources_do_not_fail() {
    let f = extract(SourceInput {
        path: "e.py",
        language: Language::Python,
        source: b"",
    })
    .unwrap();
    // Only the synthetic `<module>` declaration (empty span).
    assert_eq!(f.declarations.len(), 1);
    assert_eq!(f.module_decl, Some(0));
    assert_eq!(f.declarations[0].name, "<module>");
    let f = extract(SourceInput {
        path: "x.py",
        language: Language::Python,
        source: b"\xff\xfe def \x00 (",
    })
    .unwrap();
    assert!(f.error_count > 0);
}
