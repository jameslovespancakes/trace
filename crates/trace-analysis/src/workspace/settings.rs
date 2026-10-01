//! Repository settings written by `trace index --allow-build` / `--env <path>`, and the
//! language an `--env` path is checked against.

use std::path::Path;

use trace_core::paths::RepoPaths;
use trace_core::repo_settings::RepoSettings;
use trace_core::{CoreError, Language};

use super::OpenOptions;
use crate::languages::most_files;
use crate::pipeline::{self, IndexMode};
use crate::Result;

/// `trace index --allow-build` / `--env <path>`: store them in the repository's settings
/// (`<repo cache>/settings.json`, never in the repository) before any pipeline run. An
/// environment no ecosystem accepts is `EnvNotFound` (DESIGN §1.4): naming the product language
/// with the most files among those that declare dependencies ([`env_language`]).
pub(super) fn save_settings(
    paths: &RepoPaths,
    config: &trace_core::config::Settings,
    opts: &OpenOptions,
) -> Result<()> {
    if !opts.allow_build && opts.env.is_empty() {
        return Ok(());
    }
    let mut settings = RepoSettings::load(paths)?;
    if opts.allow_build {
        settings.allow_build = true;
    }
    for raw in &opts.env {
        let path = if raw.is_absolute() {
            raw.clone()
        } else {
            std::env::current_dir().map_err(|e| CoreError::io(raw, e))?.join(raw)
        };
        let path = trace_core::inventory::strip_verbatim(path);
        match classify_env(&path) {
            Some(ecosystem) => {
                settings.env.insert(ecosystem.as_str().to_string(), path);
            }
            None => {
                return Err(trace_core::SetupError::EnvNotFound {
                    language: env_language(paths, config),
                    path,
                }
                .into())
            }
        }
    }
    settings.save(paths)?;
    Ok(())
}

/// The ecosystem that accepts `path` as its environment (an existing directory only).
fn classify_env(path: &Path) -> Option<trace_env::EcosystemId> {
    if !path.is_dir() {
        return None;
    }
    trace_env::classify_env_path(path)
}

/// The language an `--env` path was given for (the `EnvNotFound` text; quick inventory, no
/// index needed): the product language with the most files among those whose ecosystem
/// declares dependencies in the repository (an environment is where those are installed);
/// `None` when no product language declares any.
fn env_language(paths: &RepoPaths, config: &trace_core::config::Settings) -> Option<Language> {
    let scanned = pipeline::scan(paths, config, None, IndexMode::Incremental).ok()?;
    let files: Vec<(&str, Language)> = scanned
        .sources
        .iter()
        .filter_map(|e| Some((e.entry.path.as_str(), e.entry.language?)))
        .collect();
    let configs: Vec<&str> = scanned
        .configs
        .iter()
        .map(|c| c.entry.path.rsplit('/').next().unwrap_or(&c.entry.path))
        .collect();
    env_language_of(&files, &configs)
}

/// [`env_language`] over an inventory: `files` (path, language), `configs` (basenames).
/// Ties go to the first language in language order.
pub(super) fn env_language_of(files: &[(&str, Language)], configs: &[&str]) -> Option<Language> {
    let pending = pipeline::pending_languages(files);
    most_files(
        files
            .iter()
            .map(|&(_, l)| l)
            .filter(|l| l.is_code() && !pending.contains(l) && declares_dependencies(*l, configs))
            .map(named),
    )
}

/// The language users name a file's language by (TSX files are TypeScript): the first
/// language in language order with the same display name.
fn named(language: Language) -> Language {
    Language::ALL
        .into_iter()
        .find(|l| l.display_name() == language.display_name())
        .unwrap_or(language)
}

/// Whether one of the repository's configuration files (basenames) is a dependency manifest
/// of `language`'s ecosystem ([`trace_env::EcosystemId::manifests`]).
fn declares_dependencies(language: Language, configs: &[&str]) -> bool {
    trace_env::EcosystemId::of_language(language)
        .is_some_and(|e| configs.iter().any(|c| e.manifests().contains(c)))
}
