//! `path` text: hops with their calls, conditions, carried values and language changes.

use std::collections::HashSet;
use std::fmt::Write as _;

use trace_analysis::report::PathReport;

use super::{code, count, marked, short, CARRIES_SHOWN};

pub(super) fn path_text(r: &PathReport) -> String {
    let word = if r.bounds.complete { "complete" } else { "bounded" };
    let (from, to) = (short(&r.from.id), short(&r.to.id));
    let paths: Vec<_> = r.paths.iter().filter(|_| r.found).collect();
    if paths.is_empty() {
        // Undecided sites that can lead to the target: the path may exist (I-01).
        let status = if r.possible_path {
            "a possible path exists (trace path --deep)".to_string()
        } else if r.bounds.complete && r.unresolved > 0 {
            format!("{} unresolved", r.unresolved)
        } else {
            word.to_string()
        };
        return format!("{from} \u{2192} {to}  no path  {status}");
    }
    let mut out = Vec::new();
    if paths.len() == 1 {
        out.push(format!(
            "{from} \u{2192} {to}  {} \u{b7} {word}",
            count(paths[0].edges.len(), "hop", "hops")
        ));
    } else {
        out.push(format!("{from} \u{2192} {to}  {} paths \u{b7} {word}", paths.len()));
    }
    for (n, p) in paths.iter().enumerate() {
        if paths.len() > 1 {
            out.push(format!("path {} \u{b7} {}", n + 1, count(p.edges.len(), "hop", "hops")));
        }
        let locs: Vec<String> = p
            .edges
            .iter()
            .map(|e| marked(&format!("{}:{}", e.at.file, e.at.line), e.tier))
            .collect();
        let width = locs.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let pad = " ".repeat(width + 2);
        let langs = &p.languages;
        let mut carried: HashSet<&str> = HashSet::new();
        for (i, (e, loc)) in p.edges.iter().zip(&locs).enumerate() {
            if let Some(b) = &e.bridge {
                out.push(format!(
                    "{loc:<width$}  {}   \u{21e2} {} {} \u{2192} {}",
                    code(&e.text),
                    b.kind,
                    b.label,
                    e.to
                ));
                carried.clear();
                continue;
            }
            let site = e.site.as_ref();
            let call = site
                .map(|s| s.call.as_str())
                .filter(|c| !c.is_empty())
                .unwrap_or(e.text.as_str());
            let mut line = format!("{loc:<width$}  {}", code(call));
            if let Some(b) = langs.get(i + 1).filter(|&b| langs.get(i) != Some(b)) {
                let _ = write!(line, " ({})", b.as_str());
            }
            out.push(line);
            let Some(site) = site else { continue };
            if !site.when.is_empty() {
                out.push(format!("{pad}if {}", site.when.join(" && ")));
            }
            let shown: Vec<_> = site
                .carries
                .iter()
                .filter(|c| c.argument != c.parameter || carried.contains(c.argument.as_str()))
                .collect();
            if !shown.is_empty() {
                let pairs: Vec<String> = shown
                    .iter()
                    .take(CARRIES_SHOWN)
                    .map(|c| format!("{} \u{2192} {}", c.argument, c.parameter))
                    .collect();
                let mut line = format!("{pad}carries  {}", pairs.join(" \u{b7} "));
                if shown.len() > CARRIES_SHOWN {
                    let _ = write!(line, " (+{})", shown.len() - CARRIES_SHOWN);
                }
                out.push(line);
            }
            carried = shown.iter().map(|c| c.parameter.as_str()).collect();
        }
    }
    out.join("\n")
}

// ------------------------------------------------------------------ context
