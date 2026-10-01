//! Cargo projects: manifests, workspaces and members, path dependencies, `Cargo.lock`.

use super::*;

/// One package manifest of the repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CargoPackage {
    /// Directory relative to the root (`""` = the root).
    pub dir: String,
    pub name: String,
    /// `[lib] proc-macro = true`.
    pub proc_macro: bool,
    /// A build script (`build.rs` or `package.build`).
    pub build_script: bool,
    /// Registry / path dependency names as written (renames resolved to the package name).
    pub dependencies: Vec<String>,
}

/// A Cargo project root trace loads: a workspace root or a standalone package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CargoProject {
    /// Directory relative to the root (`""` = the root).
    pub dir: String,
    /// `<dir>/Cargo.toml`, relative.
    pub manifest: String,
    pub workspace: bool,
    /// Member package directories (relative to the repository root), incl. the root package.
    pub members: Vec<String>,
    /// `<dir>/Cargo.lock` exists.
    pub lock: bool,
}

/// The Cargo projects of a repository.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CargoLayout {
    /// Required project roots (not nested inside another project).
    pub projects: Vec<CargoProject>,
    /// Nested projects that are not members of a required project.
    pub subprojects: Vec<SubProject>,
    /// Every package manifest found (required or not), by directory.
    pub packages: Vec<CargoPackage>,
    /// Highest `rust-version` of the required packages: (version, "rust-version in <manifest>").
    pub rust_version: Option<(Version, String)>,
}

impl CargoLayout {
    /// Packages that are members of a required project.
    pub fn required_packages(&self) -> Vec<&CargoPackage> {
        let members: BTreeSet<&str> = self
            .projects
            .iter()
            .flat_map(|p| p.members.iter().map(String::as_str))
            .collect();
        self.packages
            .iter()
            .filter(|p| members.contains(p.dir.as_str()))
            .collect()
    }
}

/// The Cargo layout of `root` from the manifests above the inventoried `.rs` files.
pub fn cargo_layout(root: &Path, files: &[(&str, Language)]) -> CargoLayout {
    let dirs = relpath::manifest_dirs(root, files, &[Language::Rust], "Cargo.toml");
    let mut manifests: BTreeMap<String, Value> = BTreeMap::new();
    for dir in dirs.into_iter().take(MAX_MANIFESTS) {
        if let Some(v) = relpath::read_toml(&relpath::under(root, &dir).join("Cargo.toml")) {
            manifests.insert(dir, v);
        }
    }
    let mut layout = CargoLayout::default();
    for (dir, m) in &manifests {
        if let Some(pkg) = m.get("package") {
            let name = pkg.get("name").and_then(Value::as_str).unwrap_or_default();
            let proc_macro = m
                .get("lib")
                .and_then(|l| l.get("proc-macro").or_else(|| l.get("proc_macro")))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let build_script = match pkg.get("build") {
                Some(Value::Bool(b)) => *b,
                Some(Value::String(_)) => true,
                _ => relpath::under(root, dir).join("build.rs").is_file(),
            };
            layout.packages.push(CargoPackage {
                dir: dir.clone(),
                name: name.to_string(),
                proc_macro,
                build_script,
                dependencies: declared_dependencies(m),
            });
        }
    }

    // Workspace roots and their members.
    let mut owner: BTreeMap<String, String> = BTreeMap::new(); // package dir -> project dir
    let mut roots: BTreeMap<String, CargoProject> = BTreeMap::new();
    for (dir, m) in &manifests {
        if m.get("workspace").is_some() {
            roots.insert(
                dir.clone(),
                CargoProject {
                    dir: dir.clone(),
                    manifest: relpath::join(dir, "Cargo.toml"),
                    workspace: true,
                    members: Vec::new(),
                    lock: relpath::under(root, dir).join("Cargo.lock").is_file(),
                },
            );
        }
    }
    for (dir, m) in &manifests {
        if m.get("package").is_none() {
            continue;
        }
        let explicit = m
            .get("package")
            .and_then(|p| p.get("workspace"))
            .and_then(Value::as_str)
            .and_then(|rel| relpath::normalize(dir, rel));
        let workspace = explicit.filter(|w| roots.contains_key(w)).or_else(|| {
            relpath::ancestors(dir)
                .into_iter()
                .find(|a| roots.contains_key(a))
                .filter(|a| is_member(&manifests, a, dir))
        });
        match workspace {
            Some(ws) => {
                owner.insert(dir.clone(), ws);
            }
            None => {
                owner.insert(dir.clone(), dir.clone());
                roots.entry(dir.clone()).or_insert_with(|| CargoProject {
                    dir: dir.clone(),
                    manifest: relpath::join(dir, "Cargo.toml"),
                    workspace: false,
                    members: Vec::new(),
                    lock: relpath::under(root, dir).join("Cargo.lock").is_file(),
                });
            }
        }
    }
    // Path dependencies inside a workspace directory are members (fixpoint, bounded).
    for _ in 0..8 {
        let mut changed = false;
        let pairs: Vec<(String, String)> = owner.iter().map(|(a, b)| (a.clone(), b.clone())).collect();
        for (pkg_dir, ws) in pairs {
            let Some(m) = manifests.get(&pkg_dir) else { continue };
            for dep_dir in path_dependencies(m, &pkg_dir) {
                if !manifests.contains_key(&dep_dir) || !relpath::within(&dep_dir, &ws) {
                    continue;
                }
                if owner.get(&dep_dir) != Some(&ws) && owner.get(&dep_dir) == Some(&dep_dir) && dep_dir != ws
                {
                    owner.insert(dep_dir.clone(), ws.clone());
                    if roots.get(&dep_dir).is_some_and(|r| !r.workspace) {
                        roots.remove(&dep_dir);
                    }
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    for (pkg_dir, ws) in &owner {
        if let Some(project) = roots.get_mut(ws) {
            project.members.push(pkg_dir.clone());
        }
    }

    // Required = roots not nested inside another root's directory.
    let root_dirs: Vec<String> = roots.keys().cloned().collect();
    for (dir, project) in roots {
        let nested = root_dirs
            .iter()
            .any(|other| *other != dir && relpath::within(&dir, other));
        if nested {
            layout.subprojects.push(SubProject {
                dir: dir.clone(),
                reason: "a separate Cargo project (not a member of the enclosing workspace)".into(),
            });
        } else {
            layout.projects.push(project);
        }
    }

    // MSRV of the required packages.
    let required: BTreeSet<&str> = layout
        .projects
        .iter()
        .flat_map(|p| p.members.iter().map(String::as_str))
        .collect();
    for (dir, m) in &manifests {
        if !required.contains(dir.as_str()) {
            continue;
        }
        let Some(pkg) = m.get("package") else { continue };
        let text = match pkg.get("rust-version") {
            Some(Value::String(s)) => Some((s.clone(), relpath::join(dir, "Cargo.toml"))),
            Some(Value::Object(o)) if o.get("workspace").and_then(Value::as_bool) == Some(true) => owner
                .get(dir)
                .and_then(|ws| manifests.get(ws).map(|wm| (ws, wm)))
                .and_then(|(ws, wm)| {
                    wm.get("workspace")?
                        .get("package")?
                        .get("rust-version")?
                        .as_str()
                        .map(|s| (s.to_string(), relpath::join(ws, "Cargo.toml")))
                }),
            _ => None,
        };
        if let Some((text, manifest)) = text {
            if let Some(v) = Version::parse(&text) {
                if layout.rust_version.as_ref().is_none_or(|(cur, _)| v > *cur) {
                    layout.rust_version = Some((v, format!("rust-version in {manifest}")));
                }
            }
        }
    }
    layout
}

/// Whether the package in `dir` is a member of the workspace rooted at `ws`.
pub(super) fn is_member(manifests: &BTreeMap<String, Value>, ws: &str, dir: &str) -> bool {
    if ws == dir {
        return true;
    }
    let Some(w) = manifests.get(ws).and_then(|m| m.get("workspace")) else {
        return false;
    };
    let rel = relpath::relative_to(dir, ws);
    let matches = |key: &str| {
        w.get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items.iter().filter_map(Value::as_str).any(|pattern| {
                    let pattern = pattern.trim_start_matches("./").trim_end_matches('/');
                    pattern == rel
                        || globset::Glob::new(pattern)
                            .map(|g| g.compile_matcher().is_match(&rel))
                            .unwrap_or(false)
                        || rel.starts_with(&format!("{pattern}/")) && key == "exclude"
                })
            })
            .unwrap_or(false)
    };
    matches("members") && !matches("exclude")
}

/// Dependency package names declared by a manifest (all dependency tables, incl. target ones).
pub(super) fn declared_dependencies(m: &Value) -> Vec<String> {
    let mut out = BTreeSet::new();
    let mut tables: Vec<&Value> = Vec::new();
    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        tables.extend(m.get(key));
    }
    if let Some(targets) = m.get("target").and_then(Value::as_object) {
        for t in targets.values() {
            for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
                tables.extend(t.get(key));
            }
        }
    }
    for table in tables {
        if let Some(obj) = table.as_object() {
            for (k, v) in obj {
                let name = v.get("package").and_then(Value::as_str).unwrap_or(k);
                out.insert(name.to_string());
            }
        }
    }
    out.into_iter().collect()
}

/// Directories (relative) of the path dependencies of the manifest in `dir`.
pub(super) fn path_dependencies(m: &Value, dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(obj) = m.get(key).and_then(Value::as_object) {
            for v in obj.values() {
                if let Some(p) = v.get("path").and_then(Value::as_str) {
                    out.extend(relpath::normalize(dir, p));
                }
            }
        }
    }
    out
}

/// `Cargo.lock` packages: `(name, version, is_registry, dependencies as written)`, read with
/// the TOML parser (`[[package]]` tables with `name`, `version`, `source`, `dependencies`).
pub fn cargo_lock_packages(lock: &str) -> Vec<LockPackage> {
    let Some(value) = trace_core::formats::toml_value(lock) else {
        return Vec::new();
    };
    let Some(packages) = value.get("package").and_then(Value::as_array) else {
        return Vec::new();
    };
    packages
        .iter()
        .filter_map(|p| {
            let name = p.get("name")?.as_str()?.to_string();
            let version = p
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let registry = p
                .get("source")
                .and_then(Value::as_str)
                .is_some_and(|s| s.starts_with("registry+") || s.starts_with("sparse+"));
            let dependencies = p
                .get("dependencies")
                .and_then(Value::as_array)
                .map(|d| d.iter().filter_map(Value::as_str).map(str::to_string).collect())
                .unwrap_or_default();
            (!name.is_empty()).then_some(LockPackage {
                name,
                version,
                registry,
                dependencies,
            })
        })
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LockPackage {
    pub name: String,
    pub version: String,
    pub registry: bool,
    /// `"name"`, `"name version"` or `"name version (source)"` entries.
    pub dependencies: Vec<String>,
}

impl LockPackage {
    /// Resolve a dependency entry to the index of a package of `all`.
    pub(crate) fn resolve_dep(entry: &str, all: &[LockPackage]) -> Option<usize> {
        let mut parts = entry.split_whitespace();
        let name = parts.next()?;
        let version = parts.next();
        let hits: Vec<usize> = all
            .iter()
            .enumerate()
            .filter(|(_, p)| p.name == name && version.is_none_or(|v| p.version == v))
            .map(|(i, _)| i)
            .collect();
        (hits.len() == 1)
            .then(|| hits[0])
            .or_else(|| hits.first().copied().filter(|_| version.is_some()))
    }
}
