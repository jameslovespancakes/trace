use super::*;
use crate::test_support::facts::{decl_at, find};
use trace_core::model::SymbolKind;

/// The synthetic `<module>` declaration of a file (FileFacts::module_decl).
fn module_of<'a>(table: &DeclTable<'a>, path: &str) -> Option<DeclRef<'a>> {
    let (path, file) = table.entry(path)?;
    file.facts.module_decl.map(|decl| DeclRef { path, decl })
}

/// Rule: percent-encoded drive letters, lower-case drives and `localhost` hosts map to the
/// same workspace file as the path trace wrote.
#[test]
fn rule_encoded_drive_uri_maps_to_workspace() {
    if cfg!(windows) {
        let root = Path::new(r"C:\Work\cache\workspaces\lsp-gopls\tree");
        for uri in [
            "file:///c%3A/Work/cache/workspaces/lsp-gopls/tree/pkg/a%20b.go",
            "file:///C:/Work/cache/workspaces/lsp-gopls/tree/pkg/a%20b.go",
            "file:///c:/work/CACHE/workspaces/lsp-gopls/tree/pkg/a%20b.go",
            "file://localhost/C:/Work/cache/workspaces/lsp-gopls/tree/pkg/a%20b.go",
        ] {
            let Some(UriTarget::File(path)) = parse_uri(uri) else {
                panic!("{uri} is a file");
            };
            assert!(path.to_string_lossy().starts_with("C:"), "{uri}: {path:?}");
            assert_eq!(relative_to(root, &path).as_deref(), Some("pkg/a b.go"), "{uri}");
        }
        assert_eq!(canonical_uri("file:///c%3A/Work/x.py"), "file:///C:/Work/x.py", "one spelling per file");
    } else {
        let root = Path::new("/work/cache/workspaces/lsp-gopls/tree");
        let Some(UriTarget::File(path)) =
            parse_uri("file:///work/cache/workspaces/lsp-gopls/tree/pkg/a%20b.go")
        else {
            panic!("a file");
        };
        assert_eq!(relative_to(root, &path).as_deref(), Some("pkg/a b.go"));
        assert_eq!(relative_to(root, Path::new("/work/other/x.go")), None);
    }
    assert_eq!(parse_uri("file://remote-host/share/a.go"), None, "remote hosts never map");
    assert_eq!(parse_uri("c:\\x.go"), None, "a bare path is not a URI");
}

/// Rule: archive entries and virtual documents are always external (never mapped into
/// the workspace), whatever their inner path looks like.
#[test]
fn rule_jar_and_jdt_uris_are_external() {
    for uri in [
        "jar:file:///C:/m2/guava-33.jar!/com/google/common/base/Strings.class",
        "jar:///home/u/.m2/x.jar!/a/B.java",
        "jdt://contents/rt.jar/java.lang/String.class?=proj/%3Cjava.lang(String.class",
        "csharp:/metadata/projects/app/assemblies/System.Runtime/symbols/System.String.cs",
        "zipfile:///x.zip::a/b.scala",
    ] {
        assert_eq!(parse_uri(uri), Some(UriTarget::Virtual(uri.to_string())), "{uri}");
        assert_eq!(canonical_uri(uri), uri);
    }
}

#[test]
fn lsp_positions_map_by_name_with_bom_and_crlf() {
    let src =
        "\u{FEFF}def f(x):\r\n    return x\r\n\r\nclass C:\r\n    def m(self, é, y): pass\r\n".as_bytes();
    let f_at = find(src, "f(");
    let c_at = find(src, "C:");
    let m_at = find(src, "m(");
    let facts = FileFacts {
        declarations: vec![
            decl_at(src, "f", "f", SymbolKind::Function, (3, c_at - 4), f_at),
            decl_at(src, "C", "C", SymbolKind::Class, (c_at - 6, src.len() as u32), c_at),
            decl_at(src, "m", "C.m", SymbolKind::Method, (m_at - 4, src.len() as u32 - 2), m_at),
        ],
        ..FileFacts::default()
    };
    let table = DeclTable::new([("pkg/a.py", src, &facts)]);
    // Line 0: the BOM is not a UTF-16 column; `f` is at column 4.
    let f = table.at_lsp("pkg/a.py", 0, 4).unwrap();
    assert_eq!(table.uid(f), "pkg/a.py:f");
    // The parameter `x` on the def line does not map to `f`.
    assert_eq!(table.at_lsp("pkg/a.py", 0, 6), None);
    assert_eq!(table.uid(table.at_lsp("pkg/a.py", 3, 6).unwrap()), "pkg/a.py:C");
    let m = table.at_lsp("pkg/a.py", 4, 8).unwrap();
    assert_eq!(table.uid(m), "pkg/a.py:C.m");
    assert_eq!(table.name_line(m), Some(5));
    // Round trip for a position after a non-ASCII character on a CRLF line.
    let y = find(src, "y)");
    let (line, col) = table.lsp_of("pkg/a.py", y).unwrap();
    // `é` is two bytes but one UTF-16 unit.
    assert_eq!((line, col), (4, 19));
    assert_eq!(table.byte_of("pkg/a.py", line, col), Some(y));
    // Case-insensitive path fallback.
    assert_eq!(table.path_key("PKG/A.py"), Some("pkg/a.py"));
}

#[test]
fn overload_lines_map_to_the_implementation_and_uids_count_redefinitions() {
    let src = b"@overload\ndef g(a: int) -> int: ...\n@overload\ndef g(a: str) -> str: ...\ndef g(a): return a\ndef g(a): return 2\n";
    let lines = LineIndex::new(src);
    let second_impl = find(src, "def g(a): return 2") + 4;
    let first_impl = find(src, "def g(a): return a") + 4;
    let mut implementation =
        decl_at(src, "g", "g", SymbolKind::Function, (first_impl - 4, second_impl - 5), first_impl);
    implementation.declaration_lines = vec![2, 4, lines.line1(first_impl)];
    let redefinition =
        decl_at(src, "g", "g", SymbolKind::Function, (second_impl - 4, src.len() as u32), second_impl);
    let facts = FileFacts {
        declarations: vec![implementation, redefinition],
        ..FileFacts::default()
    };
    let table = DeclTable::new([("m.py", &src[..], &facts)]);
    let from_overload = table.at_lsp("m.py", 1, 4).unwrap();
    assert_eq!(table.uid(from_overload), "m.py:g");
    assert_eq!(table.name_line(from_overload), Some(5));
    let redefined = table.at_lsp("m.py", 5, 4).unwrap();
    assert_eq!(table.uid(redefined), "m.py:g#2");
    assert_eq!(table.by_uid("m.py:g#2"), Some(redefined));
}

#[test]
fn compiler_spans_map_exactly_or_by_containment() {
    let src = b"export function run() { return 1; }\nclass K { f = () => { go(); } }\n";
    let run_at = find(src, "run");
    let export_start = 0;
    let fn_start = find(src, "function");
    let end = find(src, "}\n") + 1;
    let k_at = find(src, "K {");
    let f_at = find(src, "f = ");
    let arrow = find(src, "() =>");
    let arrow_end = find(src, "} }") + 1;
    let mut field = decl_at(src, "f", "K.f", SymbolKind::Method, (f_at, arrow_end), f_at);
    field.body_start = find(src, "{ go");
    let facts = FileFacts {
        declarations: vec![
            decl_at(src, "run", "run", SymbolKind::Function, (export_start, end), run_at),
            decl_at(src, "K", "K", SymbolKind::Class, (k_at - 6, src.len() as u32 - 1), k_at),
            field,
        ],
        ..FileFacts::default()
    };
    let table = DeclTable::new([("a.ts", &src[..], &facts)]);
    let exact = table.at_span("a.ts", ByteSpan::new(0, end), "run").unwrap();
    assert_eq!(exact.decl, 0);
    let inner = table.at_span("a.ts", ByteSpan::new(fn_start, end), "run").unwrap();
    assert_eq!(inner.decl, 0);
    assert_eq!(table.at_span("a.ts", ByteSpan::new(fn_start, end), "other"), None);
    let anonymous = table
        .at_span("a.ts", ByteSpan::new(arrow, arrow_end), "<anonymous@22>")
        .unwrap();
    assert_eq!(table.uid(anonymous), "a.ts:K.f");
}

/// Synthetic declarations: `<module>` is never a position target (servers point at byte
/// 0 for module files) and maps from the worker's module owner; an unbound callback arrow
/// maps by span onto its `<lambda>` even when the syntax span includes `async`.
#[test]
fn synthetic_module_and_lambda_scopes_map_by_span() {
    use trace_core::facts::{AnonymousKind, AnonymousScope, Consumer};
    let src = b"go(async (c) => c.x());\n";
    let lambda_at = find(src, "async");
    let paren = find(src, "(c)");
    let lambda_end = find(src, "());") + 2;
    let mut lambda = decl_at(src, "(", "<lambda>", SymbolKind::Function, (lambda_at, lambda_end), lambda_at);
    lambda.name = "<lambda>".into();
    let mut module = decl_at(src, "", "<module>", SymbolKind::Module, (0, src.len() as u32), 0);
    module.name = "<module>".into();
    let facts = FileFacts {
        declarations: vec![lambda, module],
        anonymous: vec![AnonymousScope {
            decl: 0,
            kind: AnonymousKind::Lambda,
            created_in: None,
            consumer: Consumer::Other,
            eager: None,
        }],
        module_decl: Some(1),
        ..FileFacts::default()
    };
    let table = DeclTable::new([("cb.ts", &src[..], &facts)]);
    assert_eq!(table.at_lsp("cb.ts", 0, 0), None, "<module> is not a position target");
    assert_eq!(
        table
            .at_span("cb.ts", ByteSpan::new(0, src.len() as u32), "<module>")
            .map(|r| r.decl),
        Some(1)
    );
    // The compiler node starts at `(` (no `async`), ends where the syntax lambda ends.
    let mapped = table
        .at_span("cb.ts", ByteSpan::new(paren, lambda_end), &format!("<anonymous@{paren}>"))
        .unwrap();
    assert_eq!(mapped.decl, 0);
    assert!(table.is_synthetic(mapped));
    assert!(table.declared_names.is_empty(), "synthetic names are not declared names");
    assert_eq!(module_of(&table, "cb.ts").map(|r| r.decl), Some(1));
}
