//! The declared-vs-installed dependency check, the install hint and the sub-projects.

use super::*;

pub(super) fn check_deps(
    cx: &DetectContext<'_>,
    env: Option<&PythonEnv>,
    python_version: Option<&str>,
    root_manifests: &Manifests,
) -> DepsReport {
    let marker_env = pep508::MarkerEnv::new(cx.platform, python_version);
    let mut own_names: BTreeSet<String> = BTreeSet::new();
    if let Some(n) = &root_manifests.name {
        own_names.insert(normalize(n));
    }
    // uv workspace members are required together with the root.
    let members = member_dirs(cx.root, &root_manifests.members);
    let member_manifests: Vec<Manifests> =
        members.iter().map(|m| Manifests::read(&cx.root.join(m))).collect();
    for m in &member_manifests {
        if let Some(n) = &m.name {
            own_names.insert(normalize(n));
        }
    }
    let mut declared: BTreeSet<Declared> = BTreeSet::new();
    let mut declares = root_manifests.declares;
    for m in std::iter::once(root_manifests).chain(member_manifests.iter()) {
        declares |= m.declares;
        for r in &m.requirements {
            if let Some(d) = evaluate(r, &marker_env) {
                if !own_names.contains(&d.name) {
                    declared.insert(d);
                }
            }
        }
    }
    let installed = env.map(installed_names).unwrap_or_default();
    let mut missing_runtime: Vec<String> = Vec::new();
    let mut missing_dev: Vec<String> = Vec::new();
    for d in &declared {
        if installed.contains(&d.name) {
            continue;
        }
        match d.group {
            Group::Runtime => missing_runtime.push(d.raw.clone()),
            Group::Dev => missing_dev.push(d.raw.clone()),
        }
    }
    missing_runtime.dedup();
    missing_dev.dedup();
    let has_runtime = declared.iter().any(|d| d.group == Group::Runtime);
    let status = if !missing_runtime.is_empty() {
        DepsStatus::Missing
    } else if has_runtime || (declares && env.is_some()) {
        DepsStatus::Installed
    } else {
        DepsStatus::NoneDeclared
    };
    let hint = hint(cx.root);
    let mut notes = Vec::new();
    if !missing_dev.is_empty() {
        notes.push(format!("test/dev dependencies not installed: {} ({hint})", missing_dev.join(", ")));
    }
    match env {
        Some(e) => notes.push(format!(
            "environment {}{}",
            e.root.display(),
            e.version
                .as_deref()
                .map(|v| format!(" (python {v})"))
                .unwrap_or_default()
        )),
        None if status == DepsStatus::Missing => {
            notes.push("no Python environment found for this project".to_string())
        }
        None => {}
    }
    // Sub-projects: nested manifests that are not the root or a workspace member.
    let subprojects = subprojects(cx, &members);
    for sp in &subprojects {
        let m = Manifests::read(&cx.root.join(&sp.dir));
        let own = m.name.as_deref().map(normalize);
        let missing: Vec<String> = m
            .requirements
            .iter()
            .filter(|r| r.group == Group::Runtime)
            .filter_map(|r| evaluate(r, &marker_env))
            .filter(|d| {
                !installed.contains(&d.name) && Some(&d.name) != own.as_ref() && !own_names.contains(&d.name)
            })
            .map(|d| d.raw)
            .collect();
        if !missing.is_empty() {
            notes.push(format!(
                "{}: dependencies not installed ({}); its files are analyzed with the root environment",
                sp.dir,
                missing.join(", ")
            ));
        }
    }
    let mut roots = Vec::new();
    if let Some(e) = env {
        for sp in &e.site_packages {
            roots.push(LibraryRoot {
                path: sp.clone(),
                kind: LibraryKind::Dependency,
                ecosystem: EcosystemId::Python,
                layout: "site_packages",
                version: None,
            });
        }
        for sp in &e.system_site_packages {
            roots.push(LibraryRoot {
                path: sp.clone(),
                kind: LibraryKind::Dependency,
                ecosystem: EcosystemId::Python,
                layout: "site_packages",
                version: None,
            });
        }
        if let Some(lib) = &e.stdlib {
            roots.push(LibraryRoot {
                path: lib.clone(),
                kind: LibraryKind::Stdlib,
                ecosystem: EcosystemId::Python,
                layout: "toolchain_stdlib",
                version: e.version.clone(),
            });
        }
    }
    let mut h = blake3::Hasher::new();
    h.update(&root_manifests.bytes);
    for m in &member_manifests {
        h.update(&m.bytes);
    }
    h.update(python_version.unwrap_or_default().as_bytes());
    if let Some(e) = env {
        h.update(e.root.to_string_lossy().as_bytes());
        h.update(&fs::read(e.root.join("pyvenv.cfg")).unwrap_or_default());
        for sp in e.library_dirs() {
            h.update(sp.to_string_lossy().as_bytes());
            for (name, _) in entries(&sp) {
                h.update(name.as_bytes());
                h.update(b"\n");
            }
        }
    }
    DepsReport {
        status,
        missing: missing_runtime,
        hint,
        roots,
        fingerprint: hex(h),
        subprojects,
        notes,
    }
}

/// Evaluate one requirement line: name + markers. `None` for lines that are not index
/// packages or whose markers exclude this platform / Python.
pub(super) fn evaluate(r: &Requirement, env: &pep508::MarkerEnv) -> Option<Declared> {
    let parsed = pep508::parse(&r.text)?;
    if let Some(marker) = &parsed.marker {
        if !pep508::evaluate(marker, env) {
            return None;
        }
    }
    Some(Declared {
        group: r.group,
        name: normalize(&parsed.name),
        raw: parsed.name,
    })
}

/// PEP 503 normalisation: lower case, runs of `-`, `_`, `.` -> `-`.
pub fn normalize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut sep = false;
    for c in name.trim().chars() {
        if matches!(c, '-' | '_' | '.') {
            sep = true;
            continue;
        }
        if sep && !out.is_empty() {
            out.push('-');
        }
        sep = false;
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// Normalised names of every distribution installed in the environment: `*.dist-info`,
/// `*.egg-info`, `*.egg-link`, editable `__editable__.<name>-*.pth`.
pub(super) fn installed_names(env: &PythonEnv) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for dir in env.library_dirs() {
        for (name, _) in entries(&dir) {
            if let Some(stem) = name.strip_suffix(".dist-info") {
                if let Some((n, _)) = stem.rsplit_once('-') {
                    out.insert(normalize(n));
                }
            } else if let Some(stem) = name.strip_suffix(".egg-info") {
                out.insert(normalize(stem.split('-').next().unwrap_or(stem)));
            } else if let Some(stem) = name.strip_suffix(".egg-link") {
                out.insert(normalize(stem));
            } else if let Some(rest) = name.strip_prefix("__editable__.") {
                let stem = rest.strip_suffix(".pth").unwrap_or(rest);
                let n = stem.split('-').next().unwrap_or(stem);
                // `__editable___pkg_1_0_finder.py` style helpers are not distributions.
                if !n.ends_with("_finder") {
                    out.insert(normalize(n));
                }
            }
        }
    }
    out
}

/// Hint by lockfile (first match).
pub(super) fn hint(root: &Path) -> String {
    let has = |n: &str| root.join(n).is_file();
    if has("uv.lock") {
        "uv sync".into()
    } else if has("poetry.lock") {
        "poetry install".into()
    } else if has("Pipfile.lock") || has("Pipfile") {
        "pipenv install --dev".into()
    } else if has("pdm.lock") {
        "pdm install".into()
    } else if has("environment.yml") {
        "conda env create -f environment.yml".into()
    } else if has("environment.yaml") {
        "conda env create -f environment.yaml".into()
    } else if has("requirements.txt") {
        "pip install -r requirements.txt".into()
    } else {
        "pip install -e .".into()
    }
}

/// uv workspace member directories (globs relative to the root, `/`-separated).
pub(super) fn member_dirs(root: &Path, globs: &[String]) -> Vec<String> {
    if globs.is_empty() {
        return Vec::new();
    }
    let mut builder = globset::GlobSetBuilder::new();
    for g in globs {
        if let Ok(glob) = globset::GlobBuilder::new(g.trim_end_matches('/'))
            .literal_separator(true)
            .build()
        {
            builder.add(glob);
        }
    }
    let Ok(set) = builder.build() else {
        return Vec::new();
    };
    manifest_dirs(root, SUBPROJECT_DEPTH)
        .into_iter()
        .filter(|d| !d.is_empty() && set.is_match(d.as_str()))
        .collect()
}

/// Nested project directories (relative) that are not the root or a member.
pub(super) fn subprojects(cx: &DetectContext<'_>, members: &[String]) -> Vec<SubProject> {
    manifest_dirs(cx.root, SUBPROJECT_DEPTH)
        .into_iter()
        .filter(|d| !d.is_empty() && !members.contains(d))
        .filter(|d| cx.allowed(&cx.root.join(d)))
        .map(|dir| {
            let file = ["pyproject.toml", "setup.py", "setup.cfg", "Pipfile", "requirements.txt"]
                .into_iter()
                .find(|f| cx.root.join(&dir).join(f).is_file())
                .unwrap_or("pyproject.toml");
            SubProject {
                reason: format!("separate Python project ({file})"),
                dir,
            }
        })
        .collect()
}

/// Directories (relative, `/`; `""` = root) holding a Python project manifest.
pub(super) fn manifest_dirs(root: &Path, depth: usize) -> Vec<String> {
    const MANIFESTS: &[&str] = &["pyproject.toml", "setup.py", "setup.cfg", "Pipfile"];
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, rel, d)) = stack.pop() {
        if MANIFESTS.iter().any(|m| dir.join(m).is_file())
            || (!rel.is_empty() && dir.join("requirements.txt").is_file() && has_python_files(&dir))
        {
            out.push(rel.clone());
        }
        if d >= depth {
            continue;
        }
        for (name, path) in subdirs(&dir) {
            if name.starts_with('.')
                || SKIP_DIRS.contains(&name.as_str())
                || VENV_NAMES.contains(&name.as_str())
                || name == "__pypackages__"
                || name == "site-packages"
                || path.join("pyvenv.cfg").is_file()
            {
                continue;
            }
            let child = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            stack.push((path, child, d + 1));
        }
    }
    out.sort();
    out
}

pub(super) fn has_python_files(dir: &Path) -> bool {
    entries(dir)
        .iter()
        .any(|(n, p)| p.is_file() && (n.ends_with(".py") || n.ends_with(".pyi")))
}
