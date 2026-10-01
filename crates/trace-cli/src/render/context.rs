//! `context` text: source, callers, calls, tests and next steps.

use std::fmt::Write as _;

use trace_analysis::report::ContextReport;

use super::{
    code, count, deps::call_lines, deps::unresolved_calls, marked, short, uses::test_lines, CALLS_CAP,
    CONTEXT_CALLERS, NAMES_CAP,
};

pub(super) fn context_text(r: &ContextReport) -> String {
    context_text_with_lines(r, false)
}

pub(super) fn context_text_with_lines(r: &ContextReport, numbered: bool) -> String {
    let s = &r.symbol;
    let own_unresolved = unresolved_calls(&r.calls);
    let status = if own_unresolved > 0 {
        format!("{own_unresolved} unresolved")
    } else {
        "complete".to_string()
    };
    let mut out = vec![format!(
        "context {}  {}:{}-{} \u{b7} {} \u{b7} {} \u{b7} {status}",
        short(&s.id),
        s.file,
        s.line,
        s.end_line,
        count(r.callers.len(), "caller", "callers"),
        count(r.calls.len(), "call", "calls")
    )];
    out.push(if numbered {
        super::numbered_source(&r.source, s.line)
    } else {
        r.source.trim_end_matches(['\n', '\r']).to_string()
    });
    if !r.callers.is_empty() {
        out.push("called by".to_string());
        for c in r.callers.iter().take(CONTEXT_CALLERS) {
            let loc = marked(&format!("{}:{}", c.at.file, c.at.line), c.tier);
            out.push(format!("  {loc}  {}", short(&c.caller)));
            let pad = " ".repeat(loc.chars().count() + 4);
            out.push(format!("{pad}{}", code(&c.text)));
            if !c.when.is_empty() {
                out.push(format!("{pad}when  {}", c.when.join(" \u{b7} ")));
            }
            if !c.carries.is_empty() {
                let pairs: Vec<String> = c
                    .carries
                    .iter()
                    .map(|p| format!("{} \u{2190} {}", p.parameter, p.argument))
                    .collect();
                out.push(format!("{pad}{}", pairs.join(" \u{b7} ")));
            }
        }
        if r.callers.len() > CONTEXT_CALLERS {
            out.push(format!(
                "  (+{} more \u{2192} uses {})",
                r.callers.len() - CONTEXT_CALLERS,
                short(&s.id)
            ));
        }
    }
    if !r.calls.is_empty() {
        out.push("calls".to_string());
        out.extend(call_lines(&r.calls));
        if r.calls.len() > CALLS_CAP {
            out.push(format!("  (+{} more)", r.calls.len() - CALLS_CAP));
        }
    }
    if r.deep && !r.below.is_empty() {
        let names: Vec<String> = r.below.iter().map(|b| marked(short(&b.card.id), b.tier)).collect();
        let shown = &names[..names.len().min(NAMES_CAP * 4)];
        let mut line = format!("below   {}", shown.join(", "));
        if names.len() > shown.len() {
            let _ = write!(line, " (+{})", names.len() - shown.len());
        }
        out.push(line);
    }
    if r.deep && !r.callers_of_callers.is_empty() {
        let names: Vec<&str> = r.callers_of_callers.iter().map(|c| short(c)).collect();
        let shown = &names[..names.len().min(NAMES_CAP * 4)];
        let mut line = format!("callers of callers  {}", shown.join(", "));
        if names.len() > shown.len() {
            let _ = write!(line, " (+{})", names.len() - shown.len());
        }
        out.push(line);
    }
    out.extend(test_lines(&r.tests, r.tests_total));
    if !r.next.is_empty() {
        out.push(format!("next    {}", r.next.join(" \u{b7} ")));
    }
    out.join("\n")
}

// ------------------------------------------------------------------ status
