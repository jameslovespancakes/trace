mod support;
use serde_json::Value;
use support::Fixture;

#[test]
fn audit_data_definitions_exact_discovery_and_cache_refresh() {
    let fx = Fixture::new("audit-data", "python");
    let source = "# complete assignment fixture\nSETTING = make(\n    'first',\n)\nSETTING = 'second'\nif enabled:\n    MAYBE = 3\nleft = right = 4\nclass Owner:\n    member = 5\n    def method(self):\n        local = 6\ndef needle():\n    return SETTING\ndef carrier():\n    return external.marker_payload(needle)\ncallable_collision = 7\ndef callable_collision():\n    return 8\n";
    fx.write("data_case.py", source);
    fx.write("other_data.py", "SETTING = 'elsewhere'\n");
    fx.write("reexport_case.py", "from data_case import SETTING as imported_setting\n");
    std::fs::create_dir_all(fx.root.join("tests")).unwrap();
    fx.write("tests/test_data_case.py", "TEST_VALUE = 9\ndef test_value():\n    assert TEST_VALUE == 9\n");
    let Some(_) = fx.index_or_skip() else { return };

    let shown = fx.json(&[
        "--audit",
        "show",
        "data_case.py:SETTING",
        "data_case.py:SETTING#2",
        "data_case.py:SETTING",
    ]);
    assert_eq!(shown["symbols"].as_array().unwrap().len(), 2);
    assert_eq!(shown["symbols"][0]["source"], "SETTING = make(\n    'first',\n)");
    assert_eq!(shown["symbols"][1]["source"], "SETTING = 'second'");
    for item in shown["symbols"].as_array().unwrap() {
        assert_eq!(item["symbol"]["kind"], "data");
        assert_eq!(item["definition"]["binding_definitions"], 2);
        assert_eq!(item["definition"]["callable"], false);
        assert_eq!(item["definition"]["provenance"], "syntax");
        assert!(item["callers"].is_null() && item["calls"].is_null());
    }
    let text = fx.text(&["--audit", "show", "data_case.py:SETTING"]);
    assert!(text.contains("3 |     'first',"), "{text}");
    assert!(text.contains("runtime binding unknown"), "{text}");
    assert!(!text.contains("0 callers"), "{text}");
    let line = fx.json(&["--audit", "show", "data_case.py:3"]);
    assert_eq!(line["symbols"][0]["symbol"]["id"], "data_case.py:SETTING");
    let conditional = fx.json(&["--audit", "show", "data_case.py:MAYBE"]);
    assert_eq!(conditional["symbols"][0]["definition"]["conditional"], true);
    let chain = fx.json(&["--audit", "show", "data_case.py:left", "data_case.py:right"]);
    assert_eq!(chain["symbols"][0]["source"], "left = right = 4");
    assert_eq!(chain["symbols"][0]["source"], chain["symbols"][1]["source"]);

    // A graph declaration keeps its existing ID; data gets a collision-free source ID.
    let collision = fx.json(&["symbols", "callable_collision", "--file", "data_case.py"]);
    assert_eq!(collision["total"], 2);
    for hit in collision["matches"].as_array().unwrap() {
        let item = fx.json(&["--audit", "show", hit["id"].as_str().unwrap()]);
        assert_eq!(item["symbols"][0]["symbol"]["kind"], hit["kind"]);
    }
    // Bare ambiguous names must not guess either a file or a reassignment's current value.
    let fail = fx.run(&["--json", "--audit", "show", "SETTING"]);
    assert_eq!(fail.status.code(), Some(2));
    let failed: Value = serde_json::from_slice(&fail.stdout).unwrap();
    assert_eq!(failed["errors"][0]["error_type"], "ambiguous_symbol");
    assert_eq!(failed["errors"][0]["candidates"].as_array().unwrap().len(), 3);

    // Imports, attribute stores and local bindings are not fabricated module definitions.
    let partial = fx.json(&[
        "--audit",
        "show",
        "data_case.py:SETTING",
        "data_case.py:Wrong.SETTING",
        "reexport_case.py:imported_setting",
        "data_case.py:local",
        "data_case.py:Owner.member",
    ]);
    assert_eq!(partial["symbols"].as_array().unwrap().len(), 1);
    assert_eq!(partial["errors"].as_array().unwrap().len(), 4);
    assert_eq!(fx.json(&["symbols", "imported_setting", "--file", "reexport_case.py"])["total"], 0);
    assert_eq!(
        fx.json(&["show", "data_case.py:SETTING"])["symbols"][0]["source"],
        shown["symbols"][0]["source"]
    );
    assert_eq!(fx.run(&["--audit", "context", "data_case.py:SETTING"]).status.code(), Some(2));

    let exact = fx.json(&["symbols", "needle"]);
    assert_eq!(exact["match_mode"], "exact");
    assert_eq!(exact["total"], 1);
    assert_eq!(exact["matches"][0]["id"], "data_case.py:needle");
    let data = fx.json(&["symbols", "SETTING", "--file", "data_case.py", "--limit", "1"]);
    assert_eq!(data["match_mode"], "exact");
    assert_eq!(data["total"], 2);
    assert_eq!(data["next_offset"], 1);
    let second = fx.json(&["symbols", "SETTING", "--file", "data_case.py", "--limit", "1", "--offset", "1"]);
    assert_eq!(second["matches"][0]["id"], "data_case.py:SETTING#2");
    assert!(second["next_offset"].is_null());
    assert_eq!(fx.json(&["symbols", "need"])["match_mode"], "name");
    let broad = fx.json(&["symbols", "marker_payload", "--file", "data_case.py"]);
    assert_eq!(broad["match_mode"], "broad");
    assert_eq!(broad["matches"][0]["id"], "data_case.py:carrier");
    let tests = fx.json(&["symbols", "TEST_VALUE", "--tests"]);
    assert_eq!(tests["matches"][0]["id"], "tests/test_data_case.py:TEST_VALUE");
    assert!(tests["matches"][0]["source"].is_null());

    fx.write("data_case.py", &source.replace("'first'", "'updated'"));
    let fresh = fx.json(&["--audit", "show", "data_case.py:SETTING"]);
    assert_eq!(fresh["symbols"][0]["source"], "SETTING = make(\n    'updated',\n)");
    assert_eq!(fx.json(&["symbols", "SETTING", "--file", "data_case.py"])["total"], 2);
}
