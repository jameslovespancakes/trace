//! `show` text: the exact source of each symbol.

use trace_analysis::report::ShowReport;

use super::count;

pub(super) fn show_text(r: &ShowReport) -> String {
    let mut blocks = Vec::new();
    for item in &r.symbols {
        let s = &item.symbol;
        let mut block = format!(
            "{}:{}-{}  {}  {} \u{b7} {} \u{b7} {}\n",
            s.file,
            s.line,
            s.end_line,
            s.qualified_name,
            s.kind,
            count(item.callers, "caller", "callers"),
            count(item.calls, "call", "calls")
        );
        block.push_str(item.source.trim_end_matches(['\n', '\r']));
        blocks.push(block);
    }
    blocks.join("\n\n")
}

// ------------------------------------------------------------------ uses
