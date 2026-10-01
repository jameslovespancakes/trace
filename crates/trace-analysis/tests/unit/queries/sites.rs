use super::*;
use trace_syntax::ArgumentView;

fn arg(position: Option<u32>, keyword: Option<&str>, text: &str) -> ArgumentView {
    ArgumentView {
        position,
        keyword: keyword.map(str::to_string),
        text: text.into(),
    }
}

fn symbol(kind: SymbolKind, params: &[&str]) -> Symbol {
    Symbol {
        id: SymbolId(0),
        uid: "a.py:f".into(),
        file: FileId(0),
        decl: 0,
        name: "f".into(),
        qualified_name: "f".into(),
        kind,
        language: trace_core::Language::Python,
        span: trace_core::model::Span::default(),
        name_span: trace_core::model::ByteSpan::default(),
        body_start: 0,
        parent: None,
        container: None,
        doc: None,
        decorators: Vec::new(),
        bases: Vec::new(),
        parameters: params.iter().map(|p| p.to_string()).collect(),
        execution: trace_core::model::ExecutionModel::default(),
        is_stub: false,
        is_test: false,
        declaration_lines: Vec::new(),
        semantic: true,
    }
}

#[test]
fn rule_carries_bind_arguments_to_parameters() {
    let view = SiteView {
        call: "obj.m(a, key=b)".into(),
        when: Vec::new(),
        guards: Vec::new(),
        arguments: vec![arg(Some(0), None, "a"), arg(None, Some("key"), "b"), arg(Some(1), None, "c")],
    };
    let method = symbol(SymbolKind::Method, &["self", "x", "y", "key"]);
    let pairs: Vec<(String, String)> = carries(&view, &method)
        .into_iter()
        .map(|c| (c.argument, c.parameter))
        .collect();
    assert_eq!(pairs, [("a".into(), "x".into()), ("b".into(), "key".into()), ("c".into(), "y".into())]);
    let function = symbol(SymbolKind::Function, &["p"]);
    let pairs = carries(&view, &function);
    assert_eq!(pairs.len(), 1, "extra positional and unknown keyword arguments bind nothing");
    assert_eq!(pairs[0].parameter, "p");
}
