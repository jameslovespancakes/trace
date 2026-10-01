//! `index_diff <cache dir A> <cache dir B> [--json]`: load two persisted indexes of the same
//! repository (repository cache directories holding `index.bin` + `meta.json`) and print
//! their differences (`trace_analysis::equivalence::compare`); exit 1 when any. Used by the
//! verify stage (incremental == full rebuild); not a CLI command.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use trace_core::cache::{load_index, RepoMeta};
use trace_core::Index;

fn load(dir: &Path) -> Result<Index, String> {
    let meta = RepoMeta::load(&dir.join("meta.json"))
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .ok_or_else(|| format!("{}: no meta.json", dir.display()))?;
    load_index(&dir.join("index.bin"), &meta.root).map_err(|e| format!("{}: {e}", dir.display()))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    let dirs: Vec<PathBuf> = args.iter().filter(|a| *a != "--json").map(PathBuf::from).collect();
    if dirs.len() != 2 {
        eprintln!("usage: index_diff <cache dir A> <cache dir B> [--json]");
        return ExitCode::from(2);
    }
    let (a, b) = match (load(&dirs[0]), load(&dirs[1])) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("Error: {e}");
            return ExitCode::from(2);
        }
    };
    let differences = trace_analysis::equivalence::compare(&a, &b);
    if json {
        println!("{}", serde_json::to_string_pretty(&differences).unwrap_or_else(|_| "[]".into()));
    } else {
        for d in &differences {
            println!("{} {}: {} != {}", d.area, d.key, d.incremental, d.full);
        }
        println!("{} differences", differences.len());
    }
    if differences.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
