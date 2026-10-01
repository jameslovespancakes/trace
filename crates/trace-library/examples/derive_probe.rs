//! Derivation probe (diagnosis): derive one library file and print the summaries of the
//! functions whose qualified name contains a filter.
//!
//! ```text
//! derive_probe --language <language> --file <library file> [--root <dir>]... [--only <text>]
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use trace_core::Language;
use trace_library::derive::{derive_with, DeriveContext, FsLoader, ParsedFiles, SourceLoader};
use trace_library::table::Tables;

fn main() -> ExitCode {
    let mut language = None;
    let mut file = None;
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut only = String::new();
    let mut decls = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--language" => language = it.next().and_then(|l| l.parse::<Language>().ok()),
            "--file" => file = it.next().map(PathBuf::from),
            "--root" => roots.extend(it.next().map(PathBuf::from)),
            "--only" => only = it.next().unwrap_or_default(),
            "--decls" => decls = true,
            _ => {
                eprintln!("Error: Unknown argument {a}");
                return ExitCode::from(2);
            }
        }
    }
    let (Some(language), Some(file)) = (language, file) else {
        eprintln!(
            "Usage: derive_probe --language <language> --file <file> [--root <dir>]... [--only <text>]"
        );
        return ExitCode::from(2);
    };
    let tables = Tables::builtin();
    let bytes = FsLoader.read(&file).unwrap_or_default();
    if decls {
        let path = file.to_string_lossy().into_owned();
        match trace_syntax::extract(trace_syntax::SourceInput {
            path: &path,
            language,
            source: &bytes,
        }) {
            Ok(facts) => {
                println!("{} declarations, {} syntax errors", facts.declarations.len(), facts.error_count);
                for d in &facts.declarations {
                    println!("  {:?} {} line {}", d.kind, d.qualified_name, d.span.start_line);
                }
            }
            Err(e) => println!("extract failed: {e}"),
        }
        return ExitCode::SUCCESS;
    }
    let cx = DeriveContext {
        leaves: &tables,
        loader: &FsLoader,
        parsed: &ParsedFiles::default(),
        roots: &roots,
        limits: &trace_core::config::current().derive,
    };
    let started = std::time::Instant::now();
    let summaries = derive_with(language, &file, &bytes, &cx);
    println!("derived {} functions in {:.2?}", summaries.functions.len(), started.elapsed());
    for f in summaries.functions.values() {
        if !only.is_empty() && !f.qualified.contains(&only) && !f.symbol.contains(&only) {
            continue;
        }
        println!("{} ({}) line {}", f.qualified, f.symbol, f.line + 1);
        for (sel, effects) in &f.params {
            println!("    {sel:?}: {effects:?}");
        }
        for e in &f.effects {
            println!("    fn: {e:?}");
        }
    }
    ExitCode::SUCCESS
}
