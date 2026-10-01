use crate::{extract, SourceInput};
use trace_core::Language;

fn facts_of(path: &str, language: Language, src: &str) -> trace_core::facts::FileFacts {
    extract(SourceInput {
        path,
        language,
        source: src.as_bytes(),
    })
    .unwrap()
}

fn facts(src: &str) -> trace_core::facts::FileFacts {
    facts_of("m.py", Language::Python, src)
}

fn fp(path: &str, language: Language, src: &str) -> trace_core::Hash32 {
    facts_of(path, language, src).interface
}

#[test]
fn rule_interface_fingerprint_ignores_body_edits() {
    let a = facts("def f():\n    x = 1\n    print(x)\n    return None\n");
    let b = facts("def f():\n    x = 2\n    print(x, x)\n    return None\n");
    let c = facts("def g():\n    x = 1\n    print(x)\n    return None\n");
    assert_eq!(a.interface, b.interface);
    assert_ne!(a.interface, c.interface);
}

#[test]
fn rule_interface_fingerprint_ignores_whitespace_and_comments() {
    let a = fp("m.ts", Language::TypeScript, "export function f(a: number): string { return g(a); }\n");
    let b = fp(
        "m.ts",
        Language::TypeScript,
        "// helper\nexport function f(a:   number)  :  string {\n  // body\n  return g(a);\n}\n\n",
    );
    assert_eq!(a, b);
}

#[test]
fn rule_interface_fingerprint_sees_signature_changes() {
    let base = fp("M.java", Language::Java, "class M { int f(int a) { return a; } }\n");
    let params = fp("M.java", Language::Java, "class M { int f(long a) { return a; } }\n");
    let ret = fp("M.java", Language::Java, "class M { long f(int a) { return a; } }\n");
    let modifier = fp("M.java", Language::Java, "class M { private int f(int a) { return a; } }\n");
    let extends = fp("M.java", Language::Java, "class M extends B { int f(int a) { return a; } }\n");
    let body = fp("M.java", Language::Java, "class M { int f(int a) { return a + 1; } }\n");
    assert_ne!(base, params);
    assert_ne!(base, ret);
    assert_ne!(base, modifier);
    assert_ne!(base, extends);
    assert_eq!(base, body, "Java return types are declared: a body edit keeps the interface");
}

#[test]
fn rule_return_change_counts_for_inferred_return_languages() {
    // Python: the returned value decides the inferred return type.
    let a = facts("def f():\n    return Store()\n");
    let b = facts("def f():\n    return Cache()\n");
    assert_ne!(a.interface, b.interface);
    // JavaScript arrow expression bodies are returned values too.
    let a = fp("m.js", Language::JavaScript, "export const f = () => new Store();\n");
    let b = fp("m.js", Language::JavaScript, "export const f = () => new Cache();\n");
    assert_ne!(a, b);
    // R: the last expression is the value (implicit return).
    let a = fp("m.R", Language::R, "f <- function() {\n  Store()\n}\n");
    let b = fp("m.R", Language::R, "f <- function() {\n  Cache()\n}\n");
    assert_ne!(a, b);
    // Negative: Go declares return types; a body edit keeps the interface.
    let a = fp("m.go", Language::Go, "package m\nfunc F() int { return 1 }\n");
    let b = fp("m.go", Language::Go, "package m\nfunc F() int { return 2 }\n");
    assert_eq!(a, b);
}

/// I-12: a `return` edit inside a callable with a declared return type keeps the
/// interface (dependents see the declared type), so no dependent is re-queried.
#[test]
fn rule_return_change_in_declared_callable_keeps_the_interface() {
    let a = fp(
        "m.ts",
        Language::TypeScript,
        "export class C {\n  get(a: Store, b: Store): Store {\n    return a;\n  }\n}\n",
    );
    let b = fp(
        "m.ts",
        Language::TypeScript,
        "export class C {\n  get(a: Store, b: Store): Store {\n    return b;\n  }\n}\n",
    );
    assert_eq!(a, b, "typed TypeScript method");
    let a = facts("def f(a, b) -> Store:\n    return a\n");
    let b = facts("def f(a, b) -> Store:\n    return b\n");
    assert_eq!(a.interface, b.interface, "annotated Python function");
}

/// I-12 negative: a `return` edit inside a callable without a declared return type changes
/// the interface (its return type is inferred from the returned value), also for
/// module-level arrow expression bodies.
#[test]
fn rule_return_change_in_untyped_callable_changes_it() {
    let a = fp(
        "m.ts",
        Language::TypeScript,
        "export class C {\n  get(a: Store, b: Store) {\n    return a;\n  }\n}\n",
    );
    let b = fp(
        "m.ts",
        Language::TypeScript,
        "export class C {\n  get(a: Store, b: Store) {\n    return b;\n  }\n}\n",
    );
    assert_ne!(a, b, "untyped TypeScript method");
    let a = facts("def f(a, b):\n    return a\n");
    let b = facts("def f(a, b):\n    return b\n");
    assert_ne!(a.interface, b.interface, "unannotated Python function");
    let a = fp("m.ts", Language::TypeScript, "export const f = (a: number, b: number) => a;\n");
    let b = fp("m.ts", Language::TypeScript, "export const f = (a: number, b: number) => b;\n");
    assert_ne!(a, b, "arrow expression body without a return type");
}

#[test]
fn rule_attribute_stores_are_part_of_the_interface() {
    let a = facts("class A:\n    def __init__(self):\n        self.store = Store()\n");
    let b = facts("class A:\n    def __init__(self):\n        self.store = Cache()\n");
    assert_ne!(a.interface, b.interface);
    // A local variable of a method is not visible to other files.
    let c =
        facts("class A:\n    def __init__(self):\n        local = Store()\n        self.store = Store()\n");
    let d =
        facts("class A:\n    def __init__(self):\n        local = Other()\n        self.store = Store()\n");
    assert_eq!(c.interface, d.interface);
}
