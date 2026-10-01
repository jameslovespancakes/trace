use trace_core::{facts::FileFacts, Language};
use trace_syntax::{extract, SourceInput};

fn facts(path: &str, language: Language, source: &str) -> FileFacts {
    extract(SourceInput {
        path,
        language,
        source: source.as_bytes(),
    })
    .unwrap()
}

#[test]
fn go_bindings_are_grammar_derived_complete_groups_not_graph_nodes() {
    let code = "package example\nconst (\n First = iota\n Second\n _ = 99\n)\nconst Left, Right = 1, 2\nvar Ready = build()\nvar Uninitialized int\nfunc local() { const Hidden = 3; var AlsoHidden = 4 }\n";
    let f = facts("arbitrary.go", Language::Go, code);
    let ds = &f.data_definitions;
    assert_eq!(
        ds.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        ["First", "Second", "Left", "Right", "Ready"]
    );
    assert_eq!(ds[0].span, ds[1].span);
    assert_eq!(&code[ds[1].span.bytes.range()], "const (\n First = iota\n Second\n _ = 99\n)");
    assert_eq!(&code[ds[2].span.bytes.range()], "const Left, Right = 1, 2");
    assert_eq!(&code[ds[4].span.bytes.range()], "var Ready = build()");
    assert!(f
        .declarations
        .iter()
        .all(|d| !["First", "Second", "Left", "Right", "Ready"].contains(&d.name.as_str())));
    for d in ds {
        assert_eq!(&code[d.name_span.range()], d.name);
        assert!(!d.conditional);
    }
}

#[test]
fn framework_imports_work_in_arbitrary_paths_and_with_aliases() {
    for (path, language) in [
        ("odd/place.ts", Language::TypeScript),
        ("odd/place.tsx", Language::Tsx),
        ("odd/place.js", Language::JavaScript),
    ] {
        let code = "import renamed from 'tap'\nconst execute = () => {\n renamed.test('same title', t => { t.equal(1, 1) })\n renamed.test('same title', t => { t.equal(2, 2) })\n}\n";
        let f = facts(path, language, code);
        assert_eq!(f.tests.len(), 2, "{path}");
        assert_ne!(f.tests[0].span, f.tests[1].span);
        for b in f.tests {
            assert_eq!(b.name, "same title");
            assert_eq!(b.line, b.end_line);
            assert!(code[b.span.range()].starts_with("renamed.test("));
        }
    }
    let f = facts(
        "random.ts",
        Language::TypeScript,
        "import { test as renamed } from 'node:test'\nrenamed('alias', () => { assert(true) })\n",
    );
    assert_eq!(f.tests.len(), 1);
}

#[test]
fn ordinary_callbacks_and_strings_are_not_test_blocks() {
    let code = "import suite from 'tap'\nimport bus from 'events'\nbus.on('payload', () => { return 1 })\nsuite.test('not a block')\nconst text = \"test('not syntax', () => {})\"\n";
    assert!(facts("production.ts", Language::TypeScript, code).tests.is_empty());
    assert!(facts(
        "production.ts",
        Language::TypeScript,
        "import suite from 'unrelated'\nsuite.test('ordinary', () => {})\n"
    )
    .tests
    .is_empty());
    let shadow = "import { test as renamed } from 'node:test'\nfunction ordinary(renamed: Function) { renamed('shadowed import', () => {}) }\n";
    assert!(facts("production.ts", Language::TypeScript, shadow).tests.is_empty());
}
