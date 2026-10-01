//! `uses` text: the use rows grouped by file, conditions, entry points, tests, impact and
//! the `--deep` blocks.

use std::collections::HashSet;
use std::fmt::Write as _;

use trace_analysis::report::{
    Completeness, DeepImpact, GuardTest, ImpactChain, ReferenceRow, TestRow, UsesSummary,
};

use super::{
    check_rows, code, collapse, count, marked, short, status_word, Rows, CALLERS_CAP, CHECK_CAP,
    IMPACT_SHOWN, NAMES_CAP, SUMMARY_TESTS_SHOWN, TESTS_SHOWN, USES_CAP,
};

/// `path:name` of a test id without the path (`a_test.go::TestX` -> `TestX`).
fn test_name(test: &str) -> &str {
    test.split_once("::").map_or(test, |(_, n)| n)
}

// ------------------------------------------------------------------ shared blocks

/// `impact  caller ← entry, entry` lines (one per direct caller).
fn impact_lines(chains: &[ImpactChain]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, c) in chains.iter().take(IMPACT_SHOWN).enumerate() {
        let label = if i == 0 { "impact  " } else { "        " };
        let line = if c.entry_points.len() == 1 && c.entry_points[0] == c.caller {
            format!("{label}{} (entry point)", short(&c.caller))
        } else {
            let entries: Vec<&str> = c.entry_points.iter().map(|e| short(e)).collect();
            let mut s = format!("{label}{} \u{2190} {}", short(&c.caller), entries.join(", "));
            if c.entry_points_total > c.entry_points.len() {
                let _ = write!(s, " (+{})", c.entry_points_total - c.entry_points.len());
            }
            s
        };
        out.push(line);
    }
    if chains.len() > IMPACT_SHOWN {
        out.push(format!("        (+{} more callers)", chains.len() - IMPACT_SHOWN));
    }
    out
}

/// Distinct entry points of all chains (shown ones, plus the ones cut from each chain).
fn entry_point_count(chains: &[ImpactChain]) -> usize {
    let shown: HashSet<&str> = chains
        .iter()
        .flat_map(|c| c.entry_points.iter().map(String::as_str))
        .collect();
    shown.len()
        + chains
            .iter()
            .map(|c| c.entry_points_total.saturating_sub(c.entry_points.len()))
            .sum::<usize>()
}

/// `tests   file:line Name   <line that sets the option>` lines, then `+N more → --deep`.
pub(super) fn test_lines(tests: &[GuardTest], total: usize) -> Vec<String> {
    let shown: Vec<&GuardTest> = tests.iter().take(SUMMARY_TESTS_SHOWN).collect();
    if shown.is_empty() {
        return Vec::new();
    }
    let cells: Vec<String> = shown
        .iter()
        .map(|t| format!("{}:{} {}", t.file, t.line, test_name(&t.test)))
        .collect();
    let width = shown
        .iter()
        .zip(&cells)
        .filter(|(t, _)| !t.sets.is_empty())
        .map(|(_, c)| c.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = Vec::new();
    for (i, (t, cell)) in shown.iter().zip(&cells).enumerate() {
        let label = if i == 0 { "tests   " } else { "        " };
        let line = if t.sets.is_empty() {
            format!("{label}{cell}")
        } else {
            format!("{label}{cell:<width$}   {}", code(&t.sets))
        };
        out.push(line);
    }
    let more = total.saturating_sub(shown.len());
    if more > 0 {
        out.push(format!("        +{more} more \u{2192} --deep"));
    }
    out
}

pub(super) fn uses_text(
    symbol: &str,
    rows: &[ReferenceRow],
    c: Option<&Completeness>,
    summary: &UsesSummary,
    impact: Option<&DeepImpact>,
    deep: bool,
) -> String {
    let n = rows.iter().filter(|r| r.kind != "declaration").count();
    let k = rows
        .iter()
        .filter(|r| matches!(r.kind, "override" | "implements"))
        .count();
    let mut head = format!("{}  {}", short(symbol), count(n, "use", "uses"));
    if k > 0 {
        let _ = write!(head, " (incl. {})", count(k, "override", "overrides"));
    }
    if !summary.impact.is_empty() {
        let _ = write!(
            head,
            " \u{b7} {}",
            count(entry_point_count(&summary.impact), "entry point", "entry points")
        );
    }
    if summary.tests_total > 0 {
        let _ = write!(head, " \u{b7} {}", count(summary.tests_total, "test", "tests"));
    }
    if let Some(i) = impact {
        let _ = write!(head, " \u{b7} {} callers of callers", i.transitive_total);
    }
    let _ = write!(head, "  {}", status_word(c, true));
    let mut out = vec![head];

    let shown: Vec<&ReferenceRow> = rows.iter().filter(|r| deep || r.kind != "declaration").collect();
    for r in shown.iter().take(USES_CAP) {
        let loc = marked(&format!("{}:{}", r.file, r.line), r.tier);
        let who = r.owner.as_deref().map(short).unwrap_or("");
        let kind = match r.kind {
            "call" => String::new(),
            "declaration" => "(def)".to_string(),
            other => format!("({other})"),
        };
        let label = [who, kind.as_str()]
            .iter()
            .filter(|p| !p.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        let via = match r.via.as_deref() {
            Some(v) if !v.is_empty() && matches!(r.kind, "override" | "implements") => {
                format!("   ({})", short(v))
            }
            Some(v) if !v.is_empty() => format!("   (via {})", short(v)),
            _ => String::new(),
        };
        out.push(format!("{loc}  {label}{via}").trim_end().to_string());
        let pad = " ".repeat(loc.chars().count() + 2);
        out.push(format!("{pad}{}", code(&r.text)));
        if !r.when.is_empty() {
            out.push(format!("{pad}when  {}", r.when.join(" \u{b7} ")));
        }
    }
    if shown.len() > USES_CAP {
        out.push(format!("(+{} more)", shown.len() - USES_CAP));
    }
    out.extend(impact_lines(&summary.impact));
    out.extend(test_lines(&summary.tests, summary.tests_total));
    if let Some(i) = impact {
        out.extend(deep_blocks(i));
    }

    let printed: HashSet<(&str, u32, u32)> =
        rows.iter().map(|r| (r.file.as_str(), r.line, r.start_byte)).collect();
    let check = check_rows(c, &printed);
    if !check.is_empty() {
        let mut tail = Rows::default();
        tail.section(Some("check:"), collapse(check), CHECK_CAP);
        tail.render(&mut out);
    }
    out.join("\n")
}

/// `uses --deep`: callers of callers grouped per `through` caller, then per file; every test.
pub(super) fn deep_blocks(impact: &DeepImpact) -> Vec<String> {
    type Files<'a> = Vec<(&'a str, Vec<&'a str>)>;
    let mut groups: Vec<(Option<&str>, Files<'_>)> = Vec::new();
    for t in &impact.transitive {
        let through = t.through.as_deref().filter(|s| !s.is_empty());
        let g = match groups.iter().position(|(k, _)| *k == through) {
            Some(g) => g,
            None => {
                groups.push((through, Vec::new()));
                groups.len() - 1
            }
        };
        let files = &mut groups[g].1;
        let file = t.card.file.as_str();
        let name = t.card.qualified_name.as_str();
        match files.iter_mut().find(|(f, _)| *f == file) {
            Some((_, names)) => names.push(name),
            None => files.push((file, vec![name])),
        }
    }
    let mut out = Vec::new();
    let mut file_lines = 0usize;
    for (through, files) in &groups {
        out.push(match through {
            Some(t) => format!("callers of callers (via {}):", short(t)),
            None => "callers of callers:".to_string(),
        });
        for (file, names) in files {
            file_lines += 1;
            if file_lines > CALLERS_CAP {
                continue;
            }
            let shown = &names[..names.len().min(NAMES_CAP)];
            let mut line = format!("  {file}: {}", shown.join(", "));
            if names.len() > NAMES_CAP {
                let _ = write!(line, " (+{})", names.len() - NAMES_CAP);
            }
            out.push(line);
        }
    }
    let rest = file_lines.saturating_sub(CALLERS_CAP)
        + impact.transitive_total.saturating_sub(impact.transitive.len());
    if rest > 0 && !groups.is_empty() {
        out.push(format!("  (+{rest} more)"));
    }
    if let Some(line) = tests_line(&impact.tests) {
        out.push(line);
    }
    out
}

/// `all tests: a.py::t1, ::t2, b.py::t3 (+N more direct, +M via callers)`.
pub(super) fn tests_line(tests: &[TestRow]) -> Option<String> {
    if tests.is_empty() {
        return None;
    }
    let direct: Vec<&TestRow> = tests.iter().filter(|t| t.directness == "direct").collect();
    let via: Vec<&TestRow> = tests.iter().filter(|t| t.directness != "direct").collect();
    let pool = if direct.is_empty() { &via } else { &direct };
    let shown = &pool[..pool.len().min(TESTS_SHOWN)];
    let mut names = Vec::with_capacity(shown.len());
    let mut last: Option<&str> = None;
    for t in shown {
        let (file, name) = t.test.split_once("::").unwrap_or((t.file.as_str(), t.test.as_str()));
        names.push(if last == Some(file) {
            format!("::{name}")
        } else {
            format!("{file}::{name}")
        });
        last = Some(file);
    }
    let mut more = Vec::new();
    if !direct.is_empty() {
        if direct.len() > shown.len() {
            more.push(format!("+{} more direct", direct.len() - shown.len()));
        }
        if !via.is_empty() {
            more.push(format!("+{} via callers", via.len()));
        }
    } else if via.len() > shown.len() {
        more.push(format!("+{} more via callers", via.len() - shown.len()));
    }
    let mut line = format!("all tests: {}", names.join(", "));
    if !more.is_empty() {
        let _ = write!(line, " ({})", more.join(", "));
    }
    Some(line)
}

// ------------------------------------------------------------------ deps
