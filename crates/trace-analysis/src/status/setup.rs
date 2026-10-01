//! Setup rows per language (the preflight of every code language, read-only), the install
//! hints of missing default servers and the pending rows.

use std::collections::BTreeMap;

use trace_core::facts::FileFacts;
use trace_core::repo_settings::RepoSettings;
use trace_core::{Index, Language, SupportLevel};
use trace_semantic::setup::SetupRow;
use trace_semantic::ToolEnv;

use crate::pipeline::{self, IndexMode};
use crate::report::PendingRow;
use crate::workspace::Workspace;
use crate::Result;

/// Setup rows of the product languages (module docs): the languages of the index (pending
/// ones excluded), else of a quick inventory when there is no index yet. Report mode: static
/// checks only; a setup error is a row, never a failure of `status`.
pub fn setup_rows(ws: &Workspace, settings: &RepoSettings) -> Result<Vec<SetupRow>> {
    let tools = ToolEnv::discover(&ws.config, &ws.paths.home, &ws.paths.root)?;
    let backends = trace_semantic::registry(&tools);
    let from_inventory: Vec<(String, Language)>;
    let files: Vec<(&str, Language)> = match ws.index.as_ref() {
        Some(index) => index
            .files
            .iter()
            .filter(|f| f.support != SupportLevel::Pending && f.language.is_code())
            .map(|f| (f.path.as_str(), f.language))
            .collect(),
        None => {
            let scanned = pipeline::scan(&ws.paths, &ws.config, None, IndexMode::Incremental)?;
            from_inventory = scanned
                .sources
                .iter()
                .filter_map(|e| Some((e.entry.path.clone(), e.entry.language?)))
                .collect();
            let refs: Vec<(&str, Language)> = from_inventory.iter().map(|(p, l)| (p.as_str(), *l)).collect();
            let pending = pipeline::pending_languages(&refs);
            refs.into_iter()
                .filter(|(_, l)| l.is_code() && !pending.contains(l))
                .collect()
        }
    };
    let present: Vec<Language> = files
        .iter()
        .map(|(_, l)| *l)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let assign = trace_semantic::backend::plan(&backends, &present);
    let plan: Vec<(&trace_semantic::registry::BackendEntry, Vec<Language>)> = assign
        .iter()
        .filter_map(|(b, l)| tools.registry.entry(b.id()).map(|e| (e, l.clone())))
        .collect();
    let facts = |p: &str| -> Option<&FileFacts> {
        let index = ws.index.as_ref()?;
        index.file(index.file_by_path(p)?).facts.as_ref()
    };
    let platform = trace_env::os::Platform::current();
    let vars = trace_env::os::EnvVars::from_process();
    let inputs = trace_semantic::setup::SetupInputs {
        repo: &ws.paths,
        settings,
        tools: &tools,
        files: &files,
        facts: &facts,
        platform: &platform,
        vars: &vars,
    };
    let mut rows = trace_semantic::setup::report(&plan, &inputs);
    for error in trace_semantic::setup::unserved(&present, &tools.registry, &platform) {
        let Some(language) = error.language() else {
            continue;
        };
        rows.push(SetupRow {
            language,
            backend: String::new(),
            server: None,
            toolchain: None,
            dependencies: None,
            build: "not needed",
            status: "error",
            error: Some(trace_semantic::setup::one_line(&error)),
            error_type: Some(error.kind()),
            notes: Vec::new(),
        });
    }
    rows.sort_by_key(|r| r.language);
    Ok(rows)
}

/// "Python language server: installs automatically on first use (or: trace status --install
/// default)" for default languages whose server is missing (PLAN decision 10).
pub(crate) fn default_install_lines(setup: &[SetupRow]) -> Vec<String> {
    let mut seen: Vec<&str> = Vec::new();
    let mut out = Vec::new();
    for row in setup
        .iter()
        .filter(|r| r.language.is_default() && r.error_type == Some("server_missing"))
    {
        let name = row.language.display_name();
        if seen.contains(&name) {
            continue;
        }
        seen.push(name);
        out.push(format!(
            "{name} language server: installs automatically on first use (or: trace status --install default)"
        ));
    }
    out
}

/// Pending languages and sub-projects of the index: one row per (language, reason).
pub(crate) fn pending_rows(index: &Index) -> Vec<PendingRow> {
    let mut by: BTreeMap<(Language, &str), usize> = BTreeMap::new();
    for f in index.files.iter().filter(|f| f.support == SupportLevel::Pending) {
        let reason = f.pending.as_deref().unwrap_or("set up when a query needs it");
        *by.entry((f.language, reason)).or_insert(0) += 1;
    }
    by.into_iter()
        .map(|((language, reason), files)| PendingRow {
            language,
            files,
            reason: reason.to_string(),
        })
        .collect()
}
