//! Setup: product languages, the automatic install of missing default servers and the
//! preflight of every product language (one combined error, no fallback).

use std::collections::BTreeSet;

use trace_core::facts::FileFacts;
use trace_core::setup_error::SetupError;
use trace_core::Language;
use trace_semantic::backend::Backend;
use trace_semantic::{Prepared, ToolEnv};

use super::{progress::IndexProgress, Host, SemanticPlan};
use crate::Result;

/// The code languages analysed now: every code language of `files` except pending ones.
pub(super) fn product_languages(files: &[(&str, Language)], pending: &BTreeSet<Language>) -> Vec<Language> {
    files
        .iter()
        .map(|(_, l)| *l)
        .filter(|l| l.is_code() && !pending.contains(l))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// PLAN decision 10: install the default languages among `product` whose server or server
/// runtime `missing_defaults` reports (never a non-default language). Returns whether
/// anything was installed; an install failure is its own `SetupError` (no fallback). Runs
/// before the preflight (the caller resolves the tools again when it returns true).
fn install_missing(
    enabled: bool,
    product: &[Language],
    missing_defaults: &dyn Fn(&[Language]) -> Vec<Language>,
    install: &mut dyn FnMut(&[Language]) -> std::result::Result<(), SetupError>,
) -> std::result::Result<bool, SetupError> {
    if !enabled {
        return Ok(false);
    }
    let defaults: Vec<Language> = product.iter().copied().filter(|l| l.is_default()).collect();
    if defaults.is_empty() {
        return Ok(false);
    }
    let missing: Vec<Language> = missing_defaults(&defaults)
        .into_iter()
        .filter(|l| l.is_default())
        .collect();
    if missing.is_empty() {
        return Ok(false);
    }
    install(&missing)?;
    Ok(true)
}

/// The install progress line of §1.2 for a `start` step and the lock wait (other steps
/// print nothing: exactly one line per tool).
fn install_progress_line(p: &trace_semantic::install::InstallProgress) -> Option<String> {
    matches!(p.step, "start" | "wait").then(|| trace_semantic::install::progress_line(p))
}

/// [`install_missing`] with the real installer (`trace_semantic::install::auto_install`),
/// progress lines through [`IndexProgress::install_line`].
pub(super) fn auto_install(
    host: &Host<'_>,
    tools: &ToolEnv,
    product: &[Language],
    progress: &mut dyn IndexProgress,
) -> Result<bool> {
    let tools_dir = trace_core::config::semantic_tools_dir(host.config);
    let missing = |languages: &[Language]| -> Vec<Language> {
        trace_semantic::install::missing_defaults_at(
            &tools_dir,
            &tools.registry,
            languages,
            &host.platform,
            Some(&host.paths.root),
        )
    };
    let mut install = |languages: &[Language]| -> std::result::Result<(), SetupError> {
        let mut report = |p: &trace_semantic::install::InstallProgress| {
            if let Some(line) = install_progress_line(p) {
                progress.install_line(&line);
            }
        };
        trace_semantic::install::auto_install(trace_semantic::install::InstallRequest {
            tools_dir: &tools_dir,
            registry: &tools.registry,
            languages,
            repo_root: Some(&host.paths.root),
            platform: &host.platform,
            vars: &host.vars,
            progress: &mut report,
            licences: trace_semantic::install::LicenceAnswer::Refuse,
        })
        .map(|_| ())
    };
    Ok(install_missing(host.auto_install, product, &missing, &mut install)?)
}

/// Languages and preflight (module docs, steps 3 and 5): one registry entry per product
/// language; code languages without an entry are failures in the same combined error. Fills
/// the prepared results and the backend fingerprints.
pub(super) fn setup_phase<'b, 'f>(
    backends: &'b [Box<dyn Backend>],
    host: &Host<'_>,
    tools: &'b ToolEnv,
    files: &[(&str, Language)],
    facts: &'f dyn Fn(&str) -> Option<&'f FileFacts>,
    pending: &BTreeSet<Language>,
) -> Result<SemanticPlan<'b>> {
    let present = product_languages(files, pending);
    let assign = trace_semantic::backend::plan(backends, &present);
    let unserved = trace_semantic::setup::unserved(&present, &tools.registry, &host.platform);
    let product_files: Vec<(&str, Language)> =
        files.iter().copied().filter(|(_, l)| present.contains(l)).collect();
    let plan: Vec<(&trace_semantic::registry::BackendEntry, Vec<Language>)> = assign
        .iter()
        .filter_map(|(b, l)| tools.registry.entry(b.id()).map(|e| (e, l.clone())))
        .collect();
    // Re-borrow the facts lookup at the (shorter) lifetime of this call's file list.
    let lookup = |path: &str| -> Option<&FileFacts> { facts(path) };
    let inputs = trace_semantic::setup::SetupInputs {
        repo: host.paths,
        settings: host.settings,
        tools,
        files: &product_files,
        facts: &lookup,
        platform: &host.platform,
        vars: &host.vars,
    };
    let prepared = trace_semantic::setup::preflight_with(
        &plan,
        &inputs,
        &|id| trace_semantic::server_for(id),
        unserved,
    )?;
    let prepared: Vec<Prepared> = assign
        .iter()
        .map(|(b, l)| {
            prepared
                .iter()
                .find(|p| p.backend == b.id())
                .cloned()
                .unwrap_or_else(|| Prepared {
                    backend: b.id().to_string(),
                    languages: l.clone(),
                    ..Prepared::default()
                })
        })
        .collect();
    let fingerprints = assign
        .iter()
        .zip(&prepared)
        .map(|((b, _), p)| b.fingerprint(tools, p))
        .collect();
    Ok(SemanticPlan {
        tools,
        assign,
        prepared,
        fingerprints,
    })
}

#[cfg(test)]
#[path = "../../tests/unit/pipeline/setup.rs"]
mod tests;
