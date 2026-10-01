//! Text and JSON rendering (SPEC.md §11 "Text output"; PLAN §5). JSON (`--json`) is
//! `serde_json::to_string_pretty` of the report struct (every query report carries
//! `"schema": 1` and full provenance). Text is written for a model's token budget:
//!
//! 1. one header line: the subject, where it is, counts and a status word (`complete`,
//!    `<N> unresolved`, `unknown`, `bounded`);
//! 2. code is exact: `show` and `context` print the declaration verbatim; rows print the
//!    call on one line (`deps`, `path`) or the exact line (`uses`, callers), trimmed and
//!    capped at [`CODE_CAP`] characters;
//! 3. conditions follow their site: `when a · b` under a use or caller, `if a && b` after a
//!    call; conditions shared by consecutive calls are printed once above them (`if a:`)
//!    with the calls indented below;
//! 4. marks after a line number: none = proven, `~` = inferred, `?` = possible;
//! 5. long lists end with `(+N more)` or `+N more → --deep`; unresolved same-name sites come
//!    last under `check:` (at most [`CHECK_CAP`] rows in the ranked order);
//! 6. no legends, banners or footers.
//!
//! Files: shared row helpers and the output caps here; one file per command text (`show`,
//! `uses`, `deps`, `path`, `context`, `status`, `index`).

use std::collections::HashSet;
use std::fmt::Write as _;

use trace_analysis::report::{
    Completeness, ContextReport, DependenciesReport, IndexReport, PathReport, ShowReport, StatusReport,
    UsesReport,
};

mod context;
mod deps;
mod index;
mod path;
mod show;
mod status;
mod uses;

use context::context_text;
use deps::deps_text;
use index::index_text;
pub use index::watch_line;
use path::path_text;
use show::show_text;
use status::{status_text, StatusView};
use uses::uses_text;

/// Code column cap in characters (119 + `…` when longer).
pub const CODE_CAP: usize = 120;
/// `uses` rows shown.
pub const USES_CAP: usize = 200;
/// `check:` rows shown (`uses`; the JSON keeps every ranked site).
pub const CHECK_CAP: usize = 20;
/// `check:` rows shown (`deps --deep`).
pub const DEPS_CHECK_CAP: usize = 30;
/// `deps` / `context` call rows shown.
pub const CALLS_CAP: usize = 80;
/// `then` lines shown (`deps`).
pub const THEN_CAP: usize = 30;
/// `callers of callers` file lines shown (`uses --deep`).
pub const CALLERS_CAP: usize = 30;
/// Names per `then` / `callers of callers` line.
pub const NAMES_CAP: usize = 8;
/// Tests named on the `all tests:` line (`uses --deep`).
pub const TESTS_SHOWN: usize = 10;
/// Tests listed under `tests` (`uses`, `context`).
pub const SUMMARY_TESTS_SHOWN: usize = 5;
/// Callers listed under `impact`.
pub const IMPACT_SHOWN: usize = 10;
/// Callers listed by `context`.
pub const CONTEXT_CALLERS: usize = 20;
/// `argument → parameter` pairs per `path` hop.
pub const CARRIES_SHOWN: usize = 4;
/// Widest call column before the condition / target column.
pub const CALL_COLUMN: usize = 64;

/// Any command result.
pub enum Output {
    Index(IndexReport),
    Show(ShowReport),
    Context(Box<ContextReport>),
    Uses(Box<UsesReport>),
    Deps(DependenciesReport),
    Path(PathReport),
    Status(Box<StatusReport>),
}

impl Output {
    /// Audit text retains ordinary relationship output and numbers only included source.
    pub fn audit_text(&self) -> String {
        match self {
            Output::Context(r) => context::context_text_with_lines(r, true),
            _ => self.text(),
        }
    }

    /// `--json`: the pretty-printed report; else the text layout.
    pub fn render(&self, json: bool) -> String {
        if json {
            self.json()
        } else {
            self.text()
        }
    }

    pub fn json(&self) -> String {
        let r = match self {
            Output::Index(r) => serde_json::to_string_pretty(r),
            Output::Show(r) => serde_json::to_string_pretty(r),
            Output::Context(r) => serde_json::to_string_pretty(r),
            Output::Uses(r) => serde_json::to_string_pretty(r),
            Output::Deps(r) => serde_json::to_string_pretty(r),
            Output::Path(r) => serde_json::to_string_pretty(r),
            Output::Status(r) => serde_json::to_string_pretty(r),
        };
        r.unwrap_or_else(|e| {
            serde_json::json!({"error": format!("serialization failed: {e}"), "error_type": "serialize"})
                .to_string()
        })
    }

    pub fn text(&self) -> String {
        let mut out = match self {
            Output::Index(r) => index_text(r),
            Output::Show(r) => show_text(r),
            Output::Context(r) => context_text(r),
            Output::Uses(r) => uses_text(
                &r.symbol.id,
                &r.uses,
                r.envelope.completeness.as_ref(),
                &r.summary,
                r.impact.as_ref(),
                r.deep,
            ),
            Output::Deps(r) => deps_text(r),
            Output::Path(r) => path_text(r),
            Output::Status(r) => status_text(&StatusView::of(r)),
        };
        while out.ends_with('\n') {
            out.pop();
        }
        out
    }
}

/// Absolute file-line gutter. The JSON source remains verbatim and complete.
pub fn numbered_source(source: &str, start: u32) -> String {
    let lines: Vec<_> = source.lines().collect();
    let width = (u64::from(start) + lines.len().saturating_sub(1) as u64)
        .to_string()
        .len();
    lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| format!("{:>width$} | {line}", u64::from(start) + i as u64))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn audit_show(r: &trace_analysis::queries::audit::AuditShow, json: bool) -> String {
    if json {
        return serde_json::to_string_pretty(r).expect("audit report serializes");
    }
    let mut blocks = Vec::new();
    for item in &r.symbols {
        let s = &item.symbol;
        let detail = if s.kind == "test_block" {
            "syntax-only; test candidate, collection unknown".to_owned()
        } else if let Some(d) = &item.definition {
            format!(
                "syntax-only; runtime binding unknown · {}{}",
                count(d.binding_definitions.unwrap_or(0), "binding definition", "binding definitions"),
                if d.conditional == Some(true) {
                    " · conditional"
                } else {
                    ""
                }
            )
        } else {
            format!(
                "{} · {}",
                count(item.callers.expect("callable report has counts"), "caller", "callers"),
                count(item.calls.expect("callable report has counts"), "call", "calls")
            )
        };
        blocks.push(format!(
            "{} · lines {}–{} · {} · {}\n{}",
            s.id,
            s.line,
            s.end_line,
            s.kind,
            detail,
            numbered_source(&item.source, s.line)
        ));
    }
    for e in &r.errors {
        let mut text = format!("Not found: {}\n  {}", e.requested, e.error);
        if !e.suggestions.is_empty() {
            let _ = write!(text, "\n  Suggestions: {}", e.suggestions.join(", "));
        }
        if !e.candidates.is_empty() {
            let _ = write!(
                text,
                "\n  Candidates: {}",
                e.candidates
                    .iter()
                    .map(|c| c.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        text.push_str("\n  Use symbols to discover exact ids.");
        blocks.push(text);
    }
    blocks.join("\n\n")
}

pub fn source_search(r: &trace_analysis::queries::source::SearchReport, json: bool) -> String {
    if json {
        return serde_json::to_string_pretty(r).expect("search report serializes");
    }
    let mut out = format!(
        "search · {} matching lines shown · {} eligible files · literal excerpts",
        r.matches.len(),
        r.eligible_files
    );
    for m in &r.matches {
        let _ = write!(
            out,
            "\n{}:{}:{}{}\n{}",
            m.file,
            m.line,
            m.column,
            if m.truncated {
                " [preview truncated; use source for full line]"
            } else {
                ""
            },
            m.preview
        );
        if let Some(owner) = &m.owner {
            let _ = write!(out, "\n  owner: {owner}");
        }
    }
    if let Some(next) = r.next_offset {
        let _ = write!(out, "\nMore: repeat query/filters with --offset {next}");
    }
    out.push_str(
        "\nRead exact lines: source <file> --start <line> --lines <count>; full definitions: show <id>.",
    );
    out
}

pub fn source_window(r: &trace_analysis::queries::source::SourceReport, json: bool) -> String {
    if json {
        return serde_json::to_string_pretty(r).expect("source report serializes");
    }
    let mut out = format!(
        "{} · lines {}–{} of {} · source window, not a complete definition\n{}",
        r.file,
        r.start_line,
        r.end_line,
        r.file_lines,
        numbered_source(&r.source, r.start_line)
    );
    if let Some(next) = r.next_line {
        let _ = write!(out, "\nMore: source {} --start {next} --lines <count>", r.file);
    }
    out
}

pub fn discovery(r: &trace_analysis::queries::audit::Discovery, json: bool) -> String {
    if json {
        return serde_json::to_string_pretty(r).expect("discovery report serializes");
    }
    let mut out = format!(
        "symbols · {} matches · showing {} from offset {} · {} matches",
        r.total,
        r.matches.len(),
        r.offset,
        r.match_mode
    );
    for s in &r.matches {
        let _ = write!(
            out,
            "\n{}  lines {}–{}  {}{}{}",
            s.id,
            s.line,
            s.end_line,
            s.kind,
            match s.test_role {
                Some("case_candidate") => " [test case?]",
                Some("block_candidate") => " [test block?]",
                Some("helper_candidate") => " [test helper?]",
                Some("container") => " [test container]",
                Some("data") => " [test data]",
                _ if s.is_test => " [test code]",
                _ => "",
            },
            if s.semantic {
                ""
            } else {
                " [not semantically analyzed]"
            }
        );
        if let Some(label) = &s.label {
            let _ = write!(out, " · {label:?}");
        }
    }
    if r.total == 0 {
        let _ = write!(out, "\n{} eligible definitions after filters; not proof that source is absent. Try search <literal> [--file <path>] for bounded source evidence.", r.eligible_symbols);
    }
    if let Some(next) = r.next_offset {
        let _ = write!(out, "\nMore: repeat the same query/filters with --offset {next}");
    }
    out
}

// ------------------------------------------------------------------ rows

/// One `check:` row: `  <prefix padded>  <code><suffix>` under its file line.
#[derive(Clone, Debug, PartialEq)]
struct Row {
    file: String,
    prefix: String,
    code: String,
    suffix: String,
}

struct Section {
    title: Option<&'static str>,
    rows: Vec<Row>,
    overflow: usize,
}

/// Row sections grouped by file (path printed when it changes), the prefix column padded to
/// a width shared by the sections.
#[derive(Default)]
struct Rows {
    sections: Vec<Section>,
}

impl Rows {
    fn section(&mut self, title: Option<&'static str>, mut rows: Vec<Row>, cap: usize) {
        let overflow = rows.len().saturating_sub(cap);
        rows.truncate(cap);
        self.sections.push(Section {
            title,
            rows,
            overflow,
        });
    }

    fn width(&self) -> usize {
        self.sections
            .iter()
            .flat_map(|s| &s.rows)
            .map(|r| r.prefix.chars().count())
            .max()
            .unwrap_or(0)
    }

    fn render(&self, out: &mut Vec<String>) {
        let width = self.width();
        for s in &self.sections {
            if let Some(title) = s.title {
                out.push(title.to_string());
            }
            let mut last: Option<&str> = None;
            for r in &s.rows {
                if last != Some(r.file.as_str()) {
                    out.push(r.file.clone());
                    last = Some(r.file.as_str());
                }
                let line = format!("  {:<width$}  {}{}", r.prefix, r.code, r.suffix);
                out.push(line.trim_end().to_string());
            }
            if s.overflow > 0 {
                out.push(format!("  (+{} more)", s.overflow));
            }
        }
    }
}

/// Identical consecutive rows -> one row with ` (Nx)`.
fn collapse(rows: Vec<Row>) -> Vec<Row> {
    let mut out: Vec<(Row, usize)> = Vec::with_capacity(rows.len());
    for r in rows {
        match out.last_mut() {
            Some((last, n)) if *last == r => *n += 1,
            _ => out.push((r, 1)),
        }
    }
    out.into_iter()
        .map(|(mut r, n)| {
            if n > 1 {
                let _ = write!(r.suffix, " ({n}x)");
            }
            r
        })
        .collect()
}

/// `~` inferred, `?` possible, nothing for proven.
fn mark(tier: &str) -> &'static str {
    match tier {
        "inferred" => "~",
        "possible" => "?",
        _ => "",
    }
}

/// `<text>[ <mark>]`.
fn marked(text: &str, tier: &str) -> String {
    match mark(tier) {
        "" => text.to_string(),
        m => format!("{text} {m}"),
    }
}

/// `<line> <kind>[ <mark>]` (`check:` rows).
fn prefix(line: u32, kind: &str, mark: &str) -> String {
    if mark.is_empty() {
        format!("{line} {kind}")
    } else {
        format!("{line} {kind} {mark}")
    }
}

/// The exact source line, trimmed, capped at [`CODE_CAP`] characters.
fn code(text: &str) -> String {
    let t = text.trim();
    if t.chars().count() <= CODE_CAP {
        t.to_string()
    } else {
        let mut s: String = t.chars().take(CODE_CAP - 1).collect();
        s.push('\u{2026}');
        s
    }
}

/// `path:Qualified.name#k` -> `Qualified.name`.
fn short(uid: &str) -> &str {
    let q = uid.split_once(':').map_or(uid, |(_, q)| q);
    q.split('#').next().unwrap_or(q)
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Header status word.
fn status_word(c: Option<&Completeness>, bounds_complete: bool) -> String {
    if !bounds_complete {
        return "bounded".to_string();
    }
    match c {
        None => "complete".to_string(),
        Some(c) => match c.status {
            "complete" => "complete".to_string(),
            "partial" => format!("{} unresolved", c.unresolved.len()),
            _ => "unknown".to_string(),
        },
    }
}

/// `check:` rows: unresolved same-name sites not already printed, in the report order
/// (ranked: same module, importers, name-only). The file line is printed whenever the file
/// changes.
fn check_rows(c: Option<&Completeness>, printed: &HashSet<(&str, u32, u32)>) -> Vec<Row> {
    let Some(c) = c else {
        return Vec::new();
    };
    c.unresolved
        .iter()
        .filter(|u| !printed.contains(&(u.at.file.as_str(), u.at.line, u.at.start_byte)))
        .map(|u| Row {
            file: u.at.file.clone(),
            prefix: prefix(u.at.line, u.kind, "?"),
            code: code(&u.text),
            suffix: String::new(),
        })
        .collect()
}

#[cfg(test)]
#[path = "../../tests/unit/render/mod.rs"]
mod tests;
