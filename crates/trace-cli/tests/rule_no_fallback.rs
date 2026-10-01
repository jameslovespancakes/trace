//! E2E: no fallback (PLAN decision 3). There is no syntax-only mode: a missing language
//! server, an environment trace cannot use or a build that was not approved stops `trace`
//! with one clear error in the approved style (`Error: ` + text, JSON `error_type`), exit 3,
//! and the index is not written.
//!
//! Tests that need a registry entry use a `semantic.registry` override directory (a test
//! entry for Scala, a non-default language, so nothing is ever installed automatically) and
//! an empty tools folder (`TRACE_SEMANTIC_TOOLS`). Every run is `TRACE_OFFLINE=1` with
//! `TRACE_NO_AUTO_INSTALL=1`: no network.

mod support;

use std::fs;
use std::path::PathBuf;

use serde_json::Value;
use support::Fixture;

/// A registry override with one Scala entry (installed from an archive; optionally a build
/// that needs approval) and the config that selects it; returns the empty tools folder.
fn test_registry(fx: &Fixture, requires_build: bool) -> PathBuf {
    let dir = fx.base.join("registry");
    fs::create_dir_all(&dir).expect("registry dir");
    let build = if requires_build {
        r#","requires_build": {"tool": "Gradle", "runs": "this project's build scripts", "when": "always"}"#
    } else {
        ""
    };
    let entry = format!(
        r#"{{"schema": 2, "backends": [{{
            "id": "lsp:test-scala",
            "kind": "lsp",
            "languages": ["scala"],
            "language_ids": ["scala"],
            "server": {{"name": "test-scala", "version": "0.0.1", "license": "MIT"}},
            "executable": {{"from": "tool", "tool": "test-scala", "path": "bin/test-scala"}},
            "install": {{
                "id": "test-scala", "version": "0.0.1", "license": "MIT",
                "display": "Scala language server", "product": "test-scala",
                "recipe": "archive",
                "artifacts": [{{"platform": "any", "url": "https://example.invalid/test-scala.zip",
                                "sha256": "{sha}", "strip": 0}}],
                "executables": ["bin/test-scala"]
            }}{build}
        }}]}}"#,
        sha = "ab".repeat(32),
    );
    fs::write(dir.join("scala.json"), entry).expect("registry file");
    let config = serde_json::json!({"semantic": {"registry": dir}});
    fs::write(fx.cache.join("config.json"), config.to_string()).expect("config");
    let tools = fx.base.join("tools");
    fs::create_dir_all(&tools).expect("tools dir");
    tools
}

/// `trace --json <args>` with an empty tools folder: (exit code, stderr JSON error).
fn run_error(fx: &Fixture, tools: &PathBuf, args: &[&str]) -> (Option<i32>, Value) {
    let out = fx
        .command()
        .env("TRACE_SEMANTIC_TOOLS", tools)
        .arg("--json")
        .args(args)
        .output()
        .expect("run trace");
    let err: Value = serde_json::from_slice(&out.stderr).unwrap_or(Value::Null);
    (out.status.code(), err)
}

/// A missing language server of a non-default language: `server_missing`, the exact text,
/// exit 3, nothing written.
#[test]
fn rule_missing_server_is_an_error_not_a_fallback() {
    let fx = Fixture::new("no-fallback-server", "rule-no-fallback-scala");
    let tools = test_registry(&fx, false);
    let (code, err) = run_error(&fx, &tools, &["index"]);
    assert_eq!(code, Some(3), "{err}");
    assert_eq!(err["error_type"], "server_missing", "{err}");
    assert_eq!(
        err["error"],
        "The Scala language server is not installed. Install it: trace status --install scala"
    );
    // The index is not written; a query stops with the same error (never a syntax answer).
    let written = fs::read_dir(fx.cache.join("repos"))
        .map(|dirs| {
            dirs.filter_map(Result::ok)
                .any(|d| d.path().join("index.bin").exists())
        })
        .unwrap_or(false);
    assert!(!written, "no index after a setup error");
    let (code, err) = run_error(&fx, &tools, &["deps", "anything"]);
    assert_eq!(code, Some(3), "{err}");
    assert_eq!(err["error_type"], "server_missing", "{err}");
    // Human output: `Error: ` + the text.
    let out = fx
        .command()
        .env("TRACE_SEMANTIC_TOOLS", &tools)
        .arg("index")
        .output()
        .expect("run trace");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "Error: The Scala language server is not installed. Install it: trace status --install scala"
        ),
        "{stderr}"
    );
}

/// `--env <path>` that no ecosystem accepts: `env_not_found` naming the language that
/// declares dependencies, exit 3; nothing is remembered.
#[test]
fn rule_env_path_must_be_an_environment() {
    let fx = Fixture::new("no-fallback-env", "python");
    let empty = fx.base.join("not-a-venv");
    fs::create_dir_all(&empty).expect("empty dir");
    let out = fx
        .command()
        .args(["--json", "index", "--env"])
        .arg(&empty)
        .output()
        .expect("run trace");
    assert_eq!(out.status.code(), Some(3));
    let err: Value = serde_json::from_slice(&out.stderr).expect("JSON error");
    assert_eq!(err["error_type"], "env_not_found", "{err}");
    let text = err["error"].as_str().unwrap_or_default();
    assert!(text.starts_with("No Python environment found at "), "{err}");
    assert!(text.ends_with("not-a-venv"), "{err}");
}

/// A build that needs approval: `build_not_allowed` with the approved two lines; after
/// `trace index --allow-build` the approval is remembered for this repository (the test
/// server is still not installed: that error stays, the approval error never comes back).
#[test]
fn rule_build_needs_approval_once_per_repository() {
    let fx = Fixture::new("no-fallback-build", "rule-no-fallback-scala");
    let tools = test_registry(&fx, true);
    let (code, err) = run_error(&fx, &tools, &["index"]);
    assert_eq!(code, Some(3), "{err}");
    // Two independent failures of one language are reported together (PLAN decision 15).
    assert_eq!(err["error_type"], "setup_incomplete", "{err}");
    let kinds: Vec<&str> = err["errors"]
        .as_array()
        .expect("errors")
        .iter()
        .filter_map(|e| e["error_type"].as_str())
        .collect();
    assert_eq!(kinds, ["server_missing", "build_not_allowed"], "{err}");
    let build = &err["errors"][1];
    assert_eq!(
        build["error"],
        "Scala needs Gradle, which runs this project's build scripts. Only allow this for projects you trust: trace index --allow-build"
    );
    let out = fx
        .command()
        .env("TRACE_SEMANTIC_TOOLS", &tools)
        .arg("index")
        .output()
        .expect("run trace");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("Error: This repository needs 2 things before trace can analyze it:\n  1. "),
        "{stderr}"
    );

    // Approve once: remembered; the approval error does not come back.
    let (code, err) = run_error(&fx, &tools, &["index", "--allow-build"]);
    assert_eq!(code, Some(3), "{err}");
    assert_eq!(err["error_type"], "server_missing", "{err}");
    let (_, err) = run_error(&fx, &tools, &["index"]);
    assert_eq!(err["error_type"], "server_missing", "approval remembered: {err}");
    let status = fx.json(&["status"]);
    assert_eq!(status["build_approval"], "allowed", "{status}");
}
