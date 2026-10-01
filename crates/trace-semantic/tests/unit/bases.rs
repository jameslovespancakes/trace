use super::*;
use trace_core::facts::{Declaration, RefKind};
use trace_core::model::{ByteSpan, Span, SymbolKind};

fn decl(
    name: &str,
    kind: SymbolKind,
    name_span: (u32, u32),
    body_start: u32,
    bases: &[&str],
    parent: Option<u32>,
) -> Declaration {
    Declaration {
        name: name.to_string(),
        qualified_name: name.to_string(),
        kind,
        span: Span {
            bytes: ByteSpan::new(name_span.0.saturating_sub(6), body_start + 20),
            start_line: 1,
            end_line: 2,
        },
        name_span: ByteSpan::new(name_span.0, name_span.1),
        body_start,
        parent,
        container: None,
        doc: None,
        decorators: Vec::new(),
        bases: bases.iter().map(|b| b.to_string()).collect(),
        parameters: Vec::new(),
        execution: Default::default(),
        is_stub: false,
        is_test: false,
        declaration_lines: Vec::new(),
        identifiers: Vec::new(),
    }
}

fn reference(name: &str, start: u32) -> Reference {
    Reference {
        span: ByteSpan::new(start, start + name.len() as u32),
        name: name.to_string(),
        owner: None,
        in_decorator: false,
        local: false,
        kind: RefKind::Read,
    }
}

/// `class Client(lib.Base[T], Mixin, metaclass=Meta):` - the base name identifiers are
/// asked; generic arguments and keyword arguments are not bases.
#[test]
fn rule_library_base_is_a_header_base_identifier() {
    let mut facts = FileFacts::default();
    facts
        .declarations
        .push(decl("Client", SymbolKind::Class, (6, 12), 60, &["lib.Base[T]", "Mixin"], None));
    assert!(is_header_base(&facts, &reference("Base", 17)));
    assert!(is_header_base(&facts, &reference("Mixin", 26)));
    assert!(!is_header_base(&facts, &reference("T", 22)), "generic argument");
    assert!(!is_header_base(&facts, &reference("Meta", 45)), "keyword argument");
    assert!(!is_header_base(&facts, &reference("Base", 70)), "inside the body");
}

/// A class declared inside a function is part of that function's reuse unit: its bases
/// are not asked (an incremental run could not reproduce the answer).
#[test]
fn rule_library_base_outside_callables_only() {
    let mut facts = FileFacts::default();
    facts
        .declarations
        .push(decl("make", SymbolKind::Function, (4, 8), 12, &[], None));
    facts
        .declarations
        .push(decl("Local", SymbolKind::Class, (26, 31), 40, &["Base"], Some(0)));
    assert!(!is_header_base(&facts, &reference("Base", 32)));
}
