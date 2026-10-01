//! The static dependency check against the Cargo registry cache, vendored sources and the
//! toolchain's standard library sources.

use super::*;

/// Registry packages of the required projects' lockfiles that are not installed (module docs).
pub fn deps(cx: &DetectContext<'_>, toolchain: Option<&Toolchain>) -> DepsReport {
    let layout = cargo_layout(cx.root, cx.files);
    let mut report = DepsReport::none_declared();
    report.hint = DEPS_HINT.to_string();
    report.subprojects = layout.subprojects.clone();
    let cargo_home = cargo_home(cx);
    if let Some(home) = &cargo_home {
        for (_, index) in subdirs(&home.join("registry").join("src")) {
            report.roots.push(LibraryRoot {
                path: index,
                kind: LibraryKind::Dependency,
                ecosystem: EcosystemId::Rust,
                layout: "cargo_registry",
                version: None,
            });
        }
    }
    if let Some(t) = toolchain {
        if let Some(src) = t.facts.get("rust_src") {
            report.roots.push(LibraryRoot {
                path: PathBuf::from(src),
                kind: LibraryKind::Stdlib,
                ecosystem: EcosystemId::Rust,
                layout: "rust_src",
                version: t.version.as_ref().map(|v| v.text.clone()),
            });
        }
    }
    if layout.projects.is_empty() {
        return report;
    }
    let host = HostCfg::from_triple(
        &toolchain
            .and_then(|t| t.facts.get("host").cloned())
            .unwrap_or_else(|| host_triple(cx.platform)),
    );
    let present = cargo_home.as_deref().map(registry_present).unwrap_or_default();
    let vendored = vendored_dir(cx.root);
    let mut fingerprint = blake3::Hasher::new();
    if let Some(home) = &cargo_home {
        fingerprint.update(home.display().to_string().as_bytes());
    }
    let mut declared_any = false;
    let mut missing: BTreeSet<String> = BTreeSet::new();
    for project in &layout.projects {
        let lock_path = relpath::under(cx.root, &project.dir).join("Cargo.lock");
        let Ok(lock_text) = std::fs::read_to_string(&lock_path) else {
            let declares = layout
                .packages
                .iter()
                .filter(|p| project.members.contains(&p.dir))
                .any(|p| !p.dependencies.is_empty());
            if declares {
                declared_any = true;
                report.notes.push(format!(
                    "{} has no Cargo.lock: Cargo checks its dependencies when trace runs",
                    project.manifest
                ));
            }
            continue;
        };
        fingerprint.update(lock_text.as_bytes());
        let lock = cargo_lock_packages(&lock_text);
        if lock.iter().any(|p| p.registry) {
            declared_any = true;
        }
        let members: BTreeMap<String, PathBuf> = layout
            .packages
            .iter()
            .filter(|p| project.members.contains(&p.dir))
            .map(|p| (p.name.clone(), relpath::under(cx.root, &p.dir).join("Cargo.toml")))
            .collect();
        let is_present = |p: &LockPackage| {
            present.contains(&format!("{}-{}", p.name, p.version))
                || vendored.as_ref().is_some_and(|v| {
                    v.join(&p.name).is_dir() || v.join(format!("{}-{}", p.name, p.version)).is_dir()
                })
        };
        let manifest_of = |p: &LockPackage| -> Option<Value> {
            if !p.registry {
                return members.get(&p.name).and_then(|m| relpath::read_toml(m));
            }
            let home = cargo_home.as_ref()?;
            subdirs(&home.join("registry").join("src"))
                .into_iter()
                .map(|(_, index)| index.join(format!("{}-{}", p.name, p.version)).join("Cargo.toml"))
                .find(|m| m.is_file())
                .and_then(|m| relpath::read_toml(&m))
        };
        for name in missing_needed(&lock, &is_present, &manifest_of, &host) {
            missing.insert(name);
        }
    }
    fingerprint.update(format!("{}", present.len()).as_bytes());
    report.fingerprint = crate::hex(fingerprint);
    report.status = if !missing.is_empty() {
        DepsStatus::Missing
    } else if declared_any {
        DepsStatus::Installed
    } else {
        DepsStatus::NoneDeclared
    };
    report.missing = missing.into_iter().collect();
    report
}

/// Registry packages of the standard library's own `Cargo.lock` (rust-analyzer loads the
/// sysroot as a Cargo workspace) that the host needs and the Cargo home lacks. `None` when the
/// toolchain has no standard library source (checked separately).
pub fn std_deps_missing(toolchain: &Toolchain, cargo_home: Option<&Path>) -> Option<Vec<String>> {
    let library = PathBuf::from(toolchain.facts.get("rust_src")?);
    let lock_text = std::fs::read_to_string(library.join("Cargo.lock")).ok()?;
    let lock = cargo_lock_packages(&lock_text);
    let present = cargo_home.map(registry_present).unwrap_or_default();
    let host = HostCfg::from_triple(
        &toolchain
            .facts
            .get("host")
            .cloned()
            .unwrap_or_else(|| host_triple(&Platform::current())),
    );
    let path_manifests: BTreeMap<String, PathBuf> = subdirs(&library)
        .into_iter()
        .map(|(_, d)| d.join("Cargo.toml"))
        .filter(|m| m.is_file())
        .filter_map(|m| {
            let name = relpath::read_toml(&m)?
                .get("package")?
                .get("name")?
                .as_str()?
                .to_string();
            Some((name, m))
        })
        .collect();
    let is_present = |p: &LockPackage| present.contains(&format!("{}-{}", p.name, p.version));
    let manifest_of = |p: &LockPackage| -> Option<Value> {
        if !p.registry {
            return path_manifests.get(&p.name).and_then(|m| relpath::read_toml(m));
        }
        let home = cargo_home?;
        subdirs(&home.join("registry").join("src"))
            .into_iter()
            .map(|(_, index)| index.join(format!("{}-{}", p.name, p.version)).join("Cargo.toml"))
            .find(|m| m.is_file())
            .and_then(|m| relpath::read_toml(&m))
    };
    Some(missing_needed(&lock, &is_present, &manifest_of, &host))
}

/// `<name>-<version>` of every crate in the Cargo home (downloaded `.crate` or unpacked source).
pub(super) fn registry_present(home: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (_, index) in subdirs(&home.join("registry").join("cache")) {
        for (name, _) in entries(&index) {
            if let Some(stem) = name.strip_suffix(".crate") {
                out.insert(stem.to_string());
            }
        }
    }
    for (_, index) in subdirs(&home.join("registry").join("src")) {
        for (name, _) in subdirs(&index) {
            out.insert(name);
        }
    }
    out
}

/// The vendored source directory configured in `<root>/.cargo/config.toml` (`[source.crates-io]
/// replace-with` -> `[source.<name>] directory`).
pub(super) fn vendored_dir(root: &Path) -> Option<PathBuf> {
    let config = ["config.toml", "config"]
        .iter()
        .find_map(|n| relpath::read_toml(&root.join(".cargo").join(n)))?;
    let sources = config.get("source")?;
    let replacement = sources.get("crates-io")?.get("replace-with")?.as_str()?;
    let dir = sources.get(replacement)?.get("directory")?.as_str()?;
    let path = PathBuf::from(dir);
    Some(if path.is_absolute() { path } else { root.join(path) })
}

/// Names (`name@version`) of registry packages of `lock` that are missing and needed on the host.
/// Needed = reachable from the local packages (and from registry packages nothing depends on)
/// through dependency edges that the depending package declares in an untargeted dependency
/// table, in a target table whose `cfg` holds (or cannot be decided) on the host, or whose
/// manifest cannot be read. A package that is present but itself not needed on the host (e.g.
/// fetched earlier for another target) never makes its dependencies needed. A missing package
/// is reported itself; its own dependencies are not followed (its manifest is not there).
pub(super) fn missing_needed(
    lock: &[LockPackage],
    is_present: &dyn Fn(&LockPackage) -> bool,
    manifest_of: &dyn Fn(&LockPackage) -> Option<Value>,
    host: &HostCfg,
) -> Vec<String> {
    if lock.iter().all(|p| !p.registry || is_present(p)) {
        return Vec::new();
    }
    let children: Vec<Vec<usize>> = lock
        .iter()
        .map(|p| {
            p.dependencies
                .iter()
                .filter_map(|d| LockPackage::resolve_dep(d, lock))
                .collect()
        })
        .collect();
    let mut has_parent = vec![false; lock.len()];
    for c in children.iter().flatten() {
        has_parent[*c] = true;
    }
    let mut needed: Vec<bool> = lock
        .iter()
        .enumerate()
        .map(|(i, p)| !p.registry || !has_parent[i])
        .collect();
    let mut stack: Vec<usize> = (0..lock.len()).filter(|&i| needed[i]).collect();
    // Bounded by the lockfile: every package is pushed at most once.
    while let Some(i) = stack.pop() {
        let parent = &lock[i];
        if parent.registry && !is_present(parent) {
            continue;
        }
        let manifest = manifest_of(parent);
        for &c in &children[i] {
            if needed[c] {
                continue;
            }
            let declared = match &manifest {
                None => true,
                Some(v) => declares_for_host(v, &lock[c].name, !parent.registry, host) != Tri::False,
            };
            if declared {
                needed[c] = true;
                stack.push(c);
            }
        }
    }
    let mut out: Vec<String> = lock
        .iter()
        .enumerate()
        .filter(|(i, p)| needed[*i] && p.registry && !is_present(p))
        .map(|(_, p)| format!("{}@{}", p.name, p.version))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Whether manifest `m` declares a dependency on package `name` for the host.
pub(super) fn declares_for_host(m: &Value, name: &str, with_dev: bool, host: &HostCfg) -> Tri {
    let keys: &[&str] = if with_dev {
        &["dependencies", "build-dependencies", "dev-dependencies"]
    } else {
        &["dependencies", "build-dependencies"]
    };
    let in_table = |t: &Value| {
        t.as_object().is_some_and(|obj| {
            obj.iter()
                .any(|(k, v)| v.get("package").and_then(Value::as_str).unwrap_or(k) == name)
        })
    };
    let mut result = Tri::False;
    for key in keys {
        if m.get(*key).is_some_and(in_table) {
            return Tri::True;
        }
    }
    if let Some(targets) = m.get("target").and_then(Value::as_object) {
        for (spec, t) in targets {
            if keys.iter().any(|k| t.get(*k).is_some_and(in_table)) {
                result = result.or(eval_target(spec, host));
                if result == Tri::True {
                    return result;
                }
            }
        }
    }
    result
}
