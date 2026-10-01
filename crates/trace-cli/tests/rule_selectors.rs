//! Rule e2e tests of the one selector grammar (general fixes, rules 17 and 18):
//! `Type:method`, JS property-assigned functions `obj.prop`, `::` / `.` / `:` separators,
//! `file:line` on module-level code (an error naming the nearest symbols, never `<module>`),
//! numbered candidates for ambiguous bare names, no fallback for inexact selectors across
//! files. Navigation retains its legacy named-file normalization; source Show is strict.
//!
//! Every test runs with `TRACE_OFFLINE=1` on a private fixture copy.

mod support;

use support::Fixture;

/// Rule 17: `Type:method` with and without a file part, and every
/// equivalent separator, select the same method.
#[test]
fn rule_selector_type_colon_method() {
    let fx = Fixture::new("rule-selector-colon", "rule-selector-colon");
    let Some(_) = fx.index_or_skip() else { return };
    let mut ids = Vec::new();
    for selector in [
        "app/pickers.py:Picker:set_selection",
        "Picker:set_selection",
        "Picker.set_selection",
        "Picker::set_selection",
        "app/pickers.py:Picker.set_selection",
    ] {
        let uses = fx.json(&["uses", selector]);
        assert_eq!(uses["symbol"]["name"], "set_selection", "{selector}: {}", uses["symbol"]);
        assert_eq!(uses["symbol"]["file"], "app/pickers.py", "{selector}");
        ids.push(uses["symbol"]["id"].as_str().unwrap().to_string());
    }
    ids.dedup();
    assert_eq!(ids.len(), 1, "one symbol for every spelling: {ids:?}");
    // The uid is itself a selector.
    let again = fx.json(&["deps", &ids[0]]);
    assert_eq!(again["symbol"]["id"], ids[0].as_str());
    // A method that does not exist is an error, never another symbol.
    let err = fx.json_error(&["uses", "Picker:set_selections"], 2);
    assert_eq!(err["error_type"], "symbol_not_found", "{err}");
}

/// Rule 17: `obj.prop` names a function assigned to a property
/// (`res.redirect = function redirect(url) {...}`).
#[test]
fn rule_selector_property_assigned_function() {
    let fx = Fixture::new("rule-selector-js-property", "rule-selector-js-property");
    let Some(_) = fx.index_or_skip() else { return };
    for selector in ["lib/response.js:res.redirect", "res.redirect"] {
        let uses = fx.json(&["uses", selector]);
        assert_eq!(uses["symbol"]["name"], "redirect", "{selector}: {}", uses["symbol"]);
        assert_eq!(uses["symbol"]["file"], "lib/response.js", "{selector}");
        assert_eq!(uses["symbol"]["line"], 3, "{selector}");
    }
    let send = fx.json(&["uses", "res.send"]);
    assert_eq!(send["symbol"]["name"], "send");
    let err = fx.json_error(&["uses", "req.redirect"], 2);
    assert_eq!(err["error_type"], "symbol_not_found", "{err}");
}

/// Rule 18: `file:line` on module-level code is `symbol_not_found` (exit 2) naming the
/// nearest named symbols; `<module>` is reachable by its exact uid only.
#[test]
fn rule_selector_file_line_without_named_symbol_is_an_error() {
    let fx = Fixture::new("rule-selector-line", "rule-selector-line");
    let Some(_) = fx.index_or_skip() else { return };
    // Line 7 is inside `helper`.
    assert_eq!(fx.json(&["deps", "app/main.py:7"])["symbol"]["id"], "app/main.py:helper");
    for line in ["app/main.py:10", "app/main.py:3"] {
        let err = fx.json_error(&["uses", line], 2);
        assert_eq!(err["error_type"], "symbol_not_found", "{err}");
        let nearest: Vec<&str> = err["nearest"]
            .as_array()
            .expect("nearest")
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        assert_eq!(nearest.first().copied(), Some("helper (line 6)"), "{err}");
        assert!(
            err["error"]
                .as_str()
                .unwrap()
                .contains("is not inside a named function"),
            "{err}"
        );
    }
    // Text: the error on stderr, nothing on stdout, exit 2.
    let out = fx.run(&["uses", "app/main.py:10"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("Error: "));
    // `<module>` by exact uid only.
    assert_eq!(fx.json(&["deps", "app/main.py:<module>"])["symbol"]["qualified_name"], "<module>");
    let err = fx.json_error(&["deps", "<module>"], 2);
    assert_eq!(err["error_type"], "symbol_not_found");
}

/// Rule 18: an inexact selector never falls back; an ambiguous bare name lists numbered
/// candidates (exit 2); module-qualified spellings are exact.
#[test]
fn rule_selector_never_falls_back() {
    let fx = Fixture::new("rule-selector-none", "rule-selector-line");
    let Some(_) = fx.index_or_skip() else { return };
    let err = fx.json_error(&["uses", "helper"], 2);
    assert_eq!(err["error_type"], "ambiguous_symbol", "{err}");
    let ids: Vec<&str> = err["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["app/main.py:helper", "app/other.py:helper"]);
    assert_eq!(err["candidates"][1]["n"], 2);
    let out = fx.run(&["uses", "helper"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("\n  1. app/main.py:helper\n  2. app/other.py:helper"), "{stderr}");
    // Exact spellings.
    assert_eq!(fx.json(&["uses", "app/main.py:helper"])["symbol"]["id"], "app/main.py:helper");
    assert_eq!(fx.json(&["uses", "app.main.helper"])["symbol"]["id"], "app/main.py:helper");
    // Inexact: an unindexed file, a missing member, a path-like prefix.
    for selector in [
        "app/nosuch.py:helper",
        "app/main.py:helpers",
        "helper.extra",
        "nosuch.py:helper",
    ] {
        let err = fx.json_error(&["uses", selector], 2);
        assert_eq!(err["error_type"], "symbol_not_found", "{selector}: {err}");
    }
    // Without a file part, the error names the symbols called exactly like the last segment.
    let err = fx.json_error(&["uses", "helper.extra.helper"], 2);
    assert!(err["error"].as_str().unwrap().contains("app/main.py:helper"), "{err}");
}

/// A wrong or extra qualifier inside the named file (`file:Type.name` for a free function
/// `name` of that file) resolves to the file's unique symbol with that name; never to a
/// symbol of another file.
#[test]
fn rule_selector_wrong_qualifier_in_the_named_file() {
    let fx = Fixture::new("rule-selector-qualifier", "rule-selector-line");
    let Some(_) = fx.index_or_skip() else { return };
    for selector in ["app/main.py:Nope.helper", "app/main.py:HELPER", "main.py:Engine.helper"] {
        assert_eq!(fx.json(&["deps", selector])["symbol"]["id"], "app/main.py:helper", "{selector}");
    }
    // Promoted citation-friendly Show must not silently discard a supplied owner.
    let out = fx.run(&["--json", "show", "app/other.py:X.helper"]);
    assert_eq!(out.status.code(), Some(2));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(report["symbols"].as_array().unwrap().is_empty());
    assert_eq!(report["errors"][0]["requested"], "app/other.py:X.helper");
    assert_eq!(report["errors"][0]["error_type"], "symbol_not_found");
    assert_eq!(
        fx.json(&["show", "app/other.py:helper"])["symbols"][0]["symbol"]["id"],
        "app/other.py:helper"
    );
}
