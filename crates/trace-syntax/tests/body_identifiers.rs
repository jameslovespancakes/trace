use trace_core::Language;
use trace_syntax::{extract, SourceInput};

#[test]
fn body_search_terms_exclude_outer_headers_across_languages() {
    let cases = [
        (Language::Python, "sample.py", "def header_only(unused_header):\n    return body_marker()\n"),
        (
            Language::TypeScript,
            "sample.ts",
            "export const header_only = (unused_header: number) => body_marker();",
        ),
        (Language::JavaScript, "sample.js", "function header_only(unused_header) { return body_marker(); }"),
        (
            Language::Go,
            "sample.go",
            "package sample\nfunc header_only(unused_header int) int { return body_marker() }",
        ),
        (Language::Rust, "sample.rs", "fn header_only(unused_header: i32) -> i32 { body_marker() }"),
        (
            Language::CSharp,
            "sample.cs",
            "class Sample { int header_only(int unused_header) => body_marker(); }",
        ),
        (Language::Scala, "sample.scala", "def header_only(unused_header: Int) = body_marker()"),
        (Language::R, "sample.R", "header_only <- function(unused_header) { body_marker() }"),
        (Language::Bash, "sample.sh", "header_only() { body_marker; }"),
    ];
    for (language, path, source) in cases {
        let facts = extract(SourceInput {
            path,
            language,
            source: source.as_bytes(),
        })
        .unwrap();
        let declaration = facts.declarations.iter().find(|d| d.name == "header_only").unwrap();
        let terms = facts.body_identifiers.get(&declaration.body_start);
        assert!(
            terms.is_some_and(|xs| xs.iter().any(|(s, _)| s == "body_marker")),
            "{language:?}: {terms:?}"
        );
        assert!(
            !terms
                .unwrap()
                .iter()
                .any(|(s, _)| s == "header_only" || s == "unused_header"),
            "{language:?}: {terms:?}"
        );
    }
}
