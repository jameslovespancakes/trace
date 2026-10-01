//! Node (JavaScript / TypeScript): `node_modules` detection and the declared-vs-installed
//! dependency check. Read-only: `package.json`, workspace declarations
//! (`workspaces`, `pnpm-workspace.yaml`, `lerna.json`), install markers and directory entries.
//! No package manager, no install script and no project code runs; the user's Node is not
//! needed at all (the TypeScript worker runs on trace's own Node).
//!
//! Required projects: the root `package.json` plus its declared workspace members (without a
//! root `package.json`: the top-most `package.json` directories and their members). Every
//! other nested `package.json` is a sub-project (analysed with the root's dependencies, its
//! missing dependencies are a status line). Required: `dependencies` + `devDependencies`
//! (package managers install both by default); `optionalDependencies`/`peerDependencies` are
//! not. A package is installed when `node_modules/<name>/package.json` exists in the project
//! directory or one of its ancestors up to the root (npm hoisting, pnpm per-importer
//! symlinks, yarn classic, bun), or in the `--env` directory. Workspace-internal specs
//! (`workspace:`, `link:`, `file:`, `portal:`) and member names are satisfied by the member.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use trace_core::formats::yaml;
use trace_core::Language;

use crate::python::VENV_NAMES;
use crate::{
    entries, hex, subdirs, DepsReport, DepsStatus, DetectContext, EcosystemId, LibraryKind, LibraryRoot,
    SubProject, Toolchain, ToolchainStatus, SKIP_DIRS,
};

/// Maximum directory depth searched for `package.json` + `node_modules`.
const NODE_DEPTH: usize = 4;
/// Maximum directory depth searched for workspace members and sub-projects.
const PROJECT_DEPTH: usize = 6;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NodeModules {
    /// Repository-relative directory of the `package.json` (`""` = the root).
    pub dir: String,
    /// The `node_modules` directory.
    pub path: PathBuf,
}

/// A project shape the TypeScript compiler cannot resolve (detected up front).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeLimit {
    /// Yarn Plug'n'Play: resolution needs executing `.pnp.cjs`.
    YarnPnp,
    /// A Deno-only project (`deno.json`, no `package.json`, no `node_modules`).
    DenoOnly,
}

/// Everything the TypeScript preflight needs, computed once.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NodeSetup {
    pub limit: Option<NodeLimit>,
    /// Required project directories (relative, `""` = root), sorted.
    pub projects: Vec<String>,
    /// Every `node_modules` next to a `package.json` (mapped into the worker), plus the
    /// `--env` directory as the root's `node_modules`.
    pub node_modules: Vec<NodeModules>,
    /// `--env` pointed to a directory that is no `node_modules`.
    pub env_not_found: Option<PathBuf>,
    pub deps: DepsReport,
}

/// The user's Node is never needed: the worker runs on trace's own Node runtime.
pub fn toolchain(_cx: &DetectContext<'_>) -> ToolchainStatus {
    ToolchainStatus::NotNeeded
}

/// Declared dependencies against the installed `node_modules` (see [`setup`]).
pub fn deps(cx: &DetectContext<'_>, _toolchain: Option<&Toolchain>) -> DepsReport {
    setup(cx).deps
}

/// A `node_modules` directory, or a project directory holding `package.json` and
/// `node_modules`.
pub(crate) fn accepts_env_path(path: &Path) -> bool {
    env_modules_dir(path).is_some()
}

fn env_modules_dir(path: &Path) -> Option<PathBuf> {
    if !path.is_dir() {
        return None;
    }
    if path.file_name().is_some_and(|n| n == "node_modules") {
        return Some(path.to_path_buf());
    }
    let nested = path.join("node_modules");
    (path.join("package.json").is_file() && nested.is_dir()).then_some(nested)
}

/// Project shape, required projects, `node_modules` mappings and the dependency report.
pub fn setup(cx: &DetectContext<'_>) -> NodeSetup {
    let root = cx.root;
    let allowed = |p: &Path| cx.allowed(p);
    let walkable = script_dirs(cx.files);
    let mut node_modules = find_node_modules(root, &allowed, walkable.as_ref());
    let mut env_not_found = None;
    let mut override_dir = None;
    if let Some(path) = cx.env_override {
        match env_modules_dir(path) {
            Some(dir) => {
                node_modules.retain(|m| !m.dir.is_empty());
                node_modules.insert(
                    0,
                    NodeModules {
                        dir: String::new(),
                        path: dir.clone(),
                    },
                );
                override_dir = Some(dir);
            }
            None => env_not_found = Some(path.to_path_buf()),
        }
    }
    let limit = limit(root, &node_modules);
    let mut all = package_dirs(root, cx, walkable.as_ref());
    // A package.json matters only when an analysed JavaScript/TypeScript file lies in its
    // directory tree: folders excluded in trace's settings (or git-ignored) hold no
    // inventoried file, so their manifests never make dependencies required.
    if !cx.files.is_empty() {
        all.retain(|d| {
            cx.files.iter().any(|(f, l)| {
                matches!(l, Language::JavaScript | Language::TypeScript | Language::Tsx)
                    && is_below(&f.replace('\\', "/"), d)
            })
        });
    }
    let projects = required_projects(root, &all);
    let deps = check(cx, &projects, &all, override_dir.as_deref(), &node_modules);
    NodeSetup {
        limit,
        projects,
        node_modules,
        env_not_found,
        deps,
    }
}

/// The directories holding an inventoried JavaScript / TypeScript file, and their ancestors
/// (relative, `""` = the root); `None` without an inventory (every directory is walked).
/// The project walks enter only these directories, so they follow the inventory's own
/// ignore rules (`.gitignore` / `.ignore`, hidden and dependency folders, the user's
/// `exclude` globs): an ignored folder holds no inventoried file, is never entered, and
/// none of its manifests or `node_modules` is a project, a sub-project or a mapping.
fn script_dirs(files: &[(&str, Language)]) -> Option<BTreeSet<String>> {
    if files.is_empty() {
        return None;
    }
    let mut dirs = BTreeSet::new();
    dirs.insert(String::new());
    for (file, language) in files {
        if !matches!(language, Language::JavaScript | Language::TypeScript | Language::Tsx) {
            continue;
        }
        let file = file.replace('\\', "/");
        let mut rel = file.as_str();
        // Ancestors are inserted bottom-up, so a known directory has all its ancestors.
        while let Some((parent, _)) = rel.rsplit_once('/') {
            if !dirs.insert(parent.to_string()) {
                break;
            }
            rel = parent;
        }
    }
    Some(dirs)
}

/// Whether a walk may enter the relative directory `rel` ([`script_dirs`]).
fn enters(walkable: Option<&BTreeSet<String>>, rel: &str) -> bool {
    walkable.is_none_or(|dirs| dirs.contains(rel))
}

/// Every `node_modules` next to a `package.json`, at the root and in package directories up
/// to `NODE_DEPTH` levels deep (only directories the inventory entered, [`script_dirs`]),
/// sorted by directory.
pub(crate) fn find_node_modules(
    root: &Path,
    allowed: &dyn Fn(&Path) -> bool,
    walkable: Option<&BTreeSet<String>>,
) -> Vec<NodeModules> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, rel, depth)) = stack.pop() {
        let modules = dir.join("node_modules");
        if dir.join("package.json").is_file() && modules.is_dir() && allowed(&modules) {
            out.push(NodeModules {
                dir: rel.clone(),
                path: modules,
            });
        }
        if depth >= NODE_DEPTH {
            continue;
        }
        for (name, path) in subdirs(&dir) {
            if name.starts_with('.')
                || SKIP_DIRS.contains(&name.as_str())
                || VENV_NAMES.contains(&name.as_str())
            {
                continue;
            }
            let child = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if !enters(walkable, &child) {
                continue;
            }
            stack.push((path, child, depth + 1));
        }
    }
    out.sort_by(|a, b| a.dir.cmp(&b.dir));
    out
}

/// Yarn Plug'n'Play or a Deno-only project.
fn limit(root: &Path, node_modules: &[NodeModules]) -> Option<NodeLimit> {
    let has_root_modules = node_modules.iter().any(|m| m.dir.is_empty());
    if root.join(".pnp.cjs").is_file() || root.join(".pnp.js").is_file() {
        return Some(NodeLimit::YarnPnp);
    }
    if !has_root_modules && root.join("yarn.lock").is_file() {
        if let Ok(text) = fs::read_to_string(root.join(".yarnrc.yml")) {
            // Yarn Berry defaults to Plug'n'Play unless `nodeLinker` says otherwise.
            let linker = yaml::parse(&text)
                .and_then(|v| v.get("nodeLinker").and_then(Value::as_str).map(str::to_string));
            if linker.as_deref().is_none_or(|l| l == "pnp") {
                return Some(NodeLimit::YarnPnp);
            }
        }
    }
    let deno = root.join("deno.json").is_file() || root.join("deno.jsonc").is_file();
    if deno && !root.join("package.json").is_file() && !has_root_modules {
        return Some(NodeLimit::DenoOnly);
    }
    None
}

/// One `package.json`.
#[derive(Clone, Debug, Default)]
struct Manifest {
    /// Relative directory (`""` = root).
    dir: String,
    name: Option<String>,
    /// (name, spec) of `dependencies` + `devDependencies`.
    deps: Vec<(String, String)>,
    /// Workspace globs declared by this manifest (`workspaces`, pnpm, lerna).
    workspaces: Vec<String>,
    bytes: Vec<u8>,
}

fn read_manifest(root: &Path, dir: &str) -> Option<Manifest> {
    let base = if dir.is_empty() {
        root.to_path_buf()
    } else {
        root.join(dir)
    };
    let bytes = fs::read(base.join("package.json")).ok()?;
    let doc: Value = serde_json::from_slice(&bytes)
        .ok()
        .or_else(|| trace_core::formats::jsonc::parse(&String::from_utf8_lossy(&bytes)))?;
    let mut m = Manifest {
        dir: dir.to_string(),
        name: doc.get("name").and_then(Value::as_str).map(str::to_string),
        bytes,
        ..Manifest::default()
    };
    for key in ["dependencies", "devDependencies"] {
        if let Some(obj) = doc.get(key).and_then(Value::as_object) {
            for (name, spec) in obj {
                m.deps
                    .push((name.clone(), spec.as_str().unwrap_or_default().to_string()));
            }
        }
    }
    match doc.get("workspaces") {
        Some(Value::Array(a)) => m
            .workspaces
            .extend(a.iter().filter_map(Value::as_str).map(str::to_string)),
        Some(Value::Object(o)) => {
            if let Some(Value::Array(a)) = o.get("packages") {
                m.workspaces
                    .extend(a.iter().filter_map(Value::as_str).map(str::to_string));
            }
        }
        _ => {}
    }
    if let Ok(text) = fs::read_to_string(base.join("pnpm-workspace.yaml")) {
        m.bytes.extend_from_slice(text.as_bytes());
        if let Some(Value::Array(a)) = yaml::parse(&text).and_then(|v| v.get("packages").cloned()) {
            m.workspaces
                .extend(a.iter().filter_map(Value::as_str).map(str::to_string));
        }
    }
    if let Ok(bytes) = fs::read(base.join("lerna.json")) {
        if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
            if let Some(Value::Array(a)) = v.get("packages") {
                m.workspaces
                    .extend(a.iter().filter_map(Value::as_str).map(str::to_string));
            }
        }
    }
    Some(m)
}

/// Every directory (relative) with a `package.json`, outside dependency and build folders
/// and outside folders the inventory never entered ([`script_dirs`]).
fn package_dirs(root: &Path, cx: &DetectContext<'_>, walkable: Option<&BTreeSet<String>>) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, rel, depth)) = stack.pop() {
        if dir.join("package.json").is_file() && cx.allowed(&dir) {
            out.push(rel.clone());
        }
        if depth >= PROJECT_DEPTH {
            continue;
        }
        for (name, path) in subdirs(&dir) {
            if name.starts_with('.')
                || SKIP_DIRS.contains(&name.as_str())
                || VENV_NAMES.contains(&name.as_str())
            {
                continue;
            }
            let child = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if !enters(walkable, &child) {
                continue;
            }
            stack.push((path, child, depth + 1));
        }
    }
    out.sort();
    out
}

/// The root project (or the top-most projects without a root manifest) plus their declared
/// workspace members.
fn required_projects(root: &Path, all: &[String]) -> Vec<String> {
    let tops: Vec<String> = if all.iter().any(String::is_empty) {
        vec![String::new()]
    } else {
        all.iter()
            .filter(|d| !all.iter().any(|o| o != *d && is_below(d, o)))
            .cloned()
            .collect()
    };
    let mut required: BTreeSet<String> = tops.iter().cloned().collect();
    for top in &tops {
        let Some(m) = read_manifest(root, top) else { continue };
        for member in members(&m, all) {
            required.insert(member);
        }
    }
    required.into_iter().collect()
}

/// `d` is strictly below `parent` (both relative, `""` = root).
fn is_below(d: &str, parent: &str) -> bool {
    if parent.is_empty() {
        return !d.is_empty();
    }
    d.len() > parent.len() && d.starts_with(parent) && d.as_bytes()[parent.len()] == b'/'
}

/// Directories matched by a manifest's workspace globs (`!` globs exclude).
fn members(m: &Manifest, all: &[String]) -> Vec<String> {
    if m.workspaces.is_empty() {
        return Vec::new();
    }
    let mut include = globset::GlobSetBuilder::new();
    let mut exclude = globset::GlobSetBuilder::new();
    for g in &m.workspaces {
        let (negated, pattern) = match g.strip_prefix('!') {
            Some(p) => (true, p),
            None => (false, g.as_str()),
        };
        let pattern = pattern.trim_start_matches("./").trim_end_matches('/');
        let full = if m.dir.is_empty() {
            pattern.to_string()
        } else {
            format!("{}/{pattern}", m.dir)
        };
        let Ok(glob) = globset::GlobBuilder::new(&full).literal_separator(true).build() else {
            continue;
        };
        if negated {
            exclude.add(glob);
        } else {
            include.add(glob);
        }
    }
    let (Ok(include), Ok(exclude)) = (include.build(), exclude.build()) else {
        return Vec::new();
    };
    all.iter()
        .filter(|d| !d.is_empty() && **d != m.dir)
        .filter(|d| include.is_match(d.as_str()) && !exclude.is_match(d.as_str()))
        .cloned()
        .collect()
}

/// Workspace-internal specs are satisfied by the workspace itself.
fn internal_spec(spec: &str) -> bool {
    ["workspace:", "link:", "file:", "portal:"]
        .iter()
        .any(|p| spec.starts_with(p))
}

/// `node_modules/<name>/package.json` in `dir` or an ancestor up to the root, or in the
/// `--env` directory.
fn installed(root: &Path, dir: &str, name: &str, override_dir: Option<&Path>) -> bool {
    let rel_name: PathBuf = name.split('/').collect();
    let mut current = if dir.is_empty() {
        root.to_path_buf()
    } else {
        root.join(dir)
    };
    loop {
        if current
            .join("node_modules")
            .join(&rel_name)
            .join("package.json")
            .is_file()
        {
            return true;
        }
        if current == root {
            break;
        }
        match current.parent() {
            Some(p) if p.starts_with(root) => current = p.to_path_buf(),
            _ => break,
        }
    }
    override_dir.is_some_and(|o| o.join(&rel_name).join("package.json").is_file())
}

/// Hint by lockfile (first match).
fn hint(root: &Path) -> String {
    let has = |n: &str| root.join(n).is_file();
    if has("pnpm-lock.yaml") {
        "pnpm install".into()
    } else if has("yarn.lock") {
        "yarn install".into()
    } else if has("bun.lock") || has("bun.lockb") {
        "bun install".into()
    } else {
        "npm install".into()
    }
}

/// Missing dependency names of one manifest.
fn missing_of(
    root: &Path,
    m: &Manifest,
    member_names: &BTreeSet<String>,
    override_dir: Option<&Path>,
) -> Vec<String> {
    let mut missing: Vec<String> = m
        .deps
        .iter()
        .filter(|(name, spec)| !internal_spec(spec) && !member_names.contains(name))
        .filter(|(name, _)| !installed(root, &m.dir, name, override_dir))
        .map(|(name, _)| name.clone())
        .collect();
    missing.sort();
    missing.dedup();
    missing
}

fn check(
    cx: &DetectContext<'_>,
    projects: &[String],
    all: &[String],
    override_dir: Option<&Path>,
    node_modules: &[NodeModules],
) -> DepsReport {
    let root = cx.root;
    let manifests: Vec<Manifest> = projects.iter().filter_map(|d| read_manifest(root, d)).collect();
    let member_names: BTreeSet<String> = manifests.iter().filter_map(|m| m.name.clone()).collect();
    let mut declared = false;
    let mut missing: Vec<String> = Vec::new();
    for m in &manifests {
        declared |= !m.deps.is_empty();
        missing.extend(missing_of(root, m, &member_names, override_dir));
    }
    missing.sort();
    missing.dedup();
    let hint = hint(root);
    let mut notes = Vec::new();
    let subprojects: Vec<SubProject> = all
        .iter()
        .filter(|d| !projects.contains(d))
        .map(|d| SubProject {
            dir: d.clone(),
            reason: "separate JavaScript project (package.json)".into(),
        })
        .collect();
    for sp in &subprojects {
        let Some(m) = read_manifest(root, &sp.dir) else { continue };
        let names: BTreeSet<String> = member_names.iter().cloned().chain(m.name.clone()).collect();
        let sub_missing = missing_of(root, &m, &names, override_dir);
        if !sub_missing.is_empty() {
            notes.push(format!(
                "{}: dependencies not installed ({}); its files are analyzed with the root's dependencies",
                sp.dir,
                sub_missing.join(", ")
            ));
        }
    }
    let status = if !missing.is_empty() {
        DepsStatus::Missing
    } else if declared {
        DepsStatus::Installed
    } else {
        DepsStatus::NoneDeclared
    };
    let roots = node_modules
        .iter()
        .map(|m| LibraryRoot {
            path: m.path.clone(),
            kind: LibraryKind::Dependency,
            ecosystem: EcosystemId::Node,
            layout: "node_modules",
            version: None,
        })
        .collect();
    let mut h = blake3::Hasher::new();
    for m in &manifests {
        h.update(m.dir.as_bytes());
        h.update(&m.bytes);
    }
    for m in node_modules {
        h.update(m.dir.as_bytes());
        h.update(m.path.to_string_lossy().as_bytes());
        h.update(&install_marker(&m.path));
    }
    DepsReport {
        status,
        missing,
        hint,
        roots,
        fingerprint: hex(h),
        subprojects,
        notes,
    }
}

/// The install marker of a `node_modules` (npm, pnpm, yarn classic / berry), else its
/// top-level listing: any install or upgrade changes it.
fn install_marker(modules: &Path) -> Vec<u8> {
    let markers = [
        ".package-lock.json",
        ".modules.yaml",
        ".pnpm/lock.yaml",
        ".yarn-integrity",
        ".yarn-state.yml",
    ];
    let mut out = Vec::new();
    for marker in markers {
        if let Ok(bytes) = fs::read(modules.join(marker)) {
            out.extend_from_slice(marker.as_bytes());
            out.extend_from_slice(&bytes);
        }
    }
    if out.is_empty() {
        for (name, _) in entries(modules) {
            out.extend_from_slice(name.as_bytes());
            out.push(b'\n');
        }
    }
    out
}

/// The Node ecosystem ([`crate::Ecosystem`]).
pub struct Node;

impl crate::Ecosystem for Node {
    fn id(&self) -> crate::EcosystemId {
        crate::EcosystemId::Node
    }

    fn accepts_env_path(&self, path: &Path) -> bool {
        accepts_env_path(path)
    }

    fn toolchain(&self, cx: &DetectContext<'_>) -> ToolchainStatus {
        toolchain(cx)
    }

    fn deps(&self, read: &DetectContext<'_>, toolchain: Option<&Toolchain>) -> DepsReport {
        deps(read, toolchain)
    }
}

#[cfg(test)]
#[path = "../../tests/unit/ecosystems/node.rs"]
mod tests;
