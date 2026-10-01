//! Rule e2e tests of servers and speed, SPEC section 8.8 and DESIGN §1.13: start only what
//! is needed (a pending language, only test / fixture code of a repository dominated by
//! another language, is set up by the first query that needs one of its files) and syntax
//! answers (same-file shell functions answered from syntax: identical `uses` / `deps`
//! answers with far fewer server requests). Tests skip when a language server, toolchain or
//! dependency is missing on this machine (`index_or_skip`); nothing here builds or executes
//! the fixtures.

mod support;

use serde_json::Value;
use support::*;

/// Sum of `field` over the backend runs serving `language`.
fn backend_sum(report: &Value, language: &str, field: &str) -> u64 {
    report["backends"]
        .as_array()
        .map(|runs| {
            runs.iter()
                .filter(|r| {
                    r["languages"]
                        .as_array()
                        .is_some_and(|l| l.iter().any(|x| x == language))
                })
                .map(|r| r[field].as_u64().unwrap_or(0))
                .sum()
        })
        .unwrap_or(0)
}

/// Every backend run carries the readiness field (`true` / `false` / `null`).
fn assert_ready_field(report: &Value) {
    for run in report["backends"].as_array().into_iter().flatten() {
        assert!(run.get("ready").is_some(), "BackendRun.ready missing: {run}");
    }
}

/// DESIGN §1.13: Python product code plus Java files only in test source sets (`src/test`,
/// and the Gradle-style `src/jvmTest` that is not a test path by itself). Java is pending:
/// `index` does not set it up (no jdtls, no project import) and lists its files as not
/// analyzed; `uses` of a Java test symbol sets Java up first, then answers from the server.
#[test]
fn rule_pending_file_query_sets_up_its_language() {
    let fx = Fixture::new("rule_server_partitions", "rule-server-partitions");
    let Some(index) = fx.index_or_skip() else { return };
    assert_ready_field(&index);
    assert_eq!(backend_sum(&index, "java", "queried_files"), 0, "no jdtls queries at index time: {index}");
    assert_eq!(backend_sum(&index, "java", "requests"), 0, "{index}");
    assert!(index["pending_files"].as_u64().unwrap_or(0) >= 2, "{index}");
    let status = fx.json(&["status"]);
    let pending = status["pending"].as_array().expect("pending rows");
    assert!(pending.iter().any(|p| p["language"] == "java"), "{status}");
    assert!(
        status["setup"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["language"] != "java"),
        "a pending language is not required: {status}"
    );
    let test_file = "app/src/test/java/com/example/FormatTest.java";
    let selector = format!("{test_file}:FormatTest.banner");
    let Some(uses) = fx.json_or_skip(&["uses", selector.as_str()]) else { return };
    assert_no_contradictions(&uses);
    assert_eq!(uses["index"]["updated"], "incremental", "uses set up the pending language");
    let status = fx.json(&["status"]);
    assert_eq!(status["index"]["pending_files"], 0, "{status}");
    assert_eq!(backend_sum(&status, "java", "queried_files"), 2, "both Java files analyzed now: {status}");
    assert_ready_field(&status);
    let rows = rows_at(&uses, test_file, 11);
    assert!(!rows.is_empty(), "banner(\"x\") is a use: {uses}");
    assert!(rows.iter().all(|r| r["tier"] == "proven"), "{rows:?}");
    // Python is the repository's product code: analysed at index time.
    let python = fx.json(&["uses", "app/greeter/format.py:trim_name"]);
    assert_no_contradictions(&python);
}

/// (file, line, start, kind, tier, owner) of every `uses` row.
fn use_rows(report: &Value) -> Vec<(String, u64, u64, String, String, String)> {
    let mut rows: Vec<_> = report["uses"]
        .as_array()
        .expect("uses rows")
        .iter()
        .map(|r| {
            (
                r["file"].as_str().unwrap_or_default().to_string(),
                r["line"].as_u64().unwrap_or(0),
                r["start_byte"].as_u64().unwrap_or(0),
                r["kind"].as_str().unwrap_or_default().to_string(),
                r["tier"].as_str().unwrap_or_default().to_string(),
                r["owner"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    rows.sort();
    rows
}

/// (id, tier, distance, via) of `deps` results and (from, to, kind, tier, start) of its edges.
type DepsAnswer = (Vec<(String, String, u64, String)>, Vec<(String, String, String, String, u64)>);

fn deps_answer(report: &Value) -> DepsAnswer {
    let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let mut results: Vec<_> = report["results"]
        .as_array()
        .expect("deps results")
        .iter()
        .map(|r| (text(&r["id"]), text(&r["tier"]), r["distance"].as_u64().unwrap_or(0), text(&r["via"])))
        .collect();
    results.sort();
    let mut edges: Vec<_> = report["edges"]
        .as_array()
        .expect("deps edges")
        .iter()
        .map(|e| {
            (
                text(&e["from"]),
                text(&e["to"]),
                text(&e["kind"]),
                text(&e["tier"]),
                e["at"]["start_byte"].as_u64().unwrap_or(0),
            )
        })
        .collect();
    edges.sort();
    (results, edges)
}

/// `--json` run with syntax answers switched off (setting `debug.syntax_answers: false` in
/// the fixture's cache home).
fn json_without_syntax_answers(fx: &Fixture, args: &[&str]) -> Value {
    std::fs::write(fx.cache.join("config.json"), r#"{"debug": {"syntax_answers": false}}"#)
        .expect("write config.json");
    let out = fx.command().arg("--json").args(args).output().expect("run trace");
    assert!(out.status.success(), "trace {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).expect("JSON report")
}

/// Upper bound of bash-language-server requests on the fixture with syntax answers:
/// `initialize` plus its 4 cross-file calls (`lib_log` in util.sh; `lib_trim`,
/// `util_banner`, `lib_log` in main.sh), with slack for retried requests. The 6 same-file
/// calls are answered from syntax and the commands no file declares (`echo`, `printf`,
/// `tr`, `source`, `.`) are not asked (without syntax answers: 19 definitions).
const BASH_REQUEST_BOUND: u64 = 8;

/// Rule 15: three shell scripts with same-file and sourced calls. The same fixture indexed
/// with and without syntax answers gives identical `uses` and `deps` answers; with them the
/// server gets at most [`BASH_REQUEST_BOUND`] requests (19 without).
#[test]
fn rule_bash_same_file_definitions_answer_identically_with_fewer_requests() {
    let fast = Fixture::new("rule_server_bash_fast", "rule-server-bash-syntax-definitions");
    let slow = Fixture::new("rule_server_bash_slow", "rule-server-bash-syntax-definitions");
    let Some(index_fast) = fast.index_or_skip() else { return };
    assert_ready_field(&index_fast);
    if !semantic_or_skip(&index_fast, "bash") {
        return;
    }
    let index_slow = json_without_syntax_answers(&slow, &["index"]);
    let (with, without) =
        (backend_sum(&index_fast, "bash", "requests"), backend_sum(&index_slow, "bash", "requests"));
    assert!(with <= BASH_REQUEST_BOUND, "{with} requests with syntax answers: {index_fast}");
    assert!(with < without, "syntax answers must save requests ({with} vs {without})");
    for symbol in [
        "lib.sh:lib_trim",
        "lib.sh:lib_upper",
        "lib.sh:lib_log",
        "util.sh:util_line",
        "main.sh:main_step",
        "main.sh:main_run",
    ] {
        let a = fast.json(&["uses", symbol]);
        let b = json_without_syntax_answers(&slow, &["uses", symbol]);
        assert_no_contradictions(&a);
        assert_eq!(use_rows(&a), use_rows(&b), "uses {symbol}");
    }
    for symbol in ["main.sh:main_run", "util.sh:util_banner", "lib.sh:lib_log"] {
        let a = fast.json(&["deps", symbol]);
        let b = json_without_syntax_answers(&slow, &["deps", symbol]);
        assert_eq!(deps_answer(&a), deps_answer(&b), "deps {symbol}");
    }
    // The answers themselves: a same-file call (syntax) and a sourced call (server).
    let trim = fast.json(&["uses", "lib.sh:lib_trim"]);
    for (file, line) in [("lib.sh", 10), ("main.sh", 7)] {
        let rows = rows_at(&trim, file, line);
        assert!(rows.iter().any(|r| r["kind"] == "call" && r["tier"] == "proven"), "{file}:{line}: {trim}");
    }
    let run = fast.json(&["deps", "main.sh:main_run"]);
    let ids: Vec<&str> = run["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["id"].as_str())
        .collect();
    for id in ["main.sh:main_step", "util.sh:util_banner", "lib.sh:lib_log"] {
        assert!(ids.contains(&id), "{id} in {ids:?}");
    }
}
