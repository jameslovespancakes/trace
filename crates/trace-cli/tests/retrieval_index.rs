mod support;
use support::Fixture;

#[test]
fn anonymous_test_blocks_have_distinct_retrievable_source_identities() {
    let fx = Fixture::new("retrieval-blocks", "typescript");
    let source = "import { test as renamed } from 'node:test'\nexport function production() { return 7 }\nexport function runSuite() {\n renamed('same label', () => { production() })\n renamed('same label', () => { production() })\n}\n";
    fx.write("src/odd.ts", source);
    let Some(_) = fx.index_or_skip() else { return };
    let found = fx.json(&["symbols", "same label", "--file", "src/odd.ts", "--tests", "--mode", "name"]);
    let matches = found["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 2);
    assert_ne!(matches[0]["id"], matches[1]["id"]);
    for row in matches {
        assert_eq!(row["kind"], "test_block");
        assert_eq!(row["test_role"], "block_candidate");
        assert_eq!(row["label"], "same label");
        assert_eq!(row["semantic"], false);
        let shown = fx.json(&["show", row["id"].as_str().unwrap()]);
        let item = &shown["symbols"][0];
        assert_eq!(item["source"], "renamed('same label', () => { production() })");
        assert!(item["callers"].is_null() && item["calls"].is_null());
        assert_eq!(item["definition"]["callable"], false);
        assert!(item["definition"]["conditional"].is_null());
        assert!(item["definition"]["binding_definitions"].is_null());
    }
    let body = fx.json(&["symbols", "production", "--file", "src/odd.ts", "--tests", "--mode", "body"]);
    for row in matches {
        assert!(body["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == row["id"]));
    }
    // Mixed files do not turn unrelated production functions into test candidates.
    assert_eq!(
        fx.json(&["symbols", "production", "--file", "src/odd.ts", "--tests", "--mode", "name"])["total"],
        0
    );
    let text = fx.text(&["symbols", "same label", "--tests"]);
    assert!(text.contains("same label") && text.contains("test block?"));
}

#[test]
fn go_constants_are_retrieval_only_with_whole_group_provenance() {
    let fx = Fixture::new("retrieval-go-data", "go");
    let source =
        "package main\nconst (\n Alpha = iota\n Beta\n)\nvar Initialized = 42\nvar Uninitialized int\n";
    fx.write("bindings.go", source);
    let Some(_) = fx.index_or_skip() else { return };
    let shown = fx.json(&["show", "Alpha", "Beta", "Initialized", "Uninitialized"]);
    assert_eq!(shown["symbols"].as_array().unwrap().len(), 3);
    assert_eq!(shown["errors"].as_array().unwrap().len(), 1);
    assert_eq!(shown["symbols"][0]["source"], "const (\n Alpha = iota\n Beta\n)");
    assert_eq!(shown["symbols"][0]["source"], shown["symbols"][1]["source"]);
    for row in shown["symbols"].as_array().unwrap() {
        assert_eq!(row["symbol"]["kind"], "data");
        assert!(row["callers"].is_null() && row["calls"].is_null());
    }
    assert_eq!(fx.run(&["context", "bindings.go:Beta"]).status.code(), Some(2));
    fx.write("bindings.go", &source.replace("Alpha = iota", "Alpha = 50"));
    assert_eq!(fx.json(&["show", "Beta"])["symbols"][0]["source"], "const (\n Alpha = 50\n Beta\n)");
}
