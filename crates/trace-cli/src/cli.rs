//! Command-line interface: exactly seven commands (`show`, `context`, `uses`, `deps`,
//! `path`, `index`, `status`), one query flag `--deep` (context, uses, deps, path: everything
//! in full, unconfirmed links included), the setup flags `--watch`, `--allow-build`,
//! `--env <path>` (index) and `--install`, `--yes` (status), and two hidden global options
//! for scripts (`--root`, `--json`). `TRACE_OFFLINE=1` turns off automatic installs. A flag a command would ignore is rejected (clap error, exit 2). `trace --help`
//! prints [`crate::commands::HELP`] verbatim; every other `about` / `long_about` / argument
//! help comes from [`crate::commands`].

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::commands::{arg_help, global_help, long_about, summary, HELP};

/// Trace what code uses, what it uses, and how it connects (read-only).
#[derive(Parser, Debug)]
#[command(
    name = "trace",
    version,
    propagate_version = true,
    max_term_width = 100,
    override_help = HELP
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalOpts,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Args, Debug, Clone, Default)]
pub struct GlobalOpts {
    #[arg(long, global = true, hide = true, value_name = "DIR", help = global_help("root"))]
    pub root: Option<PathBuf>,
    #[arg(long, global = true, hide = true, help = global_help("json"))]
    pub json: bool,
    /// Compatibility alias: numbered source and independent Show results are now default.
    #[arg(long, global = true, hide = true)]
    pub audit: bool,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    #[command(about = summary("search"), long_about = long_about("search"))]
    Search {
        #[arg(help = arg_help("search", "query"))]
        query: String,
        #[arg(long, help = arg_help("search", "file"))]
        file: Option<String>,
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=100), help = arg_help("search", "limit"))]
        limit: u32,
        #[arg(long, default_value_t = 0, help = arg_help("search", "offset"))]
        offset: u32,
    },
    #[command(about = summary("source"), long_about = long_about("source"))]
    Source {
        #[arg(help = arg_help("source", "file"))]
        file: String,
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..), help = arg_help("source", "start"))]
        start: u32,
        #[arg(long, default_value_t = 40, value_parser = clap::value_parser!(u32).range(1..=200), help = arg_help("source", "lines"))]
        lines: u32,
    },
    #[command(about = "Find indexed declarations without loading their source")]
    Symbols {
        #[arg(help = "Name, documentation or identifier search; omit to list declarations")]
        query: Option<String>,
        #[arg(long, help = "Restrict to paths containing this text")]
        file: Option<String>,
        #[arg(
            long,
            help = "Test code; auto search ranks Python case candidates ahead of helpers"
        )]
        tests: bool,
        #[arg(long, default_value = "auto", value_parser = ["auto", "name", "body"], help = "auto: identifier-first/test-aware; name: names only; body: indexed body identifiers only")]
        mode: String,
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=100))]
        limit: u32,
        #[arg(long, default_value_t = 0)]
        offset: u32,
    },
    #[command(about = summary("show"), long_about = long_about("show"))]
    Show {
        #[arg(required = true, num_args = 1.., value_name = "SYMBOL", help = arg_help("show", "symbols"))]
        symbols: Vec<String>,
    },
    #[command(about = summary("context"), long_about = long_about("context"))]
    Context {
        #[arg(help = arg_help("context", "symbol"))]
        symbol: String,
        #[arg(long, help = arg_help("context", "deep"))]
        deep: bool,
    },
    #[command(about = summary("uses"), long_about = long_about("uses"))]
    Uses {
        #[arg(help = arg_help("uses", "symbol"))]
        symbol: String,
        #[arg(long, help = arg_help("uses", "deep"))]
        deep: bool,
    },
    #[command(about = summary("deps"), long_about = long_about("deps"))]
    Deps {
        #[arg(help = arg_help("deps", "symbol"))]
        symbol: String,
        #[arg(long, help = arg_help("deps", "deep"))]
        deep: bool,
    },
    #[command(about = summary("path"), long_about = long_about("path"))]
    Path {
        #[arg(help = arg_help("path", "from"))]
        from: String,
        #[arg(help = arg_help("path", "to"))]
        to: String,
        #[arg(long, help = arg_help("path", "deep"))]
        deep: bool,
    },
    #[command(about = summary("index"), long_about = long_about("index"))]
    Index {
        #[arg(long, help = arg_help("index", "watch"))]
        watch: bool,
        #[arg(long, help = arg_help("index", "allow_build"))]
        allow_build: bool,
        #[arg(long, value_name = "PATH", help = arg_help("index", "env"))]
        env: Vec<std::path::PathBuf>,
    },
    #[command(about = summary("status"), long_about = long_about("status"))]
    Status {
        #[arg(long, value_name = "LANG", help = arg_help("status", "install"))]
        install: Option<String>,
        #[arg(long, requires = "install", help = arg_help("status", "yes"))]
        yes: bool,
    },
}

impl Command {
    /// Command name used in outputs and errors.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Search { .. } => "search",
            Command::Source { .. } => "source",
            Command::Symbols { .. } => "symbols",
            Command::Show { .. } => "show",
            Command::Context { .. } => "context",
            Command::Uses { .. } => "uses",
            Command::Deps { .. } => "deps",
            Command::Path { .. } => "path",
            Command::Index { .. } => "index",
            Command::Status { .. } => "status",
        }
    }

    /// `--deep`: everything in full, unconfirmed (tier `possible`) links included (commands
    /// without the flag: false).
    pub fn deep(&self) -> bool {
        match self {
            Command::Context { deep, .. }
            | Command::Uses { deep, .. }
            | Command::Deps { deep, .. }
            | Command::Path { deep, .. } => *deep,
            Command::Show { .. }
            | Command::Symbols { .. }
            | Command::Search { .. }
            | Command::Source { .. }
            | Command::Index { .. }
            | Command::Status { .. } => false,
        }
    }
}

/// The command names, for reporting argument errors before clap has produced a `Cli`.
pub const NAMES: [&str; 10] = [
    "show", "context", "uses", "deps", "path", "index", "status", "symbols", "search", "source",
];

/// Best-effort command name and `--json` from raw arguments, for reporting argument errors
/// before clap has produced a `Cli` (the JSON error shape needs both).
pub fn sniff_args(args: &[std::ffi::OsString]) -> (&'static str, bool) {
    let mut command = "trace";
    let mut json = false;
    let mut iter = args.iter().skip(1).filter_map(|a| a.to_str());
    while let Some(arg) = iter.next() {
        if arg == "--json" {
            json = true;
        } else if arg == "--root" {
            iter.next(); // the directory, never a command name
        } else if command == "trace" {
            if let Some(name) = NAMES.iter().find(|n| **n == arg) {
                command = name;
            }
        }
    }
    (command, json)
}

#[cfg(test)]
#[path = "../tests/unit/cli.rs"]
mod tests;
