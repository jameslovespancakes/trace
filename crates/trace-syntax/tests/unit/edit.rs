use super::*;
use trace_core::facts::{CallSite, Declaration, Expr};
use trace_core::model::{ByteSpan, ExecutionModel, Span, SymbolKind};

fn decl(name: &str, parent: Option<u32>) -> Declaration {
    Declaration {
        name: name.into(),
        qualified_name: name.into(),
        kind: SymbolKind::Function,
        span: Span::default(),
        name_span: ByteSpan::default(),
        body_start: 0,
        parent,
        container: None,
        doc: None,
        decorators: vec![],
        bases: vec![],
        parameters: vec![],
        execution: ExecutionModel::Ordinary,
        is_stub: false,
        is_test: false,
        declaration_lines: vec![],
        identifiers: vec![],
    }
}

fn call(owner: Option<u32>) -> CallSite {
    CallSite {
        owner,
        lexical_owner: owner,
        span: ByteSpan::default(),
        callee_span: ByteSpan::default(),
        callee: "f".into(),
        member: None,
        receiver: None,
        line: 1,
        activation: Default::default(),
        is_new: false,
        arg_count: 0,
    }
}

#[test]
fn removal_remaps_indices() {
    let mut facts = FileFacts {
        declarations: vec![decl("a", None), decl("a", None), decl("inner", Some(1)), decl("b", None)],
        calls: vec![call(Some(2)), call(Some(3))],
        flow: vec![
            FlowFact::Return {
                function: 3,
                value: Expr::Opaque,
            },
            FlowFact::Eval {
                scope: Scope::Decl(1),
                call: Expr::Opaque,
            },
        ],
        ..FileFacts::default()
    };
    remove_declarations(&mut facts, &[false, true, false, false]);
    let names: Vec<&str> = facts.declarations.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, vec!["a", "b"]);
    assert_eq!(facts.calls[0].owner, None);
    assert_eq!(facts.calls[0].lexical_owner, None);
    assert_eq!(facts.calls[1].owner, Some(1));
    assert_eq!(facts.flow.len(), 1);
    assert!(matches!(facts.flow[0], FlowFact::Return { function: 1, .. }));
}

#[test]
fn removal_remaps_synthetic_declarations() {
    use trace_core::facts::{AnonymousKind, AnonymousScope, Consumer};
    let lambda = |span: u32| Expr::Lambda {
        span: ByteSpan::new(span, span + 1),
        function: Some(2),
    };
    let mut facts = FileFacts {
        declarations: vec![decl("a", None), decl("a", None), decl("<lambda>", Some(0))],
        flow: vec![FlowFact::Bind {
            target: BindTarget::Var {
                scope: Scope::Decl(0),
                name: "g".into(),
            },
            value: lambda(5),
            scope: Scope::Decl(0),
        }],
        anonymous: vec![AnonymousScope {
            decl: 2,
            kind: AnonymousKind::Lambda,
            created_in: Some(0),
            consumer: Consumer::Bound,
            eager: None,
        }],
        ..FileFacts::default()
    };
    remove_declarations(&mut facts, &[false, true, false]);
    assert_eq!(facts.declarations.len(), 2);
    assert!(matches!(
        &facts.flow[0],
        FlowFact::Bind {
            value: Expr::Lambda {
                function: Some(1),
                ..
            },
            ..
        }
    ));
    assert_eq!(facts.anonymous[0].decl, 1);
    assert_eq!(facts.anonymous[0].created_in, Some(0));
    assert_eq!(facts.anonymous_of(1).map(|a| a.kind), Some(AnonymousKind::Lambda));
}
