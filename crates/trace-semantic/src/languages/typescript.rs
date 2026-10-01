//! TypeScript / JavaScript setup hooks (owner script).
//!
//! Preflight (every independent failure collected, PLAN decision 15):
//! 1. server: the TypeScript tool (compiler API + native compiler + bundled `@types/node`) and
//!    trace's Node runtime (default languages: installed automatically before preflight);
//! 2. project shape (`trace_env::node`): Yarn Plug'n'Play and Deno-only projects cannot be
//!    resolved by the compiler -> `Unsupported` with the fix;
//! 3. dependencies: `dependencies` + `devDependencies` of the root project and its workspace
//!    members must be installed (`node_modules` walking up; `--env` accepted) ->
//!    `DepsMissing` with the lockfile's hint; nested non-member projects are status lines;
//! 4. no build approval: nothing of the project runs.
//!
//! `Prepared.data` = [`crate::backends::typescript::TsSetup`]: the `node_modules` mappings, the project's
//! configuration texts (every `tsconfig*.json` / `jsconfig*.json` / `package.json` in the
//! directories of the analysed files and their ancestors, plus the configs their `extends` /
//! `references` name inside the repository) and whether trace's bundled Node types are
//! needed (no `@types/node` installed by the project).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use trace_core::formats::jsonc;
use trace_core::setup_error::SetupError;
use trace_core::Language;
use trace_env::node::NodeLimit;
use trace_env::EcosystemId;

use super::{default_prepared, read_context, Prepared, Server, SetupContext};
use crate::backends::fntype::FnTypeRoute;
use crate::backends::typescript::TsSetup;
use crate::setup::{deps_error, require_runtimes, require_server, Collect};

/// Largest configuration file read (bytes).
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
/// Bound on configuration files followed through `extends` / `references`.
const MAX_CONFIGS: usize = 500;

pub struct Hooks;

impl Server for Hooks {
    fn preflight(&self, cx: &SetupContext<'_>) -> Result<Prepared, SetupError> {
        let language = match cx.language() {
            Language::Tsx => Language::TypeScript,
            l => l,
        };
        let mut collect = Collect::default();
        collect.check(require_server(cx));
        collect.check(require_runtimes(cx));

        // node_modules, package.json and project configs are only READ (inside the repository
        // too): only the user's protected roots are off limits (`cx.tools.forbidden_roots` =
        // the repository, which guards what may be executed).
        let readable = trace_core::paths::forbidden_roots();
        let dcx = read_context(cx, EcosystemId::Node, &readable);
        let setup = trace_env::node::setup(&dcx);
        if let Some(limit) = setup.limit {
            collect.push(limit_error(language, limit));
        }
        match &setup.env_not_found {
            Some(path) => collect.push(SetupError::EnvNotFound {
                language: Some(language),
                path: path.clone(),
            }),
            None if setup.limit.is_none() => {
                if let Some(e) = deps_error(language, &setup.deps) {
                    collect.push(e);
                }
            }
            None => {}
        }

        let files: Vec<&str> = cx.files.iter().map(|(p, _)| *p).collect();
        let configs = project_configs(&cx.repo.root, &files, &readable);
        let installed = |package: &str| {
            setup
                .node_modules
                .iter()
                .any(|m| m.path.join(package).join("package.json").is_file())
        };
        let ts = TsSetup {
            node_modules: setup.node_modules.clone(),
            configs,
            bundle_node_types: !installed("@types/node"),
            bundle_undici_types: !installed("undici-types"),
        };
        let mut prepared = default_prepared(cx);
        prepared.library_roots = setup.deps.roots.clone();
        prepared.status = setup.deps.notes.clone();
        if !ts.configs.is_empty() {
            let names: Vec<&str> = ts
                .configs
                .iter()
                .map(|(p, _)| p.as_str())
                .filter(|p| !p.ends_with("package.json"))
                .collect();
            if !names.is_empty() {
                prepared
                    .status
                    .push(format!("project configuration: {}", names.join(", ")));
            }
        }
        if ts.bundle_node_types {
            prepared
                .status
                .push("Node types: trace's bundled @types/node (the project installs none)".into());
        }
        let mut h = blake3::Hasher::new();
        h.update(setup.deps.fingerprint.as_bytes());
        for (path, text) in &ts.configs {
            h.update(path.as_bytes());
            h.update(b"\0");
            h.update(text.as_bytes());
            h.update(b"\0");
        }
        h.update(&[u8::from(ts.bundle_node_types), u8::from(ts.bundle_undici_types)]);
        prepared.fingerprint = h.finalize().to_hex()[..32].to_string();
        prepared.data = Some(Arc::new(ts));
        collect.finish(prepared)
    }

    fn fn_type_route(&self, _language: Language) -> FnTypeRoute {
        FnTypeRoute::Checker
    }
}

/// The approved-style error for a project shape the compiler cannot resolve.
pub fn limit_error(language: Language, limit: NodeLimit) -> SetupError {
    match limit {
        NodeLimit::YarnPnp => SetupError::Unsupported {
            language,
            first: "This project uses Yarn Plug'n'Play, which trace cannot read.".into(),
            second: Some(
                "Set nodeLinker: node-modules in .yarnrc.yml, run yarn install and run trace again.".into(),
            ),
        },
        NodeLimit::DenoOnly => SetupError::Unsupported {
            language,
            first: "Deno projects are not supported yet (deno.json).".into(),
            second: None,
        },
    }
}

fn is_config_name(name: &str) -> bool {
    name == "package.json"
        || ((name.starts_with("tsconfig") || name.starts_with("jsconfig")) && name.ends_with(".json"))
}

/// Configuration texts (relative path, text), sorted by path: the `tsconfig*.json` /
/// `jsconfig*.json` / `package.json` files in every directory of `files` and its ancestors,
/// then the repository configs their `extends` / `references` name (bounded).
pub fn project_configs(root: &Path, files: &[&str], forbidden: &[PathBuf]) -> Vec<(String, String)> {
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    dirs.insert(String::new());
    for f in files {
        let mut rel = *f;
        while let Some((parent, _)) = rel.rsplit_once('/') {
            if !dirs.insert(parent.to_string()) {
                break;
            }
            rel = parent;
        }
    }
    let allowed = |p: &Path| !forbidden.iter().any(|f| p.starts_with(f));
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut queue: Vec<String> = Vec::new();
    for dir in &dirs {
        let abs = if dir.is_empty() {
            root.to_path_buf()
        } else {
            root.join(dir)
        };
        let Ok(entries) = std::fs::read_dir(&abs) else { continue };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| is_config_name(n))
            .collect();
        names.sort();
        for name in names {
            queue.push(if dir.is_empty() {
                name
            } else {
                format!("{dir}/{name}")
            });
        }
    }
    while let Some(rel) = queue.pop() {
        if out.contains_key(&rel) || out.len() >= MAX_CONFIGS {
            continue;
        }
        let abs = root.join(&rel);
        if !allowed(&abs) {
            continue;
        }
        let Some(text) = read_small(&abs) else { continue };
        let base = rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        if rel.ends_with(".json") && !rel.ends_with("package.json") {
            if let Some(doc) = jsonc::parse(&text) {
                for target in linked_configs(&doc) {
                    if let Some(next) = resolve_config(root, base, &target) {
                        queue.push(next);
                    }
                }
            }
        }
        out.insert(rel, text);
    }
    out.into_iter().collect()
}

/// Relative config paths named by `extends` (string or array) and `references[].path`.
fn linked_configs(doc: &Value) -> Vec<String> {
    let mut out = Vec::new();
    match doc.get("extends") {
        Some(Value::String(s)) => out.push(s.clone()),
        Some(Value::Array(a)) => out.extend(a.iter().filter_map(Value::as_str).map(str::to_string)),
        _ => {}
    }
    if let Some(Value::Array(refs)) = doc.get("references") {
        out.extend(
            refs.iter()
                .filter_map(|r| r.get("path").and_then(Value::as_str))
                .map(str::to_string),
        );
    }
    // Package names (`@tsconfig/node16/tsconfig.json`) are read from node_modules by the
    // worker; only relative paths are repository files.
    out.retain(|t| t.starts_with("./") || t.starts_with("../") || t == "." || t == "..");
    out
}

/// A relative config target (file, or directory holding `tsconfig.json`) inside the root.
fn resolve_config(root: &Path, base: &str, target: &str) -> Option<String> {
    let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for part in target.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    let mut rel = parts.join("/");
    let abs = root.join(&rel);
    if abs.is_dir() {
        rel = if rel.is_empty() {
            "tsconfig.json".into()
        } else {
            format!("{rel}/tsconfig.json")
        };
    } else if !abs.is_file() && !rel.ends_with(".json") {
        rel.push_str(".json");
    }
    root.join(&rel).is_file().then_some(rel)
}

fn read_small(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_CONFIG_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
#[path = "../../tests/unit/languages/typescript.rs"]
mod tests;
