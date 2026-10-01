use trace_core::facts::{BindTarget, Expr, FlowFact, ImplicitKind, Scope};
use trace_core::Language;

use crate::{extract, SourceInput};

fn flow(src: &str) -> trace_core::facts::FileFacts {
    let f = extract(SourceInput {
        path: "f.py",
        language: Language::Python,
        source: src.as_bytes(),
    })
    .expect("extract");
    crate::test_support::strip_module(f, src.len())
}

#[test]
fn defaults_self_and_decorators() {
    let src = "def make_key(args):\n    return args\n\nclass Cached:\n    def __init__(self, func, key=None):\n        self.func = func\n        self.key_func = key or make_key\n\n    @classmethod\n    def build(cls, tb):\n        return cls(tb)\n\n    @staticmethod\n    def util(x):\n        return x\n";
    let f = flow(src);
    let init = f
        .declarations
        .iter()
        .position(|d| d.qualified_name == "Cached.__init__")
        .unwrap() as u32;
    let cached = f.declarations.iter().position(|d| d.name == "Cached").unwrap() as u32;
    let build = f.declarations.iter().position(|d| d.name == "build").unwrap() as u32;
    let util = f.declarations.iter().position(|d| d.name == "util").unwrap() as u32;
    assert!(f.flow.iter().any(|x| matches!(x,
        FlowFact::Bind { target: BindTarget::Var { scope: Scope::Decl(s), name }, scope: Scope::Module, .. }
        if *s == init && name == "key")));
    assert!(f.flow.iter().any(|x| matches!(x,
        FlowFact::ImplicitSelf { function, param, class, is_class: false }
        if *function == init && param == "self" && *class == cached)));
    assert!(f.flow.iter().any(|x| matches!(x,
        FlowFact::ImplicitSelf { function, is_class: true, .. } if *function == build)));
    assert!(!f.flow.iter().any(|x| matches!(x,
        FlowFact::ImplicitSelf { function, .. } if *function == util)));
    assert!(f.flow.iter().any(|x| matches!(x,
        FlowFact::Decorated { function, target: BindTarget::Member { class, .. }, .. }
        if *function == build && *class == cached)));
    // self.key_func = key or make_key
    assert!(f.flow.iter().any(|x| matches!(x,
        FlowFact::Bind { target: BindTarget::FieldOf { name, .. }, value: Expr::Choice(alts), scope: Scope::Decl(s) }
        if name == "key_func" && alts.len() == 2 && *s == init)));
    assert!(f.flow.iter().any(|x| matches!(x,
        FlowFact::Return { function, value: Expr::Call { .. } } if *function == build)));
}

#[test]
fn class_members_and_implicit_operations() {
    let src = "class Info:\n    point_type = Callpoint\n\nclass LRU(Store):\n    def fill(self, key):\n        self[key] = 1\n        with self.lock:\n            for x in self.items:\n                del self[x]\n        return self[key]\n";
    let f = flow(src);
    let info = f.declarations.iter().position(|d| d.name == "Info").unwrap() as u32;
    let fill = f.declarations.iter().position(|d| d.name == "fill").unwrap() as u32;
    assert!(f.flow.iter().any(|x| matches!(x,
        FlowFact::Bind { target: BindTarget::Member { class, name }, scope: Scope::Module, .. }
        if *class == info && name == "point_type")));
    assert!(f.flow.iter().any(|x| matches!(x,
        FlowFact::Bind { target: BindTarget::Field { name }, .. } if name == "point_type")));
    let kinds: Vec<ImplicitKind> = f
        .implicit
        .iter()
        .filter(|op| op.scope == Scope::Decl(fill))
        .map(|op| op.kind)
        .collect();
    for k in [
        ImplicitKind::SubscriptStore,
        ImplicitKind::WithEnter,
        ImplicitKind::Iterate,
        ImplicitKind::SubscriptDelete,
        ImplicitKind::SubscriptLoad,
        ImplicitKind::DescriptorGet,
    ] {
        assert!(kinds.contains(&k), "{k:?} missing from {kinds:?}");
    }
    // Store / delete targets are not loads.
    let loads = kinds.iter().filter(|k| **k == ImplicitKind::SubscriptLoad).count();
    assert_eq!(loads, 1);
}

#[test]
fn lambdas_are_synthetic_function_scopes() {
    let src = "def f():\n    g = lambda: helper()\n    return g\n";
    let f = flow(src);
    assert_eq!(f.declarations[1].name, "<lambda>");
    // The lambda body is a statement of the lambda, not of `f`.
    assert!(f.flow.iter().any(|x| matches!(
        x,
        FlowFact::Eval {
            scope: Scope::Decl(1),
            call: Expr::Call { .. }
        }
    )));
    assert!(!f.flow.iter().any(|x| matches!(
        x,
        FlowFact::Eval {
            scope: Scope::Decl(0),
            ..
        }
    )));
    assert!(f.flow.iter().any(|x| matches!(
        x,
        FlowFact::Return {
            function: 1,
            value: Expr::Call { .. }
        }
    )));
    assert!(f.flow.iter().any(|x| matches!(x,
        FlowFact::Bind { target: BindTarget::Var { scope: Scope::Decl(0), name }, value: Expr::Lambda { function: Some(1), .. }, .. }
        if name == "g")));
    let call = f.calls.iter().find(|c| c.callee == "helper").unwrap();
    assert_eq!(call.owner, Some(1));
    assert_eq!(call.lexical_owner, Some(1));
}

#[test]
fn lambda_passed_as_callback_flows_into_the_call() {
    // server 10-compaction: `settle_even_if_abandoned(lambda: record_compaction_usage(...))`.
    let src = "def settle_even_if_abandoned(fn):\n    return fn()\n\n\
               def record_compaction_usage(n):\n    pass\n\n\
               async def compact(n):\n    settle_even_if_abandoned(lambda: record_compaction_usage(n))\n";
    let f = flow(src);
    let compact = f.declarations.iter().position(|d| d.name == "compact").unwrap() as u32;
    let lambda = f.declarations.iter().position(|d| d.name == "<lambda>").unwrap() as u32;
    assert_eq!(f.declarations[lambda as usize].parent, Some(compact));
    assert_eq!(f.declarations[lambda as usize].qualified_name, "compact.<lambda>");
    // The call passes the lambda value (flow into parameter `fn`).
    assert!(f.flow.iter().any(|x| matches!(x,
        FlowFact::Eval { scope: Scope::Decl(s), call: Expr::Call { args, .. } }
        if *s == compact
            && matches!(args.as_slice(), [Expr::Lambda { function: Some(l), .. }] if *l == lambda))));
    // The body call belongs to the lambda.
    let record = f
        .calls
        .iter()
        .find(|c| c.callee == "record_compaction_usage")
        .unwrap();
    assert_eq!(record.owner, Some(lambda));
    assert!(f.flow.iter().any(|x| matches!(x,
        FlowFact::Return { function, .. } if *function == lambda)));
}

#[test]
fn generator_expression_bodies_are_their_own_scope() {
    let src = "def nth_prime(n, primes):\n    return all(_strong_probable_prime(n, b) for b in bases(primes) if b > 1)\n";
    let f = flow(src);
    let g = f.declarations.iter().position(|d| d.name == "<genexpr>").unwrap() as u32;
    // `bases(primes)` (first iterable) runs in nth_prime; the element in the generator.
    assert!(f.flow.iter().any(|x| matches!(x,
        FlowFact::Eval { scope: Scope::Decl(0), call: Expr::Call { func, .. } }
        if matches!(func.as_ref(), Expr::Name { name, .. } if name == "bases"))));
    assert!(f.flow.iter().any(|x| matches!(x,
        FlowFact::Eval { scope: Scope::Decl(s), call: Expr::Call { func, .. } }
        if *s == g && matches!(func.as_ref(), Expr::Name { name, .. } if name == "_strong_probable_prime"))));
    // `all(<genexpr>)` receives the generator object created from the first iterable.
    assert!(f.flow.iter().any(|x| matches!(x,
        FlowFact::Return { function: 0, value: Expr::Call { args, .. } }
        if matches!(args.as_slice(), [Expr::Call { func, .. }]
            if matches!(func.as_ref(), Expr::Lambda { function: Some(l), .. } if *l == g)))));
}

#[test]
fn lambdas_in_defaults_and_decorators_are_lowered() {
    let src = "def f(key=lambda x: norm(x)):\n    return key\n\n@register(check=lambda: probe())\ndef g():\n    pass\n\nclass C:\n    area = lambda self: self.w\n";
    let f = flow(src);
    let lambdas: Vec<u32> = f
        .declarations
        .iter()
        .enumerate()
        .filter(|(_, d)| d.name == "<lambda>")
        .map(|(i, _)| i as u32)
        .collect();
    assert_eq!(lambdas.len(), 3);
    for l in &lambdas {
        assert!(f
            .flow
            .iter()
            .any(|x| matches!(x, FlowFact::Return { function, .. } if function == l)));
    }
    // Only the class-body-bound lambda receives the instance.
    let class = f.declarations.iter().position(|d| d.name == "C").unwrap() as u32;
    let self_facts: Vec<u32> = f
        .flow
        .iter()
        .filter_map(|x| match x {
            FlowFact::ImplicitSelf {
                function, class: c, ..
            } if *c == class => Some(*function),
            _ => None,
        })
        .collect();
    assert_eq!(self_facts, vec![lambdas[2]]);
    // The default is bound to the parameter as the lambda value.
    assert!(f.flow.iter().any(|x| matches!(x,
        FlowFact::Bind { target: BindTarget::Var { scope: Scope::Decl(0), name }, value: Expr::Lambda { function: Some(l), .. }, scope: Scope::Module }
        if name == "key" && *l == lambdas[0])));
    // The decorator lambda's body is not a module statement.
    assert!(!f.flow.iter().any(|x| matches!(x,
        FlowFact::Eval { scope: Scope::Module, call: Expr::Call { func, .. } }
        if matches!(func.as_ref(), Expr::Name { name, .. } if name == "probe"))));
}

#[test]
fn javascript_this_receiver() {
    let src = "class A {\n  run() { return this.helper(); }\n  static make() { return new A(); }\n}\n";
    let f = extract(SourceInput {
        path: "a.js",
        language: Language::JavaScript,
        source: src.as_bytes(),
    })
    .unwrap();
    assert!(f.flow.iter().any(|x| matches!(x,
        FlowFact::ImplicitSelf { function: 1, param, class: 0, is_class: false } if param == "this")));
    assert!(f.flow.iter().any(|x| matches!(
        x,
        FlowFact::ImplicitSelf {
            function: 2,
            is_class: true,
            ..
        }
    )));
    assert!(f.flow.iter().any(|x| matches!(
        x,
        FlowFact::Return {
            function: 2,
            value: Expr::Call { is_new: true, .. }
        }
    )));
    assert!(f.implicit.is_empty());
}

/// Go composite literals of a named type are allocations of that type (also as a call
/// receiver, `Error{..}.Error()`); slice and map literals stay opaque.
#[test]
fn go_composite_literals_allocate_their_named_type() {
    let src = "package p\n\ntype Error struct{ Code int }\n\nfunc (e Error) Error() string { return \"\" }\n\nfunc f() string {\n\tx := Error{Code: 1}\n\ty := []int{1}\n\tz := pkg.T{}\n\t_, _ = y, z\n\treturn Error{Code: x.Code}.Error()\n}\n";
    let f = extract(SourceInput {
        path: "p.go",
        language: Language::Go,
        source: src.as_bytes(),
    })
    .unwrap();
    let bound = |var: &str| {
        f.flow.iter().find_map(|x| match x {
            FlowFact::Bind {
                target: BindTarget::Var { name, .. },
                value,
                ..
            } if name == var => Some(value.clone()),
            _ => None,
        })
    };
    assert!(matches!(bound("x"), Some(Expr::Call { is_new: true, ref func, .. })
        if matches!(func.as_ref(), Expr::Name { name, .. } if name == "Error")));
    assert!(matches!(bound("y"), Some(Expr::Opaque) | None));
    assert!(matches!(bound("z"), Some(Expr::Call { is_new: true, ref func, .. })
        if matches!(func.as_ref(), Expr::Name { name, .. } if name == "pkg.T")));
    let ci = f
        .calls
        .iter()
        .position(|c| c.member.as_deref() == Some("Error"))
        .expect("call");
    let receiver = f.call_detail(ci).and_then(|d| d.receiver.as_ref());
    assert!(matches!(receiver, Some(Expr::Call { is_new: true, func, .. })
        if matches!(func.as_ref(), Expr::Name { name, .. } if name == "Error")));
}

fn facts_of(path: &str, language: Language, src: &str) -> trace_core::facts::FileFacts {
    extract(SourceInput {
        path,
        language,
        source: src.as_bytes(),
    })
    .expect("extract")
}

/// Library mode: subscript stores keep their key, subscript reads are container reads,
/// loops bind their targets, string literals are values; the index facts are unchanged.
#[test]
fn rule_library_lowering_models_subscripts_loops_and_literals() {
    use crate::lower::{lower_library, LibraryOp, INDEX_READ};
    let src = "class R:\n    def add(self, key, fn):\n        self.m[key] = fn\n\n    def run(self, key):\n        for h in self.hs:\n            h()\n        return self.m[key](\"GET\")\n";
    let facts = facts_of("r.py", Language::Python, src);
    let low = lower_library(Language::Python, src.as_bytes(), &facts).expect("library lowering");
    assert!(low.ops.iter().any(|o| matches!(o,
        LibraryOp::IndexStore { key: Expr::Name { name, .. }, value: Expr::Name { name: v, .. }, .. }
        if name == "key" && v == "fn")));
    assert!(low.ops.iter().any(|o| matches!(o,
        LibraryOp::Iterate { targets, iterable: Expr::Attr { attr, .. }, .. }
        if targets == &vec!["h".to_string()] && attr == "hs")));
    assert!(low.flow.iter().any(|f| matches!(f,
        FlowFact::Return { value: Expr::Call { func, args, .. }, .. }
        if matches!(func.as_ref(), Expr::Call { func: inner, .. }
            if matches!(inner.as_ref(), Expr::Attr { attr, .. } if attr == INDEX_READ))
        && matches!(args.as_slice(), [Expr::Name { name, .. }] if name == "<lit>GET"))));
    // Index facts: the subscript callee stays opaque.
    assert!(facts.flow.iter().any(|f| matches!(f,
        FlowFact::Return { value: Expr::Call { func, .. }, .. } if matches!(func.as_ref(), Expr::Opaque))));
}

/// Java annotation types with their meta-annotations and element aliases.
#[test]
fn rule_annotation_types_read_meta_annotations() {
    let src = "@Target(ElementType.METHOD)\n@Mapped(method = Verb.GET)\npublic @interface GetRoute {\n  @Alias(annotation = Mapped.class)\n  String[] value() default {};\n}\n";
    let types = crate::lower::annotation_types(Language::Java, src.as_bytes());
    assert_eq!(types.len(), 1);
    let t = &types[0];
    assert_eq!(t.name, "GetRoute");
    assert!(t.annotations.iter().any(
        |a| a.name == "Mapped" && a.values == vec![(Some("method".to_string()), "Verb.GET".to_string())]
    ));
    assert_eq!(t.elements.len(), 1);
    assert_eq!(t.elements[0].name, "value");
    assert!(t.elements[0].annotations.iter().any(|a| a.name == "Alias"));
}

fn yielded_values(f: &trace_core::facts::FileFacts, function: u32) -> Vec<Expr> {
    f.flow
        .iter()
        .filter_map(|x| match x {
            FlowFact::Bind {
                target:
                    BindTarget::Var {
                        scope: Scope::Decl(s),
                        name,
                    },
                value,
                ..
            } if *s == function && name == crate::lower::YIELDED => Some(value.clone()),
            _ => None,
        })
        .collect()
}

/// `yield v` binds `v` to the generator's yielded values; `yield from` delegates
/// (iterates) and binds nothing.
#[test]
fn rule_generator_yields_bind_the_yielded_values() {
    let src = "def fx():\n    app = make()\n    yield app\n\ndef chain():\n    yield from fx()\n";
    let f = flow(src);
    let fx = f.declarations.iter().position(|d| d.name == "fx").unwrap() as u32;
    let chain = f.declarations.iter().position(|d| d.name == "chain").unwrap() as u32;
    let values = yielded_values(&f, fx);
    assert!(matches!(values.as_slice(), [Expr::Name { name, .. }] if name == "app"), "{values:?}");
    assert!(yielded_values(&f, chain).is_empty());
}

/// `with e as x` binds `x` to `e.__enter__()` (Python language rule), so a generator
/// yielding the target yields the entered context.
#[test]
fn rule_with_target_receives_the_enter_result() {
    let src = "def fx(app):\n    with app.app_context() as ctx:\n        yield ctx\n";
    let f = flow(src);
    let fx = f.declarations.iter().position(|d| d.name == "fx").unwrap() as u32;
    assert!(f.flow.iter().any(|x| matches!(x,
        FlowFact::Bind { target: BindTarget::Var { scope: Scope::Decl(s), name }, value: Expr::Call { func, .. }, .. }
        if *s == fx && name == "ctx" && matches!(func.as_ref(), Expr::Attr { attr, .. } if attr == "__enter__"))));
    let values = yielded_values(&f, fx);
    assert!(matches!(values.as_slice(), [Expr::Name { name, .. }] if name == "ctx"), "{values:?}");
}

/// String keyword arguments of a decorator call are literal values of the decoration
/// (`@provider(name="client")`); other keywords keep their lowering.
#[test]
fn rule_decorator_string_keywords_are_literal_values() {
    let src = "@fixture(name=\"client\", scope=session)\ndef _client():\n    return 1\n";
    let f = flow(src);
    let decorators = f
        .flow
        .iter()
        .find_map(|x| match x {
            FlowFact::Decorated { decorators, .. } => Some(decorators.clone()),
            _ => None,
        })
        .expect("decorated");
    let Some(Expr::Call { kwargs, .. }) = decorators.first() else {
        panic!("decorator call: {decorators:?}");
    };
    assert!(
        kwargs
            .iter()
            .any(|(k, v)| k == "name" && matches!(v, Expr::Name { name, .. } if name == "<lit>client")),
        "{kwargs:?}"
    );
    assert!(kwargs
        .iter()
        .any(|(k, v)| k == "scope" && matches!(v, Expr::Name { name, .. } if name == "session")));
}

/// JavaScript object literals are plain-object values whose keyed entries (pairs,
/// shorthand names, methods) are members; computed keys and spreads are left out.
#[test]
fn rule_object_literals_are_values_with_keyed_members() {
    let src = "const o = { a: f, b, [k]: g, ...rest, m() { return 1; } };\n";
    let f = facts_of("o.js", Language::JavaScript, src);
    let m = f.declarations.iter().position(|d| d.name == "m").unwrap() as u32;
    let value = f
        .flow
        .iter()
        .find_map(|x| match x {
            FlowFact::Bind {
                target: BindTarget::Var { name, .. },
                value,
                ..
            } if name == "o" => Some(value.clone()),
            _ => None,
        })
        .expect("binding of o");
    let Expr::Call { func, kwargs, .. } = &value else {
        panic!("object value: {value:?}");
    };
    assert!(matches!(func.as_ref(), Expr::Name { name, .. } if name == crate::lower::OBJECT_CALLEE));
    let keys: Vec<&str> = kwargs.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, vec!["a", "b", "m"]);
    assert!(matches!(&kwargs[1].1, Expr::Name { name, .. } if name == "b"));
    assert!(matches!(&kwargs[2].1, Expr::Lambda { function: Some(d), .. } if *d == m));
    // Call details keep the plain IR (object literals stay opaque there).
    let g = facts_of("p.js", Language::JavaScript, "use({ a: f });\n");
    let detail = g.call_detail(0).expect("detail");
    assert!(detail.arguments.iter().all(|a| matches!(a.value, Expr::Opaque)));
}

fn member_stores(f: &trace_core::facts::FileFacts) -> Vec<(String, Expr)> {
    f.flow
        .iter()
        .filter_map(|x| match x {
            FlowFact::Bind {
                target: BindTarget::FieldOf { name, .. },
                value,
                ..
            } => Some((name.clone(), value.clone())),
            _ => None,
        })
        .collect()
}

/// `LIST.forEach(function (m) { o[m] = .. })` and `for (const m of LIST) o[m] = ..` over a
/// literal list of strings (inline or bound once in the file) define each listed member;
/// an unknown key defines nothing.
#[test]
fn rule_computed_store_over_literal_list_defines_each_member() {
    let src = "var methods = ['get', 'post'];\nvar app = {};\nmethods.forEach(function (m) {\n  app[m] = function () { return m; };\n});\nfor (const k of ['put']) { app[k] = 1; }\napp['del'] = 2;\napp[unknown] = 3;\n";
    let f = facts_of("app.js", Language::JavaScript, src);
    let stores = member_stores(&f);
    let names: Vec<&str> = stores.iter().map(|(n, _)| n.as_str()).collect();
    for expected in ["get", "post", "put", "del"] {
        assert!(names.contains(&expected), "{expected} missing from {names:?}");
    }
    assert!(!names.contains(&"unknown"));
    // The stored value is the callback's function.
    assert!(stores.iter().any(|(n, v)| n == "get"
        && matches!(
            v,
            Expr::Lambda {
                function: Some(_),
                ..
            }
        )));
    // Negative: a list bound twice is not a known list.
    let twice = "var ms = ['a'];\nms = ['b'];\nvar o = {};\nms.forEach(function (m) { o[m] = 1; });\n";
    let g = facts_of("twice.js", Language::JavaScript, twice);
    assert!(member_stores(&g).is_empty(), "{:?}", member_stores(&g));
}

/// Functions defined under a member name are member stores in languages with dynamic
/// members (`obj.run = function () {}`).
#[test]
fn rule_member_named_functions_are_member_stores() {
    let js =
        facts_of("run.js", Language::JavaScript, "const obj = {};\nobj.run = function () { return 1; };\n");
    assert!(member_stores(&js).iter().any(|(n, v)| n == "run"
        && matches!(
            v,
            Expr::Lambda {
                function: Some(_),
                ..
            }
        )));
    // Negative: Python never stores definitions into members.
    let py = flow("class C:\n    def m(self):\n        pass\n");
    assert!(member_stores(&py).is_empty());
}
