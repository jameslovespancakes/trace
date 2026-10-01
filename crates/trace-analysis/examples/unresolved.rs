//! Debug dump of the calls trace could not resolve (I-13; a maintainer tool, not a CLI
//! command):
//!
//! ```text
//! cargo run --release -p trace-analysis --example unresolved -- --root <repo> [--cache <dir>]
//!     [--language <name>] [--top <n>] [--json]
//! ```
//!
//! Reads the persisted index of `<repo>` (no update, no language server) from the cache home
//! (`--cache`, else `TRACE_CACHE_DIR` / the default) and prints per language the resolved
//! buckets (repository edge, library, by name, external, inferred) and every unresolved call
//! grouped by reason (`no_semantic_target`, `ambiguous`, `template_dependent`,
//! `inactive_code`, `no_answer`, ..., plus `pending` and `outside_build` for files not
//! analysed) and callee, with `path:line` of the first calls. Counts are complete; the lists
//! are cut at `--top` (default 20). The totals equal the `trace status` resolution numbers.

use std::path::Path;
use std::process::ExitCode;

use trace_analysis::status::{unresolved_dump, LanguageDump};
use trace_core::cache::load_index;
use trace_core::paths::RepoPaths;
use trace_core::Language;

const USAGE: &str =
    "usage: unresolved --root <repo> [--cache <dir>] [--language <name>] [--top <n>] [--json]";

/// The value after `flag`.
fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

/// A language by its name (`python`, `cpp`, `tsx`, ...) or install alias (`c++`, `ts`, ...).
fn language(name: &str) -> Option<Language> {
    Language::ALL
        .iter()
        .copied()
        .find(|l| l.as_str().eq_ignore_ascii_case(name))
        .or_else(|| Language::from_install_arg(name))
}

fn print_text(dump: &[LanguageDump]) {
    for d in dump {
        let name = d.language.map_or("?", Language::as_str);
        println!(
            "{name}: {} calls \u{b7} {} repository \u{b7} {} library \u{b7} {} by name \u{b7} {} external \u{b7} {} inferred \u{b7} {} unresolved \u{b7} {} pending \u{b7} {} outside the build",
            d.calls,
            d.repository,
            d.library,
            d.by_name,
            d.external,
            d.inferred,
            d.unresolved,
            d.pending,
            d.outside_build
        );
        for g in &d.groups {
            println!("  {} ({})", g.reason, g.calls);
            for c in &g.callees {
                println!("    {} x{}  {}", c.callee, c.calls, c.at.join(", "));
            }
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(root) = value(&args, "--root") else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let only = match value(&args, "--language") {
        Some(name) => match language(name) {
            Some(l) => Some(l),
            None => {
                eprintln!("Error: Unknown language: {name}");
                return ExitCode::from(2);
            }
        },
        None => None,
    };
    let top = match value(&args, "--top").map(str::parse::<usize>) {
        None => 20,
        Some(Ok(n)) => n,
        Some(Err(_)) => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let paths = match value(&args, "--cache") {
        Some(home) => RepoPaths::resolve_in(Path::new(root), Path::new(home)),
        None => RepoPaths::resolve(Path::new(root)),
    };
    let paths = match paths {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::from(2);
        }
    };
    let index = match load_index(&paths.index_file, &paths.root_display()) {
        Ok(index) => index,
        Err(e) => {
            eprintln!("Error: No usable index for {root} ({e}). Run: trace index");
            return ExitCode::from(2);
        }
    };
    let dump = unresolved_dump(&index, only, top);
    if args.iter().any(|a| a == "--json") {
        match serde_json::to_string_pretty(&dump) {
            Ok(text) => println!("{text}"),
            Err(e) => {
                eprintln!("Error: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        print_text(&dump);
    }
    ExitCode::SUCCESS
}
