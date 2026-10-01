//! Rule tests of the general fixes plan (package S, SPEC §6.3): local bindings, member
//! accesses, conformance facts, header type references, stub declarations, declared /
//! constructed / comment-annotated types and property-assigned function names.

use trace_core::facts::{Declaration, FileFacts, RefKind, Scope, TypeSource, TypeSubject};
use trace_core::model::{ByteSpan, SymbolKind};
use trace_core::Language;

use crate::{extract, SourceInput};

fn facts(path: &str, language: Language, src: &str) -> FileFacts {
    extract(SourceInput {
        path,
        language,
        source: src.as_bytes(),
    })
    .unwrap_or_else(|e| panic!("{language}: {e}"))
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Spans of the whole-word occurrences of `word` in `src` (test fixture text only).
fn occurrences(src: &str, word: &str) -> Vec<ByteSpan> {
    let bytes = src.as_bytes();
    let w = word.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + w.len() <= bytes.len() {
        if &bytes[i..i + w.len()] == w {
            let before = i == 0 || !is_word(bytes[i - 1]) || !is_word(w[0]);
            let after = i + w.len() == bytes.len() || !is_word(bytes[i + w.len()]);
            if before && after {
                out.push(ByteSpan::new(i as u32, (i + w.len()) as u32));
            }
        }
        i += 1;
    }
    out
}

/// Span of `sub` inside the first occurrence of `needle`.
fn span_in(src: &str, needle: &str, sub: &str) -> ByteSpan {
    let at = src.find(needle).unwrap_or_else(|| panic!("no {needle:?} in fixture"));
    let off = needle.find(sub).unwrap_or_else(|| panic!("no {sub:?} in {needle:?}"));
    let start = (at + off) as u32;
    ByteSpan::new(start, start + sub.len() as u32)
}

fn decls<'f>(f: &'f FileFacts, qualified: &str) -> Vec<(u32, &'f Declaration)> {
    f.declarations
        .iter()
        .enumerate()
        .filter(|(_, d)| d.qualified_name == qualified)
        .map(|(i, d)| (i as u32, d))
        .collect()
}

fn decl(f: &FileFacts, qualified: &str) -> u32 {
    let found = decls(f, qualified);
    let all: Vec<&str> = f.declarations.iter().map(|d| d.qualified_name.as_str()).collect();
    found
        .iter()
        .find(|(_, d)| !d.is_stub)
        .or(found.first())
        .map(|(i, _)| *i)
        .unwrap_or_else(|| panic!("no declaration {qualified} in {all:?}"))
}

// ---------------------------------------------------------------------------------------
// Rule 2: local bindings
// ---------------------------------------------------------------------------------------

struct LocalCase {
    language: Language,
    path: &'static str,
    source: &'static str,
    /// `(identifier, local? per whole-word occurrence in source order)`.
    expect: &'static [(&'static str, &'static [bool])],
}

const T: bool = true;
const F: bool = false;

const LOCAL_CASES: &[LocalCase] = &[
    LocalCase {
        language: Language::JavaScript,
        path: "src/f.js",
        source: r#"function helper(x) { return x; }
function f(p) {
  let v = p;
  v = v + 1;
  const g = (c) => c + v;
  { let w = 1; use(w); }
  obj.v();
  return helper(v) + g(2);
}
"#,
        expect: &[
            ("p", &[T, T]),
            ("v", &[T, T, T, T, F, T]),
            ("c", &[T, T]),
            ("w", &[T, T]),
            ("helper", &[F, F]),
            ("obj", &[F]),
        ],
    },
    LocalCase {
        language: Language::TypeScript,
        path: "src/f.ts",
        source: r#"function helper(x: number): number { return x; }
function f(p: number): number {
  let v: number = p;
  v = v + 1;
  const g = (c: number): number => c + v;
  { let w = 1; use(w); }
  obj.v();
  return helper(v) + g(2);
}
"#,
        expect: &[
            ("p", &[T, T]),
            ("v", &[T, T, T, T, F, T]),
            ("c", &[T, T]),
            ("w", &[T, T]),
            ("helper", &[F, F]),
        ],
    },
    LocalCase {
        language: Language::Rust,
        path: "src/f.rs",
        source: r#"fn helper(x: u32) -> u32 { x }
fn f(p: u32) -> u32 {
    let v = p;
    let v = v + 1;
    let g = |c: u32| c + v;
    { let w = 1; use_it(w); }
    obj.v();
    helper(v) + g(2)
}
"#,
        expect: &[
            ("p", &[T, T]),
            ("v", &[T, T, T, T, F, T]),
            ("g", &[T, T]),
            ("c", &[T, T]),
            ("w", &[T, T]),
            ("helper", &[F, F]),
            ("obj", &[F]),
        ],
    },
    LocalCase {
        language: Language::Go,
        path: "f.go",
        source: r#"package m

func helper(x int) int { return x }

func f(p int) int {
	v := p
	v = v + 1
	g := func(c int) int { return c + v }
	{
		w := 1
		use(w)
	}
	obj.v()
	return helper(v) + g(2)
}
"#,
        expect: &[
            ("p", &[T, T]),
            ("v", &[T, T, T, T, F, T]),
            ("g", &[T, T]),
            ("c", &[T, T]),
            ("w", &[T, T]),
            ("helper", &[F, F]),
        ],
    },
    LocalCase {
        language: Language::Java,
        path: "A.java",
        source: r#"class A {
  int helper(int x) { return x; }
  int f(int p) {
    int v = p;
    v = v + 1;
    Op g = (c) -> c + v;
    { int w = 1; use(w); }
    obj.v();
    return helper(v) + g.apply(2);
  }
}
"#,
        expect: &[
            ("p", &[T, T]),
            ("v", &[T, T, T, T, F, T]),
            ("g", &[T, T]),
            ("c", &[T, T]),
            ("w", &[T, T]),
            ("helper", &[F, F]),
        ],
    },
    LocalCase {
        language: Language::C,
        path: "f.c",
        source: r#"int helper(int x) { return x; }
int f(int p) {
    int v = p;
    v = v + 1;
    { int w = 1; use(w); }
    obj.v();
    return helper(v);
}
"#,
        expect: &[("p", &[T, T]), ("v", &[T, T, T, F, T]), ("w", &[T, T]), ("helper", &[F, F])],
    },
    LocalCase {
        language: Language::Cpp,
        path: "f.cpp",
        source: r#"int helper(int x) { return x; }
int f(int p) {
    int v = p;
    v = v + 1;
    auto g = [&](int c) { return c + v; };
    { int w = 1; use(w); }
    obj.v();
    return helper(v) + g(2);
}
"#,
        expect: &[
            ("p", &[T, T]),
            ("v", &[T, T, T, T, F, T]),
            ("g", &[T, T]),
            ("c", &[T, T]),
            ("w", &[T, T]),
            ("helper", &[F, F]),
        ],
    },
    LocalCase {
        language: Language::CSharp,
        path: "A.cs",
        source: r#"class A {
  int Helper(int x) { return x; }
  int F(int p) {
    var v = p;
    v = v + 1;
    Func<int, int> g = (c) => c + v;
    { var w = 1; Use(w); }
    obj.v();
    return Helper(v) + g(2);
  }
}
"#,
        expect: &[
            ("p", &[T, T]),
            ("v", &[T, T, T, T, F, T]),
            ("g", &[T, T]),
            ("c", &[T, T]),
            ("w", &[T, T]),
            ("Helper", &[F, F]),
        ],
    },
    LocalCase {
        language: Language::Scala,
        path: "M.scala",
        source: r#"object M {
  def helper(x: Int): Int = x
  def f(p: Int): Int = {
    var v = p
    v = v + 1
    val g = (c: Int) => c + v
    { val w = 1; use(w) }
    obj.v()
    helper(v) + g(2)
  }
}
"#,
        expect: &[
            ("p", &[T, T]),
            ("v", &[T, T, T, T, F, T]),
            ("g", &[T, T]),
            ("c", &[T, T]),
            ("w", &[T, T]),
            ("helper", &[F, F]),
        ],
    },
    LocalCase {
        language: Language::Php,
        path: "f.php",
        source: r#"<?php
function helper($x) { return $x; }
function f($p) {
    $v = $p;
    $v = $v + 1;
    $g = fn($c) => $c + $v;
    $obj->v();
    return helper($v) + $g(2);
}
"#,
        expect: &[
            ("$p", &[T, T]),
            ("$v", &[T, T, T, T, T]),
            ("$g", &[T, T]),
            ("$c", &[T, T]),
            ("$obj", &[F]),
        ],
    },
    LocalCase {
        language: Language::Bash,
        path: "f.sh",
        source: r#"helper() { echo "$1"; }
f() {
  local v=1
  v=2
  echo "$v"
  g=3
  echo "$g"
}
"#,
        expect: &[("v", &[T, T, T]), ("g", &[F, F])],
    },
    LocalCase {
        language: Language::Haskell,
        path: "M.hs",
        source: r#"module M where

f p = g p
  where g c = c + p
"#,
        expect: &[("p", &[T, T, T]), ("c", &[T, T]), ("g", &[F, F])],
    },
];

/// Parameters, `let` / `val` / `var` reads and writes, closure parameters and nested blocks
/// are local bindings in every grammar with lexical scoping; a same-named member
/// (`obj.v()`) and declared functions (`helper`) never are. `Reference::local` agrees with
/// `FileFacts::local_spans` for every reference.
#[test]
fn rule_local_bindings_shadow_in_every_language() {
    let grammars = LOCAL_CASES.iter().filter(|c| !c.expect.is_empty()).count();
    assert!(grammars >= 12, "{grammars} grammars");
    for case in LOCAL_CASES {
        let lang = case.language;
        let f = facts(case.path, lang, case.source);
        assert_eq!(f.error_count, 0, "{lang}: fixture must parse");
        assert!(
            f.local_spans
                .windows(2)
                .all(|w| (w[0].start, w[0].end) < (w[1].start, w[1].end)),
            "{lang}: local spans sorted and unique"
        );
        for (word, want) in case.expect {
            let occ = occurrences(case.source, word);
            assert_eq!(occ.len(), want.len(), "{lang}: occurrences of {word}");
            for (i, (at, local)) in occ.iter().zip(want.iter()).enumerate() {
                assert_eq!(
                    f.is_local(*at),
                    *local,
                    "{lang}: {word} #{i} at byte {} (line {})",
                    at.start,
                    case.source[..at.start as usize].matches('\n').count() + 1
                );
            }
        }
        for r in &f.references {
            assert_eq!(r.local, f.is_local(r.span), "{lang}: Reference::local of {}", r.name);
        }
    }
}

/// Python keeps its scoping rule; parameters, variables and local callees are local spans.
#[test]
fn rule_local_bindings_python_scoping_is_unchanged() {
    let src = "def helper():\n    pass\n\ndef run(fn, items):\n    total = 0\n    fn(total)\n    helper()\n    return total\n";
    let f = facts("m.py", Language::Python, src);
    for word in ["fn", "items", "total"] {
        for at in occurrences(src, word) {
            assert!(f.is_local(at), "{word} at {}", at.start);
        }
    }
    for at in occurrences(src, "helper") {
        assert!(!f.is_local(at));
    }
    for r in &f.references {
        assert_eq!(r.local, f.is_local(r.span), "Reference::local of {}", r.name);
    }
}

// ---------------------------------------------------------------------------------------
// Member accesses
// ---------------------------------------------------------------------------------------

fn assert_member(f: &FileFacts, src: &str, needle: &str, sub: &str, root: Option<&str>, self_receiver: bool) {
    let at = span_in(src, needle, sub);
    let lang = f.language.unwrap();
    let m = f
        .member_access(at)
        .unwrap_or_else(|| panic!("{lang}: no member access {sub} in {needle:?}: {:?}", f.member_accesses));
    assert_eq!(m.receiver_root.as_deref(), root, "{lang}: receiver root of {sub} in {needle:?}");
    assert_eq!(m.self_receiver, self_receiver, "{lang}: self receiver of {sub} in {needle:?}");
}

fn assert_no_member(f: &FileFacts, src: &str, needle: &str, sub: &str) {
    let at = span_in(src, needle, sub);
    let lang = f.language.unwrap();
    assert!(f.member_access(at).is_none(), "{lang}: {sub} in {needle:?} is not a member access");
}

#[test]
fn rule_member_accesses_record_receiver_roots() {
    let src = "import * as ns from \"m\";\nclass C { m() { this.a.b(); this.c = 1; } }\nobj.x;\np?.y();\na.b.c = 1;\nf(o.cb);\nns.run();\n";
    let f = facts("m.js", Language::JavaScript, src);
    assert_member(&f, src, "this.a.b()", "b", None, false);
    assert_member(&f, src, "this.a.b()", "a", None, true);
    assert_member(&f, src, "this.c = 1", "c", None, true);
    assert_member(&f, src, "obj.x", "x", Some("obj"), false);
    assert_member(&f, src, "p?.y", "y", Some("p"), false);
    assert_member(&f, src, "a.b.c = 1", "c", Some("a"), false);
    assert_member(&f, src, "a.b.c = 1", "b", Some("a"), false);
    assert_member(&f, src, "f(o.cb)", "cb", Some("o"), false);
    assert_member(&f, src, "ns.run()", "run", Some("ns"), false);
    assert_no_member(&f, src, "f(o.cb)", "f");
    assert!(f
        .member_accesses
        .windows(2)
        .all(|w| (w[0].span.start, w[0].span.end) < (w[1].span.start, w[1].span.end)));

    let src =
        "import os.path\n\nclass K:\n    def m(self):\n        self.x.y()\n        os.path.join(self.x)\n";
    let f = facts("m.py", Language::Python, src);
    assert_member(&f, src, "self.x.y()", "y", None, false);
    assert_member(&f, src, "self.x.y()", "x", None, true);
    assert_member(&f, src, "os.path.join", "join", Some("os"), false);
    assert_no_member(&f, src, "import os.path", "path");

    let src = "fn f() { Foo::new(); x.m(); self.v; }\n";
    let f = facts("m.rs", Language::Rust, src);
    assert_no_member(&f, src, "Foo::new()", "new");
    assert_member(&f, src, "x.m()", "m", Some("x"), false);
    assert_member(&f, src, "self.v", "v", None, true);

    let src = "void f(struct s *p, struct s q) { p->x = 1; q.y = 2; }\n";
    let f = facts("m.c", Language::C, src);
    assert_member(&f, src, "p->x", "x", Some("p"), false);
    assert_member(&f, src, "q.y", "y", Some("q"), false);

    let src = "<?php\nclass K { function f($o) { $o->m(); $this->x = 1; Foo::bar(); } }\n";
    let f = facts("m.php", Language::Php, src);
    assert_member(&f, src, "$o->m()", "m", Some("$o"), false);
    assert_member(&f, src, "$this->x", "x", None, true);
    assert_no_member(&f, src, "Foo::bar()", "bar");

    let src = "class K { void f() { o.m(); this.g = 1; } }\n";
    let f = facts("K.java", Language::Java, src);
    assert_member(&f, src, "o.m()", "m", Some("o"), false);
    assert_member(&f, src, "this.g", "g", None, true);

    let src = "package m\n\nfunc f() { s.g() }\n";
    let f = facts("m.go", Language::Go, src);
    assert_member(&f, src, "s.g()", "g", Some("s"), false);

    let src = "class K { void F() { o?.M(); } }\n";
    let f = facts("K.cs", Language::CSharp, src);
    assert_member(&f, src, "o?.M()", "M", Some("o"), false);
}

// ---------------------------------------------------------------------------------------
// Rule 6: conformance facts and header type references
// ---------------------------------------------------------------------------------------

#[test]
fn rule_conformance_facts_for_instances() {
    let src = "module M where\n\nclass Describe a where\n  describe :: a -> String\n\ninstance Describe Shape where\n  describe _ = \"shape\"\n";
    let f = facts("M.hs", Language::Haskell, src);
    assert!(
        f.impls
            .iter()
            .any(|i| i.type_name == "Shape" && i.trait_name == "Describe"),
        "{:?}",
        f.impls
    );
    let d = decl(&f, "Shape.describe");
    assert_eq!(f.declarations[d as usize].container.as_deref(), Some("Shape"));
}

#[test]
fn rule_header_type_references_in_impl_headers() {
    let cases: &[(Language, &str, &str, &[&str])] = &[
        (Language::Java, "A.java", "class A extends B implements C {}\n", &["B", "C"]),
        (Language::Rust, "a.rs", "struct S;\nimpl Tr for S {}\n", &["Tr"]),
        (Language::CSharp, "A.cs", "class A : B, C {}\n", &["B", "C"]),
        (Language::Cpp, "a.cpp", "class A : public B {};\n", &["B"]),
        (Language::TypeScript, "a.ts", "class A extends B implements C {}\n", &["B", "C"]),
        (Language::Scala, "A.scala", "class A extends B with C\n", &["B", "C"]),
        (Language::Php, "a.php", "<?php\nclass A extends B implements C {}\n", &["B", "C"]),
        (Language::Haskell, "M.hs", "module M where\n\ninstance C T where\n", &["C", "T"]),
    ];
    for (lang, path, src, bases) in cases {
        let f = facts(path, *lang, src);
        for base in *bases {
            let at = occurrences(src, base);
            let last = *at.last().unwrap_or_else(|| panic!("{lang}: no {base}"));
            let r = f
                .references
                .iter()
                .find(|r| r.span.start == last.start && r.span.end == last.end)
                .unwrap_or_else(|| panic!("{lang}: no reference at base {base}: {:?}", f.references));
            assert_eq!(r.kind, RefKind::Type, "{lang}: base {base}");
            assert_eq!(r.name, *base, "{lang}");
        }
    }
    // Python bases stay value (`read`) references.
    let f = facts("m.py", Language::Python, "class A(B):\n    pass\n");
    let b = f.references.iter().find(|r| r.name == "B").unwrap();
    assert_eq!(b.kind, RefKind::Read);
}

// ---------------------------------------------------------------------------------------
// Rule 9: declarations that are not definitions
// ---------------------------------------------------------------------------------------

#[test]
fn rule_prototypes_and_signatures_are_stub_declarations() {
    // C header prototypes and forward declarations.
    let src = "int add(int a, int b);\nstatic char **names(void);\nstruct point;\n";
    let f = facts("include/m.h", Language::C, src);
    for name in ["add", "names"] {
        let found = decls(&f, name);
        assert_eq!(found.len(), 1, "{name}");
        assert!(found[0].1.is_stub, "{name} is a stub");
        assert_eq!(found[0].1.kind, SymbolKind::Function);
    }

    // A C++ header classified as C (`.h`) is extracted with the C++ grammar when only that
    // grammar parses it, and its facts say so (DESIGN F4: `facts.language` comes from
    // `header::header_language`; the inventory language of the file record stays C).
    let src = "#include <cstdio>\nauto open_buffered_file(FILE** fp = nullptr) -> fmt::buffered_file;\nclass file {\npublic:\n    file();\n    const char* name() const;\n};\n";
    let f = facts("test/util.h", Language::C, src);
    assert_eq!(f.language, Some(Language::Cpp));
    assert_eq!(f.error_count, 0);
    let open = decls(&f, "open_buffered_file");
    assert_eq!(open.len(), 1, "{:?}", f.declarations.iter().map(|d| &d.qualified_name).collect::<Vec<_>>());
    assert!(open[0].1.is_stub);
    assert!(decls(&f, "file.name")
        .iter()
        .all(|(_, d)| d.is_stub && d.kind == SymbolKind::Method));
    assert!(!decls(&f, "file.name").is_empty());
    assert!(decls(&f, "file.file")
        .iter()
        .any(|(_, d)| d.is_stub && d.kind == SymbolKind::Constructor));

    // C++ class member prototypes, qualified like their out-of-line definitions.
    let src = "class Service {\npublic:\n    Service(int n);\n    void run(int x);\n    const Config& config() const;\n    Item* make();\n    virtual void stop() = 0;\n};\n\nvoid Service::run(int x) { go(x); }\n";
    let f = facts("src/service.cpp", Language::Cpp, src);
    for name in ["Service.config", "Service.make", "Service.stop"] {
        let found = decls(&f, name);
        assert_eq!(found.len(), 1, "{name}");
        assert!(found[0].1.is_stub, "{name}");
    }
    let run = decls(&f, "Service.run");
    assert_eq!(run.len(), 2);
    assert!(run[0].1.is_stub && !run[1].1.is_stub);
    assert!(decls(&f, "Service.Service").iter().all(|(_, d)| d.is_stub));

    // Haskell signatures: one declaration per name, `a, b :: T` included.
    let src = "module M where\n\nclass Describe a where\n  describe :: a -> String\n\na, b :: Int\na = 1\nb = 2\n\narea :: Shape -> Int\narea s = 1\n";
    let f = facts("src/M.hs", Language::Haskell, src);
    for name in ["a", "b", "area"] {
        let found = decls(&f, name);
        assert_eq!(found.len(), 2, "{name}: signature + definition");
        assert!(found[0].1.is_stub, "{name}: signature first");
        assert!(!found[1].1.is_stub, "{name}: definition");
    }
    let describe = decls(&f, "Describe.describe");
    assert_eq!(describe.len(), 1);
    assert!(describe[0].1.is_stub);
    assert_eq!(describe[0].1.kind, SymbolKind::Method);

    // TypeScript: `declare function`, interface overload signatures, abstract members and
    // overload signatures before their implementation are declarations of their own.
    let src = "declare function foo(a: string): void;\ninterface I { f(x: number): string; f(x: string): string; }\nfunction pick(a: string): string;\nfunction pick(a: number): number;\nfunction pick(a: any) { return a; }\nabstract class Base { abstract run(): void; }\n";
    let f = facts("src/s.ts", Language::TypeScript, src);
    assert!(decls(&f, "foo")[0].1.is_stub);
    let sigs = decls(&f, "I.f");
    assert_eq!(sigs.len(), 2);
    assert!(sigs.iter().all(|(_, d)| d.is_stub));
    let pick = decls(&f, "pick");
    assert_eq!(pick.len(), 3);
    assert!(pick[0].1.is_stub && pick[1].1.is_stub && !pick[2].1.is_stub);
    assert_eq!(pick.iter().map(|(_, d)| d.span.start_line).collect::<Vec<_>>(), vec![3, 4, 5]);
    assert!(decls(&f, "Base.run")[0].1.is_stub);

    // C++ namespaces qualify declarations; out-of-line definitions `a::B::f` are qualified
    // like the in-class prototype.
    let src =
        "namespace shapes {\nclass Box {\npublic:\n    int volume() const;\n};\nint scale(int value);\n}\n";
    let f = facts("cpp/shapes.hpp", Language::Cpp, src);
    assert!(decls(&f, "shapes.Box.volume")[0].1.is_stub);
    assert!(decls(&f, "shapes.scale")[0].1.is_stub);
    let src = "#include \"shapes.hpp\"\nnamespace shapes {\nint scale(int value) { return value * 2; }\n}\nint shapes::Box::volume() const { return shapes::scale(1); }\n";
    let f = facts("cpp/shapes.cc", Language::Cpp, src);
    let volume = decls(&f, "shapes.Box.volume");
    assert_eq!(volume.len(), 1, "{:?}", f.declarations.iter().map(|d| &d.qualified_name).collect::<Vec<_>>());
    assert!(!volume[0].1.is_stub);
    assert_eq!(volume[0].1.container.as_deref(), Some("Box"));
    assert!(!decls(&f, "shapes.scale")[0].1.is_stub);

    // Java / C# abstract and interface members without a body.
    /// (language, path, source, stub declarations, declarations with a body)
    type StubCase<'a> = (Language, &'a str, &'a str, &'a [&'a str], &'a [&'a str]);
    let cases: &[StubCase<'_>] = &[
        (
            Language::Java,
            "S.java",
            "interface Shape { double area(); }\nabstract class Base { abstract void run(); void go() { run(); } }\n",
            &["Shape.area", "Base.run"],
            &["Base.go"],
        ),
        (
            Language::CSharp,
            "S.cs",
            "interface IShape { double Area(); }\nabstract class Base { public abstract void Run(); public void Go() { Run(); } }\n",
            &["IShape.Area", "Base.Run"],
            &["Base.Go"],
        ),
    ];
    for (lang, path, src, stubs, defs) in cases {
        let f = facts(path, *lang, src);
        for name in *stubs {
            let found = decls(&f, name);
            assert!(!found.is_empty() && found.iter().all(|(_, d)| d.is_stub), "{lang}: {name} stub");
        }
        for name in *defs {
            let found = decls(&f, name);
            assert!(!found.is_empty() && found.iter().all(|(_, d)| !d.is_stub), "{lang}: {name} definition");
        }
    }
}

// ---------------------------------------------------------------------------------------
// Rules 10 and 11: declared, constructed and comment-annotated types
// ---------------------------------------------------------------------------------------

fn var(decl: u32, name: &str) -> TypeSubject {
    TypeSubject::Var {
        scope: Scope::Decl(decl),
        name: name.to_string(),
    }
}

fn module_var(name: &str) -> TypeSubject {
    TypeSubject::Var {
        scope: Scope::Module,
        name: name.to_string(),
    }
}

fn field(class: u32, name: &str) -> TypeSubject {
    TypeSubject::Field {
        class,
        name: name.to_string(),
    }
}

fn has_type(f: &FileFacts, subject: &TypeSubject, type_name: &str, source: TypeSource) -> bool {
    f.types_of(subject)
        .any(|t| t.type_name == type_name && t.source == source)
}

fn assert_type(f: &FileFacts, subject: TypeSubject, type_name: &str, source: TypeSource) {
    let lang = f.language.unwrap();
    assert!(
        has_type(f, &subject, type_name, source),
        "{lang}: expected {subject:?} : {type_name} ({source:?}); got {:?}",
        f.types
    );
}

#[test]
fn rule_declared_and_constructed_types() {
    use TypeSource::{Constructed, Declared};

    let src = "class Client {}\nfunction connect(url: string, opts: Options | null): Promise<Client> {\n  const c = new Client();\n  let t: Transport = make();\n  return c;\n}\n";
    let f = facts("src/net.ts", Language::TypeScript, src);
    let connect = decl(&f, "connect");
    assert_type(&f, var(connect, "opts"), "Options", Declared);
    assert_eq!(f.types_of(&var(connect, "opts")).count(), 1, "null dropped");
    assert!(f.types_of(&var(connect, "url")).next().is_none(), "primitive types are skipped");
    assert_type(&f, TypeSubject::Return { decl: connect }, "Promise", Declared);
    assert_type(&f, var(connect, "c"), "Client", Constructed);
    assert_type(&f, var(connect, "t"), "Transport", Declared);
    assert!(f.types.windows(2).all(|w| w[0].span.start <= w[1].span.start), "sorted by span");

    let src = "package m\n\ntype Server struct{ name Name }\n\nfunc (s *Server) Run(req *Request) error {\n\tsrv := &Server{}\n\tvar h Handler\n\treturn nil\n}\n";
    let f = facts("server.go", Language::Go, src);
    let server = decl(&f, "Server");
    let run = decl(&f, "Server.Run");
    assert_type(&f, field(server, "name"), "Name", Declared);
    assert_type(&f, var(run, "s"), "Server", Declared);
    assert_type(&f, var(run, "req"), "Request", Declared);
    assert_type(&f, TypeSubject::Return { decl: run }, "error", Declared);
    assert_type(&f, var(run, "srv"), "Server", Constructed);
    assert_type(&f, var(run, "h"), "Handler", Declared);

    let src = "struct Walker { depth: usize, root: Node }\n\nimpl Walker {\n    fn new(root: &Node) -> Self { Walker { depth: 0, root: root.clone() } }\n    fn walk(&self) -> Option<Entry> {\n        let w = Walker::new(&self.root);\n        let p = Point { x: 1 };\n        let e: Entry = make();\n        None\n    }\n}\n";
    let f = facts("src/walk.rs", Language::Rust, src);
    let walker = decl(&f, "Walker");
    let new = decl(&f, "Walker.new");
    let walk = decl(&f, "Walker.walk");
    assert_type(&f, field(walker, "root"), "Node", Declared);
    assert!(f.types_of(&field(walker, "depth")).next().is_none(), "primitive");
    assert_type(&f, var(new, "root"), "Node", Declared);
    assert_type(&f, TypeSubject::Return { decl: new }, "Walker", Declared);
    assert_type(&f, TypeSubject::Return { decl: walk }, "Option", Declared);
    assert_type(&f, var(walk, "w"), "Walker", Constructed);
    assert_type(&f, var(walk, "p"), "Point", Constructed);
    assert_type(&f, var(walk, "e"), "Entry", Declared);

    let src = "class Service {\n  private Store store;\n  Result run(Request req) {\n    Parser p = new Parser();\n    var q = new Queue<String>();\n    return null;\n  }\n}\n";
    let f = facts("Service.java", Language::Java, src);
    let service = decl(&f, "Service");
    let run = decl(&f, "Service.run");
    assert_type(&f, field(service, "store"), "Store", Declared);
    assert_type(&f, var(run, "req"), "Request", Declared);
    assert_type(&f, TypeSubject::Return { decl: run }, "Result", Declared);
    assert_type(&f, var(run, "p"), "Parser", Declared);
    assert_type(&f, var(run, "p"), "Parser", Constructed);
    assert_type(&f, var(run, "q"), "Queue", Constructed);
    assert!(!has_type(&f, &var(run, "q"), "var", Declared), "`var` declares no type");

    let src = "from typing import Optional\n\nclass Store:\n    pass\n\nclass Service:\n    limit: int = 3\n\n    def __init__(self, store: Optional[Store]) -> None:\n        self.store = Store()\n        self.cache: \"Cache\" = make()\n\n    def run(self, x: \"pkg.Item\") -> Result:\n        y = Builder()\n        z = compute()\n        return y\n";
    let f = facts("svc.py", Language::Python, src);
    let service = decl(&f, "Service");
    let init = decl(&f, "Service.__init__");
    let run = decl(&f, "Service.run");
    assert_type(&f, field(service, "limit"), "int", Declared);
    assert_type(&f, var(init, "store"), "Store", Declared);
    assert!(f.types_of(&TypeSubject::Return { decl: init }).next().is_none(), "None dropped");
    assert_type(&f, field(service, "store"), "Store", Constructed);
    assert_type(&f, field(service, "cache"), "Cache", Declared);
    assert_type(&f, var(run, "x"), "pkg.Item", Declared);
    assert_type(&f, TypeSubject::Return { decl: run }, "Result", Declared);
    assert_type(&f, var(run, "y"), "Builder", Constructed);
    assert!(f.types_of(&var(run, "z")).next().is_none());

    let src = "<?php\nclass Service {\n    private ?Client $client;\n    public function run(Request $req): ?Response {\n        $b = new Builder();\n        return null;\n    }\n}\n";
    let f = facts("Service.php", Language::Php, src);
    let service = decl(&f, "Service");
    let run = decl(&f, "Service.run");
    assert_type(&f, field(service, "client"), "Client", Declared);
    assert_type(&f, var(run, "$req"), "Request", Declared);
    assert_type(&f, TypeSubject::Return { decl: run }, "Response", Declared);
    assert_type(&f, var(run, "$b"), "Builder", Constructed);

    let src = "class Engine {\n    Config config;\npublic:\n    Result* start(const Options& opts) {\n        auto e = new Engine();\n        Session s;\n        return nullptr;\n    }\n};\n";
    let f = facts("engine.cpp", Language::Cpp, src);
    let engine = decl(&f, "Engine");
    let start = decl(&f, "Engine.start");
    assert_type(&f, field(engine, "config"), "Config", Declared);
    assert_type(&f, var(start, "opts"), "Options", Declared);
    assert_type(&f, TypeSubject::Return { decl: start }, "Result", Declared);
    assert_type(&f, var(start, "e"), "Engine", Constructed);
    assert_type(&f, var(start, "s"), "Session", Declared);

    let src = "class Service {\n    private Store store;\n    public Result Run(Request req) {\n        var b = new Builder();\n        Parser p = Make();\n        return null;\n    }\n}\n";
    let f = facts("Service.cs", Language::CSharp, src);
    let service = decl(&f, "Service");
    let run = decl(&f, "Service.Run");
    assert_type(&f, field(service, "store"), "Store", Declared);
    assert_type(&f, var(run, "req"), "Request", Declared);
    assert_type(&f, TypeSubject::Return { decl: run }, "Result", Declared);
    assert_type(&f, var(run, "b"), "Builder", Constructed);
    assert_type(&f, var(run, "p"), "Parser", Declared);

    let src = "class Service(store: Store) {\n  def run(req: Request): Result = {\n    val b = Builder()\n    val n = new Node()\n    val q: Queue = make()\n    null\n  }\n}\n";
    let f = facts("Service.scala", Language::Scala, src);
    let service = decl(&f, "Service");
    let run = decl(&f, "Service.run");
    assert_type(&f, field(service, "store"), "Store", Declared);
    assert_type(&f, var(run, "req"), "Request", Declared);
    assert_type(&f, TypeSubject::Return { decl: run }, "Result", Declared);
    assert_type(&f, var(run, "b"), "Builder", Constructed);
    assert_type(&f, var(run, "n"), "Node", Constructed);
    assert_type(&f, var(run, "q"), "Queue", Declared);
}

const UTIL_JS: &str = include_str!("../../../../tests/fixtures/rule-syntax-annotations/util.js");
const SERVICE_PHP: &str = include_str!("../../../../tests/fixtures/rule-syntax-annotations/service.php");

/// Comment annotations are read from comment nodes only and attach to the adjacent
/// declaration / binding. Negative controls: annotation text inside a string literal, a
/// comment separated by a blank line, a comment before a statement that binds nothing.
#[test]
fn rule_comment_annotations_jsdoc_phpdoc() {
    use TypeSource::Comment;
    let named = |f: &FileFacts, name: &str| f.types.iter().any(|t| t.type_name == name);

    let f = facts("src/util.js", Language::JavaScript, UTIL_JS);
    let handle = decl(&f, "handle");
    let boxed = decl(&f, "Box");
    assert_type(&f, var(handle, "req"), "Request", Comment);
    assert_type(&f, var(handle, "opts"), "Options", Comment);
    assert_type(&f, TypeSubject::Return { decl: handle }, "Promise", Comment);
    assert_type(&f, var(handle, "cache"), "Cache", Comment);
    assert_type(&f, module_var("onError"), "Handler", Comment);
    assert_type(&f, field(boxed, "store"), "Store", Comment);
    assert!(!named(&f, "Nope"), "string literal");
    assert!(!named(&f, "Lost"), "blank line");

    let f = facts("src/Service.php", Language::Php, SERVICE_PHP);
    let service = decl(&f, "Service");
    let handle = decl(&f, "Service.handle");
    assert_type(&f, field(service, "client"), "Client", Comment);
    assert_type(&f, var(handle, "$req"), "Request", Comment);
    assert_type(&f, TypeSubject::Return { decl: handle }, "Service", Comment);
    assert_type(&f, var(handle, "$p"), "Parser", Comment);
    assert!(!named(&f, "Nope"), "string literal");
}

// ---------------------------------------------------------------------------------------
// Rule 17: property-assigned functions
// ---------------------------------------------------------------------------------------

#[test]
fn rule_property_assigned_function_names() {
    let src = "res.redirect = function redirect(url) { return url; };\nmodule.exports.render = function () {};\nexports.send = function () {};\nFoo.prototype.bar = function () {};\nfunction Box() { this.open = function () {}; }\napp.handlers.error = (e) => e;\n";
    let f = facts("lib/response.js", Language::JavaScript, src);
    let redirect = decl(&f, "res.redirect");
    let d = &f.declarations[redirect as usize];
    assert_eq!(d.name, "redirect");
    assert_eq!(d.container.as_deref(), Some("res"));
    for name in ["render", "send"] {
        let d = &f.declarations[decl(&f, name) as usize];
        assert_eq!(d.container, None, "CommonJS exports are module-level functions");
    }
    let bar = decl(&f, "Foo.bar");
    assert_eq!(f.declarations[bar as usize].container.as_deref(), Some("Foo"));
    let open = decl(&f, "Box.open");
    assert_eq!(f.declarations[open as usize].container, None, "`this.f = ...` has no container");
    let error = decl(&f, "app.handlers.error");
    assert_eq!(f.declarations[error as usize].container.as_deref(), Some("app.handlers"));

    let src = "const res = {};\nres.redirect = function (url: string): void {};\n";
    let f = facts("src/response.ts", Language::TypeScript, src);
    assert_eq!(f.declarations[decl(&f, "res.redirect") as usize].container.as_deref(), Some("res"));
}

// ---------------------------------------------------------------------------------------
// Callback arguments (DESIGN §1.10 item 1): positional index / keyword, callback forms
// ---------------------------------------------------------------------------------------

struct CallbackCase {
    language: Language,
    path: &'static str,
    source: &'static str,
    /// `(name, index, keyword)` of every recorded callback argument, in source order.
    expect: &'static [(&'static str, Option<u32>, Option<&'static str>)],
}

const CALLBACK_CASES: &[CallbackCase] = &[
    CallbackCase {
        language: Language::Python,
        path: "app.py",
        source: "def h():\n    pass\n\ndef main():\n    run(1, h, key=h)\n",
        expect: &[("h", Some(1), None), ("h", None, Some("key"))],
    },
    CallbackCase {
        language: Language::JavaScript,
        path: "app.js",
        source: "function h() {}\nsetTimeout(h, 10);\n",
        expect: &[("h", Some(0), None)],
    },
    CallbackCase {
        language: Language::Java,
        path: "A.java",
        source: "class A {\n  void h() {}\n  void m() { run(1, this::h); list.forEach(A::h); }\n}\n",
        expect: &[("h", Some(1), None), ("h", Some(0), None)],
    },
    CallbackCase {
        language: Language::Rust,
        path: "src/main.rs",
        source: "fn h() {}\nfn main() { run(1, module::h); }\n",
        expect: &[("h", Some(1), None)],
    },
    CallbackCase {
        language: Language::C,
        path: "main.c",
        source: "void h(void) {}\nint main(void) { atexit(&h); signal(2, h); return 0; }\n",
        expect: &[("h", Some(0), None), ("h", Some(1), None)],
    },
    CallbackCase {
        language: Language::Php,
        path: "app.php",
        source: "<?php\nfunction h() {}\nrun(1, h(...));\n",
        expect: &[("h", Some(1), None)],
    },
];

/// Every callback argument records its positional index among the call's arguments or its
/// keyword, and the language's callback forms (method references, `&f`, scoped paths, PHP
/// `f(...)`) name the passed function.
#[test]
fn rule_callback_arg_index_and_keyword() {
    for case in CALLBACK_CASES {
        let f = facts(case.path, case.language, case.source);
        let got: Vec<(&str, Option<u32>, Option<&str>)> = f
            .callbacks
            .iter()
            .filter(|c| c.name == "h")
            .map(|c| (c.name.as_str(), c.index, c.keyword.as_deref()))
            .collect();
        assert_eq!(got, case.expect.to_vec(), "{}: {:?}", case.language, f.callbacks);
        for c in f.callbacks.iter().filter(|c| c.name == "h") {
            let spelled = &case.source[c.arg_span.start as usize..c.arg_span.end as usize];
            assert_eq!(spelled.trim_start_matches(':'), "h", "{}", case.language);
        }
    }
}

// ---------------------------------------------------------------------------------------
// Calls that are not calls, calls by name, declarations by language rule
// ---------------------------------------------------------------------------------------

fn callees(f: &FileFacts) -> Vec<&str> {
    f.calls.iter().map(|c| c.callee.as_str()).collect()
}

fn spelled(src: &str, at: ByteSpan) -> &str {
    &src[at.start as usize..at.end as usize]
}

/// Calling a type converts a value in Go (`[]byte(s)`, `(func())(g)`, `(*T)(p)`): no
/// function runs, so none of them is a call site; the pointer type's name is a `type`
/// reference.
#[test]
fn rule_go_type_conversion_is_not_a_call() {
    let src = "package p\n\nfunc f(b []byte, s string) {\n\tx := []byte(s)\n\ty := (*Header)(unsafe.Pointer(&b))\n\tz := (func())(g)\n\tuse(x, y, z)\n}\n";
    let f = facts("p.go", Language::Go, src);
    let all = callees(&f);
    assert!(!all.iter().any(|c| c.starts_with('(') || c.starts_with('[')), "{all:?}");
    assert!(all.contains(&"unsafe.Pointer") && all.contains(&"use"), "{all:?}");
    assert_eq!(f.calls.len(), f.call_details.len());
    let header: Vec<&trace_core::facts::Reference> =
        f.references.iter().filter(|r| r.name == "Header").collect();
    assert_eq!(header.len(), 1, "{:?}", f.references);
    assert_eq!(header[0].kind, RefKind::Type);
}

/// `(*pkg.T)(p)` and `(*List[int])(p)` convert to pointer types; the type names are `type`
/// references.
#[test]
fn rule_parenthesized_pointer_type_call_is_a_conversion() {
    let src = "package p\n\nfunc f(p unsafe.Pointer) {\n\ta := (*http.Request)(p)\n\tb := (*List[int])(p)\n\tkeep(a, b)\n}\n";
    let f = facts("p.go", Language::Go, src);
    assert_eq!(callees(&f), ["keep"]);
    for name in ["Request", "List"] {
        let found = f
            .references
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("{name}: {:?}", f.references));
        assert_eq!(found.kind, RefKind::Type, "{name}");
    }
}

/// Negative: a parenthesized function value is called (`(g)(2)`), and so is a dereferenced
/// local pointer to a function (`(*fp)(1)`).
#[test]
fn rule_function_value_in_parentheses_is_a_call() {
    let src = "package p\n\nfunc f(g func(int)) {\n\tfp := &g\n\t(*fp)(1)\n\t(g)(2)\n}\n";
    let f = facts("p.go", Language::Go, src);
    let all = callees(&f);
    assert!(all.contains(&"(*fp)") && all.contains(&"(g)"), "{all:?}");
}

/// C++ functional casts of built-in types (`int(x)`) are conversions; `T(x)` constructs.
#[test]
fn rule_cpp_functional_cast_of_a_builtin_type_is_not_a_call() {
    let src = "void f(double x) {\n    auto a = int(x);\n    auto b = T(x);\n}\n";
    let f = facts("f.cpp", Language::Cpp, src);
    assert_eq!(callees(&f), ["T"]);
}

/// A function-like macro (`#define F(x) ...`) is a callable declaration named at the
/// `#define` name (never a stub, parameters read), so a call `F(a)` that the server maps
/// to the definition lands on it; object-like macros are not declarations.
#[test]
fn rule_function_like_macro_is_a_declaration() {
    let src = "#define CFUNC(func, name, arity) {func, name, arity}\n#define NOOP(x)\n#define LIMIT 10\nint main(void) { CFUNC(a, \"n\", 1); return LIMIT; }\n";
    for language in [Language::C, Language::Cpp] {
        let f = facts("src/builtin.c", language, src);
        let cfunc = &f.declarations[decl(&f, "CFUNC") as usize];
        assert_eq!(cfunc.kind, SymbolKind::Function, "{language}");
        assert!(!cfunc.is_stub, "{language}");
        assert_eq!(spelled(src, cfunc.name_span), "CFUNC");
        assert_eq!(cfunc.span.start_line, 1);
        let params: Vec<&str> = cfunc.parameters.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(params, ["func", "name", "arity"], "{language}");
        assert!(!f.declarations[decl(&f, "NOOP") as usize].is_stub, "{language}");
        assert!(decls(&f, "LIMIT").is_empty(), "{language}");
    }
}

/// The call of a macro is an ordinary call site owned by its function: the server's
/// definition (the `#define` name) maps to the macro declaration.
#[test]
fn rule_macro_call_maps_to_the_macro() {
    let src = "#define GET_REAL(sym) real_##sym\nstatic int run(void) { return GET_REAL(open)(1); }\n";
    let f = facts("src/inject.c", Language::C, src);
    let macro_decl = decl(&f, "GET_REAL");
    let run = decl(&f, "run");
    let call = f.calls.iter().find(|c| c.callee == "GET_REAL").expect("macro call");
    assert_eq!(call.owner, Some(run));
    let target = &f.declarations[macro_decl as usize];
    assert_eq!(target.name, call.callee);
    assert!(target.name_span.start < call.callee_span.start);
}

/// Scala `x.y` evaluates the parameterless member `y`: a member-access argument passes its
/// value, never a function; eta expansion (`f _`, `obj.m _`), plain names and placeholder
/// functions (`_.name`) stay callback arguments.
#[test]
fn rule_scala_member_access_argument_is_not_a_function_reference() {
    let src = "object A {\n  def run(xs: List[Foo]): Unit = {\n    check(CodecTests[Foo].codec)\n    xs.map(f)\n    xs.map(obj.m _)\n    xs.map(g _)\n    xs.map(_.name)\n  }\n}\n";
    let f = facts("A.scala", Language::Scala, src);
    let names: Vec<&str> = f.callbacks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["f", "m", "g", "name"]);
}

/// `do.call("f", args)`, `do.call(f, args)`, `do.call(what = "f", ...)` and
/// `base::do.call(f, ...)` call `f` (a call site, not a callback argument); a computed name
/// adds nothing.
#[test]
fn rule_do_call_with_literal_calls_that_function() {
    let src = "k <- function(a, args) {\n  do.call(\"f\", list(a))\n  do.call(g, args)\n  do.call(what = \"h\", args = list())\n  base::do.call(m, args)\n  do.call(paste0(\"x\", a), args)\n}\n";
    let f = facts("R/k.R", Language::R, src);
    let k = decl(&f, "k");
    for name in ["f", "g", "h", "m"] {
        let call = f
            .calls
            .iter()
            .find(|c| c.callee == name)
            .unwrap_or_else(|| panic!("{name}: {:?}", callees(&f)));
        assert_eq!(call.owner, Some(k), "{name}");
        assert_eq!(spelled(src, call.callee_span), name);
        assert_eq!(call.receiver, None, "{name}");
    }
    assert!(!f.callbacks.iter().any(|c| c.name == "g" || c.name == "m"), "{:?}", f.callbacks);
    assert_eq!(f.calls.len(), f.call_details.len());
    // Separators are no arguments: `do.call("f", list(a))` has two.
    let first = f
        .calls
        .iter()
        .find(|c| c.member.as_deref() == Some("do.call"))
        .expect("do.call");
    assert_eq!(first.arg_count, 2);
}

/// An R function whose body dispatches with `UseMethod("g")` is an S3 generic: a stub whose
/// methods `g.<class>` implement it. Other functions are not.
#[test]
fn rule_function_calling_usemethod_is_a_generic_stub() {
    let src = "print_it <- function(x, ...) {\n  UseMethod(\"print_it\")\n}\nprint_it.default <- function(x, ...) cat(x)\nhelper <- function(x) x + 1\n";
    let f = facts("R/print.R", Language::R, src);
    assert!(f.declarations[decl(&f, "print_it") as usize].is_stub);
    assert!(!f.declarations[decl(&f, "print_it.default") as usize].is_stub);
    assert!(!f.declarations[decl(&f, "helper") as usize].is_stub);
}

/// Haskell: class method signatures are stub members of the class; an instance is an
/// out-of-line relation whose span holds its member equations (declared for the instance
/// type), so each instance member implements the class member.
#[test]
fn rule_instance_relation_encloses_its_members() {
    let src = "module M where\n\nclass Describe a where\n  describe :: a -> String\n\ninstance Describe Shape where\n  describe s = \"x\"\n\ninstance Show a => Describe (Tree a) where\n  describe t = \"t\"\n";
    let f = facts("src/M.hs", Language::Haskell, src);
    let signature = decls(&f, "Describe.describe");
    assert_eq!(signature.len(), 1);
    assert!(signature[0].1.is_stub && signature[0].1.kind == SymbolKind::Method);
    assert_eq!(f.impls.len(), 2, "{:?}", f.impls);
    for rel in &f.impls {
        assert_eq!(rel.trait_name, "Describe");
        let members: Vec<&Declaration> = f
            .declarations
            .iter()
            .filter(|d| d.name == "describe" && d.container.as_deref() == Some(rel.type_name.as_str()))
            .collect();
        assert_eq!(members.len(), 1, "{}", rel.type_name);
        assert!(rel.span.encloses(members[0].span.bytes), "{}", rel.type_name);
        assert!(!members[0].is_stub);
    }
    // Rust relations keep the `impl` block as their span.
    let src = "trait Run { fn run(&self); }\nstruct S;\nimpl Run for S {\n    fn run(&self) {}\n}\n";
    let f = facts("src/lib.rs", Language::Rust, src);
    let rel = &f.impls[0];
    assert_eq!(spelled(src, rel.span).lines().next(), Some("impl Run for S {"));
}

/// One Go parameter declaration naming several parameters declares each of them, in order
/// (`func (g *G) handle(method, path string, hs ...H)` has three parameters).
#[test]
fn rule_go_parameter_declaration_with_several_names_declares_each() {
    let src = "package p\n\ntype H func()\ntype G struct{}\n\nfunc (g *G) handle(method, path string, hs ...H) {}\n";
    let f = facts("p.go", Language::Go, src);
    let d = &f.declarations[decl(&f, "G.handle") as usize];
    let names: Vec<&str> = d.parameters.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["method", "path", "hs"]);
    assert_eq!(d.parameters[2].kind, trace_core::facts::ParamKind::VarPositional);
}
