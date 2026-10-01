//! Per-language extraction audit (NEXT.md item 17): one inline fixture per benchmark
//! language covering definitions (function, method, class, constructor where the language
//! has them), plain and method calls, imports, nested and anonymous functions, a
//! module-level call executed by `<module>`, a callback call owned by its `<lambda>`, a
//! `write` reference and an `import` reference. The coverage report over the 21 benchmark
//! repositories is `scripts/multilang/extraction_audit.py` (examples/dump_facts.rs).

use trace_core::facts::{FileFacts, RefKind};
use trace_core::model::SymbolKind;
use trace_core::Language;

use crate::{extract, SourceInput};

/// Expected declaration kind (`Any` when the grammar decides, e.g. Python `__init__`).
#[derive(Clone, Copy, Debug)]
enum K {
    Any,
    Function,
    Method,
    Class,
    Interface,
    Constructor,
    Lambda,
}

struct Fixture {
    language: Language,
    path: &'static str,
    source: &'static str,
    /// Declarations that must exist: (qualified name, kind).
    defs: &'static [(&'static str, K)],
    /// Calls: (member or callee text, qualified name of the executing declaration;
    /// `<module>` = module-level code).
    calls: &'static [(&'static str, &'static str)],
    /// Local names bound by imports (`*` for wildcard imports).
    imports: &'static [&'static str],
    /// `import` references (the imported-name identifier of a binding).
    import_refs: &'static [&'static str],
    /// `write` references (store targets, attribute targets included).
    writes: &'static [&'static str],
}

fn run(fx: &Fixture) -> FileFacts {
    let f = extract(SourceInput {
        path: fx.path,
        language: fx.language,
        source: fx.source.as_bytes(),
    })
    .unwrap_or_else(|e| panic!("{}: {e}", fx.language));
    assert_eq!(f.error_count, 0, "{}: fixture must parse without errors", fx.language);
    f
}

fn qualified(f: &FileFacts, decl: Option<u32>) -> Option<&str> {
    decl.and_then(|d| f.declarations.get(d as usize))
        .map(|d| d.qualified_name.as_str())
}

fn check(fx: &Fixture) {
    let lang = fx.language;
    let f = run(fx);
    let all: Vec<&str> = f.declarations.iter().map(|d| d.qualified_name.as_str()).collect();
    // `<module>`: last, whole file, executes owner-less code.
    let module = f.module_decl.expect("module declaration");
    assert_eq!(module as usize, f.declarations.len() - 1, "{lang}");
    assert_eq!(f.declarations[module as usize].kind, SymbolKind::Module, "{lang}");
    assert_eq!(f.declarations[module as usize].span.bytes.end as usize, fx.source.len(), "{lang}");

    for (name, kind) in fx.defs {
        let found: Vec<(u32, &trace_core::facts::Declaration)> = f
            .declarations
            .iter()
            .enumerate()
            .filter(|(_, d)| d.qualified_name == *name)
            .map(|(i, d)| (i as u32, d))
            .collect();
        assert!(!found.is_empty(), "{lang}: no declaration {name} in {all:?}");
        let ok = found.iter().any(|(i, d)| match kind {
            K::Any => true,
            K::Function => d.kind == SymbolKind::Function && d.name != "<lambda>",
            K::Method => d.kind == SymbolKind::Method,
            K::Class => d.kind == SymbolKind::Class,
            K::Interface => d.kind == SymbolKind::Interface,
            K::Constructor => d.kind == SymbolKind::Constructor,
            K::Lambda => d.name == "<lambda>" && f.is_synthetic(*i),
        });
        let kinds: Vec<SymbolKind> = found.iter().map(|(_, d)| d.kind).collect();
        assert!(ok, "{lang}: {name} has kinds {kinds:?}, expected {kind:?}");
    }

    for (callee, owner) in fx.calls {
        let call = f
            .calls
            .iter()
            .find(|c| c.member.as_deref() == Some(*callee) || c.callee == *callee)
            .unwrap_or_else(|| {
                let seen: Vec<&str> = f.calls.iter().map(|c| c.callee.as_str()).collect();
                panic!("{lang}: no call {callee} in {seen:?}")
            });
        let executing = qualified(&f, f.executing_owner(call.owner));
        assert_eq!(executing, Some(*owner), "{lang}: owner of {callee}");
        if *owner == "<module>" {
            assert_eq!(call.owner, None, "{lang}: module-level {callee} keeps owner None");
        }
    }

    for local in fx.imports {
        assert!(
            f.imports.iter().any(|i| i.local == *local),
            "{lang}: no import {local} in {:?}",
            f.imports.iter().map(|i| (&i.local, &i.target)).collect::<Vec<_>>()
        );
    }
    let refs = |kind: RefKind| -> Vec<&str> {
        f.references
            .iter()
            .filter(|r| r.kind == kind)
            .map(|r| r.name.as_str())
            .collect()
    };
    for name in fx.import_refs {
        assert!(
            refs(RefKind::Import).contains(name),
            "{lang}: no import reference {name}: {:?}",
            refs(RefKind::Import)
        );
    }
    for name in fx.writes {
        assert!(
            refs(RefKind::Write).contains(name),
            "{lang}: no write reference {name}: {:?}",
            refs(RefKind::Write)
        );
    }

    // Owner-less calls only in module-level / class-body code or declaration headers
    // (decorators, defaults, base lists), never inside a callable's body.
    for c in &f.calls {
        if c.owner.is_some() {
            continue;
        }
        if let Some(lex) = c.lexical_owner {
            let d = &f.declarations[lex as usize];
            let in_body =
                d.kind.is_callable() && c.span.start >= d.body_start && c.span.end <= d.span.bytes.end;
            assert!(!in_body, "{lang}: call {} in the body of {} has no owner", c.callee, d.qualified_name);
        }
    }
    // Every synthetic lambda has an anonymous-scope record whose parent is its lexical
    // enclosing declaration.
    for (i, d) in f.declarations.iter().enumerate() {
        if d.name == "<lambda>" {
            assert!(f.anonymous_of(i as u32).is_some(), "{lang}: {} without scope record", d.qualified_name);
            assert!(d.parent.is_none_or(|p| (p as usize) < i), "{lang}: pre-order");
        }
    }
    // The same facts minus `<module>` satisfy the shared invariants.
    let _ = crate::test_support::strip_module(f, fx.source.len());
}

const PYTHON: Fixture = Fixture {
    language: Language::Python,
    path: "pkg/service.py",
    source: "import os\nfrom pkg.helpers import redirect as r\n\n\nclass Service(Base):\n    def __init__(self, store):\n        self.store = store\n\n    def run(self, items):\n        def inner(x):\n            return helper(x)\n        self.store.save(items)\n        return list(map(lambda i: transform(i), items))\n\n\ndef helper(v):\n    return v\n\n\napp.redirect = r\nsetup()\n",
    defs: &[
        ("Service", K::Class),
        ("Service.__init__", K::Method),
        ("Service.run", K::Method),
        ("Service.run.inner", K::Function),
        ("Service.run.<lambda>", K::Lambda),
        ("helper", K::Function),
    ],
    calls: &[
        ("helper", "Service.run.inner"),
        ("save", "Service.run"),
        ("transform", "Service.run.<lambda>"),
        ("setup", "<module>"),
    ],
    imports: &["os", "r"],
    import_refs: &["os", "redirect"],
    writes: &["redirect", "store"],
};

const JAVASCRIPT: Fixture = Fixture {
    language: Language::JavaScript,
    path: "src/context.js",
    source: "import { Hono } from 'hono';\nimport * as util from './util.js';\n\nexport class Context {\n  constructor(req) { this.req = req; }\n  notFound() { return util.render(404); }\n  redirect = (url) => { return this.header(url); };\n}\n\nfunction outer() {\n  function nested() { return helper(); }\n  return nested();\n}\n\nconst app = new Hono();\napp.get('/x', (c) => c.notFound());\napp.redirect = outer;\nstart();\n",
    defs: &[
        ("Context", K::Class),
        ("Context.constructor", K::Constructor),
        ("Context.notFound", K::Method),
        ("Context.redirect", K::Method),
        ("outer", K::Function),
        ("outer.nested", K::Function),
        ("<lambda>", K::Lambda),
    ],
    calls: &[
        ("render", "Context.notFound"),
        ("header", "Context.redirect"),
        ("helper", "outer.nested"),
        ("notFound", "<lambda>"),
        ("get", "<module>"),
        ("start", "<module>"),
    ],
    imports: &["Hono", "util"],
    import_refs: &["Hono", "util"],
    writes: &["redirect", "req"],
};

const TYPESCRIPT: Fixture = Fixture {
    language: Language::TypeScript,
    path: "src/context.ts",
    source: "import { Hono } from 'hono';\nimport type { Env } from './types';\n\nexport interface Handler { handle(c: Context): void; }\n\nexport class Context<E extends Env = Env> {\n  constructor(private req: Request) {}\n  notFound(): Response { return this.render(404); }\n  render(code: number): Response { return new Response(null, { status: code }); }\n}\n\nexport const app = new Hono<Env>();\napp.get('/x', (c: Context) => c.notFound());\napp.onError = handler;\nexport { Hono as App } from 'hono';\ninit();\n",
    defs: &[
        ("Handler", K::Interface),
        ("Handler.handle", K::Method),
        ("Context", K::Class),
        ("Context.constructor", K::Constructor),
        ("Context.notFound", K::Method),
        ("<lambda>", K::Lambda),
    ],
    calls: &[
        ("render", "Context.notFound"),
        ("Response", "Context.render"),
        ("notFound", "<lambda>"),
        ("get", "<module>"),
        ("init", "<module>"),
    ],
    imports: &["Hono", "Env"],
    import_refs: &["Hono", "Env"],
    writes: &["onError"],
};

const RUST: Fixture = Fixture {
    language: Language::Rust,
    path: "src/walk.rs",
    source: "use std::collections::HashMap;\nuse crate::util::{helper, Config as Cfg};\npub use self::data::Data;\n\nconst LIMIT: usize = compute_limit();\n\npub struct Walker {\n    count: u32,\n}\n\nimpl<'a> Data<'a> {\n    pub fn from_bytes(bytes: &'a [u8]) -> Data<'a> {\n        Data::Bytes(bytes)\n    }\n}\n\nimpl<'a> serde::Serialize for Match<'a> {\n    fn serialize(&self) -> u32 {\n        let d = &Data::from_bytes(self.bytes);\n        helper(d)\n    }\n}\n\nimpl Walker {\n    pub fn new() -> Walker {\n        Walker { count: 0 }\n    }\n\n    pub fn walk(&mut self, items: Vec<u32>) -> u32 {\n        fn inner(x: u32) -> u32 {\n            helper(x)\n        }\n        self.count = 1;\n        items.iter().map(|x| transform(*x)).sum()\n    }\n}\n",
    defs: &[
        ("Walker", K::Class),
        ("Data.from_bytes", K::Method),
        ("Match.serialize", K::Method),
        ("Walker.new", K::Method),
        ("Walker.walk", K::Method),
        ("Walker.walk.inner", K::Function),
        ("Walker.walk.<lambda>", K::Lambda),
    ],
    calls: &[
        ("from_bytes", "Match.serialize"),
        ("map", "Walker.walk"),
        ("transform", "Walker.walk.<lambda>"),
        ("compute_limit", "<module>"),
    ],
    imports: &["HashMap", "helper", "Cfg", "Data"],
    import_refs: &["HashMap", "helper", "Config"],
    writes: &["count"],
};

const GO: Fixture = Fixture {
    language: Language::Go,
    path: "server.go",
    source: "package main\n\nimport (\n\t\"fmt\"\n\th \"net/http\"\n)\n\nvar client = newClient()\n\ntype Server struct{ count int }\n\nfunc NewServer() *Server { return &Server{} }\n\nfunc (s *Server) Run(items []int) {\n\tinner := func(x int) int { return helper(x) }\n\ts.count = 1\n\tfmt.Println(inner(1))\n\th.HandleFunc(\"/\", func(w h.ResponseWriter, r *h.Request) { serve(w) })\n}\n",
    defs: &[
        ("Server", K::Class),
        ("NewServer", K::Function),
        ("Server.Run", K::Method),
        ("Server.Run.<lambda>", K::Lambda),
    ],
    calls: &[
        ("helper", "Server.Run.<lambda>"),
        ("serve", "Server.Run.<lambda>"),
        ("Println", "Server.Run"),
        ("newClient", "<module>"),
    ],
    imports: &["fmt", "h"],
    import_refs: &["fmt", "h"],
    writes: &["count"],
};

const JAVA: Fixture = Fixture {
    language: Language::Java,
    path: "src/app/Service.java",
    source: "package app;\n\nimport java.util.List;\nimport static java.util.Objects.requireNonNull;\n\npublic class Service extends Base {\n    private int count;\n    private int limit = compute();\n\n    public Service(Store store) {\n        this.store = store;\n    }\n\n    public void run(List<String> items) {\n        this.count = 1;\n        store.save(items);\n        items.forEach(x -> transform(x));\n        Runnable r = new Runnable() { public void run() { helper(); } };\n    }\n}\n",
    defs: &[
        ("Service", K::Class),
        ("Service.Service", K::Constructor),
        ("Service.run", K::Method),
        ("Service.run.run", K::Method),
        ("Service.run.<lambda>", K::Lambda),
    ],
    calls: &[
        ("save", "Service.run"),
        ("transform", "Service.run.<lambda>"),
        ("helper", "Service.run.run"),
        ("compute", "<module>"),
    ],
    imports: &["List", "requireNonNull"],
    import_refs: &["List", "requireNonNull"],
    writes: &["count", "store"],
};

const C: Fixture = Fixture {
    language: Language::C,
    path: "src/point.c",
    source: "#include <stdio.h>\n#include \"util.h\"\n\nstatic int counter;\nint start = compute();\n\nstruct point { int x; };\n\nint helper(int v);\n\nint add(int a, int b) {\n    counter = a;\n    return helper(a) + b;\n}\n\nvoid run(struct point *p) {\n    p->x = add(1, 2);\n    printf(\"%d\", p->x);\n}\n",
    defs: &[
        ("point", K::Class),
        ("helper", K::Function),
        ("add", K::Function),
        ("run", K::Function),
    ],
    calls: &[
        ("helper", "add"),
        ("add", "run"),
        ("printf", "run"),
        ("compute", "<module>"),
    ],
    imports: &["*"],
    import_refs: &[],
    writes: &["counter", "x"],
};

const CPP: Fixture = Fixture {
    language: Language::Cpp,
    path: "src/service.cpp",
    source: "#include <vector>\nusing std::vector;\n\nnamespace app {\nclass Service : public Base {\npublic:\n    Service(int n) : count(n) {}\n    void run(vector<int>& items);\nprivate:\n    int count;\n};\n\nvoid Service::run(vector<int>& items) {\n    this->count = 1;\n    store.save(items);\n    std::for_each(items.begin(), items.end(), [](int x) { transform(x); });\n}\n}\n\nint limit = compute();\n",
    // `namespace app { ... }` qualifies its declarations (extractor 10).
    defs: &[
        ("app.Service", K::Class),
        ("app.Service.Service", K::Constructor),
        ("app.Service.run", K::Method),
        ("app.Service.run.<lambda>", K::Lambda),
    ],
    calls: &[
        ("save", "app.Service.run"),
        ("for_each", "app.Service.run"),
        ("transform", "app.Service.run.<lambda>"),
        ("compute", "<module>"),
    ],
    imports: &["vector", "*"],
    import_refs: &["vector"],
    writes: &["count"],
};

const CSHARP: Fixture = Fixture {
    language: Language::CSharp,
    path: "src/Service.cs",
    source: "using System;\nusing Col = System.Collections.Generic;\n\nConsole.WriteLine(\"start\");\n\nnamespace App {\n    public class Service : Base {\n        public int Count { get { return Compute(); } }\n        public Service(Store store) { this.store = store; }\n        public void Run(List<int> items) {\n            this.Total = 1;\n            store.Save(items);\n            items.ForEach(x => Transform(x));\n            int Local(int v) { return Helper(v); }\n        }\n    }\n}\n",
    defs: &[
        ("Service", K::Class),
        ("Service.Service", K::Constructor),
        ("Service.Run", K::Method),
        ("Service.Run.Local", K::Function),
        ("Service.Run.<lambda>", K::Lambda),
        ("Service.<lambda>", K::Lambda),
    ],
    calls: &[
        ("Save", "Service.Run"),
        ("Transform", "Service.Run.<lambda>"),
        ("Helper", "Service.Run.Local"),
        ("Compute", "Service.<lambda>"),
        ("WriteLine", "<module>"),
    ],
    imports: &["*", "Col"],
    import_refs: &["Generic"],
    writes: &["Total", "store"],
};

const PHP: Fixture = Fixture {
    language: Language::Php,
    path: "src/Service.php",
    source: "<?php\nnamespace App;\n\nuse App\\Models\\User;\nuse App\\Util\\{Helper, Other as O};\n\nclass Service extends Base {\n    public function __construct($store) { $this->store = $store; }\n    public function run($items) {\n        $this->count = 1;\n        $this->store->save($items);\n        array_map(fn($x) => transform($x), $items);\n        $f = function ($y) { return convert($y); };\n        return User::find(1);\n    }\n}\n\nbootstrap();\n",
    defs: &[
        ("Service", K::Class),
        ("Service.__construct", K::Any),
        ("Service.run", K::Method),
        ("Service.run.<lambda>", K::Lambda),
    ],
    calls: &[
        ("save", "Service.run"),
        ("find", "Service.run"),
        ("transform", "Service.run.<lambda>"),
        ("convert", "Service.run.<lambda>"),
        ("bootstrap", "<module>"),
    ],
    imports: &["User", "Helper", "O"],
    import_refs: &["User", "Helper", "Other"],
    writes: &["count", "$f"],
};

const BASH: Fixture = Fixture {
    language: Language::Bash,
    path: "bin/greet.sh",
    source: "#!/bin/bash\nsource ./lib.sh\n\ncount=0\n\ngreet() {\n  local name=\"$1\"\n  helper \"$name\"\n  count=1\n}\n\ngreet world\n",
    defs: &[("greet", K::Function)],
    calls: &[("helper", "greet"), ("greet", "<module>")],
    imports: &["*"],
    import_refs: &[],
    writes: &["count"],
};

const SCALA: Fixture = Fixture {
    language: Language::Scala,
    path: "src/Service.scala",
    source: "package app\n\nimport app.util.Helper\nimport app.util.{Other => O, Third}\n\nclass Service(store: Store) extends Base {\n  var total = 0\n  def run(items: List[Int]): Unit = {\n    total = 1\n    store.save(items)\n    items.foreach(x => transform(x))\n    def inner(y: Int): Int = helper(y)\n  }\n}\n\nobject Main {\n  val started = boot()\n}\n",
    defs: &[
        ("Service", K::Class),
        ("Service.run", K::Method),
        ("Service.run.inner", K::Any),
        ("Service.run.<lambda>", K::Lambda),
        ("Main", K::Class),
    ],
    calls: &[
        ("save", "Service.run"),
        ("transform", "Service.run.<lambda>"),
        ("helper", "Service.run.inner"),
        ("boot", "<module>"),
    ],
    imports: &["Helper", "O", "Third"],
    import_refs: &["Helper", "Other", "Third"],
    writes: &["total"],
};

const R: Fixture = Fixture {
    language: Language::R,
    path: "R/run.R",
    source: "library(dplyr)\n\nhelper <- function(x) {\n  x + 1\n}\n\nrun <- function(items) {\n  total <- 0\n  inner <- function(y) helper(y)\n  sapply(items, function(v) transform(v))\n  obj$save(items)\n}\n\nsetup()\n",
    defs: &[
        ("helper", K::Function),
        ("run", K::Function),
        ("run.inner", K::Function),
        ("run.<lambda>", K::Lambda),
    ],
    calls: &[
        ("helper", "run.inner"),
        ("transform", "run.<lambda>"),
        ("sapply", "run"),
        ("save", "run"),
        ("setup", "<module>"),
    ],
    imports: &["*"],
    import_refs: &[],
    writes: &["total"],
};

const HASKELL: Fixture = Fixture {
    language: Language::Haskell,
    path: "src/Main.hs",
    source: "module Main where\n\nimport qualified Data.Map as M\nimport Data.List (sortBy)\n\ndata Shape = Circle Int\n\nclass Describe a where\n  describe :: a -> String\n\narea :: Shape -> Int\narea (Circle r) = helper r\n\nhelper x = sortBy compare (map (\\y -> transform y) x)\n\nmain = print (area (Circle 1))\n\nmatches = flip compare\n\ncheck s = s `matches` s\n",
    defs: &[
        ("Shape", K::Class),
        ("Describe", K::Interface),
        ("area", K::Function),
        ("helper", K::Function),
        ("helper.<lambda>", K::Lambda),
        // zero-argument bindings are function values (point-free style)
        ("main", K::Function),
        ("matches", K::Function),
        ("check", K::Function),
    ],
    calls: &[
        ("helper", "area"),
        ("sortBy", "helper"),
        ("transform", "helper.<lambda>"),
        ("print", "main"),
        ("flip", "matches"),
        // backticked infix application is a call of the named function
        ("matches", "check"),
    ],
    imports: &["M", "sortBy"],
    import_refs: &["M", "sortBy"],
    writes: &[],
};

/// The 14 benchmark languages (TSX shares the TypeScript tables).
const ALL: [&Fixture; 14] = [
    &PYTHON,
    &JAVASCRIPT,
    &TYPESCRIPT,
    &RUST,
    &GO,
    &JAVA,
    &C,
    &CPP,
    &CSHARP,
    &PHP,
    &BASH,
    &SCALA,
    &R,
    &HASKELL,
];

macro_rules! audit {
    ($($name:ident => $fixture:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                check(&$fixture);
            }
        )*
    };
}

audit! {
    audit_python => PYTHON,
    audit_javascript => JAVASCRIPT,
    audit_typescript => TYPESCRIPT,
    audit_rust => RUST,
    audit_go => GO,
    audit_java => JAVA,
    audit_c => C,
    audit_cpp => CPP,
    audit_csharp => CSHARP,
    audit_php => PHP,
    audit_bash => BASH,
    audit_scala => SCALA,
    audit_r => R,
    audit_haskell => HASKELL,
}

#[test]
fn every_benchmark_language_has_a_fixture() {
    let mut languages: Vec<Language> = ALL.iter().map(|f| f.language).collect();
    languages.sort();
    languages.dedup();
    assert_eq!(languages.len(), 14);
}

#[test]
fn tsx_shares_the_typescript_extraction() {
    let src =
        "import { h } from 'x';\nexport const View = () => <div onClick={() => h()} />;\nrender(View);\n";
    let f = extract(SourceInput {
        path: "src/view.tsx",
        language: Language::Tsx,
        source: src.as_bytes(),
    })
    .unwrap();
    let names: Vec<&str> = f.declarations.iter().map(|d| d.qualified_name.as_str()).collect();
    assert_eq!(names, vec!["View", "View.<lambda>", "<module>"]);
    let h = f.calls.iter().find(|c| c.callee == "h").unwrap();
    assert_eq!(qualified(&f, h.owner), Some("View.<lambda>"));
    let render = f.calls.iter().find(|c| c.callee == "render").unwrap();
    assert_eq!(qualified(&f, f.executing_owner(render.owner)), Some("<module>"));
}

/// P3 depends on this for the ripgrep gate: generic impl blocks use the bare type name.
#[test]
fn rust_generic_impl_containers_are_bare_type_names() {
    let f = run(&RUST);
    let from_bytes = f.declarations.iter().find(|d| d.name == "from_bytes").unwrap();
    assert_eq!(from_bytes.container.as_deref(), Some("Data"));
    assert_eq!(from_bytes.qualified_name, "Data.from_bytes");
    let serialize = f.declarations.iter().find(|d| d.name == "serialize").unwrap();
    assert_eq!(serialize.container.as_deref(), Some("Match"));
    assert_eq!(serialize.qualified_name, "Match.serialize");
    assert!(f
        .impls
        .iter()
        .any(|i| i.type_name == "Match" && i.trait_name == "Serialize"));
    // `&Data::from_bytes(..)`: member and receiver of the associated-function path.
    let call = f
        .calls
        .iter()
        .find(|c| c.member.as_deref() == Some("from_bytes"))
        .unwrap();
    assert_eq!(call.receiver.as_deref(), Some("Data"));
    assert_eq!(call.callee, "Data::from_bytes");
    // A 20-step builder chain: every step is a call owned by the method, the last one is
    // `current_dir`.
    let mut chain = String::from("impl HiArgs {\n    fn walk_builder(&self) -> WalkBuilder {\n        let mut b = WalkBuilder::new(&self.paths[0]);\n        b");
    for i in 0..19 {
        chain.push_str(&format!(".step{i}(self.v{i})"));
    }
    chain.push_str(".current_dir(&self.cwd);\n        b\n    }\n}\n");
    let f = extract(SourceInput {
        path: "src/hiargs.rs",
        language: Language::Rust,
        source: chain.as_bytes(),
    })
    .unwrap();
    let walk = f
        .declarations
        .iter()
        .position(|d| d.qualified_name == "HiArgs.walk_builder")
        .unwrap() as u32;
    let current = f
        .calls
        .iter()
        .find(|c| c.member.as_deref() == Some("current_dir"))
        .unwrap();
    assert_eq!(current.owner, Some(walk));
    let steps = f.calls.iter().filter(|c| c.owner == Some(walk)).count();
    assert_eq!(steps, 21, "new + 19 steps + current_dir");
}

#[test]
fn reference_kinds_classify_non_call_uses() {
    // Python: attribute write, import, argument, decorator, annotation, `del`.
    let src = "from .helpers import redirect as r\nimport functools\n\n@functools.cache\ndef view(x: Request) -> Response:\n    handle(callback)\n    total = 1\n    total += 2\n    del cache[x]\n    del obj.attr\n    return total\n\napp.redirect = r\n";
    let f = extract(SourceInput {
        path: "m.py",
        language: Language::Python,
        source: src.as_bytes(),
    })
    .unwrap();
    let kind_of = |name: &str| -> Vec<RefKind> {
        f.references
            .iter()
            .filter(|r| r.name == name)
            .map(|r| r.kind)
            .collect()
    };
    assert_eq!(kind_of("redirect"), vec![RefKind::Import, RefKind::Write]);
    assert!(kind_of("r").contains(&RefKind::Read));
    assert_eq!(kind_of("callback"), vec![RefKind::Argument]);
    assert!(kind_of("functools").contains(&RefKind::Decorator));
    assert_eq!(kind_of("Request"), vec![RefKind::Type]);
    assert_eq!(kind_of("Response"), vec![RefKind::Type]);
    assert_eq!(kind_of("total"), vec![RefKind::Write, RefKind::Write, RefKind::Read]);
    assert_eq!(kind_of("attr"), vec![RefKind::Write]);
    // The attribute write is owned by module-level code.
    let write = f
        .references
        .iter()
        .find(|r| r.name == "redirect" && r.kind == RefKind::Write)
        .unwrap();
    assert_eq!(write.owner, None);
    assert_eq!(&src[write.span.range()], "redirect");

    // TypeScript: type positions, re-export lists, declaring stores.
    let src = "import { A } from './a';\nclass B extends Base implements I {}\nconst x: Opts = make();\nlet y = 1;\ny = 2;\nexport { A as Alias };\n";
    let f = extract(SourceInput {
        path: "m.ts",
        language: Language::TypeScript,
        source: src.as_bytes(),
    })
    .unwrap();
    let kind_of = |name: &str| -> Vec<RefKind> {
        f.references
            .iter()
            .filter(|r| r.name == name)
            .map(|r| r.kind)
            .collect()
    };
    assert_eq!(kind_of("A"), vec![RefKind::Import, RefKind::Export]);
    assert!(kind_of("Alias").is_empty(), "export aliases are labels");
    assert_eq!(kind_of("Opts"), vec![RefKind::Type]);
    assert!(kind_of("I").contains(&RefKind::Type));
    assert_eq!(kind_of("x"), Vec::<RefKind>::new(), "const declares");
    assert_eq!(kind_of("y"), vec![RefKind::Write]);
    assert_eq!(f.exports.len(), 1);
    assert_eq!((f.exports[0].exported.as_str(), f.exports[0].target.as_str()), ("Alias", "./a.A"));
}

#[test]
fn exports_record_re_exports() {
    let src = "export { a as b, c } from './m';\nexport * from './n';\nexport * as ns from './o';\nimport { x } from './x';\nexport { x };\nconst local = 1;\nexport { local };\n";
    let f = extract(SourceInput {
        path: "index.js",
        language: Language::JavaScript,
        source: src.as_bytes(),
    })
    .unwrap();
    let got: Vec<(&str, &str)> = f
        .exports
        .iter()
        .map(|e| (e.exported.as_str(), e.target.as_str()))
        .collect();
    assert_eq!(got, vec![("b", "./m.a"), ("c", "./m.c"), ("*", "./n"), ("ns", "./o"), ("x", "./x.x")]);

    let src = "pub use crate::walk::{Walker, Builder as B};\npub use self::inner::*;\nuse std::fmt;\n";
    let f = extract(SourceInput {
        path: "src/lib.rs",
        language: Language::Rust,
        source: src.as_bytes(),
    })
    .unwrap();
    let got: Vec<(&str, &str)> = f
        .exports
        .iter()
        .map(|e| (e.exported.as_str(), e.target.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("Walker", "crate::walk::Walker"),
            ("B", "crate::walk::Builder"),
            ("*", "self::inner")
        ]
    );
    let exported: Vec<&str> = f
        .references
        .iter()
        .filter(|r| r.kind == RefKind::Export)
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(exported, vec!["Walker", "Builder"]);
    assert!(f
        .references
        .iter()
        .any(|r| r.name == "fmt" && r.kind == RefKind::Import));

    let src =
        "from .models import User\nfrom . import helpers\n\n__all__ = [\"User\", \"helpers\", \"missing\"]\n";
    let f = extract(SourceInput {
        path: "pkg/__init__.py",
        language: Language::Python,
        source: src.as_bytes(),
    })
    .unwrap();
    let got: Vec<(&str, &str)> = f
        .exports
        .iter()
        .map(|e| (e.exported.as_str(), e.target.as_str()))
        .collect();
    assert_eq!(got, vec![("User", ".models.User"), ("helpers", ".helpers")]);
    // Not a package `__init__`: `__all__` is not a re-export list.
    let f = extract(SourceInput {
        path: "pkg/mod.py",
        language: Language::Python,
        source: src.as_bytes(),
    })
    .unwrap();
    assert!(f.exports.is_empty());
}

#[test]
fn module_declaration_for_empty_and_test_files() {
    for (path, language) in [
        ("empty.go", Language::Go),
        ("tests/test_x.py", Language::Python),
        ("x.sh", Language::Bash),
    ] {
        let f = extract(SourceInput {
            path,
            language,
            source: b"",
        })
        .unwrap();
        assert_eq!(f.declarations.len(), 1, "{language}");
        let d = &f.declarations[0];
        assert_eq!(d.kind, SymbolKind::Module);
        assert_eq!(d.is_test, path.starts_with("tests/"));
        assert_eq!((d.span.start_line, d.span.end_line), (1, 1));
        assert!(f.is_synthetic(0));
    }
}
