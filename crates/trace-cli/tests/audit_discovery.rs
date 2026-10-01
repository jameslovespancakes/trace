mod support;
use serde_json::Value;
use support::Fixture;

fn ids(v: &Value) -> Vec<&str> {
    v["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect()
}

#[test]
fn name_only_show_and_test_aware_discovery_preserve_source_and_ambiguity() {
    let fx = Fixture::new("audit-discovery", "python");
    fx.write("lookup_case.py", "UNIQUE_BINDING = 7\ndef unique_lookup():\n    return UNIQUE_BINDING\nclass LookupOwner:\n    def named_method(self):\n        return unique_lookup()\n");
    std::fs::create_dir_all(fx.root.join("tests")).unwrap();
    fx.write("tests/test_lookup.py", "def test_registration_hook():\n    assert True\ndef test_early_answer():\n    registration_hook()\n    assert True\ndef test_outer():\n    def test():\n        return 1\n    def test_nested():\n        registration_hook()\n    return test()\nclass TestGroup:\n    def test_method(self):\n        registration_hook()\ndef helper_fixture():\n    registration_hook()\ndef test_documented():\n    \"\"\"documentation_marker\"\"\"\n    assert True\n");
    let Some(_) = fx.index_or_skip() else { return };
    let shown = fx.json(&["--audit", "show", "unique_lookup", "LookupOwner.named_method", "UNIQUE_BINDING"]);
    let sources = shown["symbols"].as_array().unwrap();
    assert_eq!(sources.len(), 3);
    assert_eq!(sources[0]["symbol"]["id"], "lookup_case.py:unique_lookup");
    assert_eq!(sources[0]["source"], "def unique_lookup():\n    return UNIQUE_BINDING");
    assert_eq!(sources[2]["symbol"]["id"], "lookup_case.py:UNIQUE_BINDING");
    let exact = fx.json(&["symbols", "unique_lookup"]);
    assert_eq!(exact["match_mode"], "exact");
    assert_eq!(exact["total"], 1);

    // A name hit must not hide other tests mentioning the queried operation.
    let auto = fx.json(&["symbols", "registration_hook", "--tests", "--file", "tests/test_lookup.py"]);
    assert_eq!(auto["match_mode"], "test_ranked");
    assert!(ids(&auto).contains(&"tests/test_lookup.py:test_registration_hook"));
    assert!(ids(&auto).contains(&"tests/test_lookup.py:test_early_answer"));
    let roles: Vec<_> = auto["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["test_role"].as_str().unwrap())
        .collect();
    let first_helper = roles.iter().position(|r| *r == "helper_candidate").unwrap();
    assert!(roles[..first_helper]
        .iter()
        .all(|r| matches!(*r, "case_candidate" | "container" | "block_candidate")));
    assert!(roles[first_helper..].iter().all(|r| *r != "case_candidate"));
    let names = fx.json(&["symbols", "registration_hook", "--tests", "--mode", "name"]);
    assert_eq!(ids(&names), vec!["tests/test_lookup.py:test_registration_hook"]);
    let body = fx.json(&["symbols", "registration_hook", "--tests", "--mode", "body"]);
    assert_eq!(body["match_mode"], "body");
    assert!(ids(&body).contains(&"tests/test_lookup.py:test_early_answer"));
    assert!(!ids(&body).contains(&"tests/test_lookup.py:test_registration_hook"));
    assert_eq!(fx.json(&["symbols", "documentation_marker", "--mode", "body"])["total"], 0);
    assert_eq!(fx.run(&["symbols", "--mode", "body"]).status.code(), Some(2));

    // A nested function literally called `test` no longer eclipses the test inventory.
    let tests = fx.json(&["symbols", "test", "--tests", "--file", "tests/test_lookup.py"]);
    assert_eq!(tests["match_mode"], "test_ranked");
    assert_eq!(tests["matches"][0]["test_role"], "case_candidate");
    let nested = tests["matches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "tests/test_lookup.py:test_outer.test_nested")
        .unwrap();
    assert_eq!(nested["test_role"], "helper_candidate");
    // Exact test IDs/qualified names and explicit name-mode still retrieve helpers.
    let helper = fx.json(&["symbols", "test_outer.test", "--tests"]);
    assert_eq!(helper["match_mode"], "exact");
    assert_eq!(helper["matches"][0]["test_role"], "helper_candidate");
    assert_eq!(fx.json(&["symbols", "test", "--tests", "--mode", "name"])["total"], 1);
    let first = fx.json(&["symbols", "registration_hook", "--tests", "--limit", "1"]);
    let second = fx.json(&["symbols", "registration_hook", "--tests", "--limit", "1", "--offset", "1"]);
    assert_eq!(first["next_offset"], 1);
    assert_eq!(second["matches"][0], auto["matches"][1]);

    // No relaxed owner substitution, and no source omission in mixed batches.
    let partial = fx.json(&["--audit", "show", "unique_lookup", "Wrong.named_method"]);
    assert_eq!(partial["symbols"][0]["source"], sources[0]["source"]);
    assert_eq!(partial["errors"].as_array().unwrap().len(), 1);
    fx.write("second_lookup.py", "def unique_lookup():\n    return False\n");
    let ambiguous = fx.run(&["--audit", "--json", "show", "unique_lookup"]);
    assert_eq!(ambiguous.status.code(), Some(2));
    let ambiguity: Value = serde_json::from_slice(&ambiguous.stdout).unwrap();
    assert_eq!(ambiguity["errors"][0]["error_type"], "ambiguous_symbol");
    assert_eq!(ambiguity["errors"][0]["candidates"].as_array().unwrap().len(), 2);
}
