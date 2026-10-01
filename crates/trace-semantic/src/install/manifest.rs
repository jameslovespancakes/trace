//! `<tools>/MANIFEST.json` schema 2 (DESIGN §1.9; owner install): which tool versions are
//! installed, the variants inside a version (per R minor, GHC version), the
//! install extras done per backend and the licences the user accepted (per tool version).
//! Layout `<tools>/<id>/<version>[/<variant>]/...`. A missing, unreadable or schema-1
//! manifest (the dev `tools/` folder of the old layout) means "nothing installed".

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) const MANIFEST_FILE: &str = "MANIFEST.json";
pub(crate) const MANIFEST_SCHEMA: u32 = 2;

/// One installed tool version.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ManifestTool {
    pub version: String,
    /// `Platform::key()` it was installed for.
    pub platform: String,
    /// Recipe name ("archive", "npm", "go_install", ...).
    pub method: String,
    pub license: String,
    /// Seconds since the Unix epoch ("unix:<secs>").
    pub installed_at: String,
    /// PLAN decision 11: the licence of this version was accepted.
    pub licence_accepted: bool,
    /// Relative executable path -> sha256 recorded at install.
    pub files: BTreeMap<String, String>,
    /// Variant directories inside the version directory ("R-4.6"); empty for recipes
    /// without variants.
    pub variants: BTreeSet<String>,
    /// Where the bytes came from (urls, `go install <module>@<version>`, ...).
    pub sources: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    #[serde(default)]
    pub tools: BTreeMap<String, ManifestTool>,
    /// `<id>@<version>` of every licence the user accepted (remembered before the download,
    /// so a failed download never asks again).
    #[serde(default)]
    pub accepted_licences: BTreeSet<String>,
    /// Backend id -> ids of the install extras done for it (`Server::install_extras`).
    #[serde(default)]
    pub extras: BTreeMap<String, BTreeSet<String>>,
}

impl Manifest {
    /// The manifest of `tools_dir` (empty when missing, unreadable or of another schema).
    pub fn load(tools_dir: &Path) -> Manifest {
        std::fs::read(tools_dir.join(MANIFEST_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice::<Manifest>(&b).ok())
            .filter(|m| m.schema == MANIFEST_SCHEMA)
            .unwrap_or_default()
    }

    /// Atomic write of `<tools>/MANIFEST.json` (schema 2).
    pub fn save(&self, tools_dir: &Path) -> std::io::Result<()> {
        let mut out = self.clone();
        out.schema = MANIFEST_SCHEMA;
        let bytes = serde_json::to_vec_pretty(&out).map_err(std::io::Error::other)?;
        trace_core::cache::write_atomic(&tools_dir.join(MANIFEST_FILE), &bytes)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    /// `<tools>/<id>/<version>` when the manifest lists `id` and the directory exists.
    pub(crate) fn tool_dir(&self, tools_dir: &Path, id: &str) -> Option<PathBuf> {
        let tool = self.tools.get(id)?;
        let rel = crate::registry::safe_relative(&format!("{id}/{}", tool.version))?;
        let dir = tools_dir.join(rel);
        dir.is_dir().then_some(dir)
    }

    /// The installed version of `id` (its directory exists).
    pub(crate) fn installed_version(&self, tools_dir: &Path, id: &str) -> Option<&str> {
        self.tool_dir(tools_dir, id)?;
        self.tools.get(id).map(|t| t.version.as_str())
    }

    /// `id` is installed at exactly `version`.
    pub(crate) fn has_version(&self, tools_dir: &Path, id: &str, version: &str) -> bool {
        self.installed_version(tools_dir, id) == Some(version)
    }

    /// `id` is installed at `version` with the variant directory `variant`.
    pub(crate) fn has_variant(&self, tools_dir: &Path, id: &str, version: &str, variant: &str) -> bool {
        self.has_version(tools_dir, id, version)
            && self.tools.get(id).is_some_and(|t| t.variants.contains(variant))
            && self.tool_dir(tools_dir, id).is_some_and(|d| d.join(variant).is_dir())
    }

    /// PLAN decision 11: the licence of `id` at `version` was accepted before.
    pub fn licence_accepted(&self, id: &str, version: &str) -> bool {
        self.accepted_licences.contains(&licence_key(id, version))
            || self
                .tools
                .get(id)
                .is_some_and(|t| t.version == version && t.licence_accepted)
    }

    /// Remember the acceptance of the licence of `id` at `version`.
    pub(crate) fn accept_licence(&mut self, id: &str, version: &str) {
        self.accepted_licences.insert(licence_key(id, version));
    }

    /// The extra `extra` of backend `backend` was installed.
    pub(crate) fn has_extra(&self, backend: &str, extra: &str) -> bool {
        self.extras.get(backend).is_some_and(|s| s.contains(extra))
    }

    pub(crate) fn record_extra(&mut self, backend: &str, extra: &str) {
        self.extras
            .entry(backend.to_string())
            .or_default()
            .insert(extra.to_string());
    }
}

fn licence_key(id: &str, version: &str) -> String {
    format!("{id}@{version}")
}

/// "unix:<seconds>" for `ManifestTool::installed_at`.
pub(crate) fn now_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}

#[cfg(test)]
#[path = "../../tests/unit/install/manifest.rs"]
mod tests;
