mod support;
use support::Fixture;

#[test]
fn fallback_is_explicit_bounded_numbered_and_exactly_pageable() {
    let fx = Fixture::new("source-fallback", "python");
    let source = "# café needle\r\nTEXT = 'needle, needle'\r\ndef sample():\r\n    return 'needle'\r\n";
    fx.write("fallback.py", source);
    fx.write("giant.py", &format!("VALUE = '{}needle{}'\n", "x".repeat(400), "y".repeat(50_000)));
    let Some(_) = fx.index_or_skip() else { return };
    let first = fx.json(&["search", "needle", "--file", "fallback.py", "--limit", "2"]);
    assert_eq!(first["matches"].as_array().unwrap().len(), 2);
    assert_eq!(first["next_offset"], 2);
    assert_eq!(first["matches"][0]["line"], 1);
    assert_eq!(first["matches"][0]["column"], 8); // Unicode characters, not UTF-8 bytes.
    assert_eq!(first["matches"][1]["line"], 2); // Duplicate literal on one line occurs once.
    let last = fx.json(&["search", "needle", "--file", "fallback.py", "--limit", "2", "--offset", "2"]);
    assert_eq!(last["matches"].as_array().unwrap().len(), 1);
    assert_eq!(last["matches"][0]["owner"], "fallback.py:sample");
    assert!(last["next_offset"].is_null());
    assert!(fx.json(&["search", "NEEDLE", "--file", "fallback.py"])["matches"]
        .as_array()
        .unwrap()
        .is_empty());
    let window = fx.json(&["source", "fallback.py", "--start", "2", "--lines", "2"]);
    assert_eq!(window["source"], "TEXT = 'needle, needle'\r\ndef sample():\r\n");
    assert_eq!(window["next_line"], 4);
    let eof = fx.json(&["source", "fallback.py", "--start", "4", "--lines", "200"]);
    assert_eq!(eof["end_line"], 4);
    assert!(eof["next_line"].is_null());
    assert!(fx
        .text(&["source", "fallback.py", "--start", "3", "--lines", "1"])
        .contains("3 | def sample():"));
    let giant = fx.json(&["search", "needle", "--file", "giant.py"]);
    assert_eq!(giant["matches"][0]["truncated"], true);
    assert!(giant["matches"][0]["preview"].as_str().unwrap().contains("needle"));
    assert_eq!(giant["matches"][0]["preview"].as_str().unwrap().chars().count(), 160);
    fx.json_error(&["source", "giant.py"], 2); // Reject oversized range, never trim its source.
    for path in ["../fallback.py", ".env", "file:fallback.py", "missing.py"] {
        fx.json_error(&["source", path], 2);
    }
    fx.json_error(&["source", "fallback.py", "--start", "99"], 2);
    let show = fx.json(&["show", "sample", "does_not_exist"]);
    assert_eq!(show["symbols"][0]["source"], "def sample():\r\n    return 'needle'");
    assert_eq!(show["errors"].as_array().unwrap().len(), 1);
    assert!(fx.text(&["show", "sample"]).contains("3 | def sample():"));
    assert!(fx
        .text(&["symbols", "nonexistent", "--tests"])
        .contains("search <literal>"));
}

#[test]
fn fallback_bounds_are_rejected_before_repository_work() {
    let fx = Fixture::new("fallback-invalid", "python");
    for args in [
        vec!["source", "app.py", "--start", "0"],
        vec!["source", "app.py", "--lines", "201"],
        vec!["source", "app.py", "--lines", "0"],
        vec!["search", "x", "--limit", "101"],
        vec!["search", "x", "--limit", "0"],
    ] {
        assert_eq!(fx.run(&args).status.code(), Some(2));
    }
}
