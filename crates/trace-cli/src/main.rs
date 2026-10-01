//! `trace` binary entry point. Exit codes: 0 success (incl. "no path",
//! partial completeness); 2 usage errors (clap), unknown or ambiguous symbols, invalid
//! arguments; 3 setup errors (missing server / toolchain / dependencies / approval, failed
//! server or build), index, cache, lock, I/O and configuration errors.
//! Errors follow the approved style (DESIGN §1.3): `Error: ` + the first line, every further
//! line verbatim (they carry their own indentation). With `--json` errors are a JSON object
//! on stderr: `{"command": "...", "error": "<lines joined>", "error_type": "..."}` (plus
//! numbered `"candidates"` for ambiguous symbols, `"nearest"` for a line selector and
//! `"errors"` for a combined setup error). Output is always UTF-8.

mod app;
mod cli;
mod commands;
mod render;
mod safety;
mod watch;

use std::io::Write;
use std::process::ExitCode;

use clap::Parser;

/// Exit code of usage errors (clap).
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    // Usage errors, help and version are answered here.
    let cli = match cli::Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(err) => return usage_error(&args, err),
    };
    let command = cli.command.name();
    let json = cli.global.json;
    match app::run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let mut stderr = std::io::stderr().lock();
            let _ = if json {
                writeln!(stderr, "{}", app::error_json(command, &err))
            } else {
                writeln!(stderr, "Error: {}", app::error_text(&err))
            };
            ExitCode::from(app::exit_code(&err))
        }
    }
}

/// Help/version go to stdout with exit 0; argument errors exit 2 (JSON on stderr when
/// `--json` was requested).
fn usage_error(args: &[std::ffi::OsString], err: clap::Error) -> ExitCode {
    if err.exit_code() == 0 {
        let _ = err.print();
        return ExitCode::SUCCESS;
    }
    let (command, json) = cli::sniff_args(args);
    if json {
        let message = err
            .render()
            .to_string()
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("invalid arguments")
            .trim_start_matches("error: ")
            .to_string();
        let v = serde_json::json!({"command": command, "error": message, "error_type": "invalid_argument"});
        let _ = writeln!(std::io::stderr().lock(), "{v}");
    } else {
        let _ = err.print();
    }
    ExitCode::from(EXIT_USAGE)
}
