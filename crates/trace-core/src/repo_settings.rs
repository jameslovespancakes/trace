//! Per-repository settings (`trace index --allow-build`, `trace index --env <path>`).
//!
//! Stored in `<repo cache>/settings.json` ([`RepoPaths::settings_file`]), never in the
//! inspected repository. Every later command (queries re-index with them) reads them.
//!
//! They also remember which pending languages and sub-projects a query needed
//! (`Workspace::ensure_ready`): once set up on first use, they stay part of
//! every later index of this repository.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::languages::Language;
use crate::paths::RepoPaths;

pub(crate) const SETTINGS_SCHEMA: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoSettings {
    pub schema: u32,
    /// `trace index --allow-build` given once for this repository.
    pub allow_build: bool,
    /// `--env <path>` per ecosystem (`trace_env::EcosystemId::as_str()` -> absolute path).
    pub env: BTreeMap<String, PathBuf>,
    /// Pending languages (only used in tests / fixtures / examples here) that a query needed:
    /// set up and analysed like product languages from then on.
    pub ready_languages: BTreeSet<Language>,
    /// Pending sub-project directories (relative, `/`-separated) that a query needed. A
    /// preflight never returns them in `Prepared::pending_dirs`: it sets them up.
    pub ready_dirs: BTreeSet<String>,
}

impl RepoSettings {
    /// Missing file = default; a file of another schema (or one that does not parse) =
    /// default (rewritten on the next save).
    pub fn load(paths: &RepoPaths) -> Result<RepoSettings> {
        let bytes = match fs::read(&paths.settings_file) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(RepoSettings::default()),
            Err(e) => return Err(CoreError::io(&paths.settings_file, e)),
        };
        Ok(serde_json::from_slice::<RepoSettings>(&bytes)
            .ok()
            .filter(|s| s.schema == SETTINGS_SCHEMA)
            .unwrap_or_default())
    }

    /// Atomic write (`cache::write_atomic`) with the current schema.
    pub fn save(&self, paths: &RepoPaths) -> Result<()> {
        paths.ensure_repo_dir()?;
        let mut current = self.clone();
        current.schema = SETTINGS_SCHEMA;
        let text =
            serde_json::to_vec_pretty(&current).map_err(|e| CoreError::Config(format!("settings: {e}")))?;
        crate::cache::write_atomic(&paths.settings_file, &text)
    }

    pub fn env_for(&self, ecosystem: &str) -> Option<&Path> {
        self.env.get(ecosystem).map(PathBuf::as_path)
    }

    /// Whether the sub-project directory `dir` (relative) was set up by a query.
    pub fn dir_ready(&self, dir: &str) -> bool {
        self.ready_dirs.contains(dir.trim_end_matches('/'))
    }
}

#[cfg(test)]
#[path = "../tests/unit/repo_settings.rs"]
mod tests;
