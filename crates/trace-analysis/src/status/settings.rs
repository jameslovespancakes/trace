//! Non-default settings with their origin: the keys of `<home>/config.json` whose values
//! differ from the embedded defaults, and the environment variables that override a setting
//! (an override replaces the row of its key).

use std::path::Path;

use trace_core::config::Settings;
use trace_core::env;

use crate::report::SettingRow;

/// The rows of `trace status` (sorted by key).
pub(crate) fn settings_rows(settings: &Settings, home: &Path) -> Vec<SettingRow> {
    let file = Settings::path(home).display().to_string();
    let mut rows: Vec<SettingRow> = settings
        .non_default()
        .into_iter()
        .map(|(key, value)| SettingRow {
            key,
            value,
            origin: file.clone(),
        })
        .collect();
    let quoted = |p: &Path| serde_json::Value::from(p.display().to_string()).to_string();
    let mut overrides: Vec<(&str, String, &str)> = Vec::new();
    if let Some(n) = env::semantic_processes() {
        overrides.push(("workers.server_processes", n.to_string(), env::SEMANTIC_PROCESSES));
    }
    if let Some(p) = env::semantic_tools() {
        overrides.push(("semantic.tools_dir", quoted(&p), env::SEMANTIC_TOOLS));
    }
    if env::offline() {
        overrides.push(("semantic.auto_install", "false".into(), env::OFFLINE));
    } else if env::no_auto_install() {
        overrides.push(("semantic.auto_install", "false".into(), env::NO_AUTO_INSTALL));
    }
    let roots = env::forbidden_roots();
    if !roots.is_empty() {
        let list: Vec<String> = roots.iter().map(|p| quoted(p)).collect();
        overrides.push(("forbidden_roots", format!("+[{}]", list.join(",")), env::FORBIDDEN_ROOTS));
    }
    for (key, value, origin) in overrides {
        rows.retain(|r| r.key != key);
        rows.push(SettingRow {
            key: key.to_string(),
            value,
            origin: origin.to_string(),
        });
    }
    rows.sort_by(|a, b| a.key.cmp(&b.key));
    rows
}

#[cfg(test)]
#[path = "../../tests/unit/status/settings.rs"]
mod tests;
