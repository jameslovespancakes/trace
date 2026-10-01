mod support;
use serde_json::Value;
use support::Fixture;

#[test]
fn audit_source_partial_batches_and_discovery() {
    let fx = Fixture::new("audit-source", "python");
    fx.write("shop/audit_case.py", "# citation fixture\n\nclass Real:\n    def work(self):\n        return 'complete-source'\n\ndef helper():\n    return 42\n");
    fx.write("collision.py", "def root_only():\n    return 'root'\n");
    fx.write("shop/collision.py", "def nested_only():\n    return 'nested'\n");
    let Some(_) = fx.index_or_skip() else {
        return;
    };
    let good = "shop/audit_case.py:Real.work";
    let ordinary = fx.json(&["show", good]);
    let audit = fx.json(&["--audit", "show", good, "shop/audit_case.py:Wrong.work", "does_not_exist", good]);
    assert_eq!(audit["symbols"].as_array().unwrap().len(), 1);
    assert_eq!(audit["errors"].as_array().unwrap().len(), 2);
    assert_eq!(audit["symbols"][0]["source"], ordinary["symbols"][0]["source"]);
    assert_eq!(audit["symbols"][0]["symbol"]["id"], good);
    let text = fx.text(&["--audit", "show", good]);
    assert!(text.contains("5 |         return 'complete-source'"), "{text}");
    let context = fx.text(&["--audit", "context", good]);
    assert!(context.contains("5 |         return 'complete-source'"), "{context}");
    let raw_context = fx.json(&["--audit", "context", good]);
    assert_eq!(raw_context["source"], ordinary["symbols"][0]["source"]);
    let failed = fx.run(&["--json", "--audit", "show", "shop/audit_case.py:Wrong.work", "missing"]);
    assert_eq!(failed.status.code(), Some(2));
    let failed: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(failed["errors"].as_array().unwrap().len(), 2);
    assert!(failed["symbols"].as_array().unwrap().is_empty());
    // Citation-friendly partial results are the default; --audit remains an alias.
    let default = fx.json(&["show", good, "missing"]);
    assert_eq!(default["symbols"].as_array().unwrap().len(), 1);
    assert_eq!(default["errors"].as_array().unwrap().len(), 1);
    let module = fx.json(&["--audit", "show", "shop/audit_case.py"]);
    assert_eq!(
        module["symbols"][0]["source"],
        std::fs::read_to_string(fx.root.join("shop/audit_case.py")).unwrap()
    );
    let suffix = fx.json(&["--audit", "show", "audit_case.py:Real.work"]);
    assert_eq!(suffix["symbols"][0]["symbol"]["id"], good);
    let canonical_file = fx.json(&["--audit", "show", "collision.py"]);
    assert_eq!(canonical_file["symbols"][0]["symbol"]["id"], "collision.py:<module>");
    assert!(canonical_file["symbols"][0]["source"]
        .as_str()
        .unwrap()
        .contains("root_only"));
    let first = fx.json(&["symbols", "--file", "audit_case.py", "--limit", "1"]);
    assert_eq!(first["matches"].as_array().unwrap().len(), 1);
    assert!(first["total"].as_u64().unwrap() >= 3);
    assert_eq!(first["next_offset"], 1);
    let second = fx.json(&["symbols", "--file", "audit_case.py", "--limit", "1", "--offset", "1"]);
    assert_ne!(first["matches"][0]["id"], second["matches"][0]["id"]);
    let found = fx.json(&["symbols", "helper", "--file", "audit_case.py"]);
    assert_eq!(found["matches"][0]["id"], "shop/audit_case.py:helper");
    assert!(found["matches"][0].get("source").is_none());
    let tests = fx.json(&["symbols", "--tests"]);
    assert!(!tests["matches"].as_array().unwrap().is_empty());
    assert!(tests["matches"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["is_test"] == true));
    let end = fx.json(&["symbols", "--offset", "999999"]);
    assert!(end["matches"].as_array().unwrap().is_empty());
    assert!(end["next_offset"].is_null());
}
