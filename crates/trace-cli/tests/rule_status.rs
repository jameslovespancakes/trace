//! Rule e2e test of `status` (general fixes, rule 20): per language the in-repository
//! resolution rate (calls whose name matches a repository declaration) next to the raw rate,
//! and the server state (missing / not ready / failed).
//!
//! Runs with `TRACE_OFFLINE=1` on a private fixture copy.

mod support;

use support::{semantic_or_skip, Fixture};

#[test]
fn rule_status_reports_in_repo_rate_and_server_state() {
    let fx = Fixture::new("rule-status-py", "rule-status-py");
    let Some(index) = fx.index_or_skip() else { return };
    let status = fx.json(&["status"]);
    let python = status["resolution"]
        .as_array()
        .expect("resolution rows")
        .iter()
        .find(|r| r["language"] == "python")
        .unwrap_or_else(|| panic!("python row in {}", status["resolution"]))
        .clone();
    // pkg/core.py calls helper, compute (repository names) and len, str, print (builtins).
    let raw = python["resolved"].as_u64().unwrap() + python["unresolved"].as_u64().unwrap();
    let in_repo =
        python["in_repo_resolved"].as_u64().unwrap() + python["in_repo_unresolved"].as_u64().unwrap();
    assert_eq!(raw, 5, "{python}");
    assert_eq!(in_repo, 2, "{python}");
    let rate = python["in_repo_rate"]
        .as_f64()
        .expect("in_repo_rate with in-repo calls");
    assert!((0.0..=1.0).contains(&rate), "{python}");
    assert!(
        python["server"].is_null()
            || matches!(
                python["server"].as_str(),
                Some("server_missing" | "server_not_ready" | "server_failed")
            ),
        "{python}"
    );
    if python["warning"] == "low_resolution" {
        assert!(rate < 0.8, "the warning is judged on the in-repo rate: {python}");
    }
    assert!(status["semantic"]["memory_budget_mb"].is_u64(), "{}", status["semantic"]);

    let text = fx.text(&["status"]);
    let first = text.lines().next().unwrap();
    assert!(first.contains("% in-repo calls resolved ("), "{text}");
    assert!(first.contains("% all)"), "{text}");
    let line = text
        .lines()
        .find(|l| l.starts_with("python "))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(line.contains("% in-repo ("), "{line}");
    if semantic_or_skip(&index, "python") {
        // pyright resolves both repository calls.
        assert_eq!(python["in_repo_resolved"], 2, "{python}");
        assert_eq!(python["in_repo_rate"], 1.0, "{python}");
        assert!(python["warning"].is_null(), "{python}");
        if python["server"] == "server_not_ready" {
            assert!(line.ends_with("server not ready"), "{line}");
        }
    } else if python["server"] == "server_missing" {
        assert!(line.contains("server missing"), "{line}");
    }
}
