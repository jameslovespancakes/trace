//! Rule tests of the call-site views (`crate::callsite`): conditions, guards, call text and
//! arguments per language family.

use super::*;

fn view(language: Language, source: &str, marker: &str) -> SiteView {
    let point = source.find(marker).expect("marker") as u32;
    site_views(language, source.as_bytes(), &[point])
        .pop()
        .flatten()
        .expect("view")
}

fn when(language: Language, source: &str, marker: &str) -> Vec<String> {
    view(language, source, marker).when
}

const GO: &str = r#"package p

func handle(engine *Engine, c *Context) {
	for i, tl := 0, len(t); i < tl; i++ {
		if t[i].method != method {
			continue
		}
		value := root.getValue(path)
		if value.handlers != nil {
			c.Next()
			return
		}
		if method != "CONNECT" && path != "/" {
			if value.tsr && engine.RedirectTrailingSlash {
				redirectTrailingSlash(c)
				return
			}
			if engine.RedirectFixedPath && redirectFixedPath(c, root, engine.RedirectFixedPath) {
				return
			}
		}
		break
	}
	find(
		path,
		buf,       // comment
		[4]byte{},
		fix,
	)
}
"#;

#[test]
fn rule_conditions_go_branches_early_exits_and_short_circuit() {
    let v = view(Language::Go, GO, "getValue");
    assert_eq!(v.when, ["t[i].method == method"]);
    assert_eq!(v.call, "root.getValue(path)");
    let v = view(Language::Go, GO, "Next");
    assert_eq!(v.when, ["t[i].method == method", "value.handlers != nil"]);
    let v = view(Language::Go, GO, "redirectTrailingSlash(");
    assert_eq!(
        v.when,
        [
            "t[i].method == method",
            "value.handlers == nil",
            "method != \"CONNECT\" && path != \"/\"",
            "value.tsr && engine.RedirectTrailingSlash",
        ]
    );
    let v = view(Language::Go, GO, "redirectFixedPath(");
    assert_eq!(
        v.when,
        [
            "t[i].method == method",
            "value.handlers == nil",
            "method != \"CONNECT\" && path != \"/\"",
            "!(value.tsr && engine.RedirectTrailingSlash)",
            "engine.RedirectFixedPath",
        ]
    );
    assert_eq!(v.call, "redirectFixedPath(c, root, engine.RedirectFixedPath)");
    assert!(v.guards.contains(&Guard {
        name: "RedirectFixedPath".into(),
        truthy: true
    }));
    assert!(v.guards.contains(&Guard {
        name: "RedirectTrailingSlash".into(),
        truthy: false
    }));
    assert!(!v.guards.iter().any(|g| g.name == "handlers"));
    let texts: Vec<&str> = v.arguments.iter().map(|a| a.text.as_str()).collect();
    assert_eq!(texts, ["c", "root", "engine.RedirectFixedPath"]);
    assert_eq!(v.arguments[2].position, Some(2));
}

#[test]
fn rule_conditions_skip_the_initializer_of_an_if() {
    let src = "package p\n\nfunc f() {\n\tif v, ok := lookup(k); ok {\n\t\tuse(v)\n\t}\n}\n";
    assert!(view(Language::Go, src, "lookup").when.is_empty());
    assert_eq!(view(Language::Go, src, "use(").when, ["ok"]);
}

#[test]
fn rule_call_text_is_joined_without_comments() {
    let v = view(Language::Go, GO, "find(");
    assert_eq!(v.call, "find(path, buf, [4]byte{}, fix)");
    assert!(v.when.is_empty());
}

#[test]
fn rule_conditions_python_elif_else_ternary_and_not() {
    let src = "def f(self, x):\n    if x.a:\n        g()\n    elif not self.debug:\n        h()\n    else:\n        k()\n    y = m() if self.fast else n()\n    if x is None:\n        raise E()\n    p()\n";
    assert_eq!(view(Language::Python, src, "g()").when, ["x.a"]);
    assert_eq!(view(Language::Python, src, "h()").when, ["not x.a", "not self.debug"]);
    assert_eq!(view(Language::Python, src, "k()").when, ["not x.a", "self.debug"]);
    assert_eq!(view(Language::Python, src, "m()").when, ["self.fast"]);
    assert_eq!(view(Language::Python, src, "n()").when, ["not self.fast"]);
    assert_eq!(view(Language::Python, src, "p()").when, ["x is not None"]);
    let h = view(Language::Python, src, "h()");
    assert!(h.guards.contains(&Guard {
        name: "debug".into(),
        truthy: false
    }));
}

#[test]
fn rule_conditions_python_match_case_and_guard() {
    let src = "def f(k, y):\n    match k:\n        case 1 | 2:\n            a()\n        case Point(x=0) if y:\n            b()\n        case _:\n            c()\n";
    assert_eq!(when(Language::Python, src, "a()"), ["k matches 1 | 2"]);
    assert_eq!(when(Language::Python, src, "b()"), ["k matches Point(x=0)", "y"]);
    assert!(when(Language::Python, src, "c()").is_empty());
}

#[test]
fn rule_conditions_typescript_parentheses_else_and_or() {
    let src = "function f(o) {\n  if (o.ready) { a(); } else { b(); }\n  o.cached || c();\n  if (!o.ok) return;\n  d(o.x, 1);\n}\n";
    assert_eq!(view(Language::TypeScript, src, "a()").when, ["o.ready"]);
    assert_eq!(view(Language::TypeScript, src, "b()").when, ["!o.ready"]);
    assert_eq!(view(Language::TypeScript, src, "c()").when, ["!o.cached"]);
    let d = view(Language::TypeScript, src, "d(");
    assert_eq!(d.when, ["o.ok"]);
    assert_eq!(d.call, "d(o.x, 1)");
}

#[test]
fn rule_conditions_stop_at_the_function_boundary() {
    let src = "function outer(o) {\n  if (o.a) {\n    return () => { inner(); };\n  }\n}\n";
    assert!(view(Language::TypeScript, src, "inner").when.is_empty());
}

#[test]
fn rule_conditions_switch_case_label() {
    let src = "package p\n\nfunc f(k string) {\n\tswitch k {\n\tcase \"a\":\n\t\tg()\n\tdefault:\n\t\th()\n\t}\n}\n";
    assert_eq!(view(Language::Go, src, "g()").when, ["k == \"a\""]);
    assert!(view(Language::Go, src, "h()").when.is_empty());
    // Switch bodies between the case and the switch (`switch_body`) and empty cases that
    // fall through into the next one.
    let src = "function f(k) {\n  switch (k) {\n    case 1:\n    case 2:\n      a();\n      break;\n    default:\n      b();\n  }\n}\n";
    assert_eq!(when(Language::TypeScript, src, "a()"), ["k == 1 || k == 2"]);
    assert!(when(Language::TypeScript, src, "b()").is_empty());
}

#[test]
fn rule_conditions_rust_if_let_match_arm_and_early_return() {
    let src = "fn m(k: Option<u32>, flag: bool) {\n    let Some(v) = k else { return; };\n    if let Some(x) = k { a(x); }\n    match k {\n        Some(1) | Some(2) => b(),\n        Some(n) if n > 3 => c(),\n        _ => d(),\n    }\n    if !flag {\n        return;\n    }\n    e();\n}\n";
    assert_eq!(when(Language::Rust, src, "a(x)"), ["k matches Some(v)", "k matches Some(x)"]);
    assert_eq!(when(Language::Rust, src, "b()"), ["k matches Some(v)", "k matches Some(1) | Some(2)"]);
    assert_eq!(when(Language::Rust, src, "c()"), ["k matches Some(v)", "k matches Some(n)", "n > 3"]);
    assert_eq!(when(Language::Rust, src, "d()"), ["k matches Some(v)"]);
    assert_eq!(when(Language::Rust, src, "e()"), ["k matches Some(v)", "flag"]);
    let v = view(Language::Rust, src, "a(x)");
    assert_eq!(v.call, "a(x)");
    assert_eq!(v.arguments.len(), 1);
}

#[test]
fn rule_conditions_jvm_if_switch_and_match() {
    let java = "class A {\n  void m(int k, boolean x) {\n    switch (k) {\n      case 1:\n      case 2:\n        a();\n        break;\n      default:\n        b();\n    }\n    if (x) c(); else if (y) d(); else e();\n    if (!ready) return;\n    f();\n  }\n}\n";
    assert_eq!(when(Language::Java, java, "a()"), ["k == 1 || k == 2"]);
    assert!(when(Language::Java, java, "b()").is_empty());
    assert_eq!(when(Language::Java, java, "c()"), ["x"]);
    assert_eq!(when(Language::Java, java, "d()"), ["!x", "y"]);
    assert_eq!(when(Language::Java, java, "e()"), ["!x", "!y"]);
    assert_eq!(when(Language::Java, java, "f()"), ["ready"]);

    let scala = "object A {\n  def m(k: Option[Int]): Unit = {\n    if (x) a() else b()\n    k match {\n      case Some(n) if n > 1 => c(n)\n      case _ => d()\n    }\n    if (!ready) return\n    e()\n  }\n}\n";
    assert_eq!(when(Language::Scala, scala, "a()"), ["x"]);
    assert_eq!(when(Language::Scala, scala, "b()"), ["!x"]);
    assert_eq!(when(Language::Scala, scala, "c(n)"), ["k matches Some(n)", "n > 1"]);
    assert!(when(Language::Scala, scala, "d()").is_empty());
    assert_eq!(when(Language::Scala, scala, "e()"), ["ready"]);
}

#[test]
fn rule_conditions_csharp_ternary_switch() {
    let csharp = "class A {\n  void M(int k, object o) {\n    var t = x ? F() : G();\n    switch (k) {\n      case 1:\n      case 2:\n        H();\n        break;\n      case 3 when ready:\n        J();\n        break;\n      default:\n        K();\n        break;\n    }\n    var r = o switch { string s => L(s), _ => N() };\n  }\n}\n";
    assert_eq!(when(Language::CSharp, csharp, "F()"), ["x"]);
    assert_eq!(when(Language::CSharp, csharp, "G()"), ["!x"]);
    assert_eq!(when(Language::CSharp, csharp, "H()"), ["k == 1 || k == 2"]);
    assert_eq!(when(Language::CSharp, csharp, "J()"), ["k == 3", "ready"]);
    assert!(when(Language::CSharp, csharp, "K()").is_empty());
    assert_eq!(when(Language::CSharp, csharp, "L(s)"), ["o matches string s"]);
    assert!(when(Language::CSharp, csharp, "N()").is_empty());
}

#[test]
fn rule_conditions_php_if_elseif_match() {
    let src = "<?php\nfunction m($k) {\n    if ($k === 1) {\n        a();\n    } elseif ($k === 2) {\n        b();\n    } else {\n        c();\n    }\n    $r = match ($k) {\n        1, 2 => d(),\n        default => e(),\n    };\n    switch ($k) {\n        case 1:\n        case 2:\n            f();\n            break;\n    }\n}\n";
    assert_eq!(when(Language::Php, src, "a()"), ["$k === 1"]);
    assert_eq!(when(Language::Php, src, "b()"), ["$k !== 1", "$k === 2"]);
    assert_eq!(when(Language::Php, src, "c()"), ["$k !== 1", "$k !== 2"]);
    assert_eq!(when(Language::Php, src, "d()"), ["$k == 1 || $k == 2"]);
    assert!(when(Language::Php, src, "e()").is_empty());
    assert_eq!(when(Language::Php, src, "f()"), ["$k == 1 || $k == 2"]);
}

#[test]
fn rule_conditions_bash_if_case_and_lists() {
    let src = "f() {\n  if [ -f x ]; then\n    a\n  elif [ -d y ]; then\n    b\n  else\n    c\n  fi\n  case \"$1\" in\n    start|go) d ;;\n    *) e ;;\n  esac\n  [ -z \"$v\" ] && g\n  h || k\n}\n";
    assert_eq!(when(Language::Bash, src, "a\n"), ["[ -f x ]"]);
    assert_eq!(when(Language::Bash, src, "b\n"), ["! [ -f x ]", "[ -d y ]"]);
    assert_eq!(when(Language::Bash, src, "c\n"), ["! [ -f x ]", "! [ -d y ]"]);
    assert_eq!(when(Language::Bash, src, "d ;;"), ["\"$1\" == start || \"$1\" == go"]);
    assert!(when(Language::Bash, src, "e ;;").is_empty());
    assert_eq!(when(Language::Bash, src, "g\n"), ["[ -z \"$v\" ]"]);
    assert_eq!(when(Language::Bash, src, "k\n"), ["! h"]);
}

#[test]
fn rule_conditions_r_if_else() {
    let src = "k <- function(a, b) {\n  if (a > 1) f4() else if (b) f5() else f6()\n  a && f7()\n}\n";
    assert_eq!(when(Language::R, src, "f4"), ["a > 1"]);
    assert_eq!(when(Language::R, src, "f5"), ["a <= 1", "b"]);
    assert_eq!(when(Language::R, src, "f6"), ["a <= 1", "!b"]);
    assert_eq!(when(Language::R, src, "f7"), ["a"]);
}

#[test]
fn rule_conditions_haskell_guards_if_case() {
    let src = "module M where\n\nf x\n  | x > 0 = g x\n  | otherwise = h x\n\nk y = if y then m y else n y\n\nc z = case z of\n  Just a -> p a\n  Nothing -> q\n\nw a = a && r a\n";
    assert_eq!(when(Language::Haskell, src, "g x"), ["x > 0"]);
    assert_eq!(when(Language::Haskell, src, "h x"), ["x <= 0"]);
    assert_eq!(when(Language::Haskell, src, "m y"), ["y"]);
    assert_eq!(when(Language::Haskell, src, "n y"), ["not y"]);
    assert_eq!(when(Language::Haskell, src, "p a"), ["z matches Just a"]);
    assert_eq!(when(Language::Haskell, src, "r a"), ["a"]);
}

#[test]
fn rule_conditions_c_cpp_if_ternary_case() {
    let src = "void f(int k, int x) {\n    int t = x ? a() : b();\n    switch (k) {\n    case 1:\n    case 2:\n        c();\n        break;\n    default:\n        d();\n    }\n    if (x > 0) {\n        e();\n    } else if (y) {\n        g();\n    } else {\n        h();\n    }\n    if (!ready) return;\n    i();\n}\n";
    for language in [Language::C, Language::Cpp] {
        assert_eq!(when(language, src, "a()"), ["x"], "{language}");
        assert_eq!(when(language, src, "b()"), ["!x"], "{language}");
        assert_eq!(when(language, src, "c()"), ["k == 1 || k == 2"], "{language}");
        assert!(when(language, src, "d()").is_empty(), "{language}");
        assert_eq!(when(language, src, "e()"), ["x > 0"], "{language}");
        assert_eq!(when(language, src, "g()"), ["x <= 0", "y"], "{language}");
        assert_eq!(when(language, src, "h()"), ["x <= 0", "!y"], "{language}");
        assert_eq!(when(language, src, "i()"), ["ready"], "{language}");
    }
    // C++ `if (init; cond)`: the initializer runs whatever the condition is.
    let src = "void f() {\n    if (auto v = get(); v > 1) {\n        use(v);\n    }\n}\n";
    assert!(when(Language::Cpp, src, "get()").is_empty());
    assert_eq!(when(Language::Cpp, src, "use(v)"), ["v > 1"]);
}

#[test]
fn rule_conditions_negative_loops_and_plain_calls_add_nothing() {
    // A loop body runs once per iteration: no condition; neither does a plain sequence.
    let src = "fn m(xs: Vec<u32>) {\n    for x in xs {\n        a(x);\n    }\n    b();\n}\n";
    assert!(when(Language::Rust, src, "a(x)").is_empty());
    assert!(when(Language::Rust, src, "b()").is_empty());
    // An earlier `if` with an `else` never leaves the block for the code after it.
    let src = "function f(o) {\n  if (o.a) { return 1; } else { g(); }\n  h();\n}\n";
    assert!(when(Language::JavaScript, src, "h()").is_empty());
}

#[test]
fn rule_call_text_and_arguments_bash_words() {
    let src = "f() {\n  cp -r \"$src\" dst\n}\n";
    let v = view(Language::Bash, src, "cp");
    assert_eq!(v.call, "cp -r \"$src\" dst");
    let texts: Vec<&str> = v.arguments.iter().map(|a| a.text.as_str()).collect();
    assert_eq!(texts, ["-r", "\"$src\"", "dst"]);
}

#[test]
fn rule_call_text_and_arguments_haskell_spine() {
    let src = "module M where\n\ns = foo a (b c) d\n";
    let v = view(Language::Haskell, src, "foo");
    assert_eq!(v.call, "foo a (b c) d");
    let texts: Vec<&str> = v.arguments.iter().map(|a| a.text.as_str()).collect();
    assert_eq!(texts, ["a", "(b c)", "d"]);
    assert_eq!(v.arguments[2].position, Some(2));
}

#[test]
fn rule_call_text_and_arguments_r_keywords() {
    let v = view(Language::R, "f8(x = 1, 2)\n", "f8");
    assert_eq!(
        v.arguments,
        [
            ArgumentView {
                position: None,
                keyword: Some("x".into()),
                text: "1".into()
            },
            ArgumentView {
                position: Some(0),
                keyword: None,
                text: "2".into()
            },
        ]
    );
}

/// Every node kind named by the condition tables exists in at least one pinned grammar (a
/// misspelt kind would silently disable a rule).
#[test]
fn rule_condition_tables_name_real_grammar_kinds() {
    let grammars: Vec<&crate::grammar::Grammar> = crate::test_support::compiled_languages()
        .into_iter()
        .filter_map(crate::grammar::grammar)
        .collect();
    let known = |kind: &str| grammars.iter().any(|g| g.ts.id_for_node_kind(kind, true) != 0);
    let tables: [&[&str]; 10] = [
        &BRANCHES,
        &ALT_CLAUSES,
        &TERNARIES,
        &BOOLEANS,
        &EXITS,
        &CASES,
        &SWITCHES,
        &WILDCARDS,
        &PARENS,
        &["let_condition", "let_declaration", "guard", "when_clause", "switch_label"],
    ];
    for table in tables {
        for kind in table {
            assert!(known(kind), "unknown node kind {kind}");
        }
    }
}

#[test]
fn one_line_collapses_breaks_only() {
    assert_eq!(one_line("f(\n\ta,\n\tb,\n)"), "f(a, b)");
    assert_eq!(one_line("a  &&\n   b"), "a  && b");
    assert_eq!(one_line("g(\"x  y\")"), "g(\"x  y\")");
}
