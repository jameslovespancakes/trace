//! `deps` text: one row per call with its conditions and targets, then the reached symbols.

use std::fmt::Write as _;

use trace_analysis::report::{CallRow, DependenciesReport, Reached};

use super::{
    code, collapse, count, marked, prefix, short, Row, Rows, CALLS_CAP, CALL_COLUMN, DEPS_CHECK_CAP,
    NAMES_CAP, THEN_CAP,
};

/// One laid-out line of a call list: a shared condition or a call row.
enum CallLine<'r> {
    Group {
        indent: usize,
        condition: &'r str,
    },
    Call {
        indent: usize,
        row: &'r CallRow,
        inline: String,
    },
}

/// `→ target` text of a call row: the target's location, its name too when the call text
/// does not already name it; `(same file)` locations as `:line`. An undecided call (dispatch
/// whose implementation is not known) marks each target with its tier (`?` possible).
fn targets_text(row: &CallRow) -> String {
    let parts: Vec<String> = row
        .targets
        .iter()
        .map(|t| {
            let name = short(&t.id);
            let last = name.rsplit('.').next().unwrap_or(name);
            let loc = if t.file == row.at.file {
                format!(":{}", t.line)
            } else {
                format!("{}:{}", t.file, t.line)
            };
            let text = if row.call.contains(last) {
                loc
            } else {
                format!("{name} {loc}")
            };
            if row.undecided {
                marked(&text, t.tier)
            } else {
                text
            }
        })
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("\u{2192} {}", parts.join(", "))
    }
}

/// Call rows with their conditions: a condition shared by consecutive rows is printed once
/// above them (`if c:`) and the rows indented; the remaining conditions of a row follow it
/// (`if a && b`), then its targets.
pub(super) fn call_lines(rows: &[CallRow]) -> Vec<String> {
    let rows = &rows[..rows.len().min(CALLS_CAP)];
    let mut layout: Vec<CallLine<'_>> = Vec::new();
    let mut stack: Vec<&str> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let conds = &row.when;
        while stack.len() > conds.len() || stack.iter().zip(conds).any(|(a, b)| *a != b.as_str()) {
            stack.pop();
        }
        loop {
            let k = stack.len();
            if conds.len() <= k {
                break;
            }
            let shared = rows
                .get(i + 1)
                .is_some_and(|n| n.when.len() > k && n.when[..=k] == conds[..=k]);
            if !shared {
                break;
            }
            layout.push(CallLine::Group {
                indent: k,
                condition: conds[k].as_str(),
            });
            stack.push(conds[k].as_str());
        }
        layout.push(CallLine::Call {
            indent: stack.len(),
            row,
            inline: conds[stack.len()..].join(" && "),
        });
    }
    let label = |row: &CallRow| {
        let tier = if row.undecided { "possible" } else { row.tier };
        marked(&row.at.line.to_string(), tier)
    };
    let label_width = rows.iter().map(|r| label(r).chars().count()).max().unwrap_or(0);
    let left = |indent: usize, row: &CallRow| {
        let kind = if row.kind == "call" {
            String::new()
        } else {
            format!("[{}] ", row.kind)
        };
        format!("{}{kind}{}", "  ".repeat(indent), code(&row.call))
    };
    let call_width = layout
        .iter()
        .filter_map(|l| match l {
            CallLine::Call { indent, row, .. } => Some(left(*indent, row).chars().count()),
            CallLine::Group { .. } => None,
        })
        .filter(|w| *w <= CALL_COLUMN)
        .max()
        .unwrap_or(0);
    let mut out = Vec::new();
    for l in &layout {
        match l {
            CallLine::Group { indent, condition } => {
                out.push(format!("  {:>label_width$}  {}if {condition}:", "", "  ".repeat(*indent)));
            }
            CallLine::Call { indent, row, inline } => {
                let mut right = Vec::new();
                if !inline.is_empty() {
                    right.push(format!("if {inline}"));
                }
                let targets = targets_text(row);
                if !targets.is_empty() {
                    right.push(targets);
                }
                let line = format!(
                    "  {:>label_width$}  {:<call_width$}  {}",
                    label(row),
                    left(*indent, row),
                    right.join("  ")
                );
                out.push(line.trim_end().to_string());
            }
        }
    }
    out
}

/// `then` lines: symbols reached below the direct calls, grouped by the symbol whose edge
/// reached them (`Reached::from`, the reaching parent one step closer to the start);
/// distance 2 only unless `deep` (a closing line counts what is hidden).
pub(super) fn then_lines(results: &[Reached], deep: bool) -> Vec<String> {
    let mut deeper: Vec<&Reached> = results
        .iter()
        .filter(|r| r.distance >= 2 && (deep || r.distance == 2))
        .collect();
    deeper.sort_by(|a, b| (a.distance, a.card.id.as_str()).cmp(&(b.distance, b.card.id.as_str())));
    let hidden = results.iter().filter(|r| r.distance > 2).count();
    let mut parents: Vec<(&str, Vec<String>)> = Vec::new();
    for r in deeper {
        let parent = r.from.as_deref().unwrap_or("?");
        let name = marked(short(&r.card.id), r.tier);
        match parents.iter_mut().find(|(p, _)| *p == parent) {
            Some((_, names)) => names.push(name),
            None => parents.push((parent, vec![name])),
        }
    }
    let mut out = Vec::new();
    for (i, (parent, names)) in parents.iter().take(THEN_CAP).enumerate() {
        let label = if i == 0 { "then  " } else { "      " };
        let shown = &names[..names.len().min(NAMES_CAP)];
        let mut line = format!("{label}{} \u{2192} {}", short(parent), shown.join(", "));
        if names.len() > NAMES_CAP {
            let _ = write!(line, " (+{})", names.len() - NAMES_CAP);
        }
        out.push(line);
    }
    if parents.len() > THEN_CAP {
        out.push(format!("      (+{} more)", parents.len() - THEN_CAP));
    }
    if !deep && hidden > 0 {
        out.push(format!("      (+{hidden} deeper \u{2192} --deep)"));
    }
    out
}

// ------------------------------------------------------------------ show

/// Calls of a `deps` / `context` symbol that count as unresolved: without a target, or with
/// an undecided dispatch (I-01).
pub(super) fn unresolved_calls(calls: &[CallRow]) -> usize {
    calls.iter().filter(|c| c.targets.is_empty() || c.undecided).count()
}

pub(super) fn deps_text(r: &DependenciesReport) -> String {
    let s = &r.symbol;
    let own_unresolved = unresolved_calls(&r.calls);
    let status = if !r.bounds.complete {
        "bounded".to_string()
    } else if own_unresolved > 0 {
        format!("{own_unresolved} unresolved")
    } else {
        "complete".to_string()
    };
    let mut out = vec![format!(
        "{}  {}:{}-{} \u{b7} {} \u{b7} {status}",
        short(&s.id),
        s.file,
        s.line,
        s.end_line,
        count(r.calls.len(), "call", "calls")
    )];
    out.extend(call_lines(&r.calls));
    if r.calls.len() > CALLS_CAP {
        out.push(format!("  (+{} more)", r.calls.len() - CALLS_CAP));
    }
    out.extend(then_lines(&r.results, r.deep));
    let below: Vec<Row> = r
        .unresolved_inside
        .iter()
        .filter(|u| u.owner.as_deref() != Some(s.id.as_str()))
        .map(|u| Row {
            file: u.at.file.clone(),
            prefix: prefix(u.at.line, u.kind, "?"),
            code: code(&u.text),
            suffix: String::new(),
        })
        .collect();
    if r.deep && !below.is_empty() {
        let mut tail = Rows::default();
        tail.section(Some("check:"), collapse(below), DEPS_CHECK_CAP);
        tail.render(&mut out);
    } else if !below.is_empty() {
        out.push(format!("      (+{} unresolved below \u{2192} --deep)", below.len()));
    }
    out.join("\n")
}

// ------------------------------------------------------------------ path
