//! Command dispatch: resolve the root, open the workspace, run the analysis, render, print.
//!
//! stdout carries only the rendered result; notices go to stderr (lock fallback, the install
//! record of `status --install`, watch lines). Every error propagates to
//! `main`, which prints it (text or JSON) and exits with [`exit_code`]: 2 for usage errors,
//! unknown and ambiguous symbols, 3 for index, cache, lock, I/O, backend and configuration
//! errors. After a query command the workspace is flushed (cache statistics).
//!
//! `--deep` opens the workspace with tier `possible`, otherwise `inferred`;
//! `TRACE_OFFLINE=1` ([`offline`]) disables the automatic install of default-language
//! servers. Traversal depth is fixed at [`trace_analysis::DEPTH`].
//!
//! Error texts (DESIGN §1.3): the library errors carry the approved texts; this module adds
//! the numbered candidates of an ambiguous selector and joins multi-line texts into one line
//! for JSON.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use trace_analysis::pipeline::{self, IndexMode, IndexProgress, StderrProgress};
use trace_analysis::report::CandidateRef;
use trace_analysis::{AnalysisError, OpenOptions, Workspace};
use trace_core::{CoreError, Tier};

use crate::cli::{Cli, Command, GlobalOpts};
use crate::render::Output;
use crate::safety::{self, CliError};

/// Exit code of a failed command from its `error_type`.
pub fn exit_code_for(kind: &str) -> u8 {
    match kind {
        "symbol_not_found"
        | "ambiguous_symbol"
        | "invalid_argument"
        | "invalid_root"
        | "outside_root"
        | "invalid_relative_path"
        | "invalid_bounds"
        | "invalid_position" => 2,
        _ => 3,
    }
}

/// Exit code of a failed command.
pub fn exit_code(err: &anyhow::Error) -> u8 {
    exit_code_for(error_kind(err))
}

/// Run one command. Output goes to stdout; notices to stderr.
pub fn run(cli: Cli) -> anyhow::Result<()> {
    let global = &cli.global;
    let root = resolve_root(global)?;
    validate(&cli.command)?;
    let deep = cli.command.deep();
    match cli.command {
        Command::Show { .. }
        | Command::Symbols { .. }
        | Command::Search { .. }
        | Command::Source { .. }
        | Command::Context { .. }
        | Command::Uses { .. }
        | Command::Deps { .. }
        | Command::Path { .. } => answer(&root, global, deep, &cli.command),
        Command::Index {
            watch,
            allow_build,
            env,
        } => {
            let opts = OpenOptions {
                allow_build,
                env,
                ..options(global, false, Mode::ReadOnly)
            };
            if watch {
                // The approval / environments are remembered before watching (same as the
                // one-shot index); opening the workspace read-only saves them.
                drop(open_with(&root, &opts)?);
                return crate::watch::run(global, &root);
            }
            let mut ws = open_with(&root, &opts)?;
            let report = {
                let mut progress = progress_for(global.json);
                ws.reindex(IndexMode::Incremental, progress.as_mut())?
            };
            flush(&mut ws);
            emit(global.json, &Output::Index(report))
        }
        Command::Status { install, yes } => {
            // `--install <lang>` is the consent: the pinned record is shown, then installed.
            let installed = match install {
                Some(language) => Some(trace_analysis::install::run(&root, &language, yes, global.json)?),
                None => None,
            };
            let ws = open_workspace(&root, global, false, Mode::ReadOnly)?;
            let mut report = trace_analysis::status::status(&ws)?;
            report.install = installed;
            emit(global.json, &Output::Status(Box::new(report)))
        }
    }
}

/// The analysis behind a query command (`show`, `context`, `uses`, `deps`, `path`) on an
/// open workspace; `None` for the other commands.
pub fn query_output(ws: &mut Workspace, command: &Command) -> Option<Result<Output, AnalysisError>> {
    Some(match command {
        Command::Show { symbols } => trace_analysis::queries::show::show(ws, symbols).map(Output::Show),
        Command::Context { symbol, deep } => {
            trace_analysis::queries::context::context(ws, symbol, *deep).map(|r| Output::Context(Box::new(r)))
        }
        Command::Uses { symbol, deep } => {
            trace_analysis::queries::uses::uses(ws, symbol, *deep).map(|r| Output::Uses(Box::new(r)))
        }
        Command::Deps { symbol, deep } => {
            trace_analysis::queries::deps::dependencies(ws, symbol, trace_analysis::DEPTH, *deep)
                .map(Output::Deps)
        }
        Command::Path { from, to, deep } => {
            trace_analysis::queries::path::path(ws, from, to, trace_analysis::DEPTH, *deep).map(Output::Path)
        }
        Command::Index { .. }
        | Command::Status { .. }
        | Command::Symbols { .. }
        | Command::Search { .. }
        | Command::Source { .. } => return None,
    })
}

/// Open a query workspace (index kept fresh), answer the query command ([`query_output`]),
/// flush, print.
fn answer(root: &Path, global: &GlobalOpts, deep: bool, command: &Command) -> anyhow::Result<()> {
    let mut ws = open_workspace(root, global, deep, Mode::Query)?;
    if let Command::Symbols {
        query,
        file,
        tests,
        mode,
        limit,
        offset,
    } = command
    {
        use trace_analysis::queries::audit::DiscoveryMode;
        let mode = match mode.as_str() {
            "name" => DiscoveryMode::Name,
            "body" => DiscoveryMode::Body,
            _ => DiscoveryMode::Auto,
        };
        let report = trace_analysis::queries::audit::symbols_in_mode(
            &ws,
            query.as_deref(),
            file.as_deref(),
            *tests,
            mode,
            *limit as usize,
            *offset as usize,
        )?;
        flush(&mut ws);
        return write_stdout(&crate::render::discovery(&report, global.json));
    }
    if let Command::Search {
        query,
        file,
        limit,
        offset,
    } = command
    {
        let report = trace_analysis::queries::source::search(
            &ws,
            query,
            file.as_deref(),
            *limit as usize,
            *offset as usize,
        )?;
        flush(&mut ws);
        return write_stdout(&crate::render::source_search(&report, global.json));
    }
    if let Command::Source { file, start, lines } = command {
        let report = trace_analysis::queries::source::source(&ws, file, *start, *lines)?;
        flush(&mut ws);
        return write_stdout(&crate::render::source_window(&report, global.json));
    }
    {
        if let Command::Show { symbols } = command {
            let report = trace_analysis::queries::audit::show(&mut ws, symbols)?;
            flush(&mut ws);
            write_stdout(&crate::render::audit_show(&report, global.json))?;
            if report.symbols.is_empty() {
                return Err(CliError::InvalidArgument(
                    "No selector resolved; see per-selector errors.".into(),
                )
                .into());
            }
            return Ok(());
        }
    }
    let output = match query_output(&mut ws, command) {
        Some(result) => result?,
        None => anyhow::bail!("{} is not a query command", command.name()),
    };
    flush(&mut ws);
    if !global.json {
        write_stdout(&output.audit_text())
    } else {
        emit(global.json, &output)
    }
}

/// Persist the cache statistics counted in memory (module docs).
pub fn flush(ws: &mut Workspace) {
    ws.flush();
}

/// Reject empty symbol / question / language arguments before any index work.
pub fn validate(command: &Command) -> Result<(), CliError> {
    let empty = |what: &str, value: &str| {
        if value.trim().is_empty() {
            Err(CliError::InvalidArgument(format!("Give a {what}; it must not be empty.")))
        } else {
            Ok(())
        }
    };
    match command {
        Command::Show { symbols } => symbols.iter().try_for_each(|s| empty("symbol", s)),
        Command::Source { file, .. } => empty("file", file),
        Command::Search { query, file, .. } => {
            empty("literal query", query)?;
            if let Some(f) = file {
                empty("file filter", f)?;
            }
            Ok(())
        }
        Command::Symbols { query, file, .. } => {
            if let Some(q) = query {
                empty("query", q)?;
            }
            if let Some(f) = file {
                empty("file filter", f)?;
            }
            Ok(())
        }
        Command::Context { symbol, .. } | Command::Uses { symbol, .. } | Command::Deps { symbol, .. } => {
            empty("symbol", symbol)
        }
        Command::Path { from, to, .. } => empty("from", from).and_then(|_| empty("to", to)),
        Command::Status {
            install: Some(language),
            ..
        } => empty("language", language),
        Command::Status { install: None, .. } | Command::Index { .. } => Ok(()),
    }
}

/// `--root` (or the current directory), canonicalized and admitted by the safety policy.
pub fn resolve_root(global: &GlobalOpts) -> anyhow::Result<PathBuf> {
    let raw = match &global.root {
        Some(root) => root.clone(),
        None => std::env::current_dir()?,
    };
    let canonical = trace_core::inventory::canonical_root(&raw)?;
    safety::ensure_allowed(&canonical)?;
    Ok(canonical)
}

/// How a command uses its workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// No automatic update (`status`, explicit `index`).
    ReadOnly,
    /// One-shot query: keep the index fresh.
    Query,
}

/// `TRACE_OFFLINE` set to anything but empty / `0`: no automatic installs.
pub fn offline() -> bool {
    trace_core::env::offline()
}

/// Evidence tier a command lists / traverses: `possible` with `--deep`, else `inferred`.
pub fn include(deep: bool) -> Tier {
    if deep {
        Tier::Possible
    } else {
        Tier::Inferred
    }
}

/// Open the workspace. Query opens keep the index fresh; when another `trace` process
/// holds the build lock, fall back to the existing index read-only (stderr notice).
pub fn open_workspace(
    root: &Path,
    global: &GlobalOpts,
    deep: bool,
    mode: Mode,
) -> Result<Workspace, AnalysisError> {
    open_with(root, &options(global, deep, mode))
}

/// The workspace options of a command.
fn options(global: &GlobalOpts, deep: bool, mode: Mode) -> OpenOptions {
    let offline = offline();
    OpenOptions {
        include: include(deep),
        read_only: mode != Mode::Query,
        progress: !global.json && io::stderr().is_terminal(),
        persistent: false,
        offline,
        no_bridges: false,
        allow_build: false,
        env: Vec::new(),
    }
}

/// [`open_workspace`] with explicit options.
fn open_with(root: &Path, opts: &OpenOptions) -> Result<Workspace, AnalysisError> {
    match Workspace::open(root, opts) {
        Err(AnalysisError::Core(CoreError::Locked(_))) if !opts.read_only => {
            eprintln!(
                "Note: another trace process is updating this index; this answer uses the index as it is."
            );
            let ws = Workspace::open(
                root,
                &OpenOptions {
                    read_only: true,
                    ..opts.clone()
                },
            )?;
            ws.index()?;
            Ok(ws)
        }
        other => other,
    }
}

/// Render and print a result on stdout.
pub fn emit(json: bool, output: &Output) -> anyhow::Result<()> {
    write_stdout(&output.render(json))
}

/// Write `text` (plus a final newline) to stdout. A closed pipe is not an error.
pub fn write_stdout(text: &str) -> anyhow::Result<()> {
    let mut out = io::stdout().lock();
    let result = out
        .write_all(text.as_bytes())
        .and_then(|_| {
            if text.ends_with('\n') {
                Ok(())
            } else {
                out.write_all(b"\n")
            }
        })
        .and_then(|_| out.flush());
    match result {
        Err(e) if e.kind() != io::ErrorKind::BrokenPipe => Err(e.into()),
        _ => Ok(()),
    }
}

/// Progress reporter for `trace index`: a line rewritten in place on stderr, only in text
/// mode on a terminal (redirected output stays clean).
pub fn progress_for(json: bool) -> Box<dyn IndexProgress> {
    if !json && io::stderr().is_terminal() {
        Box::new(StderrProgress::new(true))
    } else {
        Box::new(pipeline::Quiet)
    }
}

/// Numbered candidates of an ambiguous selector anywhere in the chain (a bare core
/// ambiguity, without index details, gets numbers and ids only).
fn ambiguous(err: &anyhow::Error) -> Option<(String, Vec<CandidateRef>)> {
    err.chain().find_map(|cause| {
        if let Some(AnalysisError::Ambiguous {
            reference,
            candidates,
        }) = cause.downcast_ref::<AnalysisError>()
        {
            return Some((reference.clone(), candidates.clone()));
        }
        let core = cause
            .downcast_ref::<AnalysisError>()
            .and_then(|e| match e {
                AnalysisError::Core(c) => Some(c),
                _ => None,
            })
            .or_else(|| cause.downcast_ref::<CoreError>());
        match core {
            Some(CoreError::AmbiguousSymbol {
                reference,
                candidates,
            }) => Some((
                reference.clone(),
                candidates
                    .iter()
                    .enumerate()
                    .map(|(i, id)| CandidateRef {
                        n: i + 1,
                        id: id.clone(),
                        kind: "symbol",
                        file: id.split_once(':').map(|(f, _)| f.to_string()).unwrap_or_default(),
                        line: 0,
                    })
                    .collect(),
            )),
            _ => None,
        }
    })
}

/// Nearest named symbols of a `file:line` selector that hit no named symbol (SPEC 9.4).
fn nearest(err: &anyhow::Error) -> Option<Vec<String>> {
    err.chain().find_map(|cause| {
        let core = cause
            .downcast_ref::<AnalysisError>()
            .and_then(|e| match e {
                AnalysisError::Core(c) => Some(c),
                _ => None,
            })
            .or_else(|| cause.downcast_ref::<CoreError>());
        match core {
            Some(CoreError::NoNamedSymbolAt { nearest, .. }) => Some(nearest.clone()),
            _ => None,
        }
    })
}

/// `error_type` for an error (core kinds, analysis kinds, else `error`).
pub fn error_kind(err: &anyhow::Error) -> &'static str {
    if let Some(e) = setup_error(err) {
        return e.kind();
    }
    for cause in err.chain() {
        if let Some(e) = cause.downcast_ref::<AnalysisError>() {
            return e.kind();
        }
        if let Some(e) = cause.downcast_ref::<CoreError>() {
            return e.kind();
        }
        if let Some(e) = cause.downcast_ref::<CliError>() {
            return e.kind();
        }
        if cause.is::<trace_infer::InferError>() {
            return "infer";
        }
        if cause.is::<trace_semantic::SemanticError>() {
            return "semantic";
        }
        if cause.is::<trace_syntax::SyntaxError>() {
            return "syntax";
        }
        if cause.is::<io::Error>() {
            return "io";
        }
    }
    "error"
}

/// The JSON error object: `{"command", "error", "error_type"}` plus numbered
/// `"candidates"` for ambiguous symbols and `"nearest"` (named symbols) for a `file:line`
/// selector on module-level code.
/// The setup error of a failed command (no fallback: missing server / toolchain / ...).
fn setup_error(err: &anyhow::Error) -> Option<&trace_core::SetupError> {
    err.chain().find_map(|cause| {
        cause
            .downcast_ref::<trace_core::SetupError>()
            .or_else(|| match cause.downcast_ref::<AnalysisError>() {
                Some(AnalysisError::Setup(e)) => Some(e),
                Some(AnalysisError::Semantic(trace_semantic::SemanticError::Setup(e))) => Some(e),
                _ => None,
            })
            .or_else(|| match cause.downcast_ref::<trace_semantic::SemanticError>() {
                Some(trace_semantic::SemanticError::Setup(e)) => Some(e),
                _ => None,
            })
    })
}

/// A multi-line error text as one line: lines trimmed and joined with a space (JSON).
fn one_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn error_value(command: &str, err: &anyhow::Error) -> Value {
    let mut v =
        json!({"command": command, "error": one_line(&err.to_string()), "error_type": error_kind(err)});
    if let Some(setup) = setup_error(err) {
        // One line (continuation lines trimmed and joined, DESIGN §1.3); a combined error
        // (PLAN decision 15) lists every item.
        v["error"] = json!(setup.lines().iter().map(|l| l.trim()).collect::<Vec<_>>().join(" "));
        if let trace_core::SetupError::Several { items } = setup {
            v["errors"] = json!(items
                .iter()
                .map(|i| json!({
                    "error_type": i.kind(),
                    "language": i.language(),
                    "error": i.item_line(),
                }))
                .collect::<Vec<_>>());
        }
    }
    if let Some((_, candidates)) = ambiguous(err) {
        v["candidates"] = json!(candidates);
    }
    if let Some(nearest) = nearest(err) {
        v["nearest"] = json!(nearest);
    }
    v
}

/// JSON error object for stderr.
pub fn error_json(command: &str, err: &anyhow::Error) -> String {
    error_value(command, err).to_string()
}

/// The error text after `Error: ` (DESIGN §1.3); ambiguous symbols list numbered candidates
/// on following lines (`  1. models/user.py:User.save`); every other error is its own text
/// (continuation lines carry their indentation).
pub fn error_text(err: &anyhow::Error) -> String {
    if let Some((reference, candidates)) = ambiguous(err) {
        let mut s = format!("\"{reference}\" matches {} symbols. Use one of:", candidates.len());
        for c in candidates {
            s.push_str(&format!("\n  {}. {}", c.n, c.id));
        }
        return s;
    }
    err.to_string()
}

#[cfg(test)]
#[path = "../tests/unit/app.rs"]
mod tests;
