use super::*;
use crate::inventory::canonical_root;
use crate::test_support::index::index_with;
use crate::test_support::Fixture;

#[test]
fn exact_bytes_and_change_detection() {
    let fx = Fixture::new("source");
    let text = "\u{FEFF}def f(x):\r\n    return x\r\n";
    fx.write("repo/a.py", text);
    let root = canonical_root(&fx.path("repo")).unwrap();
    let mut index = index_with(&["f"], &[]);
    index.header.root = root.to_string_lossy().into_owned();
    index.files[0].hash = Hash32::of(text.as_bytes());
    let sym = &mut index.symbols[0];
    sym.span.bytes = ByteSpan::new(0, text.len() as u32);
    // Body starts at `return`: BOM (3) + "def f(x):\r\n" (11) + 4 spaces.
    sym.body_start = 18;

    let store = SourceStore::new(&index);
    assert_eq!(store.symbol_source(SymbolId(0)).unwrap(), text);
    assert_eq!(store.signature(SymbolId(0)).unwrap(), "\u{FEFF}def f(x):");
    assert_eq!(store.line(FileId(0), 2).unwrap(), "    return x");
    assert_eq!(store.text(FileId(0), ByteSpan::new(3, 6)).unwrap(), "def");
    // `f` (byte 7) is column 5 of line 1 (BOM not counted); `return` starts line 2 col 5.
    assert_eq!(store.line_at(FileId(0), 7).unwrap(), (1, 5, "def f(x):".to_string()));
    assert_eq!(store.line_at(FileId(0), 18).unwrap(), (2, 5, "    return x".to_string()));

    // A modified file is refused, never shown.
    fx.write("repo/a.py", "def f(x):\n    return 2\n");
    let fresh = SourceStore::new(&index);
    assert!(matches!(fresh.symbol_source(SymbolId(0)), Err(CoreError::SourceChanged(_))));
    assert!(matches!(
        read_verified(&root, "../a.py", &index.files[0].hash),
        Err(CoreError::InvalidRelativePath(_))
    ));
}
