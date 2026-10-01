//! `trace status --install <language|default|all> [--yes]` (DESIGN §1.9; owner install).
//!
//! * `<language>`: an install id or alias (`Language::from_install_arg`); unknown ->
//!   `InstallFailure::UnknownLanguage` (lists `default`, `all` and every id).
//! * `default`: the ten default languages (PLAN decision 10, `DEFAULT_LANGUAGES`), repository
//!   independent: extras that need a repository run on the next automatic install inside one,
//!   and a server that needs a missing toolchain (gopls without Go) is skipped with a note.
//! * `all`: every product language of the repository (quick inventory; no index needed).
//!
//! Servers + their trace-managed runtimes + install extras are installed with
//! `trace_semantic::install::install` into the tools folder only, one progress line per tool
//! on stderr (text mode). Licence gates (PLAN decision 11): `--yes` accepts; on a terminal the
//! question of DESIGN §1.2 is asked; otherwise the install is refused with the
//! `LicenceNotAccepted` error. Always in-process.

use std::fmt::Write as _;
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use serde::Serialize;
use trace_core::config::semantic_tools_dir;
use trace_core::setup_error::{InstallFailure, SetupError};
use trace_core::{Language, DEFAULT_LANGUAGES, TRACE_VERSION};
use trace_env::os::{EnvVars, Platform};
use trace_semantic::install::{
    self, InstallProgress, InstallRequest, InstalledTool, LicenceAnswer, ProgressPrinter,
};
use trace_semantic::registry::{InstallSpec, LicenceGate, Registry};
use trace_semantic::ToolEnv;

use crate::workspace::{OpenOptions, Workspace};
use crate::{AnalysisError, Result};

/// What `status --install` did.
#[derive(Clone, Debug, Serialize)]
pub struct InstallReport {
    pub command: &'static str,
    pub trace_version: &'static str,
    /// The argument as given ("default", "all", "scala").
    pub request: String,
    /// The languages it covered, in install order.
    pub languages: Vec<Language>,
    pub tools_dir: String,
    /// Tools installed by this run.
    pub installed: Vec<InstalledTool>,
    /// Tools that were installed before (nothing downloaded).
    pub already: Vec<InstalledTool>,
    /// "<product> <version>" of every licence accepted by this run.
    pub licence_accepted: Vec<String>,
    /// Skipped items with their reason (`default` without the toolchain a server needs).
    pub notes: Vec<String>,
}

/// The parsed `--install` argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InstallTarget {
    Default,
    All,
    One(Language),
}

/// Parse `default`, `all` or a language install id / alias (case-insensitive).
pub(crate) fn parse_target(arg: &str) -> std::result::Result<InstallTarget, SetupError> {
    let trimmed = arg.trim();
    if trimmed.eq_ignore_ascii_case("default") {
        return Ok(InstallTarget::Default);
    }
    if trimmed.eq_ignore_ascii_case("all") {
        return Ok(InstallTarget::All);
    }
    Language::from_install_arg(trimmed)
        .filter(|l| l.is_code())
        .map(InstallTarget::One)
        .ok_or_else(|| SetupError::Install {
            language: None,
            failure: InstallFailure::UnknownLanguage { arg: arg.to_string() },
        })
}

/// Install what `arg` names (see the module docs).
pub fn run(root: &Path, arg: &str, yes: bool, json: bool) -> Result<InstallReport> {
    let target = parse_target(arg)?;
    let ws = Workspace::open(
        root,
        &OpenOptions {
            include: trace_core::model::Tier::Inferred,
            read_only: true,
            progress: false,
            persistent: false,
            offline: true,
            no_bridges: false,
            allow_build: false,
            env: Vec::new(),
        },
    )?;
    let languages = match &target {
        InstallTarget::Default => DEFAULT_LANGUAGES.to_vec(),
        InstallTarget::One(l) => vec![*l],
        InstallTarget::All => product_languages(&ws)?,
    };
    let tools = ToolEnv::discover(&ws.config, &ws.paths.home, &ws.paths.root)?;
    let tools_dir = semantic_tools_dir(&ws.config);
    let platform = Platform::current();
    let vars = EnvVars::from_process();
    let repo_root = (target != InstallTarget::Default).then_some(ws.paths.root.as_path());
    let interactive = !json && std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let mut printer = ProgressPrinter::stderr();
    let mut progress = |p: &InstallProgress| {
        if !json {
            printer.event(p);
        }
    };
    let mut ask = |spec: &InstallSpec, gate: &LicenceGate| ask_licence(spec, gate);
    let mut report = InstallReport {
        command: "install",
        trace_version: TRACE_VERSION,
        request: arg.trim().to_string(),
        languages: languages.clone(),
        tools_dir: tools_dir.to_string_lossy().into_owned(),
        installed: Vec::new(),
        already: Vec::new(),
        licence_accepted: Vec::new(),
        notes: Vec::new(),
    };
    let mut errors: Vec<SetupError> = Vec::new();
    for language in &languages {
        let licences = if yes {
            LicenceAnswer::Accepted
        } else if interactive {
            LicenceAnswer::Ask(&mut ask)
        } else {
            LicenceAnswer::Refuse
        };
        let result = install::install(InstallRequest {
            tools_dir: &tools_dir,
            registry: &tools.registry,
            languages: std::slice::from_ref(language),
            repo_root,
            platform: &platform,
            vars: &vars,
            progress: &mut progress,
            licences,
        });
        match result {
            Ok(done) => record(&mut report, &tools.registry, done),
            Err(e) if target == InstallTarget::Default && needs_toolchain(&e) => {
                report.notes.push(format!("Skipped: {}", e.lines().join(" ")));
            }
            Err(e) => errors.push(e),
        }
    }
    if errors.is_empty() {
        Ok(report)
    } else {
        Err(AnalysisError::Setup(SetupError::combine(errors)))
    }
}

/// A server that needs a toolchain the user does not have (not an error for `default`).
fn needs_toolchain(e: &SetupError) -> bool {
    matches!(
        e,
        SetupError::Install {
            failure: InstallFailure::NeedsToolchain { .. },
            ..
        }
    )
}

/// Add the tools of one install to the report (each tool once).
fn record(report: &mut InstallReport, registry: &Registry, done: Vec<InstalledTool>) {
    for tool in done {
        let seen = report
            .installed
            .iter()
            .chain(&report.already)
            .any(|t| t.id == tool.id);
        if seen {
            continue;
        }
        if !tool.already {
            let gated = registry
                .backends
                .iter()
                .filter_map(|b| b.install.as_ref())
                .find(|s| s.id == tool.id && s.licence_gate.is_some());
            if let Some(spec) = gated {
                report
                    .licence_accepted
                    .push(format!("{} {}", spec.product, spec.version));
            }
            report.installed.push(tool);
        } else {
            report.already.push(tool);
        }
    }
}

/// Every code language with product (non-test) files in the repository, sorted.
fn product_languages(ws: &Workspace) -> Result<Vec<Language>> {
    let inv = trace_core::inventory::scan(&ws.paths.root, &ws.config.inventory)?;
    let configs: Vec<(String, Vec<u8>)> = inv
        .configs
        .iter()
        .filter_map(|c| std::fs::read(&c.abs).ok().map(|b| (c.path.clone(), b)))
        .collect();
    let refs: Vec<(&str, &[u8])> = configs.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
    let tests = trace_syntax::testing::TestConfig::from_files(&refs);
    let mut out: Vec<Language> = inv
        .sources
        .iter()
        .filter_map(|s| s.language.map(|l| (s, l)))
        .filter(|(_, l)| l.is_code())
        .filter(|(s, l)| !trace_syntax::is_test_path(&s.path, *l, &tests))
        .map(|(_, l)| l)
        .collect();
    out.sort();
    out.dedup();
    Ok(out)
}

/// The §1.2 licence question on stderr; the answer is read from stdin (`y` / `yes`).
fn ask_licence(spec: &InstallSpec, gate: &LicenceGate) -> bool {
    let mut err = std::io::stderr().lock();
    let _ = write!(err, "{}", licence_question(spec, gate));
    let _ = err.flush();
    drop(err);
    let mut answer = String::new();
    if std::io::stdin().lock().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// The licence question text (three lines, the last without a newline).
pub(crate) fn licence_question(spec: &InstallSpec, gate: &LicenceGate) -> String {
    format!(
        "The {} ({} {}) is proprietary. Licence: {}\n{}\nAccept the licence and install it? [y/N] ",
        spec.display, spec.product, spec.version, gate.url, gate.summary
    )
}

/// Text rendering of the report (one line per tool).
pub fn report_text(r: &InstallReport) -> String {
    let mut out = String::new();
    for t in &r.installed {
        let _ = writeln!(out, "installed  {} {}  {}", t.id, t.version, t.dir.display());
    }
    for t in &r.already {
        let _ = writeln!(out, "installed  {} {}  {}  (already)", t.id, t.version, t.dir.display());
    }
    for l in &r.licence_accepted {
        let _ = writeln!(out, "licence    accepted: {l}");
    }
    for n in &r.notes {
        let _ = writeln!(out, "note       {n}");
    }
    if r.installed.is_empty() && r.already.is_empty() && r.notes.is_empty() {
        let _ = writeln!(out, "nothing to install ({})", r.request);
    }
    out
}

#[cfg(test)]
#[path = "../tests/unit/install.rs"]
mod tests;
