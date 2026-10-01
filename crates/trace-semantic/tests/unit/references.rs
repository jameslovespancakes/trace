use super::*;
use crate::test_support::session::{range, FakeSession, FakeUris, Handler};

fn sources<'s>(files: &'s [(&'s str, &'s [u8])]) -> HashMap<&'s str, &'s [u8]> {
    files.iter().copied().collect()
}

/// A scripted server answering `textDocument/references`: positions are UTF-16 and map
/// back to bytes (the non-ASCII character before `helper` is 2 bytes, 1 UTF-16 unit);
/// locations outside the partition are dropped and make the answer incomplete.
#[test]
fn references_map_utf16_positions_to_bytes_and_count_dropped() {
    let a: &[u8] = b"def helper():\n    pass\n";
    let b: &[u8] = "x = '\u{e9}'; helper()\nhelper()\n".as_bytes();
    let files = [("pkg/a.py", a), ("b.py", b)];
    let map = sources(&files);
    let handler: Handler = Box::new(|method, params| {
        assert_eq!(method, "textDocument/references");
        assert_eq!(params["position"], json!({"line": 0, "character": 4}));
        assert_eq!(params["context"]["includeDeclaration"], true);
        Ok(json!([
            {"uri": "file:///ws/pkg/a.py", "range": range(0, 4, 10)},
            {"uri": "file:///ws/b.py", "range": range(0, 9, 15)},
            {"uri": "file:///ws/b.py", "range": range(1, 0, 6)},
            {"uri": "file:///ws/b.py", "range": range(1, 0, 6)},
            {"uri": "file:///elsewhere/lib.pyi", "range": range(0, 0, 6)}
        ]))
    });
    let mut session = FakeSession::new(json!({"referencesProvider": true}), handler);
    let query = ReferenceQuery {
        path: "pkg/a.py".into(),
        byte: 4,
        include_declaration: true,
    };
    let found = query_references(&mut session, &FakeUris, &map, &query, "pyright")
        .unwrap()
        .unwrap();
    assert_eq!(session.batches, 1, "one request");
    assert_eq!(found.backend, "pyright");
    assert_eq!(found.dropped, 1);
    assert!(!found.complete);
    let spans: Vec<(&str, u32, u32, u32, bool)> = found
        .references
        .iter()
        .map(|r| (r.path.as_str(), r.at.start, r.at.end, r.line, r.is_declaration))
        .collect();
    let call = "x = '\u{e9}'; ".len() as u32;
    let second = "x = '\u{e9}'; helper()\n".len() as u32;
    assert_eq!(
        spans,
        vec![
            ("b.py", call, call + 6, 1, false),
            ("b.py", second, second + 6, 2, false),
            ("pkg/a.py", 4, 10, 1, true),
        ]
    );
    // Without the declaration.
    let query = ReferenceQuery {
        include_declaration: false,
        ..query
    };
    let value = json!([{"uri": "file:///ws/pkg/a.py", "range": range(0, 4, 10)}]);
    let mapped = map_locations(&value, &|u: &str| FakeUris.rel_of(u), &map, &query, "x");
    assert!(mapped.references.is_empty());
    assert!(mapped.complete);
    // Unknown query file: unsupported, not an error.
    let unknown = ReferenceQuery {
        path: "nope.py".into(),
        byte: 0,
        include_declaration: true,
    };
    assert!(query_references(&mut session, &FakeUris, &map, &unknown, "x")
        .unwrap()
        .is_none());
}
