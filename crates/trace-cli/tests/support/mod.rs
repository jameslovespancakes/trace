//! Shared harness of the rule e2e tests (`tests/rule_*.rs`): the same fixture copy, cache and
//! offline setup as `tests/cli.rs`. Each rule test file declares `mod support;`.
//!
//! Fixture sources live in `tests/fixtures/rule-<name>/` at the repository root (named after
//! the rule, never after a benchmark repository). Every test copies its fixture to
//! `<temp>/trace-tests/run-<test>-<pid>/repo` with a private cache (`TRACE_CACHE_DIR`), removed
//! afterwards unless `TRACE_TEST_KEEP` is set. Automatic installs are off (`TRACE_OFFLINE=1`, `TRACE_NO_AUTO_INSTALL=1`): no test makes a
//! network call.
//! There is no syntax-only mode: when a fixture's language server, toolchain or
//! dependencies are missing on this machine, `trace index` stops with a setup error and the
//! test is skipped with a notice ([`Fixture::index_or_skip`]); any other failure fails it.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

/// Plain spelling (no `\\?\` verbatim prefix), as a user would pass it.
pub fn plain(path: PathBuf) -> PathBuf {
    let path = path.canonicalize().unwrap_or(path);
    let text = path.to_string_lossy();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
}

/// Fixture sources (`tests/fixtures` at the repository root).
pub fn fixtures() -> PathBuf {
    plain(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures"))
}

/// Scratch for fixture copies and caches: outside the repository.
pub fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join("trace-tests");
    fs::create_dir_all(&dir).expect("scratch dir");
    plain(dir)
}

pub fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create fixture copy");
    for entry in fs::read_dir(from).expect("read fixture") {
        let entry = entry.expect("fixture entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy fixture file");
        }
    }
}

/// The binary with automatic installs off (no fixture, no root). The repository's own tools
/// folder (`tools/`, git-ignored) serves the language servers when it exists and
/// `TRACE_SEMANTIC_TOOLS` is not set (else the per-user default folder).
pub fn trace() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_trace"));
    cmd.env("TRACE_NO_AUTO_INSTALL", "1");
    let tools = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools");
    if trace_core::env::semantic_tools().is_none() && tools.is_dir() {
        cmd.env("TRACE_SEMANTIC_TOOLS", tools);
    }
    cmd
}

/// Setup error types that mean "a tool is not installed on this machine" (skip, not fail).
pub const MISSING_TOOL: &[&str] = &[
    "server_missing",
    "server_unavailable",
    "toolchain_missing",
    "toolchain_version",
    "deps_missing",
    "build_not_allowed",
    "install_failed",
];

/// Whether a JSON error object is a setup error of missing tools only (a combined error:
/// every item).
pub fn missing_tools(err: &Value) -> bool {
    match err["error_type"].as_str() {
        Some("setup_incomplete") => err["errors"].as_array().is_some_and(|items| {
            items
                .iter()
                .all(|i| i["error_type"].as_str().is_some_and(|k| MISSING_TOOL.contains(&k)))
        }),
        Some(kind) => MISSING_TOOL.contains(&kind),
        None => false,
    }
}

/// One fixture copy with its own cache.
pub struct Fixture {
    pub base: PathBuf,
    pub root: PathBuf,
    pub cache: PathBuf,
}

impl Fixture {
    pub fn new(test: &str, fixture: &str) -> Fixture {
        let source = fixtures().join(fixture);
        assert!(source.is_dir(), "missing fixture {}", source.display());
        let base = scratch().join(format!("run-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("repo");
        copy_dir(&source, &root);
        let cache = base.join("cache");
        fs::create_dir_all(&cache).expect("cache dir");
        Fixture { base, root, cache }
    }

    pub fn command(&self) -> Command {
        let mut cmd = trace();
        cmd.env("TRACE_CACHE_DIR", &self.cache)
            .env("TRACE_OFFLINE", "1")
            .arg("--root")
            .arg(&self.root);
        cmd
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().expect("run trace")
    }

    /// Run with `--json`, require exit 0 and parse stdout.
    pub fn json(&self, args: &[&str]) -> Value {
        let out = self.run(&[&["--json"], args].concat());
        assert!(out.status.success(), "trace {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).expect("JSON report")
    }

    /// Run with `--json`, require exit code `code`, parse the stderr error object.
    pub fn json_error(&self, args: &[&str], code: i32) -> Value {
        let out = self.run(&[&["--json"], args].concat());
        assert_eq!(
            out.status.code(),
            Some(code),
            "trace {args:?}: stdout {} stderr {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stderr).expect("JSON error object on stderr")
    }

    pub fn text(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(out.status.success(), "trace {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).expect("UTF-8 text")
    }

    pub fn write(&self, rel: &str, content: &str) {
        fs::write(self.root.join(rel), content).expect("write fixture file");
    }

    /// `trace --json <args>`: the report, or `None` (skipped with a notice) when the command
    /// stops with a setup error of missing tools (exit 3); any other failure fails the test.
    pub fn json_or_skip(&self, args: &[&str]) -> Option<Value> {
        let out = self.run(&[&["--json"], args].concat());
        if out.status.success() {
            return Some(serde_json::from_slice(&out.stdout).expect("JSON report"));
        }
        let err: Value = serde_json::from_slice(&out.stderr)
            .unwrap_or_else(|_| panic!("trace {args:?} failed: {}", String::from_utf8_lossy(&out.stderr)));
        assert_eq!(out.status.code(), Some(3), "trace {args:?}: {err}");
        assert!(missing_tools(&err), "trace {args:?} failed for another reason: {err}");
        eprintln!("SKIPPED (missing tools on this machine): {}", err["error"]);
        None
    }

    /// `trace --json index` or `None` when tools are missing ([`Fixture::json_or_skip`]).
    pub fn index_or_skip(&self) -> Option<Value> {
        self.json_or_skip(&["index"])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if trace_core::env::test::keep() {
            return;
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// True when `language` was analyzed semantically. An analyzer that is available but failed
/// fails the test; only a missing analyzer skips the semantic assertions.
pub fn semantic_or_skip(report: &Value, language: &str) -> bool {
    let row = report["languages"]
        .as_array()
        .expect("languages")
        .iter()
        .find(|l| l["language"] == language)
        .cloned()
        .unwrap_or(Value::Null);
    if row["support"] == "semantic" {
        return true;
    }
    assert_ne!(row["backend_available"], true, "{language} analyzer available but not used: {row}");
    eprintln!("SKIPPED semantic assertions for {language}: {}", row["reason"]);
    false
}

/// `uses` rows at `file:line` (any kind).
pub fn rows_at<'a>(report: &'a Value, file: &str, line: u64) -> Vec<&'a Value> {
    report["uses"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|r| r["file"] == file && r["line"].as_u64() == Some(line))
                .collect()
        })
        .unwrap_or_default()
}

/// A `uses` row at `file:line` of `kind`.
pub fn use_row<'a>(report: &'a Value, file: &str, line: u64, kind: &str) -> Option<&'a Value> {
    report["uses"]
        .as_array()?
        .iter()
        .find(|r| r["file"] == file && r["line"].as_u64() == Some(line) && r["kind"] == kind)
}

/// `completeness.unresolved` entries at `file:line`.
pub fn check_at<'a>(report: &'a Value, file: &str, line: u64) -> Vec<&'a Value> {
    report["completeness"]["unresolved"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|u| u["at"]["file"] == file && u["at"]["line"].as_u64() == Some(line))
                .collect()
        })
        .unwrap_or_default()
}

/// Rule 1 invariant: no `uses` row is `possible`, and no site is both a row and an
/// unresolved entry (same file and start byte).
pub fn assert_no_contradictions(report: &Value) {
    let rows = report["uses"].as_array().expect("uses rows");
    for r in rows {
        assert_ne!(r["tier"], "possible", "possible row in uses: {r}");
    }
    let unresolved = report["completeness"]["unresolved"].as_array().expect("unresolved");
    for u in unresolved {
        let same = rows
            .iter()
            .any(|r| r["file"] == u["at"]["file"] && r["start_byte"] == u["at"]["start_byte"]);
        assert!(!same, "site both in uses and unresolved: {u}");
    }
}

/// The head lines of `uses` rows (`<file>:<line>[ <mark>]  <caller>...`, not indented),
/// before `check:`.
pub fn use_heads(text: &str) -> Vec<&str> {
    text.lines()
        .skip(1)
        .take_while(|l| *l != "check:")
        .filter(|l| !l.starts_with(' '))
        .filter(|l| {
            l.split_whitespace()
                .next()
                .and_then(|at| at.rsplit_once(':'))
                .is_some_and(|(_, line)| !line.is_empty() && line.chars().all(|c| c.is_ascii_digit()))
        })
        .collect()
}

/// The mark after a line label or a `file:line` head: `~`, `?` or "" (proven).
pub fn row_mark(line: &str) -> &str {
    line.split_whitespace()
        .nth(1)
        .filter(|m| matches!(*m, "~" | "?"))
        .unwrap_or("")
}
