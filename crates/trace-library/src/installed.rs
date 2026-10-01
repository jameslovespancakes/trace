//! Installed dependency packages (DESIGN §1.15): package names per ecosystem
//! read from the prepared library roots' on-disk layouts (directory names and package
//! metadata only - nothing is executed), for `activated_by` rows of the irreducible table.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use trace_env::{EcosystemId, LibraryKind, LibraryRoot};

/// Directory entries visited per root at most.
const MAX_ENTRIES: usize = 200_000;

/// Package names installed as dependencies, from the Prepared library roots (layouts of §1.6).
#[derive(Clone, Debug, Default)]
pub struct InstalledPackages {
    by_ecosystem: BTreeMap<EcosystemId, BTreeSet<String>>,
    /// The library roots the names were read from (declarations of reflection types are
    /// located below them, [`crate::reflect::types`]).
    roots: Vec<LibraryRoot>,
}

/// Comparable form of a package name: Python/.NET/PHP names compare case-insensitively
/// with `-`, `_` and `.` equivalent (PEP 503 style); other ecosystems compare exactly.
pub(crate) fn normalize(ecosystem: EcosystemId, name: &str) -> String {
    match ecosystem {
        EcosystemId::Python | EcosystemId::Dotnet | EcosystemId::Php => {
            let mut out = String::with_capacity(name.len());
            let mut dash = false;
            for c in name.chars() {
                if matches!(c, '-' | '_' | '.') {
                    if !dash {
                        out.push('-');
                    }
                    dash = true;
                } else {
                    out.push(c.to_ascii_lowercase());
                    dash = false;
                }
            }
            out
        }
        _ => name.to_string(),
    }
}

/// `name-1.2.3` -> `name` (the version starts at the first `-` followed by a digit).
fn strip_version(entry: &str) -> &str {
    let bytes = entry.as_bytes();
    for (i, w) in bytes.windows(2).enumerate() {
        if w[0] == b'-' && w[1].is_ascii_digit() {
            return &entry[..i];
        }
    }
    entry
}

fn dir_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// Go module cache escaping: `!x` stands for `X`.
fn go_unescape(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut upper = false;
    for c in path.chars() {
        if c == '!' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Maven-style repositories: `<group path>/<artifact>/<version>/<artifact>-<version>.{pom,jar}`
/// -> `group:artifact` (bounded walk).
fn maven_packages(root: &Path, out: &mut BTreeSet<String>) {
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut visited = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        visited += 1;
        if visited > MAX_ENTRIES || depth > 12 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut has_artifact_file = false;
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push((path, depth + 1));
            } else if path.extension().is_some_and(|x| x == "pom" || x == "jar") {
                has_artifact_file = true;
            }
        }
        if has_artifact_file {
            // dir = .../<group>/<artifact>/<version>
            let Some(artifact_dir) = dir.parent() else { continue };
            let Some(group_dir) = artifact_dir.parent() else { continue };
            let Ok(group_rel) = group_dir.strip_prefix(root) else { continue };
            let group: Vec<String> = group_rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            if let Some(artifact) = artifact_dir.file_name() {
                out.insert(format!("{}:{}", group.join("."), artifact.to_string_lossy()));
            }
        }
    }
}

/// Package names under one root, by its layout.
fn packages_of(root: &LibraryRoot) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let dir = root.path.as_path();
    match root.layout {
        "site_packages" => {
            for name in dir_names(dir) {
                if let Some(stem) = name
                    .strip_suffix(".dist-info")
                    .or_else(|| name.strip_suffix(".egg-info"))
                {
                    out.insert(strip_version(stem).to_string());
                }
            }
        }
        "node_modules" => {
            for name in dir_names(dir) {
                if name.starts_with('.') {
                    continue;
                }
                if name.starts_with('@') {
                    for inner in dir_names(&dir.join(&name)) {
                        out.insert(format!("{name}/{inner}"));
                    }
                } else {
                    out.insert(name);
                }
            }
        }
        "go_modcache" => {
            let mut stack = vec![(dir.to_path_buf(), 0usize)];
            let mut visited = 0usize;
            while let Some((d, depth)) = stack.pop() {
                visited += 1;
                if visited > MAX_ENTRIES || depth > 6 {
                    continue;
                }
                for name in dir_names(&d) {
                    if name == "cache" && depth == 0 {
                        continue;
                    }
                    let child = d.join(&name);
                    if let Some((module_last, _)) = name.split_once('@') {
                        let Ok(rel) = d.strip_prefix(dir) else { continue };
                        let mut parts: Vec<String> = rel
                            .components()
                            .map(|c| c.as_os_str().to_string_lossy().into_owned())
                            .collect();
                        parts.push(module_last.to_string());
                        out.insert(go_unescape(&parts.join("/")));
                    } else {
                        stack.push((child, depth + 1));
                    }
                }
            }
        }
        "cargo_registry" => {
            let names = dir_names(dir);
            let direct: Vec<&String> = names.iter().filter(|n| strip_version(n) != n.as_str()).collect();
            if !direct.is_empty() {
                out.extend(direct.into_iter().map(|n| strip_version(n).to_string()));
            } else {
                for index in names {
                    for name in dir_names(&dir.join(index)) {
                        out.insert(strip_version(&name).to_string());
                    }
                }
            }
        }
        "r_library" | "nuget_packages" => {
            out.extend(dir_names(dir).into_iter().filter(|n| !n.starts_with('.')));
        }
        "php_vendor" => {
            for vendor in dir_names(dir) {
                if matches!(vendor.as_str(), "composer" | "bin") {
                    continue;
                }
                for name in dir_names(&dir.join(&vendor)) {
                    out.insert(format!("{vendor}/{name}"));
                }
            }
        }
        "cabal_store" => {
            for ghc in dir_names(dir) {
                out.extend(dir_names(&dir.join(ghc)).iter().map(|n| strip_version(n).to_string()));
            }
        }
        "maven_repo" | "gradle_cache" | "coursier_cache" => {
            let base = match root.layout {
                "gradle_cache" if dir.join("files-2.1").is_dir() => dir.join("files-2.1"),
                _ => dir.to_path_buf(),
            };
            if root.layout == "gradle_cache" {
                // files-2.1/<group>/<artifact>/<version>/<hash>/<file>
                for group in dir_names(&base) {
                    for artifact in dir_names(&base.join(&group)) {
                        out.insert(format!("{group}:{artifact}"));
                    }
                }
            } else {
                maven_packages(&base, &mut out);
            }
        }
        _ => {}
    }
    out.into_iter().filter(|n| !n.is_empty()).collect()
}

impl InstalledPackages {
    /// Package names of every dependency root (standard-library roots are not dependencies).
    pub fn from_roots(roots: &[LibraryRoot]) -> InstalledPackages {
        let mut by_ecosystem: BTreeMap<EcosystemId, BTreeSet<String>> = BTreeMap::new();
        for root in roots.iter().filter(|r| r.kind == LibraryKind::Dependency) {
            let names = packages_of(root);
            by_ecosystem
                .entry(root.ecosystem)
                .or_default()
                .extend(names.into_iter().map(|n| normalize(root.ecosystem, &n)));
        }
        InstalledPackages {
            by_ecosystem,
            roots: roots.to_vec(),
        }
    }

    /// The library roots of the installation (dependency and standard-library roots).
    pub fn roots(&self) -> &[LibraryRoot] {
        &self.roots
    }

    pub fn contains(&self, ecosystem: EcosystemId, package: &str) -> bool {
        self.by_ecosystem
            .get(&ecosystem)
            .is_some_and(|names| names.contains(&normalize(ecosystem, package)))
    }

    /// Every installed package of an ecosystem (sorted).
    pub fn names(&self, ecosystem: EcosystemId) -> Vec<&str> {
        self.by_ecosystem
            .get(&ecosystem)
            .map(|n| n.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[path = "../tests/unit/installed.rs"]
mod tests;
