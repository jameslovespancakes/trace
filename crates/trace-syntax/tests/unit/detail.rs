use trace_core::facts::{ArgSlot, Expr, FileFacts};
use trace_core::Language;

use crate::{extract, SourceInput};

fn facts(path: &str, language: Language, src: &str) -> FileFacts {
    extract(SourceInput {
        path,
        language,
        source: src.as_bytes(),
    })
    .expect("extract")
}

#[test]
fn details_are_aligned_with_calls() {
    let src = "def f(a):\n    g(h(a), *a, k=1, **a)\n    obj.m()\n";
    let f = facts("m.py", Language::Python, src);
    assert_eq!(f.calls.len(), f.call_details.len());
    for (i, d) in f.call_details.iter().enumerate() {
        assert_eq!(d.call as usize, i);
        assert!(f.call_detail(i).is_some());
    }
}

#[test]
fn argument_slots_and_values() {
    let src = "def f(xs, fn):\n    g(xs, *xs, fn, key=fn, **opts)\n    self.store.save(1)\n    all(check(x) for x in xs)\n";
    let f = facts("m.py", Language::Python, src);
    let g = f.calls.iter().position(|c| c.callee == "g").unwrap();
    let slots: Vec<ArgSlot> = f.call_details[g].arguments.iter().map(|a| a.slot.clone()).collect();
    assert_eq!(
        slots,
        vec![
            ArgSlot::Positional {
                index: 0,
                exact: true
            },
            ArgSlot::Unpack,
            ArgSlot::Positional {
                index: 1,
                exact: false
            },
            ArgSlot::Keyword("key".to_string()),
            ArgSlot::UnpackKeywords,
        ]
    );
    let args = &f.call_details[g].arguments;
    assert!(matches!(&args[0].value, Expr::Name { name, .. } if name == "xs"));
    assert!(matches!(&args[1].value, Expr::Name { name, .. } if name == "xs"));
    assert_eq!(&src[args[3].span.range()], "fn");
    assert_eq!(&src[args[4].span.range()], "opts");

    let save = f.calls.iter().position(|c| c.callee == "self.store.save").unwrap();
    let receiver = f.call_details[save].receiver.as_ref().unwrap();
    assert!(matches!(receiver, Expr::Attr { attr, object, .. }
        if attr == "store" && matches!(object.as_ref(), Expr::Name { name, .. } if name == "self")));

    // more-itertools 01-nth-prime: the generator expression is argument 0 of `all`.
    let all = f.calls.iter().position(|c| c.callee == "all").unwrap();
    let args = &f.call_details[all].arguments;
    assert_eq!(args.len(), 1);
    assert_eq!(
        args[0].slot,
        ArgSlot::Positional {
            index: 0,
            exact: true
        }
    );
    let genexpr = f.declarations.iter().position(|d| d.name == "<genexpr>").unwrap() as u32;
    assert!(matches!(&args[0].value,
        Expr::Call { func, args: first, .. }
        if matches!(func.as_ref(), Expr::Lambda { function: Some(g), .. } if *g == genexpr)
            && matches!(first.as_slice(), [Expr::Name { name, .. }] if name == "xs")));
}

#[test]
fn nested_partial_inside_iter() {
    // more-itertools 04-intersperse: `iter(partial(take, n, it), [])`.
    let src = "from functools import partial\n\ndef take(n, it):\n    return []\n\ndef chunked(it, n):\n    return iter(partial(take, n, it), [])\n";
    let f = facts("m.py", Language::Python, src);
    let iter = f.calls.iter().position(|c| c.callee == "iter").unwrap();
    let d = &f.call_details[iter];
    assert_eq!(d.callee_path.as_deref(), Some("builtins.iter"));
    assert_eq!(d.arguments.len(), 2);
    let Expr::Call { func, args, span, .. } = &d.arguments[0].value else {
        panic!("partial call expected");
    };
    assert!(matches!(func.as_ref(), Expr::Name { name, .. } if name == "partial"));
    assert!(matches!(&args[0], Expr::Name { name, .. } if name == "take"));
    // The nested call is itself a call site with its own detail.
    let partial = f
        .calls
        .iter()
        .position(|c| c.span == *span)
        .expect("partial call site");
    assert_eq!(f.call_details[partial].callee_path.as_deref(), Some("functools.partial"));
    assert_eq!(f.call_details[partial].arguments.len(), 3);
}

#[test]
fn javascript_spread_and_receivers() {
    let src = "function f(xs) { g(...xs, 1); this.items.push(xs); }\n";
    let f = facts("a.js", Language::JavaScript, src);
    let g = f.calls.iter().position(|c| c.callee == "g").unwrap();
    let slots: Vec<ArgSlot> = f.call_details[g].arguments.iter().map(|a| a.slot.clone()).collect();
    assert_eq!(
        slots,
        vec![
            ArgSlot::Unpack,
            ArgSlot::Positional {
                index: 0,
                exact: false
            }
        ]
    );
    let push = f.calls.iter().position(|c| c.callee == "this.items.push").unwrap();
    assert!(matches!(f.call_details[push].receiver.as_ref(),
        Some(Expr::Attr { attr, .. }) if attr == "items"));
}
