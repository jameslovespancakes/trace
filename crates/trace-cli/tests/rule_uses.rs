//! Rule e2e tests of the `uses` contract (general fixes, package A): rows are proven +
//! inferred only and never contradict `check:` (rule 1), `check:` drops local bindings,
//! server-resolved and unrelated-type sites (rule 2), is ranked and capped (rule 3), member
//! accesses never use free functions (rule 5), overload families confirm calls (rule 7), and
//! `complete` means every same-name site is resolved (rule 19).
//!
//! Every test runs with `TRACE_OFFLINE=1` on a private fixture copy.

mod support;

use serde_json::Value;
use support::{
    assert_no_contradictions, check_at, row_mark, rows_at, semantic_or_skip, use_heads, use_row, Fixture,
};

/// The `check:` section of a text answer: (row lines, overflow line).
fn check_section(text: &str) -> (Vec<&str>, Option<&str>) {
    let mut lines = text.lines().skip_while(|l| *l != "check:");
    if lines.next().is_none() {
        return (Vec::new(), None);
    }
    let mut rows = Vec::new();
    let mut more = None;
    for l in lines {
        if l.starts_with("  (+") {
            more = Some(l);
        } else if l.starts_with("  ") {
            rows.push(l);
        }
    }
    (rows, more)
}

fn tier_of(row: &Value) -> &str {
    row["tier"].as_str().unwrap_or("")
}

/// The mark of the `uses` rows before `check:` (`<file>:<line>[ mark]  <caller>`): never `?`.
fn assert_no_possible_rows(text: &str) {
    for l in use_heads(text) {
        assert_ne!(row_mark(l), "?", "possible row in uses: {l}");
    }
}

/// Rule 5: a member access (`this.handler`, `app.handler`) never uses the exported
/// function `handler`, even when value flow links them; a namespace import (`ns.handler`)
/// does.
#[test]
fn rule_member_access_never_uses_a_free_function() {
    let fx = Fixture::new("rule-uses-member-binding", "rule-uses-member-binding");
    let Some(index) = fx.index_or_skip() else { return };
    let uses = fx.json(&["uses", "src/handlers.ts:handler"]);
    assert_no_contradictions(&uses);
    let app = "src/app.ts";
    for line in [8, 13] {
        assert!(rows_at(&uses, app, line).is_empty(), "member access at {app}:{line} is not a use: {uses}");
        assert!(
            check_at(&uses, app, line).is_empty(),
            "member access at {app}:{line} is resolved elsewhere: {uses}"
        );
    }
    let c = &uses["completeness"];
    let reasons = &c["elsewhere_reasons"];
    let member_or_server =
        reasons["member_binding"].as_u64().unwrap_or(0) + reasons["server"].as_u64().unwrap_or(0);
    assert!(member_or_server >= 2, "{c}");
    // The call through the declaring file is a use.
    assert!(use_row(&uses, "src/handlers.ts", 8, "call").is_some(), "{uses}");
    if semantic_or_skip(&index, "typescript") {
        // The namespace import binds the module: the server proves the call.
        let call = use_row(&uses, app, 17, "call").unwrap_or_else(|| panic!("ns.handler row in {uses}"));
        assert_eq!(call["tier"], "proven", "{call}");
        assert!(check_at(&uses, app, 17).is_empty());
    }
    let text = fx.text(&["uses", "src/handlers.ts:handler"]);
    assert_no_possible_rows(&text);
}

/// Rule 7: overloads of one type form one family; calls whose candidates are all family
/// members are confirmed (inferred `family_overloads` / narrowed), never `?`.
#[test]
fn rule_family_candidates_confirm_overload_calls() {
    let fx = Fixture::new("rule-uses-overloads", "rule-uses-overloads");
    let Some(_) = fx.index_or_skip() else { return };
    // Two overloads: a bare name is ambiguous (numbered candidates, exit 2).
    let err = fx.json_error(&["uses", "Clone"], 2);
    assert_eq!(err["error_type"], "ambiguous_symbol", "{err}");
    let candidates = err["candidates"].as_array().expect("candidates");
    assert_eq!(candidates.len(), 2, "{err}");
    assert_eq!(candidates[0]["n"], 1);
    let first = candidates[0]["id"].as_str().unwrap();
    let second = candidates[1]["id"].as_str().unwrap();

    let uses = fx.json(&["uses", first]);
    assert_no_contradictions(&uses);
    let family: Vec<&str> = uses["family"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert!(family.contains(&second), "overload in the family: {family:?}");
    // Both declarations are declaration rows.
    for line in [10, 15] {
        assert!(use_row(&uses, "Model/Node.cs", line, "declaration").is_some(), "declaration {line}: {uses}");
    }
    // Calls of either overload are confirmed uses of the family.
    for line in [9, 14] {
        let call =
            use_row(&uses, "App/Use.cs", line, "call").unwrap_or_else(|| panic!("call {line} in {uses}"));
        assert!(matches!(tier_of(call), "proven" | "inferred"), "{call}");
        assert!(check_at(&uses, "App/Use.cs", line).is_empty(), "{uses}");
    }
    // The same answer from the other overload.
    let other = fx.json(&["uses", second]);
    assert_no_contradictions(&other);
    for line in [9, 14] {
        assert!(use_row(&other, "App/Use.cs", line, "call").is_some(), "{other}");
    }
}

/// Rule 3: unresolved sites are ranked (the target's file, files importing it, then
/// name-only matches), JSON keeps every site with its rank, text shows 20 and `(+N more)`.
#[test]
fn rule_check_is_ranked_same_module_imports_name_only() {
    let fx = Fixture::new("rule-uses-ranking", "rule-uses-ranking");
    let Some(_) = fx.index_or_skip() else { return };
    let target = "core/worker.py:Worker.process";
    let uses = fx.json(&["uses", target]);
    assert_no_contradictions(&uses);
    let c = &uses["completeness"];
    let open = c["unresolved"].as_array().unwrap();
    assert!(open.len() >= 21, "{c}");
    assert_eq!(uses["counts"]["unresolved"].as_u64().unwrap() as usize, open.len());
    // Ranks are 1..n in order; scopes never go back to an earlier group.
    let order = |scope: &str| match scope {
        "same_module" => 0,
        "imports_target" => 1,
        "name_only" => 2,
        other => panic!("unknown scope {other}"),
    };
    let mut last = 0;
    for (i, u) in open.iter().enumerate() {
        assert_eq!(u["rank"].as_u64().unwrap() as usize, i + 1, "{u}");
        let g = order(u["scope"].as_str().unwrap());
        assert!(g >= last, "ranked order: {open:?}");
        last = g;
        let file = u["at"]["file"].as_str().unwrap();
        let expected = match file {
            "core/worker.py" => "same_module",
            "app/importer.py" => "imports_target",
            _ => "name_only",
        };
        assert_eq!(u["scope"], expected, "{u}");
    }
    let name_only = open.iter().filter(|u| u["scope"] == "name_only").count();
    assert!(name_only >= 21, "{c}");
    // The summary says what is left.
    let summary = c["summary"].as_str().unwrap();
    assert!(
        summary.starts_with(&format!("partial: {} same-name sites unresolved (", open.len())),
        "{summary}"
    );

    // Text: 20 ranked rows, the file line repeated when the file changes, then the rest.
    let text = fx.text(&["uses", target]);
    let (rows, more) = check_section(&text);
    assert_eq!(rows.len(), 20, "{text}");
    assert_eq!(more, Some(format!("  (+{} more)", open.len() - 20).as_str()), "{text}");
    assert_no_possible_rows(&text);
    let first_file = open[0]["at"]["file"].as_str().unwrap();
    let after_check: Vec<&str> = text.lines().skip_while(|l| *l != "check:").skip(1).collect();
    assert_eq!(after_check[0], first_file, "{text}");
}
