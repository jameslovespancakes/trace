//! Setup errors: every "trace cannot analyse this repository here" condition (no fallback).
//!
//! `Display` = [`SetupError::lines`] joined with `\n`, without the `Error: ` prefix (the CLI
//! adds it). Continuation lines already carry their indentation (7 spaces, aligned under the
//! text after `Error: `).

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::languages::Language;

/// Indentation of continuation lines (aligned under the text after `Error: `).
pub(crate) const CONTINUATION: &str = "       ";

/// One setup failure. Exit code 3 (`Install { failure: UnknownLanguage }`: 2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetupError {
    /// "The {L} language server is not installed. Install it: trace status --install {id}"
    ServerMissing { language: Language },
    /// "The {L} language server is not available for this computer ({platform})."
    /// + optional second line `advice`.
    ServerUnavailable {
        language: Language,
        platform: String,
        advice: Option<String>,
    },
    /// needs == L:  "{L} is not installed. {install} and run trace again."
    /// otherwise:   "{L} needs {needs}, which is not installed. {install} and run trace again."
    ToolchainMissing {
        language: Language,
        needs: String,
        install: String,
    },
    /// "This {L} project needs {needs} ({source}); the installed {tool} is {found}. {install} and run trace again."
    ToolchainVersion {
        language: Language,
        needs: String,
        source: String,
        tool: String,
        found: String,
        install: String,
    },
    /// "Dependencies not installed. Install your project's dependencies ({hint})"
    /// "       or point trace to them: trace index --env <path>"
    DepsMissing { language: Language, hint: String },
    /// "{L} needs {tool}, which runs {runs}."
    /// "       Only allow this for projects you trust: trace index --allow-build"
    BuildNotAllowed {
        language: Language,
        tool: String,
        runs: String,
    },
    /// Free two-line platform / project-shape limit in the approved style.
    Unsupported {
        language: Language,
        first: String,
        second: Option<String>,
    },
    /// "The {L} language server stopped unexpectedly. Details: {log}"
    ServerCrashed { language: Language, log: PathBuf },
    /// "The {L} language server did not finish in {minutes} minutes. Details: {log}"
    /// (`1 minute` when `minutes` is 1).
    ServerTimeout {
        language: Language,
        minutes: u32,
        log: PathBuf,
    },
    /// "This {L} project could not be built ({what}). Details: {log}"
    BuildFailed {
        language: Language,
        what: String,
        log: PathBuf,
    },
    /// language Some: "No {L} environment found at {path}"; None: "No environment found at {path}"
    EnvNotFound {
        language: Option<Language>,
        path: PathBuf,
    },
    /// Installer failures (texts on [`InstallFailure`]).
    Install {
        language: Option<Language>,
        failure: InstallFailure,
    },
    /// everything the preflight found missing across all
    /// required languages, in ONE error. Built only by [`SetupError::combine`] from >= 2 items.
    /// Line 1: "This repository needs {n} things before trace can analyze it:"
    /// then per item: "  {i}. {item.item_line()}".
    Several { items: Vec<SetupError> },
}

/// Why an installation failed. `{what}` = "{L} language server" (or "Node.js runtime",
/// "Java runtime", ".NET runtime"); `{id}` = the language's install id (`all` without one).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstallFailure {
    /// "Could not download the {what}. Check your internet connection and run: trace status --install {id}"
    Download { what: String },
    /// "The download of the {what} was damaged (checksum mismatch). Run trace status --install {id} again."
    Checksum { what: String },
    /// "Installing the {what} needs {needs}, which is not installed. {install} and run trace again."
    NeedsToolchain {
        what: String,
        needs: String,
        install: String,
    },
    /// "The {what} needs {runtime}, which is not installed. Install it from {url} and run trace again."
    NeedsRuntime {
        what: String,
        runtime: String,
        url: String,
    },
    /// "Not enough disk space to install the {what} (needs {needs_mb} MB in {dir})."
    DiskSpace {
        what: String,
        needs_mb: u64,
        dir: PathBuf,
    },
    /// "Installing the {what} failed. Details: {log}"
    Failed { what: String, log: PathBuf },
    /// "Unknown language \"{arg}\". Use one of: default, python, javascript, ..." (error_type
    /// `invalid_argument`, exit 2).
    UnknownLanguage { arg: String },
    /// Licence gate. "The {what} is installed only after you accept its
    /// licence ({url})." / "       To accept it without a question (scripts): trace status
    /// --install {id} --yes" (error_type install_failed, exit 3).
    LicenceNotAccepted { what: String, url: String },
}

impl SetupError {
    /// Stable JSON `error_type`.
    pub fn kind(&self) -> &'static str {
        match self {
            SetupError::ServerMissing { .. } => "server_missing",
            SetupError::ServerUnavailable { .. } => "server_unavailable",
            SetupError::ToolchainMissing { .. } => "toolchain_missing",
            SetupError::ToolchainVersion { .. } => "toolchain_version",
            SetupError::DepsMissing { .. } => "deps_missing",
            SetupError::BuildNotAllowed { .. } => "build_not_allowed",
            SetupError::Unsupported { .. } => "unsupported",
            SetupError::ServerCrashed { .. } => "server_crashed",
            SetupError::ServerTimeout { .. } => "server_timeout",
            SetupError::BuildFailed { .. } => "build_failed",
            SetupError::EnvNotFound { .. } => "env_not_found",
            SetupError::Install {
                failure: InstallFailure::UnknownLanguage { .. },
                ..
            } => "invalid_argument",
            SetupError::Install { .. } => "install_failed",
            SetupError::Several { .. } => "setup_incomplete",
        }
    }

    /// Process exit code: 2 for an unknown install argument, 3 for every other setup error.
    pub fn exit_code(&self) -> u8 {
        match self {
            SetupError::Install {
                failure: InstallFailure::UnknownLanguage { .. },
                ..
            } => 2,
            _ => 3,
        }
    }

    pub fn language(&self) -> Option<Language> {
        match self {
            SetupError::ServerMissing { language }
            | SetupError::ServerUnavailable { language, .. }
            | SetupError::ToolchainMissing { language, .. }
            | SetupError::ToolchainVersion { language, .. }
            | SetupError::DepsMissing { language, .. }
            | SetupError::BuildNotAllowed { language, .. }
            | SetupError::Unsupported { language, .. }
            | SetupError::ServerCrashed { language, .. }
            | SetupError::ServerTimeout { language, .. }
            | SetupError::BuildFailed { language, .. } => Some(*language),
            SetupError::EnvNotFound { language, .. } | SetupError::Install { language, .. } => *language,
            SetupError::Several { items } => {
                let first = items.first().and_then(SetupError::language)?;
                items.iter().all(|i| i.language() == Some(first)).then_some(first)
            }
        }
    }

    /// ONE line for the combined error: the item's lines joined with a space after trimming,
    /// prefixed with "{L}: " when the text does not already name the language.
    pub fn item_line(&self) -> String {
        let text = self
            .lines()
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        match self.language() {
            Some(l) if !names_language(&text, l.display_name()) => {
                format!("{}: {text}", l.display_name())
            }
            _ => text,
        }
    }

    /// Flatten nested `Several`, drop exact duplicates, keep discovery order; one item -> that
    /// item. Panics on an empty vec (callers only combine failures).
    pub fn combine(items: Vec<SetupError>) -> SetupError {
        fn flatten(item: SetupError, out: &mut Vec<SetupError>) {
            match item {
                SetupError::Several { items } => {
                    for i in items {
                        flatten(i, out);
                    }
                }
                other => {
                    if !out.contains(&other) {
                        out.push(other);
                    }
                }
            }
        }
        let mut flat = Vec::new();
        for item in items {
            flatten(item, &mut flat);
        }
        match flat.len() {
            // Invariant documented on the API: callers only combine a non-empty list of failures.
            0 => panic!("SetupError::combine called without failures"),
            1 => flat.pop().expect("one item"),
            _ => SetupError::Several { items: flat },
        }
    }

    /// Display lines; continuation lines carry their indentation (7 spaces) already.
    pub fn lines(&self) -> Vec<String> {
        let more = |text: &str| format!("{CONTINUATION}{text}");
        match self {
            SetupError::ServerMissing { language } => vec![format!(
                "The {} language server is not installed. Install it: trace status --install {}",
                language.display_name(),
                language.install_id()
            )],
            SetupError::ServerUnavailable {
                language,
                platform,
                advice,
            } => {
                let mut lines = vec![format!(
                    "The {} language server is not available for this computer ({platform}).",
                    language.display_name()
                )];
                if let Some(advice) = advice {
                    lines.push(more(advice));
                }
                lines
            }
            SetupError::ToolchainMissing {
                language,
                needs,
                install,
            } => {
                let name = language.display_name();
                if needs == name {
                    vec![format!(
                        "{name} is not installed. {install} and run trace again."
                    )]
                } else {
                    vec![format!(
                        "{name} needs {needs}, which is not installed. {install} and run trace again."
                    )]
                }
            }
            SetupError::ToolchainVersion {
                language,
                needs,
                source,
                tool,
                found,
                install,
            } => vec![format!(
                "This {} project needs {needs} ({source}); the installed {tool} is {found}. {install} and run trace again.",
                language.display_name()
            )],
            SetupError::DepsMissing { hint, .. } => vec![
                format!("Dependencies not installed. Install your project's dependencies ({hint})"),
                more("or point trace to them: trace index --env <path>"),
            ],
            SetupError::BuildNotAllowed {
                language,
                tool,
                runs,
            } => vec![
                format!("{} needs {tool}, which runs {runs}.", language.display_name()),
                more("Only allow this for projects you trust: trace index --allow-build"),
            ],
            SetupError::Unsupported { first, second, .. } => {
                let mut lines = vec![first.clone()];
                if let Some(second) = second {
                    lines.push(more(second));
                }
                lines
            }
            SetupError::ServerCrashed { language, log } => vec![format!(
                "The {} language server stopped unexpectedly. Details: {}",
                language.display_name(),
                log.display()
            )],
            SetupError::ServerTimeout {
                language,
                minutes,
                log,
            } => vec![format!(
                "The {} language server did not finish in {}. Details: {}",
                language.display_name(),
                minutes_text(*minutes),
                log.display()
            )],
            SetupError::BuildFailed {
                language,
                what,
                log,
            } => vec![format!(
                "This {} project could not be built ({what}). Details: {}",
                language.display_name(),
                log.display()
            )],
            SetupError::EnvNotFound { language, path } => vec![match language {
                Some(l) => format!(
                    "No {} environment found at {}",
                    l.display_name(),
                    path.display()
                ),
                None => format!("No environment found at {}", path.display()),
            }],
            SetupError::Install { language, failure } => {
                let id = language.map(|l| l.install_id()).unwrap_or("all");
                failure.lines(id)
            }
            SetupError::Several { items } => {
                let mut lines = vec![format!(
                    "This repository needs {} things before trace can analyze it:",
                    items.len()
                )];
                for (i, item) in items.iter().enumerate() {
                    lines.push(format!("  {}. {}", i + 1, item.item_line()));
                }
                lines
            }
        }
    }
}

/// `1 minute` / `N minutes` (a duration in plain words, the approved error style).
pub(crate) fn minutes_text(minutes: u32) -> String {
    if minutes == 1 {
        "1 minute".to_string()
    } else {
        format!("{minutes} minutes")
    }
}

/// Whole minutes of a timeout for [`SetupError::ServerTimeout`]: rounded up, at least 1.
pub fn timeout_minutes(timeout: std::time::Duration) -> u32 {
    u32::try_from(timeout.as_secs().div_ceil(60).max(1)).unwrap_or(u32::MAX)
}

/// Whether `text` names the language as a word (so `item_line` does not prefix it twice).
fn names_language(text: &str, name: &str) -> bool {
    text.match_indices(name).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + name.len()..].chars().next();
        !before.is_some_and(|c| c.is_alphanumeric())
            && !after.is_some_and(|c| c.is_alphanumeric() || c == '+' || c == '#')
    })
}

impl InstallFailure {
    /// Display lines; continuation lines carry their indentation already.
    pub fn lines(&self, id: &str) -> Vec<String> {
        match self {
            InstallFailure::LicenceNotAccepted { what, url } => vec![
                format!("The {what} is installed only after you accept its licence ({url})."),
                format!(
                    "{CONTINUATION}To accept it without a question (scripts): trace status --install {id} --yes"
                ),
            ],
            other => vec![other.text(id)],
        }
    }

    /// The text joined into one line; `id` is the install id used in "trace status --install {id}".
    pub fn text(&self, id: &str) -> String {
        match self {
            InstallFailure::LicenceNotAccepted { .. } => self
                .lines(id)
                .iter()
                .map(|l| l.trim())
                .collect::<Vec<_>>()
                .join(" "),
            InstallFailure::Download { what } => format!(
                "Could not download the {what}. Check your internet connection and run: trace status --install {id}"
            ),
            InstallFailure::Checksum { what } => format!(
                "The download of the {what} was damaged (checksum mismatch). Run trace status --install {id} again."
            ),
            InstallFailure::NeedsToolchain {
                what,
                needs,
                install,
            } => format!(
                "Installing the {what} needs {needs}, which is not installed. {install} and run trace again."
            ),
            InstallFailure::NeedsRuntime { what, runtime, url } => format!(
                "The {what} needs {runtime}, which is not installed. Install it from {url} and run trace again."
            ),
            InstallFailure::DiskSpace {
                what,
                needs_mb,
                dir,
            } => format!(
                "Not enough disk space to install the {what} (needs {needs_mb} MB in {}).",
                dir.display()
            ),
            InstallFailure::Failed { what, log } => {
                format!("Installing the {what} failed. Details: {}", log.display())
            }
            InstallFailure::UnknownLanguage { arg } => {
                let ids: Vec<&str> = Language::ALL
                    .iter()
                    .filter(|l| l.is_code() && **l != Language::Tsx)
                    .map(|l| l.install_id())
                    .collect();
                format!(
                    "Unknown language \"{arg}\". Use one of: default, all, {}",
                    ids.join(", ")
                )
            }
        }
    }
}

impl fmt::Display for SetupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.lines().join("\n"))
    }
}

impl std::error::Error for SetupError {}

#[cfg(test)]
#[path = "../tests/unit/setup_error.rs"]
mod tests;
