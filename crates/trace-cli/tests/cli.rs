//! End-to-end tests of the `trace` binary on small fixture repositories.
//!
//! Fixture sources live in `tests/fixtures/<name>` at the repository root (Python with
//! `.pyi` stubs, protocols/ABCs, callbacks, decorators/descriptors, subscript dunders,
//! generators and async code; TypeScript; Rust; Go). Every test
//! copies its fixture to `<temp>/trace-tests/run-<test>-<pid>/repo` (so incrementality can
//! mutate it) with a private cache (`TRACE_CACHE_DIR`) beside it, removed afterwards.
//! Automatic installs are off (`TRACE_OFFLINE=1`): no test makes a network call or downloads
//! anything.
//!
//! There is no syntax-only mode (PLAN decision 3): a fixture whose language server,
//! toolchain or dependencies are missing on this machine makes `trace index` stop with a
//! setup error; such a test is skipped with a notice ([`Fixture::index_or_skip`]), any other
//! failure fails it. Automatic installs are off (`--offline`, `TRACE_NO_AUTO_INSTALL=1`).

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

/// Plain spelling (no `\\?\` verbatim prefix), as a user would pass it.
fn plain(path: PathBuf) -> PathBuf {
    let path = path.canonicalize().unwrap_or(path);
    let text = path.to_string_lossy();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
}

/// Fixture sources (`tests/fixtures` at the repository root).
fn fixtures() -> PathBuf {
    plain(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures"))
}

/// Scratch for fixture copies and caches: outside the repository.
fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join("trace-tests");
    fs::create_dir_all(&dir).expect("scratch dir");
    plain(dir)
}

fn copy_dir(from: &Path, to: &Path) {
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
fn trace() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_trace"));
    cmd.env("TRACE_NO_AUTO_INSTALL", "1");
    let tools = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools");
    if trace_core::env::semantic_tools().is_none() && tools.is_dir() {
        cmd.env("TRACE_SEMANTIC_TOOLS", tools);
    }
    cmd
}

/// Setup error types that mean "a tool is not installed on this machine" (skip, not fail).
const MISSING_TOOL: &[&str] = &[
    "server_missing",
    "server_unavailable",
    "toolchain_missing",
    "toolchain_version",
    "deps_missing",
    "build_not_allowed",
    "install_failed",
];

/// Whether a JSON error object is a setup error of missing tools only.
fn missing_tools(err: &Value) -> bool {
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
struct Fixture {
    base: PathBuf,
    root: PathBuf,
    cache: PathBuf,
}

impl Fixture {
    fn new(test: &str, fixture: &str) -> Fixture {
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

    fn command(&self) -> Command {
        let mut cmd = trace();
        cmd.env("TRACE_CACHE_DIR", &self.cache);
        cmd.env("TRACE_OFFLINE", "1").arg("--root").arg(&self.root);
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().expect("run trace")
    }

    /// Run with `--json`, require exit 0 and parse stdout.
    fn json(&self, args: &[&str]) -> Value {
        let out = self.run(&[&["--json"], args].concat());
        assert!(out.status.success(), "trace {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).expect("JSON report")
    }

    fn text(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(out.status.success(), "trace {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).expect("UTF-8 text")
    }

    fn write(&self, rel: &str, content: &str) {
        fs::write(self.root.join(rel), content).expect("write fixture file");
    }

    /// `trace --json index`: the report, or `None` (test skipped with a notice) when the
    /// setup stops because a language server / toolchain / dependency is missing here.
    fn index_or_skip(&self) -> Option<Value> {
        let out = self.run(&["--json", "index"]);
        if out.status.success() {
            return Some(serde_json::from_slice(&out.stdout).expect("JSON report"));
        }
        let err: Value = serde_json::from_slice(&out.stderr)
            .unwrap_or_else(|_| panic!("index failed: {}", String::from_utf8_lossy(&out.stderr)));
        assert_eq!(out.status.code(), Some(3), "{err}");
        assert!(missing_tools(&err), "index failed for another reason: {err}");
        eprintln!("SKIPPED (missing tools on this machine): {}", err["error"]);
        None
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

fn ids(rows: &Value) -> Vec<&str> {
    rows.as_array()
        .expect("array")
        .iter()
        .map(|r| r["id"].as_str().expect("id"))
        .collect()
}

fn row<'a>(rows: &'a Value, id: &str) -> Option<&'a Value> {
    rows.as_array()?.iter().find(|r| r["id"] == id)
}

/// Support level of `language` in an index or status report.
fn support<'a>(report: &'a Value, language: &str) -> &'a str {
    report["languages"]
        .as_array()
        .expect("languages")
        .iter()
        .find(|l| l["language"] == language)
        .and_then(|l| l["support"].as_str())
        .unwrap_or("absent")
}

/// True when `language` was analyzed semantically. An analyzer that is available but failed
/// fails the test; only a missing analyzer skips the semantic assertions.
fn semantic_or_skip(report: &Value, language: &str) -> bool {
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

/// A `uses` row at `file:line` of `kind`.
fn use_row<'a>(report: &'a Value, file: &str, line: u64, kind: &str) -> Option<&'a Value> {
    report["uses"]
        .as_array()?
        .iter()
        .find(|r| r["file"] == file && r["line"].as_u64() == Some(line) && r["kind"] == kind)
}

/// The `deps` / `context` call row labelled `line` (`  <line>[ <mark>]  <call>...`).
fn call_row(text: &str, line: u32) -> Option<&str> {
    let want = line.to_string();
    text.lines()
        .find(|l| l.starts_with(' ') && l.split_whitespace().next() == Some(want.as_str()))
}

/// The mark after a line label or a `file:line` head: `~`, `?` or "" (proven).
fn row_mark(line: &str) -> &str {
    line.split_whitespace()
        .nth(1)
        .filter(|m| matches!(*m, "~" | "?"))
        .unwrap_or("")
}

/// The head line of the `uses` row at `file:line` (`<file>:<line>[ mark]  <caller>...`).
fn use_head<'a>(text: &'a str, file: &str, line: u32) -> Option<&'a str> {
    let at = format!("{file}:{line}");
    text.lines()
        .find(|l| l.split_whitespace().next() == Some(at.as_str()))
}

/// The exact code line printed under a `uses` head.
fn use_code<'a>(text: &'a str, head: &str) -> Option<&'a str> {
    let mut lines = text.lines();
    lines.find(|l| *l == head)?;
    lines.next()
}

/// The hop lines of a one-path `path` answer (not indented, after the header).
fn hop_lines(text: &str) -> Vec<&str> {
    text.lines().skip(1).filter(|l| !l.starts_with(' ')).collect()
}

#[test]
fn python_every_command() {
    let fx = Fixture::new("python", "python");
    let Some(index) = fx.index_or_skip() else { return };
    assert_eq!(index["files"], 8, "sources only; .env and configs are not sources");
    assert_eq!(index["added"], 8);
    assert_eq!(support(&index, "python"), "semantic");
    // Test files are analysed at index time (no deferral).
    assert_eq!(index["pending_files"], 0, "{index}");
    let semantic = semantic_or_skip(&index, "python");

    // deps ---------------------------------------------------------------------------------
    let deps = fx.json(&["deps", "shop/app.py:checkout"]);
    assert_eq!(deps["schema"], 1);
    assert_eq!(deps["command"], "deps");
    assert_eq!(deps["symbol"]["id"], "shop/app.py:checkout");
    assert_eq!(deps["include"], "inferred");
    let text = fx.text(&["deps", "shop/app.py:checkout"]);
    assert!(text.starts_with("checkout  "), "{text}");
    if semantic {
        let results = &deps["results"];
        for id in [
            "shop/pricing.py:subtotal",
            "shop/pricing.py:validate",
            "shop/pricing.py:apply_discount",
            "shop/pricing.py:PercentOff.__init__",
        ] {
            assert_eq!(row(results, id).map(|r| &r["tier"]), Some(&json!("proven")), "{id}");
        }
        // Every edge carries its exact source line.
        for e in deps["edges"].as_array().unwrap() {
            assert!(e["text"].is_string(), "{e}");
        }
        // Generator consumed by `for` -> iterates (proven).
        assert_eq!(row(results, "shop/pricing.py:line_totals").map(|r| &r["via"]), Some(&json!("iterates")));
        // Protocol method: proven call to the stub; the implementation used is inferred
        // (unique flow candidate), the other conformer only possible.
        assert!(row(results, "shop/pricing.py:Discount.apply").is_some());
        assert_eq!(
            row(results, "shop/pricing.py:PercentOff.apply").map(|r| &r["tier"]),
            Some(&json!("inferred"))
        );
        assert!(row(results, "shop/pricing.py:FlatOff.apply").is_none());
        // Text: the proven direct call is unmarked; the inferred implementation is marked.
        let call = call_row(&text, 7).unwrap_or_else(|| panic!("row 7 in\n{text}"));
        assert_eq!(row_mark(call), "", "{call}");
        assert!(call.contains("subtotal(prices)"), "{call}");
        assert!(call.ends_with("\u{2192} shop/pricing.py:58"), "{call}");
        assert!(
            text.lines().any(|l| l
                .trim_start_matches("then")
                .trim_start()
                .starts_with("apply_discount \u{2192} ")
                && l.contains("PercentOff.apply ~")),
            "{text}"
        );
        // Every site says when it runs: line 10 runs only if `validate` passed (an earlier
        // `if not validate(...): return`).
        let calls = deps["calls"].as_array().unwrap();
        for c in calls {
            assert!(c["when"].is_array() && c["targets"].is_array() && c["call"].is_string(), "{c}");
        }
        let discount = calls
            .iter()
            .find(|c| c["at"]["line"] == 10 && c["call"].as_str().unwrap().starts_with("apply_discount("))
            .expect("line 10");
        assert_eq!(discount["when"], json!(["validate(MinimumRule(), amount)"]), "{discount}");
        // The calls of line 10 share the condition: printed once above them.
        let group = text
            .lines()
            .position(|l| l.trim() == "if validate(MinimumRule(), amount):");
        let row10 = text.lines().position(|l| call_row(l, 10).is_some());
        assert!(group.is_some() && row10 > group, "{text}");
        let possible = fx.json(&["deps", "shop/app.py:checkout", "--deep"]);
        assert_eq!(possible["include"], "possible");
        for id in ["shop/pricing.py:FlatOff.apply", "shop/pricing.py:MaximumRule.check"] {
            assert_eq!(row(&possible["results"], id).map(|r| &r["tier"]), Some(&json!("possible")), "{id}");
        }
        // `validate(MinimumRule(), ...)`: the only receiver flowing into `rule.check`.
        assert_eq!(
            row(&possible["results"], "shop/pricing.py:MinimumRule.check").map(|r| &r["tier"]),
            Some(&json!("inferred"))
        );

        // .pyi stub -> implementation (proven language rule), descriptor / subscript dunders
        // and callbacks (inferred), awaits (proven).
        let report = fx.json(&["deps", "shop/store.py:report"]);
        let r = &report["results"];
        assert!(row(r, "shop/fmt.pyi:format_price").is_some());
        assert_eq!(
            row(r, "shop/fmt.py:format_price").map(|x| &x["via"]),
            Some(&json!("stub_implementation"))
        );
        assert_eq!(row(r, "shop/fmt.py:_two_places").map(|x| &x["tier"]), Some(&json!("proven")));
        for (id, via) in [
            ("shop/store.py:Inventory.__setitem__", "inferred_implicit"),
            ("shop/store.py:Inventory.__getitem__", "inferred_implicit"),
            ("shop/store.py:cached.__get__", "inferred_implicit"),
            ("shop/store.py:on_change", "inferred_callback"),
            ("shop/store.py:Inventory.total", "inferred_call"),
        ] {
            assert_eq!(row(r, id).map(|x| &x["via"]), Some(&json!(via)), "{id}");
        }
        let main_async = fx.json(&["deps", "shop/app.py:main_async"]);
        assert_eq!(
            row(&main_async["results"], "shop/pricing.py:convert").map(|x| &x["via"]),
            Some(&json!("awaits"))
        );
        // A generator returned without being consumed is deferred (creates_generator).
        let lazy = fx.json(&["deps", "shop/pricing.py:lazy_totals"]);
        assert_eq!(
            row(&lazy["results"], "shop/pricing.py:line_totals").map(|x| &x["via"]),
            Some(&json!("creates_generator"))
        );
        // Decorated method: the decorator's wrapper is an inferred flow target.
        let main = fx.json(&["deps", "shop/app.py:main"]);
        assert!(row(&main["results"], "shop/store.py:logged.wrapper").is_some());

        // path ---------------------------------------------------------------------------
        let path = fx.json(&["path", "shop/app.py:main", "shop/fmt.py:_two_places"]);
        assert_eq!(path["schema"], 1);
        assert_eq!(path["found"], true);
        assert!(path.get("all").is_none(), "path returns one path");
        assert_eq!(path["paths"].as_array().unwrap().len(), 1);
        let nodes: Vec<&str> = path["paths"][0]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        assert_eq!(nodes.first(), Some(&"shop/app.py:main"));
        assert_eq!(nodes.last(), Some(&"shop/fmt.py:_two_places"));
        let hops = path["paths"][0]["edges"].as_array().unwrap().len();
        for e in path["paths"][0]["edges"].as_array().unwrap() {
            assert!(
                e["site"]["call"].is_string()
                    && e["site"]["when"].is_array()
                    && e["site"]["carries"].is_array(),
                "{e}"
            );
        }
        let text = fx.text(&["path", "shop/app.py:main", "shop/fmt.py:_two_places"]);
        let head = format!("main \u{2192} _two_places  {hops} hop");
        assert!(text.starts_with(&head), "{text}");
        assert_eq!(hop_lines(&text).len(), hops, "{text}");
        let every = fx.json(&["path", "shop/app.py:main", "shop/fmt.py:_two_places", "--deep"]);
        assert_eq!(every["deep"], true);
        assert!(!every["paths"].as_array().unwrap().is_empty());
        let none = fx.json(&["path", "shop/fmt.py:_two_places", "shop/app.py:main"]);
        assert_eq!(none["found"], false);
        assert_eq!(
            fx.text(&["path", "shop/fmt.py:_two_places", "shop/app.py:main"])
                .trim_end(),
            "_two_places \u{2192} main  no path  complete"
        );

        // uses --deep (was impact) ------------------------------------------------------
        // Test code was analysed at index time: callers in tests are callers.
        let deep = fx.json(&["uses", "shop/fmt.py:_two_places", "--deep"]);
        assert_eq!(deep["index"]["updated"], "none");
        let status = fx.json(&["status"]);
        assert_eq!(status["index"]["pending_files"], 0);
        assert_eq!(deep["deep"], true);
        let impact = &deep["impact"];
        assert_eq!(ids(&impact["callers"]), vec!["shop/fmt.py:format_price"]);
        let transitive = ids(&impact["transitive"]);
        assert!(transitive.contains(&"shop/app.py:main"), "{transitive:?}");
        // One direct caller: every caller of callers passes through it.
        for t in impact["transitive"].as_array().unwrap() {
            assert_eq!(t["through"], "shop/fmt.py:format_price", "{t}");
        }
        assert!(impact["tests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["file"] == "tests/test_app.py"));
        assert_eq!(deep["counts"]["callers_of_callers"], impact["transitive_total"]);
        assert_eq!(
            deep["counts"]["tests"].as_u64().unwrap() as usize,
            impact["tests"].as_array().unwrap().len()
        );
        let text = fx.text(&["uses", "shop/fmt.py:_two_places", "--deep"]);
        let head = text.lines().next().unwrap();
        assert!(head.starts_with("_two_places  ") && head.contains(" callers of callers"), "{text}");
        assert!(text.contains("\ncallers of callers (via format_price):\n"), "{text}");
        assert!(text.contains("\nall tests: tests/test_app.py::"), "{text}");

        // uses --json evidence block (was evidence) ------------------------------------
        let report_uses = fx.json(&["uses", "shop/store.py:report"]);
        let ev = &report_uses["evidence"];
        let outgoing = ev["outgoing"].as_array().unwrap();
        assert!(outgoing
            .iter()
            .any(|e| e["tier"] == "proven" && e["source"] == "pyright"));
        assert!(outgoing.iter().any(|e| e["kind"] == "passes_callback"));
        let implicit: Vec<&Value> = outgoing.iter().filter(|e| e["kind"] == "inferred_implicit").collect();
        assert!(!implicit.is_empty());
        assert!(implicit
            .iter()
            .all(|e| e["tier"] == "inferred" && e["decision"]["status"] == "decided"));
        let sites = ev["sites"].as_array().unwrap();
        assert!(sites
            .iter()
            .any(|s| s["category"] == "implicit" && s["operation"] == "subscript_store"));
        for s in sites {
            // Decisions never leave the candidate set.
            let candidates = s["candidates"].as_array().unwrap();
            for t in s["decided_targets"].as_array().unwrap() {
                assert!(candidates.contains(t));
            }
        }
        let stub = fx.json(&["uses", "shop/fmt.pyi:format_price"]);
        assert!(stub["evidence"]["outgoing"].as_array().unwrap().iter().any(|e| {
            e["kind"] == "stub_implementation"
                && e["tier"] == "proven"
                && e["other"]["id"] == "shop/fmt.py:format_price"
        }));
        let dispatch = fx.json(&["uses", "shop/pricing.py:validate"]);
        assert!(dispatch["evidence"]["sites"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["category"] == "dispatch"));
    }

    // context ----------------------------------------------------------------------------
    let ctx = fx.json(&["context", "shop/pricing.py:apply_discount"]);
    assert_eq!(ctx["schema"], 1);
    assert_eq!(ctx["command"], "context");
    assert_eq!(ctx["symbol"]["id"], "shop/pricing.py:apply_discount");
    assert!(ctx["source"].as_str().unwrap().starts_with("def apply_discount("), "{ctx}");
    for list in ["callers", "calls", "tests", "next"] {
        assert!(ctx[list].is_array(), "context.{list}");
    }
    let ctx_text = fx.text(&["context", "apply_discount"]);
    let head = ctx_text.lines().next().unwrap();
    assert!(head.starts_with("context apply_discount  shop/pricing.py:45-"), "{head}");
    assert!(
        ctx_text
            .lines()
            .nth(1)
            .unwrap()
            .starts_with("45 | def apply_discount("),
        "{ctx_text}"
    );
    if semantic {
        let checkout = ctx["callers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["caller"] == "shop/app.py:checkout")
            .unwrap_or_else(|| panic!("checkout calls apply_discount: {ctx}"));
        assert_eq!(checkout["when"], json!(["validate(MinimumRule(), amount)"]), "{checkout}");
        assert!(ctx_text.contains("\ncalled by\n  shop/app.py:10  checkout\n"), "{ctx_text}");
        assert!(ctx_text.contains("when  validate(MinimumRule(), amount)"), "{ctx_text}");
    }

    // show ------------------------------------------------------------------------------------
    let shown = fx.json(&["show", "shop/app.py:checkout", "subtotal"]);
    assert_eq!(shown["schema"], 1);
    assert_eq!(shown["command"], "show");
    let items = shown["symbols"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{shown}");
    assert_eq!(items[0]["symbol"]["id"], "shop/app.py:checkout");
    assert!(
        items[0]["source"]
            .as_str()
            .unwrap()
            .starts_with("def checkout(prices: list[float]) -> float:\n"),
        "{shown}"
    );
    assert!(
        items[0]["source"]
            .as_str()
            .unwrap()
            .trim_end()
            .ends_with("return apply_discount(PercentOff(10), amount)"),
        "{shown}"
    );
    assert_eq!(items[1]["symbol"]["id"], "shop/pricing.py:subtotal");
    let text = fx.text(&["show", "shop/app.py:checkout"]);
    let mut lines = text.lines();
    assert!(
        lines
            .next()
            .unwrap()
            .starts_with("shop/app.py:checkout · lines 6–10 · function · "),
        "{text}"
    );
    assert_eq!(lines.next(), Some(" 6 | def checkout(prices: list[float]) -> float:"), "{text}");

    // status ------------------------------------------------------------------------------
    let status = fx.json(&["status"]);
    assert_eq!(status["index"]["exists"], true);
    assert_eq!(status["index"]["fresh"], true);
    assert!(status.get("jev").is_none(), "no JEV block");
    assert!(status["setup"].is_array(), "setup rows are always present");
    assert_eq!(status["build_approval"], "not given");
    // No `trace index --watch` runs, and no command left a process behind (PLAN decision 13).
    assert_eq!(status["watching"], false);
    assert!(status["install"].is_null());
    assert!(status["semantic"]["pool_size"].as_u64().unwrap() >= 1);
    assert!(status["caches"]["semantic_files"].is_object());
    assert!(status["caches"].get("context_rank").is_none());
    assert!(Path::new(status["cache"].as_str().unwrap()).starts_with(&fx.cache));
    let text = fx.text(&["status"]);
    assert!(text.starts_with("index           fresh \u{b7} 8 files \u{b7} "), "{text}");
    assert!(text.lines().any(|l| l.starts_with("python  ")), "{text}");

    // Nothing was written into the inspected repository.
    let mut names: Vec<String> = Vec::new();
    for e in walk(&fx.root) {
        names.push(e.strip_prefix(&fx.root).unwrap().to_string_lossy().replace('\\', "/"));
    }
    names.sort();
    assert_eq!(
        names,
        [
            ".env",
            "pyproject.toml",
            "shop/__init__.py",
            "shop/app.py",
            "shop/fmt.py",
            "shop/fmt.pyi",
            "shop/hooks.py",
            "shop/pricing.py",
            "shop/store.py",
            "tests/test_app.py"
        ]
    );
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_dir() {
            out.extend(walk(&e.path()));
        } else {
            out.push(e.path());
        }
    }
    out
}

#[test]
fn index_is_incremental() {
    // In-process: each command applies the update itself and reports it.
    let fx = Fixture::new("incremental", "python");
    let Some(first) = fx.index_or_skip() else { return };
    assert_eq!((first["added"].as_u64(), first["reparsed"].as_u64()), (Some(8), Some(8)));
    assert!(first.get("removed_cache").is_none() && first.get("existed").is_none());

    let same = fx.json(&["index"]);
    assert_eq!(same["mode"], "unchanged", "{same}");
    assert_eq!(same["reparsed"], 0);
    assert_eq!(same["semantic_requeried"], 0);
    assert!(fx.text(&["index"]).starts_with("up to date: 8 files"));

    // Change one file: only it is reparsed; semantic re-query is limited to affected files.
    let fmt = fs::read_to_string(fx.root.join("shop/fmt.py")).unwrap();
    fx.write("shop/fmt.py", &format!("{fmt}\n\ndef _three_places(value):\n    return _two_places(value)\n"));
    let changed = fx.json(&["index"]);
    assert_eq!(changed["mode"], "incremental");
    assert_eq!(changed["changed"], 1);
    assert_eq!(changed["added"], 0);
    assert_eq!(changed["reparsed"], 1);
    assert!(changed["semantic_requeried"].as_u64().unwrap() < 8);
    assert_eq!(changed["symbols"].as_u64(), first["symbols"].as_u64().map(|n| n + 1));

    // Query commands update the index themselves and report it.
    fx.write(
        "shop/extra.py",
        "from shop.fmt import format_price\n\n\ndef extra():\n    return format_price(1.0)\n",
    );
    let deps = fx.json(&["deps", "shop/extra.py:extra"]);
    assert_eq!(deps["index"]["updated"], "incremental");
    if semantic_or_skip(&first, "python") {
        assert!(row(&deps["results"], "shop/fmt.pyi:format_price").is_some());
    }
    let again = fx.json(&["deps", "shop/extra.py:extra"]);
    assert_eq!(again["index"]["updated"], "none");

    fs::remove_file(fx.root.join("shop/extra.py")).unwrap();
    let removed = fx.json(&["index"]);
    assert_eq!(removed["removed"], 1);
    assert_eq!(removed["reparsed"], 0);
    let missing = fx.run(&["--json", "deps", "shop/extra.py:extra"]);
    assert_eq!(missing.status.code(), Some(2));

    // status reports staleness read-only.
    fx.write("shop/fmt.py", &fmt);
    let status = fx.json(&["status"]);
    assert_eq!(status["index"]["fresh"], false);
    assert_eq!(status["index"]["stale_files"], json!(["shop/fmt.py"]));
    let status_again = fx.json(&["status"]);
    assert_eq!(status_again["index"]["fresh"], false, "status never updates the index");
    let text = fx.text(&["status"]);
    assert!(text.starts_with("index           stale (1 file changed) \u{2014} run: trace index"), "{text}");

    // A corrupt cache is rebuilt, never trusted.
    let key_dir = fs::read_dir(fx.cache.join("repos"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let bin = key_dir.join("index.bin");
    let mut bytes = fs::read(&bin).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(&bin, bytes).unwrap();
    let healed = fx.json(&["deps", "shop/app.py:checkout"]);
    assert_eq!(healed["index"]["updated"], "full");

    // Starting over = deleting the cache directory `status` shows; the next query rebuilds.
    let cache = PathBuf::from(fx.json(&["status"])["cache"].as_str().unwrap());
    assert!(cache.starts_with(&fx.cache) && cache != fx.cache, "{}", cache.display());
    assert!(fx
        .text(&["status"])
        .lines()
        .any(|l| l.starts_with("cache ") && l.ends_with(&*cache.to_string_lossy())));
    fs::remove_dir_all(&cache).unwrap();
    let rebuilt = fx.json(&["deps", "shop/app.py:checkout"]);
    assert_eq!(rebuilt["index"]["updated"], "full");
    assert!(cache.exists());
}

#[test]
fn typescript_fixture() {
    let fx = Fixture::new("typescript", "typescript");
    let Some(index) = fx.index_or_skip() else { return };
    assert_eq!(index["files"], 4);
    let deps = fx.json(&["deps", "src/index.ts:main"]);
    if semantic_or_skip(&index, "typescript") {
        let r = &deps["results"];
        assert_eq!(
            row(r, "src/calculator.ts:Calculator.constructor").map(|x| &x["via"]),
            Some(&json!("constructor"))
        );
        for id in [
            "src/calculator.ts:Calculator.scaled",
            "src/calculator.ts:Doubler.run",
            "src/math.ts:scale",
            "src/math.ts:double",
            "src/math.ts:add",
        ] {
            assert_eq!(row(r, id).map(|x| &x["tier"]), Some(&json!("proven")), "{id}");
        }
        let deep = fx.json(&["uses", "src/math.ts:add", "--deep"]);
        assert!(ids(&deep["impact"]["callers"]).contains(&"src/math.ts:double"));
    }
}

/// `path file:line` where the line runs inside an arrow function stored by its enclosing
/// function (never called there): the selector names the enclosing function, and the path
/// is searched from the arrow's own scope when the function itself reaches nothing.
#[test]
fn path_from_a_line_inside_an_anonymous_function() {
    let fx = Fixture::new("path-line-lambda", "typescript");
    fx.write(
        "src/hooks.ts",
        "import { add } from \"./math\";\n\nexport function register(hooks: Array<(x: number) => number>) {\n  hooks.push((x: number) =>\n    add(x, 1));\n}\n",
    );
    let Some(index) = fx.index_or_skip() else { return };
    if !semantic_or_skip(&index, "typescript") {
        return;
    }
    let p = fx.json(&["path", "src/hooks.ts:5", "src/math.ts:add"]);
    assert_eq!(p["found"], json!(true), "{p}");
    let from = p["from"]["id"].as_str().unwrap();
    assert!(from.starts_with("src/hooks.ts:register.<lambda>"), "{from}");
    // The named function itself still resolves the selector for every other command.
    let deps = fx.json(&["deps", "src/hooks.ts:5"]);
    assert_eq!(deps["symbol"]["id"], json!("src/hooks.ts:register"));
}

#[test]
fn rust_fixture() {
    let fx = Fixture::new("rust", "rust");
    let Some(index) = fx.index_or_skip() else { return };
    assert_eq!(index["files"], 1);
    let deps = fx.json(&["deps", "src/lib.rs:demo", "--deep"]);
    if semantic_or_skip(&index, "rust") {
        let r = &deps["results"];
        assert_eq!(row(r, "src/lib.rs:total_area").map(|x| &x["tier"]), Some(&json!("proven")));
        assert_eq!(row(r, "src/lib.rs:Rect.new").map(|x| &x["tier"]), Some(&json!("proven")));
        assert_eq!(row(r, "src/lib.rs:Shape.area").map(|x| &x["tier"]), Some(&json!("proven")));
        // Trait dispatch: implementations are candidates, never proven.
        for id in ["src/lib.rs:Circle.area", "src/lib.rs:Rect.area"] {
            assert_eq!(row(r, id).map(|x| &x["tier"]), Some(&json!("possible")), "{id}");
        }
        let ev = fx.json(&["uses", "src/lib.rs:total_area"]);
        assert!(ev["evidence"]["sites"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["category"] == "dispatch"));
    }
}

#[test]
fn go_with_gopls() {
    let fx = Fixture::new("gopls", "go");
    let Some(index) = fx.index_or_skip() else { return };
    if !semantic_or_skip(&index, "go") {
        return;
    }
    // gopls resolves both calls: proven edges, nothing left to candidates.
    let deps = fx.json(&["deps", "main.go:Server.Run"]);
    assert_eq!(row(&deps["results"], "main.go:greet").map(|x| &x["tier"]), Some(&json!("proven")));
    let deep = fx.json(&["uses", "main.go:Server.Run", "--deep"]);
    assert!(ids(&deep["impact"]["callers"]).contains(&"main.go:main"), "{deep}");
}

/// Lines of a child's stdout on a channel; the channel disconnects at EOF (every process
/// holding the pipe, trace and anything it started, has exited).
fn stdout_lines(child: &mut std::process::Child) -> std::sync::mpsc::Receiver<String> {
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while let Some(Ok(line)) = lines.next() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// The pipe reader sees EOF once the process tree is gone: no language server (or any
/// other process trace started) outlives it holding the inherited pipe.
fn assert_pipe_closes(rx: &std::sync::mpsc::Receiver<String>) {
    loop {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(_) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("a process started by trace outlived it and keeps the pipe open")
            }
        }
    }
}

/// PLAN decision 13 (option B): `index --watch` runs in the foreground, applies a change
/// within seconds, other commands read what it publishes, `status` says it is watching,
/// and when it ends no language server is left behind.
#[test]
fn rule_index_watch_runs_in_the_foreground_and_leaves_no_process() {
    let fx = Fixture::new("watch", "python");
    let Some(_) = fx.index_or_skip() else { return };
    let mut child = fx
        .command()
        .args(["--json", "index", "--watch"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn watch");
    let rx = stdout_lines(&mut child);
    // The first report is the catch-up; then change a file and wait for another.
    let first = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("initial watch report");
    assert_eq!(serde_json::from_str::<Value>(&first).unwrap()["command"], "index");
    assert_eq!(fx.json(&["status"])["watching"], true);
    std::thread::sleep(Duration::from_millis(700));
    fx.write(
        "shop/extra.py",
        "from shop.app import checkout


def extra():
    return checkout([1.0])
",
    );
    // A command started right after the edit answers from the updated graph (it waits for
    // the watcher, or catches up itself), never from the old one.
    let uses = fx.json(&["uses", "shop/app.py:checkout"]);
    assert!(uses.to_string().contains("shop/extra.py:extra"), "{uses}");
    let update = rx.recv_timeout(Duration::from_secs(60));
    let _ = child.kill();
    let _ = child.wait();
    let update: Value = serde_json::from_str(&update.expect("watch update after change")).unwrap();
    assert_eq!(update["command"], "index");
    assert_pipe_closes(&rx);
    assert_eq!(fx.json(&["status"])["watching"], false);
}

/// PLAN decision 13: trace never leaves a process behind; `trace ... | head` returns
/// because the pipe reader sees EOF as soon as trace exits.
#[test]
fn rule_pipe_reader_sees_eof_when_trace_exits() {
    let fx = Fixture::new("pipe", "python");
    let Some(_) = fx.index_or_skip() else { return };
    // A change first, so the query starts the language server for its catch-up.
    fx.write(
        "shop/extra.py",
        "from shop.app import checkout


def extra():
    return checkout
",
    );
    let mut child = fx
        .command()
        .args(["uses", "shop/app.py:checkout"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn query");
    let rx = stdout_lines(&mut child);
    // Like `head -1`: take the first line only.
    let first = rx.recv_timeout(Duration::from_secs(120)).expect("first output line");
    assert!(first.starts_with("checkout  "), "{first}");
    assert!(child.wait().expect("trace exits").success());
    assert_pipe_closes(&rx);
}

#[test]
fn errors_exit_2_with_json_shape() {
    let fx = Fixture::new("errors", "python");
    fx.write("shop/dup.py", "def checkout():\n    return 1\n");
    let Some(_) = fx.index_or_skip() else { return };
    let out = fx.run(&["--json", "path", "checkout", "shop/fmt.py:format_price"]);
    assert_eq!(out.status.code(), Some(2));
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(err["command"], "path");
    assert_eq!(err["error_type"], "ambiguous_symbol");
    let candidates = err["candidates"].as_array().unwrap();
    let ids: Vec<&str> = candidates.iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["shop/app.py:checkout", "shop/dup.py:checkout"]);
    assert_eq!(candidates[0]["n"], 1);
    assert_eq!(candidates[1]["n"], 2);
    assert_eq!(candidates[1]["kind"], "function");
    assert_eq!(candidates[1]["file"], "shop/dup.py");
    assert_eq!(candidates[1]["line"], 1);
    let text = fx.run(&["deps", "checkout"]);
    assert_eq!(text.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&text.stderr);
    assert!(stderr.starts_with("Error: \"checkout\" matches 2 symbols. Use one of:"), "{stderr}");
    assert!(stderr.contains("\n  2. shop/dup.py:checkout"), "{stderr}");
    // Unknown symbols and synthetic scopes by bare name: exit 2.
    let missing = fx.run(&["--json", "uses", "no_such_symbol"]);
    assert_eq!(missing.status.code(), Some(2));
    let err: Value = serde_json::from_slice(&missing.stderr).unwrap();
    assert_eq!(err["command"], "uses");
    assert_eq!(err["error_type"], "symbol_not_found");
    assert_eq!(fx.run(&["deps", "<module>"]).status.code(), Some(2));
    // Success: 0 (also for "no path found").
    assert_eq!(
        fx.run(&["path", "shop/fmt.py:_two_places", "shop/app.py:main"])
            .status
            .code(),
        Some(0)
    );
    // Configuration / cache errors: exit 3.
    let config = fx
        .command()
        .env("TRACE_CACHE_DIR", "relative/cache")
        .args(["--json", "status"])
        .output()
        .unwrap();
    assert_eq!(config.status.code(), Some(3));
    let err: Value = serde_json::from_slice(&config.stderr).unwrap();
    assert_eq!(err["error_type"], "config");
    // Usage errors: exit 2 (clap), JSON shape with --json.
    let usage = fx.run(&["--json", "deps", "x", "--all"]);
    assert_eq!(usage.status.code(), Some(2));
    let err: Value = serde_json::from_slice(&usage.stderr).unwrap();
    assert_eq!(err["error_type"], "invalid_argument");
    assert_eq!(err["command"], "deps");
    let empty = fx.run(&["--json", "uses", " "]);
    assert_eq!(empty.status.code(), Some(2));
    assert_eq!(serde_json::from_slice::<Value>(&empty.stderr).unwrap()["error_type"], "invalid_argument");
    assert_eq!(fx.run(&["nonsense"]).status.code(), Some(2));
    // `status --install` with an unknown language: exit 2 before any download (the pinned
    // record would be printed to stderr first; stderr is only the JSON error).
    let install = fx.run(&["--json", "status", "--install", "nosuchlang"]);
    assert_eq!(install.status.code(), Some(2));
    let err: Value = serde_json::from_slice(&install.stderr).expect("only the JSON error on stderr");
    assert_eq!(err["command"], "status");
    assert_eq!(err["error_type"], "invalid_argument");
    assert!(install.stdout.is_empty());
}

#[test]
fn help_and_removed_commands() {
    let help = trace().arg("--help").output().unwrap();
    assert!(help.status.success());
    for command in ["show", "context", "uses", "deps", "path", "index", "status"] {
        let out = trace().args([command, "--help"]).output().unwrap();
        assert!(out.status.success(), "{command} --help");
    }
    let uses_help = String::from_utf8(trace().args(["uses", "--help"]).output().unwrap().stdout).unwrap();
    assert!(uses_help.contains("--deep"), "uses --help lists --deep:\n{uses_help}");
    for flag in ["--all", "--offline", "--budget"] {
        assert!(!uses_help.contains(flag), "uses --help does not list {flag}:\n{uses_help}");
    }
    // Old command names and removed flags are usage errors (exit 2; no aliases). Parsing
    // fails before any root is touched.
    for args in [
        &["query", "x"][..],
        &["references", "x"],
        &["impact", "x"],
        &["evidence", "x"],
        &["dependencies", "x"],
        &["doctor"],
        &["setup", "go", "--yes"],
        &["serve"],
        &["--format", "json", "status"],
        &["--no-jev", "status"],
        &["--no-bridges", "deps", "x"],
        &["--include", "possible", "deps", "x"],
        &["--depth", "3", "deps", "x"],
        &["uses", "x", "--kind", "call"],
        &["uses", "x", "--no-overrides"],
        &["uses", "x", "--callers-only"],
        &["uses", "--diff", "HEAD"],
        &["index", "--rebuild"],
        &["index", "--clean"],
        &["status", "--languages"],
        &["find", "x"],
        &["find", "x", "--all"],
        &["--offline", "status"],
        &["deps", "x", "--all"],
        &["status", "--all"],
        &["index", "--all"],
        &["status", "--deep"],
        &["show", "x", "--deep"],
        &["context", "q", "--budget", "50"],
        &["context", "how", "does", "login", "work"],
    ] {
        let out = trace().args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
    let out = trace().args(["--json", "references", "x"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(err["error_type"], "invalid_argument");
}

#[test]
fn inspected_root_must_not_contain_the_cache() {
    let fx = Fixture::new("inside", "go");
    let out = trace()
        .env("TRACE_CACHE_DIR", fx.root.join("cache-inside"))
        .env("TRACE_OFFLINE", "1")
        .args(["--root"])
        .arg(&fx.root)
        .arg("index")
        .output()
        .unwrap();
    // A cache inside the inspected root is a configuration error: exit 3.
    assert_eq!(out.status.code(), Some(3));
    assert!(!fx.root.join("cache-inside").exists());
}

#[test]
fn uses_rows_counts_evidence_and_completeness() {
    let fx = Fixture::new("uses", "python");
    let Some(index) = fx.index_or_skip() else { return };
    let semantic = semantic_or_skip(&index, "python");

    let uses = fx.json(&["uses", "shop/pricing.py:subtotal"]);
    assert_eq!(uses["schema"], 1);
    assert_eq!(uses["command"], "uses");
    assert_eq!(uses["symbol"]["id"], "shop/pricing.py:subtotal");
    assert_eq!(uses["bridges"], true);
    assert!(uses["tiers_used"].is_array());
    assert_eq!(uses["deep"], false);
    assert!(uses["impact"].is_null(), "no impact block without --deep");
    for list in ["incoming", "outgoing", "sites", "unresolved"] {
        assert!(uses["evidence"][list].is_array(), "evidence.{list}");
    }
    let completeness = &uses["completeness"];
    assert!(matches!(completeness["status"].as_str(), Some("complete" | "partial" | "unknown")));
    assert!(completeness["summary"]
        .as_str()
        .unwrap()
        .starts_with(completeness["status"].as_str().unwrap()));
    // Counts.
    let rows = uses["uses"].as_array().unwrap();
    let counts = &uses["counts"];
    assert_eq!(
        counts["uses"].as_u64().unwrap() as usize,
        rows.iter().filter(|r| r["kind"] != "declaration").count()
    );
    assert_eq!(
        counts["declarations"].as_u64().unwrap() as usize,
        rows.iter().filter(|r| r["kind"] == "declaration").count()
    );
    assert_eq!(
        counts["overrides"].as_u64().unwrap() as usize,
        rows.iter()
            .filter(|r| r["kind"] == "override" || r["kind"] == "implements")
            .count()
    );
    assert_eq!(
        counts["unresolved"].as_u64().unwrap() as usize,
        completeness["unresolved"].as_array().unwrap().len()
    );
    assert!(counts["callers_of_callers"].is_null() && counts["tests"].is_null());
    // The declaration row is always there, with the exact line text.
    let decl = use_row(&uses, "shop/pricing.py", 58, "declaration").expect("declaration row");
    assert_eq!(decl["text"], "def subtotal(prices: list[float]) -> float:");
    assert_eq!(decl["column"], 5);
    if semantic {
        let call = use_row(&uses, "shop/app.py", 7, "call").expect("call in checkout");
        assert_eq!(call["text"], "    amount = subtotal(prices)");
        assert_eq!(call["owner"], "shop/app.py:checkout");
        assert_eq!(call["tier"], "proven");
        assert_eq!(call["end_byte"].as_u64().unwrap() - call["start_byte"].as_u64().unwrap(), 8);
        let method = use_row(&uses, "shop/hooks.py", 7, "call").expect("call in a method");
        assert_eq!(method["owner"], "shop/hooks.py:Renderer.render");
        // Module-level code and lambdas at module level are owned by synthetic scopes.
        let module = use_row(&uses, "shop/hooks.py", 33, "call").expect("module-level call");
        assert_eq!(module["text"], "DEFAULT_TOTAL = subtotal([1.0, 2.0])");
        assert_eq!(module["owner"], "shop/hooks.py:<module>");
        let lambda = use_row(&uses, "shop/hooks.py", 32, "call").expect("call in a lambda");
        assert!(lambda["owner"].as_str().unwrap().contains("<lambda>"), "{lambda}");
        assert!(use_row(&uses, "shop/hooks.py", 2, "import").is_some(), "import row: {uses}");
        assert_eq!(completeness["status"], "complete", "{completeness}");
        assert!(completeness["unresolved"].as_array().unwrap().is_empty());
    }
    // Every partial answer lists its unresolved sites with their line text.
    if completeness["status"] == "partial" {
        for u in completeness["unresolved"].as_array().unwrap() {
            assert!(!u["text"].as_str().unwrap().is_empty(), "{u}");
        }
    }
    // Rows are sorted and unique per (file, start).
    let keys: Vec<(String, u64)> = rows
        .iter()
        .map(|r| (r["file"].as_str().unwrap().to_string(), r["start_byte"].as_u64().unwrap()))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(keys.len(), sorted.len(), "duplicate rows");

    // Text: header with the status word, rows grouped by file with the exact line.
    let text = fx.text(&["uses", "shop/pricing.py:subtotal"]);
    let head = text.lines().next().unwrap();
    assert!(head.starts_with("subtotal  "), "{text}");
    let word = match completeness["status"].as_str().unwrap() {
        "partial" => format!("{} unresolved", completeness["unresolved"].as_array().unwrap().len()),
        other => other.to_string(),
    };
    assert!(head.ends_with(&format!("  {word}")), "{text}");
    // The declaration is not a use: its row is listed with --deep only.
    assert!(use_head(&text, "shop/pricing.py", 58).is_none(), "{text}");
    let deep_text = fx.text(&["uses", "shop/pricing.py:subtotal", "--deep"]);
    let def =
        use_head(&deep_text, "shop/pricing.py", 58).unwrap_or_else(|| panic!("def row in\n{deep_text}"));
    assert!(def.ends_with("(def)"), "{def}");
    assert_eq!(use_code(&deep_text, def).map(str::trim), Some("def subtotal(prices: list[float]) -> float:"));
    for banned in ["include=", "JEV", "complete: all", "--format"] {
        assert!(!text.contains(banned), "no footers or banners ({banned}) in\n{text}");
    }
    if semantic {
        // Proven rows are unmarked.
        let call = use_head(&text, "shop/app.py", 7).expect("proven call row");
        assert_eq!(row_mark(call), "", "{call}");
        assert!(call.ends_with("  checkout"), "{call}");
        assert_eq!(use_code(&text, call).map(str::trim), Some("amount = subtotal(prices)"));
        // The summary: entry points above the callers and the relevant tests.
        assert!(uses["summary"]["impact"].is_array() && uses["summary"]["tests"].is_array(), "{uses}");
        assert!(
            text.lines().any(|l| l.starts_with("impact  checkout \u{2190} ")
                || l.starts_with("        checkout \u{2190} ")),
            "{text}"
        );
    }
    // Rows are proven and inferred only: --deep widens deps / path / context, never the rows
    // of `uses` (possible sites are listed under completeness.unresolved).
    let all = fx.json(&["uses", "shop/pricing.py:subtotal", "--deep"]);
    assert_eq!(all["include"], "possible");
    assert_eq!(all["uses"], uses["uses"]);
    for r in all["uses"].as_array().unwrap() {
        assert_ne!(r["tier"], "possible", "{r}");
    }

    if semantic {
        // Override family: always included, labelled with `via`.
        let family = fx.json(&["uses", "shop/hooks.py:Renderer.render"]);
        assert_eq!(ids(&family["family"]), vec!["shop/hooks.py:PlainRenderer.render"]);
        let over = use_row(&family, "shop/hooks.py", 11, "override").expect("override row");
        assert_eq!(over["via"], "shop/hooks.py:PlainRenderer.render");
        let dispatch = use_row(&family, "shop/hooks.py", 21, "call").expect("call through the base");
        assert_eq!(dispatch["owner"], "shop/hooks.py:show");
        let concrete =
            use_row(&family, "shop/hooks.py", 34, "call").expect("module-level call of the override");
        assert_eq!(concrete["via"], "shop/hooks.py:PlainRenderer.render");
        assert!(family["counts"]["overrides"].as_u64().unwrap() >= 1);
        let text = fx.text(&["uses", "shop/hooks.py:Renderer.render"]);
        assert!(text.lines().next().unwrap().contains(" (incl. 1 override)"), "{text}");
        let over = use_head(&text, "shop/hooks.py", 11).expect("override text row");
        assert!(over.ends_with("(override)   (PlainRenderer.render)"), "{over}");
        let concrete = use_head(&text, "shop/hooks.py", 34).expect("concrete call text row");
        assert!(concrete.ends_with("   (via PlainRenderer.render)"), "{concrete}");
        let plain = use_head(&text, "shop/hooks.py", 21).expect("base call text row");
        assert!(!plain.contains("(via"), "{plain}");
        // Writes are their own kind.
        let writes = fx.json(&["uses", "shop/hooks.py:Hooks.redirect"]);
        let w = use_row(&writes, "shop/hooks.py", 25, "write").expect("write row");
        assert_eq!(w["text"], "    hooks.redirect = lambda location: location.upper()");
        assert_eq!(w["owner"], "shop/hooks.py:install");
    }
}

#[test]
fn uses_deep_sections_and_through() {
    let fx = Fixture::new("uses-deep", "python");
    let Some(index) = fx.index_or_skip() else { return };
    let semantic = semantic_or_skip(&index, "python");
    let deep = fx.json(&["uses", "shop/pricing.py:subtotal", "--deep"]);
    assert_eq!(deep["deep"], true);
    assert!(deep["completeness"].is_object());
    let impact = &deep["impact"];
    for key in [
        "callers",
        "other_references",
        "transitive",
        "result_uses",
        "similar_code",
        "tests",
    ] {
        assert!(impact[key].is_array(), "impact.{key}");
    }
    for removed in ["must_change", "assumption", "diff", "targets", "callers_only"] {
        assert!(impact.get(removed).is_none(), "impact.{removed}");
    }
    let totals = &impact["totals"];
    let callers = impact["callers"].as_array().unwrap();
    assert_eq!(totals["call_sites"].as_u64().unwrap() as usize, callers.len());
    assert_eq!(totals["tests"].as_u64().unwrap() as usize, impact["tests"].as_array().unwrap().len());
    assert_eq!(totals["transitive"], impact["transitive_total"]);
    assert_eq!(deep["counts"]["callers_of_callers"], impact["transitive_total"]);
    for c in callers {
        assert!(c["call_site"].is_object() && c["text"].is_string(), "{c}");
        assert!(c["through"].is_null(), "{c}");
    }
    // Every caller of callers names the direct caller its chain passes.
    let direct: Vec<&str> = ids(&impact["callers"]);
    for t in impact["transitive"].as_array().unwrap() {
        let through = t["through"].as_str().unwrap_or_else(|| panic!("through on {t}"));
        assert!(direct.contains(&through) || ids(&impact["other_references"]).contains(&through), "{t}");
    }
    // The direct call sites are `uses` rows (plain `uses` = the old callers-only view).
    let plain = fx.json(&["uses", "shop/pricing.py:subtotal"]);
    assert!(plain["impact"].is_null());
    let rows: Vec<(String, u64)> = plain["uses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["file"].as_str().unwrap().to_string(), r["line"].as_u64().unwrap()))
        .collect();
    for c in callers {
        let site =
            (c["call_site"]["file"].as_str().unwrap().to_string(), c["call_site"]["line"].as_u64().unwrap());
        assert!(rows.contains(&site), "{site:?} not in uses rows");
    }
    assert_eq!(plain["uses"], deep["uses"], "--deep adds blocks, never changes rows");
    if semantic {
        let checkout = callers
            .iter()
            .find(|c| c["id"] == "shop/app.py:checkout")
            .expect("checkout calls subtotal");
        assert_eq!(checkout["text"], "    amount = subtotal(prices)");
        assert!(callers.iter().any(|c| c["id"] == "shop/hooks.py:<module>"), "module-level caller");
        let main = row(&impact["transitive"], "shop/app.py:main").expect("main is a caller of callers");
        assert_eq!(main["through"], "shop/app.py:checkout");
        // One row per call site: `<module>` calls subtotal once directly (the lambda owns the other).
        let module_rows = callers.iter().filter(|c| c["id"] == "shop/hooks.py:<module>").count();
        assert_eq!(module_rows, 1);
        let other = impact["other_references"].as_array().unwrap();
        assert!(other.iter().any(|o| o["relation"] == "import"), "{other:?}");
        let text = fx.text(&["uses", "shop/pricing.py:subtotal", "--deep"]);
        assert!(text.lines().next().unwrap().contains(" callers of callers"), "{text}");
        assert!(text.contains("\ncallers of callers (via checkout):\n"), "{text}");
        assert!(
            text.lines()
                .any(|l| l.starts_with("  shop/app.py: ") && l.contains("main")),
            "{text}"
        );
        assert!(text.contains("\nall tests: tests/test_app.py::"), "{text}");

        let family = fx.json(&["uses", "shop/hooks.py:Renderer.render", "--deep"]);
        assert_eq!(ids(&family["family"]), vec!["shop/hooks.py:PlainRenderer.render"]);
        assert!(family["impact"]["callers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["via"] == "shop/hooks.py:PlainRenderer.render"));
    }
}

#[test]
fn selectors_and_envelope() {
    let fx = Fixture::new("selectors", "python");
    let Some(_) = fx.index_or_skip() else { return };
    // One grammar: file:line, module-qualified names and `::`.
    assert_eq!(fx.json(&["deps", "shop/app.py:7"])["symbol"]["id"], "shop/app.py:checkout");
    assert_eq!(fx.json(&["deps", "shop.app.checkout"])["symbol"]["id"], "shop/app.py:checkout");
    assert_eq!(fx.json(&["deps", "Inventory::restock"])["symbol"]["id"], "shop/store.py:Inventory.restock");
    // Module-level code: traversal start points (`deps`, `path`) start at the executing
    // scope there (the `uses` error and the rest of the grammar: rule_selectors.rs).
    assert_eq!(fx.json(&["deps", "shop/hooks.py:33"])["symbol"]["qualified_name"], "<module>");
    // Every query command carries the same envelope with "schema": 1.
    let deps = fx.json(&["deps", "shop/app.py:checkout"]);
    assert_eq!(deps["bridges"], true);
    assert!(deps["tiers_used"].is_array());
    assert!(deps["completeness"]["summary"].is_string());
    for args in [
        &["uses", "shop/app.py:checkout"][..],
        &["path", "shop/app.py:main", "shop/fmt.py:_two_places"][..],
        &["context", "shop/pricing.py:apply_discount"][..],
        &["show", "shop/app.py:checkout"][..],
    ] {
        let r = fx.json(args);
        assert_eq!(r["schema"], 1, "{args:?}");
        assert_eq!(r["command"], args[0], "{args:?}");
        assert!(r["tiers_used"].is_array() && r["bridges"] == true, "{args:?}");
    }
    // Synthetic scopes are not search results or overview entries.
    let status = fx.json(&["status"]);
    for e in status["overview"]["entry_points"].as_array().unwrap() {
        assert!(!e["name"].as_str().unwrap().starts_with('<'), "{e}");
    }
    assert!(status["resolution"].is_array());
    assert!(status["bridges"].is_array());
    let setup = status["setup"].as_array().expect("setup rows");
    let python = setup.iter().find(|l| l["language"] == "python").expect("python row");
    assert_eq!(python["status"], "ready");
    let text = fx.text(&["status"]);
    assert!(text.lines().any(|l| l.starts_with("python  ")), "{text}");
}

#[test]
fn completeness_states() {
    let fx = Fixture::new("completeness", "go");
    // A file without syntax facts that mentions the name: the answer cannot be complete.
    fx.write("notes.sql", "select greet from greetings;\n");
    let Some(index) = fx.index_or_skip() else { return };
    let uses = fx.json(&["uses", "main.go:greet"]);
    let c = &uses["completeness"];
    assert_eq!(c["status"], "unknown", "{c}");
    assert!(c["summary"].as_str().unwrap().contains("without syntax facts"));
    let text = fx.text(&["uses", "main.go:greet"]);
    assert!(text.lines().next().unwrap().ends_with("  unknown"), "{text}");
    fs::remove_file(fx.root.join("notes.sql")).unwrap();
    let uses = fx.json(&["uses", "main.go:greet"]);
    let c = &uses["completeness"];
    assert_ne!(c["status"], "unknown", "{c}");
    assert!(c["name_matches"].as_u64().unwrap() >= 2, "{c}");
    assert_eq!(support(&index, "go"), "semantic");
}

#[test]
fn typescript_module_level_callbacks_and_writes() {
    let fx = Fixture::new("ts-uses", "typescript");
    let Some(index) = fx.index_or_skip() else { return };
    if !semantic_or_skip(&index, "typescript") {
        return;
    }
    let uses = fx.json(&["uses", "src/app.ts:Context.notFound"]);
    let call = use_row(&uses, "src/app.ts", 28, "call").expect("module-level call");
    assert_eq!(call["text"], "ctx.notFound();");
    assert_eq!(call["owner"], "src/app.ts:<module>");
    let write = use_row(&uses, "src/app.ts", 29, "write").expect("write row");
    assert_eq!(write["text"], "ctx.notFound = (): number => 410;");
    let add = fx.json(&["uses", "src/math.ts:add"]);
    let lambda = use_row(&add, "src/app.ts", 27, "call").expect("call in a module-level arrow callback");
    assert!(lambda["owner"].as_str().unwrap().contains("<lambda>"), "{lambda}");
    assert!(use_row(&add, "src/app.ts", 1, "import").is_some(), "{add}");
    let family = fx.json(&["uses", "src/app.ts:Base.compute"]);
    assert_eq!(ids(&family["family"]), vec!["src/app.ts:Plus.compute"]);
}

#[test]
fn rust_generic_selector() {
    let fx = Fixture::new("rust-selector", "rust");
    let Some(index) = fx.index_or_skip() else { return };
    let uses = fx.json(&["uses", "Data<'a>::from_bytes"]);
    assert_eq!(uses["symbol"]["id"], "src/lib.rs:Data.from_bytes");
    if semantic_or_skip(&index, "rust") {
        let call = use_row(&uses, "src/lib.rs", 68, "call").expect("assoc fn call through the type path");
        assert_eq!(call["text"], "    match Data::from_bytes(bytes) {");
        assert_eq!(call["owner"], "src/lib.rs:decode");
    }
}
