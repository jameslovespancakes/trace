use super::*;
use crate::test_support::{index, sym};
use trace_core::{ByteSpan, SymbolKind};

#[test]
fn matching_byte_not_shared_line_determines_owner() {
    let mut first = sym(0, 0, "a.js", "first", SymbolKind::Function, None, None);
    let mut second = sym(1, 0, "a.js", "second", SymbolKind::Function, None, None);
    let mut module = sym(2, 0, "a.js", "<module>", SymbolKind::Module, None, None);
    for s in [&mut first, &mut second, &mut module] {
        s.span.start_line = 1;
        s.span.end_line = 1;
    }
    first.span.bytes = ByteSpan::new(0, 20);
    second.span.bytes = ByteSpan::new(40, 60);
    module.span.bytes = ByteSpan::new(0, 100);
    let index = index(&["a.js"], vec![first, second, module], &[]);
    assert_eq!(owner(&index, FileId(0), 50).as_deref(), Some("a.js:second"));
    assert_eq!(owner(&index, FileId(0), 80).as_deref(), Some("a.js:<module>"));
}
