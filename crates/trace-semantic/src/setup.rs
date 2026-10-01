//! Setup orchestration (DESIGN §1.7; owner flow): preflight every planned backend and
//! combine ALL failures of all languages into one error (PLAN decision 15), `trace status`
//! setup rows, and the shared checks every `languages/<lang>.rs` preflight uses (same order,
//! same texts).

use serde::Serialize;
use trace_core::facts::FileFacts;
use trace_core::paths::RepoPaths;
use trace_core::repo_settings::RepoSettings;
use trace_core::setup_error::SetupError;
use trace_core::Language;
use trace_env::os::{EnvVars, Platform};

use crate::languages::{server_for, Prepared, SetupContext};
use crate::registry::{BackendEntry, BuildSpec, ToolchainSpec};
use crate::tools::ToolEnv;

/// Inputs shared by every backend's preflight.
pub struct SetupInputs<'a> {
    pub repo: &'a RepoPaths,
    pub settings: &'a RepoSettings,
    pub tools: &'a ToolEnv,
    pub files: &'a [(&'a str, Language)],
    pub facts: &'a dyn Fn(&str) -> Option<&'a FileFacts>,
    pub platform: &'a Platform,
    pub vars: &'a EnvVars,
}

/// Accumulator used by every `languages/<lang>.rs` preflight (and by [`preflight_all`]).
#[derive(Default)]
pub struct Collect {
    pub items: Vec<SetupError>,
}

impl Collect {
    /// Records the error of `r` (if any); returns the Ok value.
    pub fn check<T>(&mut self, r: Result<T, SetupError>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.items.push(e);
                None
            }
        }
    }

    pub fn push(&mut self, e: SetupError) {
        self.items.push(e);
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Ok(value) when nothing was collected, else Err(SetupError::combine(items)).
    pub fn finish<T>(self, value: T) -> Result<T, SetupError> {
        if self.items.is_empty() {
            Ok(value)
        } else {
            Err(SetupError::combine(self.items))
        }
    }
}

/// The files of `languages` among `files`.
fn files_of<'a>(files: &[(&'a str, Language)], languages: &[Language]) -> Vec<(&'a str, Language)> {
    files.iter().filter(|(_, l)| languages.contains(l)).copied().collect()
}

fn preflight_one(
    hooks: &dyn crate::languages::Server,
    entry: &BackendEntry,
    languages: &[Language],
    cx: &SetupInputs<'_>,
    report_only: bool,
) -> Result<Prepared, SetupError> {
    let files = files_of(cx.files, languages);
    // Re-borrow the facts lookup at the (shorter) lifetime of this call's file list.
    let facts = |path: &str| -> Option<&FileFacts> { (cx.facts)(path) };
    let setup = SetupContext {
        repo: cx.repo,
        entry,
        languages,
        files: &files,
        facts: &facts,
        settings: cx.settings,
        tools: cx.tools,
        platform: cx.platform,
        vars: cx.vars,
        report_only,
    };
    hooks.preflight(&setup)
}

/// Preflight every backend of `plan` (in registry order); ALL failures of all backends are
/// returned as ONE error (`SetupError::combine`, PLAN decision 15). Never stops at the first.
pub fn preflight_all(
    plan: &[(&BackendEntry, Vec<Language>)],
    cx: &SetupInputs<'_>,
) -> Result<Vec<Prepared>, SetupError> {
    preflight_with(plan, cx, &|id| server_for(id), Vec::new())
}

/// [`preflight_all`] with an explicit hook lookup, after `earlier` failures found before the
/// preflight (languages no registry entry serves, [`unserved`]): they come first in the
/// combined error, in discovery order.
pub fn preflight_with(
    plan: &[(&BackendEntry, Vec<Language>)],
    cx: &SetupInputs<'_>,
    hooks: &dyn Fn(&str) -> &'static dyn crate::languages::Server,
    earlier: Vec<SetupError>,
) -> Result<Vec<Prepared>, SetupError> {
    let mut collect = Collect { items: earlier };
    let mut prepared = Vec::with_capacity(plan.len());
    for (entry, languages) in plan {
        if let Some(p) = collect.check(preflight_one(hooks(&entry.id), entry, languages, cx, false)) {
            prepared.push(p);
        }
    }
    collect.finish(prepared)
}

/// Code languages of the repository that no registry entry serves (a `semantic.registry`
/// override without them): there is no syntax-only answer for them, so each one is a
/// `ServerUnavailable` failure of the setup (PLAN decision 3).
pub fn unserved(
    present: &[Language],
    registry: &crate::registry::Registry,
    platform: &Platform,
) -> Vec<SetupError> {
    let mut out: Vec<SetupError> = Vec::new();
    for &language in present {
        if !language.is_code() {
            continue;
        }
        let served = registry.backends.iter().any(|e| e.languages.contains(&language));
        let error = SetupError::ServerUnavailable {
            language,
            platform: platform.display(),
            advice: None,
        };
        if !served && !out.contains(&error) {
            out.push(error);
        }
    }
    out
}

/// A setup error as one status line (the combined error: its items joined with "; ").
pub fn one_line(error: &SetupError) -> String {
    match error {
        SetupError::Several { items } => {
            items.iter().map(SetupError::item_line).collect::<Vec<_>>().join("; ")
        }
        other => other
            .lines()
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// Prefix of a `Prepared::status` line that fills the `dependencies` column of the setup row
/// ("dependencies: installed (.venv)"); `toolchain: ` lines replace the toolchain column.
pub(crate) const STATUS_DEPENDENCIES: &str = "dependencies: ";
/// See [`STATUS_DEPENDENCIES`].
pub(crate) const STATUS_TOOLCHAIN: &str = "toolchain: ";

/// One `trace status` row per product language.
#[derive(Clone, Debug, Serialize)]
pub struct SetupRow {
    pub language: Language,
    pub backend: String,
    /// "jdtls 1.61.0"
    pub server: Option<String>,
    pub toolchain: Option<String>,
    pub dependencies: Option<String>,
    /// "not needed" | "allowed" | "needs --allow-build"
    pub build: &'static str,
    /// "ready" | "error"
    pub status: &'static str,
    pub error: Option<String>,
    pub error_type: Option<&'static str>,
    pub notes: Vec<String>,
}

/// `trace status`: one row per product language, never failing on setup errors (report
/// mode: static checks only).
pub fn report(plan: &[(&BackendEntry, Vec<Language>)], cx: &SetupInputs<'_>) -> Vec<SetupRow> {
    report_with(plan, cx, &|id| server_for(id))
}

/// [`report`] with an explicit hook lookup (tests).
pub(crate) fn report_with(
    plan: &[(&BackendEntry, Vec<Language>)],
    cx: &SetupInputs<'_>,
    hooks: &dyn Fn(&str) -> &'static dyn crate::languages::Server,
) -> Vec<SetupRow> {
    let mut rows = Vec::new();
    for (entry, languages) in plan {
        let outcome = preflight_one(hooks(&entry.id), entry, languages, cx, true);
        let build = match &entry.requires_build {
            None => "not needed",
            Some(_) if cx.settings.allow_build => "allowed",
            Some(_) => "needs --allow-build",
        };
        for language in languages {
            let server = format!("{} {}", entry.server.name, entry.server.version)
                .trim()
                .to_string();
            let mut row = SetupRow {
                language: *language,
                backend: entry.id.clone(),
                server: (!server.is_empty()).then_some(server),
                toolchain: None,
                dependencies: None,
                build,
                status: "ready",
                error: None,
                error_type: None,
                notes: Vec::new(),
            };
            match &outcome {
                Ok(p) => {
                    // A preflight that chose another server build than the pinned one names its
                    // version.
                    if let Some(version) = p.vars.get("server_version") {
                        row.server = Some(format!("{} {version}", entry.server.name).trim().to_string());
                    }
                    row.toolchain = p.toolchain.as_ref().map(|t| {
                        let mut text = t.id.to_string();
                        if let Some(v) = &t.version {
                            text.push(' ');
                            text.push_str(&v.text);
                        }
                        format!("{text} ({})", t.root.display())
                    });
                    let decided_by_hooks = entry
                        .requires_build
                        .as_ref()
                        .is_some_and(|b| b.when == crate::registry::BuildWhen::DecidedByHooks);
                    if decided_by_hooks {
                        // The preflight decided (gopls cgo, clangd CMake/Meson, build-less Java).
                        row.build = if p.runs_project_code {
                            "allowed"
                        } else {
                            "not needed"
                        };
                    } else if p.runs_project_code && build == "not needed" {
                        row.build = "allowed";
                    }
                    for note in &p.status {
                        if let Some(deps) = note.strip_prefix(STATUS_DEPENDENCIES) {
                            row.dependencies = Some(deps.to_string());
                        } else if let Some(tc) = note.strip_prefix(STATUS_TOOLCHAIN) {
                            row.toolchain = Some(tc.to_string());
                        } else {
                            row.notes.push(note.clone());
                        }
                    }
                    if row.dependencies.is_none() && !p.library_roots.is_empty() {
                        let deps = p
                            .library_roots
                            .iter()
                            .filter(|r| r.kind == trace_env::LibraryKind::Dependency)
                            .count();
                        if deps > 0 {
                            row.dependencies = Some(format!(
                                "installed ({deps} folder{})",
                                if deps == 1 { "" } else { "s" }
                            ));
                        }
                    }
                }
                Err(e) => {
                    row.status = "error";
                    row.error = Some(one_line(e));
                    row.error_type = Some(e.kind());
                    if matches!(e, SetupError::DepsMissing { .. }) {
                        row.dependencies = Some("not installed".into());
                    }
                }
            }
            rows.push(row);
        }
    }
    rows
}

/// Shared check: the server's tool is installed per MANIFEST (entries whose executable comes
/// from a toolchain, or without an install record, pass). Default languages normally never
/// reach the `ServerMissing` branch: the pipeline ran `install::auto_install` before
/// preflight; they get it only when automatic installs are off.
pub(crate) fn require_server(cx: &SetupContext<'_>) -> Result<(), SetupError> {
    let Some(install) = &cx.entry.install else {
        return Ok(());
    };
    if matches!(install.recipe, crate::registry::Recipe::FromToolchain { .. }) {
        return Ok(());
    }
    if cx.tools.tool_dir(&install.id).is_some() {
        Ok(())
    } else {
        Err(SetupError::ServerMissing {
            language: cx.language(),
        })
    }
}

/// Shared check: every runtime id of the entry is installed in `<tools>` (always
/// trace-managed, never the user's runtime).
pub(crate) fn require_runtimes(cx: &SetupContext<'_>) -> Result<(), SetupError> {
    if cx.entry.runtime.iter().all(|id| cx.tools.tool_dir(id).is_some()) {
        Ok(())
    } else {
        Err(SetupError::ServerMissing {
            language: cx.language(),
        })
    }
}

/// Shared check: `BuildNotAllowed` unless `trace index --allow-build` was given for this
/// repository.
pub(crate) fn require_approval(cx: &SetupContext<'_>, spec: &BuildSpec) -> Result<(), SetupError> {
    if cx.settings.allow_build {
        Ok(())
    } else {
        Err(SetupError::BuildNotAllowed {
            language: cx.language(),
            tool: spec.tool.clone(),
            runs: spec.runs.clone(),
        })
    }
}

/// `DepsMissing` when the project declares dependencies that are not installed.
pub(crate) fn deps_error(language: Language, report: &trace_env::DepsReport) -> Option<SetupError> {
    (report.status == trace_env::DepsStatus::Missing).then(|| SetupError::DepsMissing {
        language,
        hint: report.hint.clone(),
    })
}

/// `ToolchainMissing` / `ToolchainVersion` from a toolchain status (optional toolchains never
/// fail).
pub(crate) fn toolchain_error(
    language: Language,
    spec: &ToolchainSpec,
    status: &trace_env::ToolchainStatus,
) -> Option<SetupError> {
    match status {
        trace_env::ToolchainStatus::Missing { .. } if !spec.optional => Some(SetupError::ToolchainMissing {
            language,
            needs: spec.needs.clone(),
            install: spec.install.clone(),
        }),
        trace_env::ToolchainStatus::TooOld {
            found,
            needed,
            source,
        } if !spec.optional => {
            let tool = language.display_name().to_string();
            Some(SetupError::ToolchainVersion {
                language,
                needs: needed.describe(&tool),
                source: source.clone(),
                tool,
                found: found
                    .version
                    .as_ref()
                    .map(|v| v.text.clone())
                    .unwrap_or_else(|| "unknown".into()),
                install: spec.install.clone(),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "../tests/unit/setup.rs"]
mod tests;
