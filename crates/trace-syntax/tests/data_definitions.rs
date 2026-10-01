use trace_core::{facts::FileFacts, Language};
use trace_syntax::{extract, SourceInput};

fn facts(source: &str) -> FileFacts {
    extract(SourceInput {
        path: "data.py",
        language: Language::Python,
        source: source.as_bytes(),
    })
    .unwrap()
}

#[test]
fn initialized_module_bindings_are_not_callable_declarations() {
    let source = "from other import value as imported\nVALUE = factory()\nalias = VALUE\nuninitialized: int\nclass C:\n    member = 1\n    def f(self):\n        local = 2\ndef outer():\n    global late\n    late = 3\nobj.attr = 4\nobj[0] = 5\n";
    let f = facts(source);
    assert_eq!(f.data_definitions.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["VALUE", "alias"]);
    assert_eq!(
        f.declarations
            .iter()
            .map(|d| d.qualified_name.as_str())
            .collect::<Vec<_>>(),
        ["C", "C.f", "outer", "<module>"]
    );
    assert!(f
        .calls
        .iter()
        .filter(|c| c.callee == "factory")
        .all(|c| c.owner.is_none()));
    assert!(f.data_definitions.iter().all(|d| !d.conditional));
}

#[test]
fn full_multiline_chained_and_nested_unpacking_spans() {
    let source = "first = second = build(\n    1,\n    2,\n)\n(a, (b, *rest)) = values\nx: list[int] = [\n    1, 2\n]\n";
    let f = facts(source);
    assert_eq!(
        f.data_definitions.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        ["first", "second", "a", "b", "rest", "x"]
    );
    assert_eq!(f.data_definitions[0].span, f.data_definitions[1].span);
    assert_eq!(
        &source[f.data_definitions[1].span.bytes.range()],
        "first = second = build(\n    1,\n    2,\n)"
    );
    let x = f.data_definitions.last().unwrap();
    assert_eq!((x.span.start_line, x.span.end_line), (6, 8));
    assert_eq!(&source[x.span.bytes.range()], "x: list[int] = [\n    1, 2\n]");
    for d in &f.data_definitions {
        assert_eq!(&source[d.name_span.range()], d.name);
    }
}

#[test]
fn conditional_and_reassigned_bindings_remain_distinct() {
    let source = "value = 1\nif enabled:\n    value = 2\nelse:\n    value = 3\nfor i in values:\n    other = i\nwith context():\n    resource = 4\nvalue += 1\n";
    let f = facts(source);
    assert_eq!(
        f.data_definitions.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        ["value", "value", "value", "other", "resource"]
    );
    assert!(!f.data_definitions[0].conditional);
    assert!(f.data_definitions[1..].iter().all(|d| d.conditional));
    assert_eq!(&source[f.data_definitions[1].span.bytes.range()], "value = 2");
}

#[test]
fn spans_preserve_unicode_and_crlf() {
    let source = "# café\r\nΔ = create(\r\n    'é',\r\n)\r\n";
    let f = facts(source);
    let d = &f.data_definitions[0];
    assert_eq!(d.name, "Δ");
    assert_eq!((d.span.start_line, d.span.end_line), (2, 4));
    assert_eq!(&source[d.span.bytes.range()], "Δ = create(\r\n    'é',\r\n)");
}

#[test]
fn lambdas_and_generator_locals_do_not_become_module_data() {
    let source = "callable_value = lambda: (inside := 1)\nvalues = [(local := x) for x in items]\n";
    let f = facts(source);
    assert_eq!(
        f.data_definitions.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        ["callable_value", "values"]
    );
    assert!(f.declarations.iter().all(|d| d.name.starts_with('<')));
}
