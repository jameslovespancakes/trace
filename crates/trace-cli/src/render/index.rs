//! `index` text and the `index --watch` update lines.

use trace_analysis::report::{IndexReport, LanguageRow};
use trace_core::SupportLevel;

use super::{count, status::language_text};

pub(super) fn index_text(r: &IndexReport) -> String {
    let total = r.seconds.total();
    let verb = match r.mode {
        "rebuild" => "rebuilt",
        "unchanged" => "up to date:",
        _ => "indexed",
    };
    let mut out = vec![format!(
        "{verb} {} (+{} ~{} -{}) \u{b7} {} \u{b7} {} proven / {} inferred / {} possible \u{b7} {} \u{b7} {total:.1}s",
        count(r.files, "file", "files"),
        r.added,
        r.changed,
        r.removed,
        count(r.symbols, "symbol", "symbols"),
        r.edges.proven,
        r.edges.inferred,
        r.edges.possible,
        count(r.sites, "site", "sites"),
    )];
    let partial: Vec<&LanguageRow> = r
        .languages
        .iter()
        .filter(|l| l.support == SupportLevel::Pending)
        .collect();
    let w = partial
        .iter()
        .map(|l| l.language.as_str().chars().count())
        .max()
        .unwrap_or(0);
    for l in partial {
        out.push(format!("  {:<w$}  {}", l.language.as_str(), language_text(l.support, &l.reason, l.files)));
    }
    if r.outside_build_files > 0 {
        out.push(format!(
            "  {} outside the build on this computer",
            count(r.outside_build_files, "file", "files")
        ));
    }
    out.join("\n")
}

/// One-line watch update: `updated <n> files (+a ~c -r) in <s>s`.
pub fn watch_line(r: &IndexReport, seconds: f64) -> String {
    format!(
        "updated {} (+{} ~{} -{}) in {seconds:.1}s",
        count(r.added + r.changed + r.removed, "file", "files"),
        r.added,
        r.changed,
        r.removed
    )
}
