use super::*;
use crate::{extract, SourceInput};

fn py(path: &str, src: &str) -> FileFacts {
    let f = extract(SourceInput {
        path,
        language: Language::Python,
        source: src.as_bytes(),
    })
    .expect("extract");
    crate::test_support::strip_module(f, src.len())
}

#[test]
fn decorators_docstrings_and_spans() {
    let src = "import functools\n\n# helper comment\ndef helper():\n    pass\n\n@functools.cache\n@route('/x')\nasync def view(request, *args, key=None, **kw):\n    \"\"\"Render the view.\n\n        Details here.\n    \"\"\"\n    await render(request)\n\nclass Session(Base, metaclass=Meta):\n    '''A session.'''\n    def login(self, user):\n        return self.store.load(user)\n";
    let f = py("src/auth.py", src);
    let names: Vec<&str> = f.declarations.iter().map(|d| d.qualified_name.as_str()).collect();
    assert_eq!(names, vec!["helper", "view", "Session", "Session.login"]);
    let helper = &f.declarations[0];
    assert_eq!(helper.doc.as_deref(), Some("# helper comment"));
    let view = &f.declarations[1];
    assert_eq!(&src[view.span.bytes.range()][..16], "@functools.cache");
    assert_eq!(view.decorators, vec!["functools.cache".to_string(), "route('/x')".to_string()]);
    assert_eq!(view.doc.as_deref(), Some("Render the view.\n\nDetails here."));
    assert_eq!(view.execution, ExecutionModel::Coroutine);
    let kinds: Vec<(&str, trace_core::facts::ParamKind)> =
        view.parameters.iter().map(|p| (p.name.as_str(), p.kind)).collect();
    use trace_core::facts::ParamKind::*;
    assert_eq!(
        kinds,
        vec![
            ("request", Positional),
            ("args", VarPositional),
            ("key", KeywordOnly),
            ("kw", VarKeyword)
        ]
    );
    let session = &f.declarations[2];
    assert_eq!(session.bases, vec!["Base".to_string()]);
    assert_eq!(session.doc.as_deref(), Some("A session."));
    let login = &f.declarations[3];
    assert_eq!(login.kind, SymbolKind::Method);
    assert_eq!(login.parent, Some(2));
    // Decorator expressions are references executed by the enclosing scope.
    let deco = f.references.iter().find(|r| r.name == "route").unwrap();
    assert!(deco.in_decorator);
    assert_eq!(deco.owner, None);
    let render = f.calls.iter().find(|c| c.callee == "render").unwrap();
    assert_eq!(render.owner, Some(1));
    assert_eq!(render.activation, trace_core::facts::Activation::Await);
    let route = f.calls.iter().find(|c| c.callee == "route").unwrap();
    assert_eq!(route.owner, None);
    assert_eq!(route.lexical_owner, None);
}

#[test]
fn overload_groups_collapse_onto_the_implementation() {
    let src = "from typing import overload\n\n@overload\ndef nth(x: int) -> int: ...\n@overload\ndef nth(x: int, d: int) -> int: ...\ndef nth(x, d=None):\n    return inner(x)\n";
    let f = py("pkg/b.py", src);
    assert_eq!(f.declarations.len(), 1);
    let nth = &f.declarations[0];
    assert_eq!(nth.declaration_lines, vec![4, 6, 7]);
    assert!(!nth.is_stub);
    let call = f.calls.iter().find(|c| c.callee == "inner").unwrap();
    assert_eq!(call.owner, Some(0));

    // Rebinding the alias disables the rule.
    let rebound = format!("{src}overload = None\n");
    assert_eq!(py("pkg/b.py", &rebound).declarations.len(), 3);

    // Without an implementation (a stub file) the group collapses onto the last overload.
    let stub = "from typing import overload\n@overload\ndef nth(x: int) -> int: ...\n@overload\ndef nth(x: int, d: int) -> int: ...\n";
    let f = py("pkg/b.pyi", stub);
    assert_eq!(f.declarations.len(), 1);
    assert_eq!(f.declarations[0].declaration_lines, vec![3, 5]);
    assert!(f.declarations[0].is_stub);

    let aliased = "import typing as t\n@t.overload\ndef g(x: int) -> int: ...\ndef g(x):\n    return x\n";
    assert_eq!(py("m.py", aliased).declarations.len(), 1);
}

#[test]
fn stubs_and_execution_models() {
    let src = "import abc\nfrom typing import Protocol\n\nclass Reader(Protocol):\n    def read(self) -> bytes: ...\n\nclass Base(abc.ABC):\n    @abc.abstractmethod\n    def run(self):\n        \"\"\"Run.\"\"\"\n        pass\n    def tell(self):\n        raise NotImplementedError\n\ndef gen():\n    yield 1\n\nasync def agen():\n    yield 1\n\ndef outer():\n    def inner():\n        yield 2\n    return inner\n";
    let f = py("m.py", src);
    let get = |n: &str| f.declarations.iter().find(|d| d.qualified_name == n).unwrap();
    assert!(get("Reader.read").is_stub);
    assert!(get("Base.run").is_stub);
    assert!(!get("Base.tell").is_stub);
    assert_eq!(get("gen").execution, ExecutionModel::Generator);
    assert_eq!(get("agen").execution, ExecutionModel::AsyncGenerator);
    assert_eq!(get("outer").execution, ExecutionModel::Ordinary);
    assert_eq!(get("outer.inner").execution, ExecutionModel::Generator);
}

#[test]
fn lazy_scopes_and_activation() {
    let src = "def run(items):\n    for x in produce():\n        pass\n    yield from chain()\n    total = sum(f(i) for i in source())\n    later = lambda: deferred()\n    return total\n";
    let f = py("m.py", src);
    let owner = |callee: &str| f.calls.iter().find(|c| c.callee == callee).unwrap().owner;
    let act = |callee: &str| f.calls.iter().find(|c| c.callee == callee).unwrap().activation;
    use trace_core::facts::{Activation, AnonymousKind, ArgSlot, Consumer};
    assert_eq!(act("produce"), Activation::Iterate);
    assert_eq!(act("chain"), Activation::Iterate);
    let names: Vec<&str> = f.declarations.iter().map(|d| d.qualified_name.as_str()).collect();
    assert_eq!(names, vec!["run", "run.<genexpr>", "run.<lambda>"]);
    assert_eq!(owner("source"), Some(0));
    assert_eq!(owner("f"), Some(1));
    assert_eq!(owner("sum"), Some(0));
    assert_eq!(owner("deferred"), Some(2));
    assert_eq!(f.declarations[1].execution, ExecutionModel::Generator);
    assert_eq!(f.declarations[1].parent, Some(0));
    let sum = f.calls.iter().position(|c| c.callee == "sum").unwrap() as u32;
    let genexpr = f.anonymous_of(1).unwrap();
    assert_eq!(genexpr.kind, AnonymousKind::GeneratorExpression);
    assert_eq!(genexpr.created_in, Some(0));
    assert_eq!(
        genexpr.consumer,
        Consumer::Argument {
            call: sum,
            slot: ArgSlot::Positional {
                index: 0,
                exact: true
            }
        }
    );
    assert_eq!(&src[genexpr.eager.unwrap().range()], "source()");
    let lambda = f.anonymous_of(2).unwrap();
    assert_eq!(lambda.kind, AnonymousKind::Lambda);
    assert_eq!(lambda.consumer, Consumer::Bound);
    assert_eq!(&src[f.declarations[2].name_span.range()], "lambda");
    assert!(f.declarations[2].identifiers.is_empty());
    // Callback / keyword evidence.
    let src = "def run():\n    first_true([1], pred=check, *extra)\n    obj.attr = 1\n";
    let f = py("m.py", src);
    let cbs: Vec<&str> = f.callbacks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(cbs, vec!["check", "extra"]);
    let kinds: Vec<(&str, trace_core::facts::AssignmentKind)> =
        f.assignments.iter().map(|a| (a.target.as_str(), a.kind)).collect();
    use trace_core::facts::AssignmentKind;
    assert!(kinds.contains(&("pred", AssignmentKind::Keyword)));
    assert!(kinds.contains(&("attr", AssignmentKind::Attribute)));
}

#[test]
fn expression_contexts() {
    assert!(is_protocol_base("typing.Protocol"));
    assert!(is_protocol_base("Protocol[T]"));
    assert!(!is_protocol_base("ProtocolBase"));
    assert_eq!(cleandoc("  First.\n\n      Indented.\n    Less.\n  "), "First.\n\n  Indented.\nLess.");
}

#[test]
fn anonymous_scope_consumers() {
    use trace_core::facts::{ArgSlot, Consumer};
    let src = "def run(xs, sentinel):
    for y in (norm(x) for x in xs):
        pass
    it = iter(lambda: read(), sentinel)
    ordered = sorted(xs, key=lambda k: rank(k))
    (lambda: now())()
    yield from (a for a in xs)
    return [*(b for b in xs)]

async def agen(xs):
    return (await fetch(x) async for x in xs)
";
    let f = py("m.py", src);
    let consumer = |i: usize| f.anonymous[i].consumer.clone();
    let call = |callee: &str| f.calls.iter().position(|c| c.callee == callee).unwrap() as u32;
    assert_eq!(f.anonymous.len(), 7);
    assert_eq!(consumer(0), Consumer::Iterated);
    assert_eq!(
        consumer(1),
        Consumer::Argument {
            call: call("iter"),
            slot: ArgSlot::Positional {
                index: 0,
                exact: true
            }
        }
    );
    assert_eq!(
        consumer(2),
        Consumer::Argument {
            call: call("sorted"),
            slot: ArgSlot::Keyword("key".to_string())
        }
    );
    assert!(matches!(consumer(3), Consumer::Called { .. }));
    assert_eq!(consumer(4), Consumer::Iterated);
    assert_eq!(consumer(5), Consumer::Iterated);
    assert_eq!(consumer(6), Consumer::Returned);
    // Calls inside each body are owned by the synthetic declaration.
    let owner = |callee: &str| f.calls.iter().find(|c| c.callee == callee).unwrap().owner;
    for (callee, i) in [("norm", 0), ("read", 1), ("rank", 2), ("now", 3), ("fetch", 6)] {
        assert_eq!(owner(callee), Some(f.anonymous[i].decl), "{callee}");
    }
    let agen = f.anonymous[6].decl as usize;
    assert_eq!(f.declarations[agen].execution, ExecutionModel::AsyncGenerator);
    // The iterated generator is still consumed by the enclosing function.
    assert_eq!(f.anonymous[0].created_in, Some(0));
}

#[test]
fn overload_removal_keeps_synthetic_indices_consistent() {
    let src = "from typing import overload

@overload
def f(x: int) -> int: ...
@overload
def f(x: str) -> str: ...
def f(x):
    return g(lambda: x)
";
    let f = py("m.py", src);
    let names: Vec<&str> = f.declarations.iter().map(|d| d.qualified_name.as_str()).collect();
    assert_eq!(names, vec!["f", "f.<lambda>"]);
    assert_eq!(f.anonymous.len(), 1);
    assert_eq!(f.anonymous[0].decl, 1);
    assert_eq!(f.anonymous[0].created_in, Some(0));
    let g = f.calls.iter().position(|c| c.callee == "g").unwrap();
    assert!(matches!(
        &f.call_details[g].arguments[0].value,
        trace_core::facts::Expr::Lambda {
            function: Some(1),
            ..
        }
    ));
}
