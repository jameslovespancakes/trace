//! The static dependency check: restore outputs (`project.assets.json`) and `packages.config`
//! packages.

use super::*;

/// Restore state of one project.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProjectRestore {
    /// What is missing ("not restored", "net8.0 not restored", "Newtonsoft.Json 13.0.1").
    pub missing: Vec<String>,
    /// Package folders of the assets file (NuGet global packages folder first).
    pub package_folders: Vec<PathBuf>,
    /// The assets file (SDK-style projects).
    pub assets: Option<PathBuf>,
}

/// Check the restore outputs of one project (read-only).
pub(crate) fn project_restore(
    root: &Path,
    project: &CsProject,
    solution_dir: Option<&str>,
) -> ProjectRestore {
    let mut out = ProjectRestore::default();
    let dir = relpath::native(root, &project.dir);
    if project.packages_config {
        check_packages_config(root, project, solution_dir, &mut out);
    }
    if !project.sdk_style {
        return out;
    }
    let obj = match &project.extensions_path {
        Some(p) => {
            let p = PathBuf::from(p);
            if p.is_absolute() {
                p
            } else {
                dir.join(p)
            }
        }
        None => dir.join("obj"),
    };
    let assets = obj.join("project.assets.json");
    let file_name = project.rel.rsplit('/').next().unwrap_or(&project.rel);
    let Some(json) = relpath::read_text(&assets).and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
        out.missing.push("not restored".to_string());
        return out;
    };
    out.assets = Some(assets);
    if !obj.join(format!("{file_name}.nuget.g.props")).is_file() {
        out.missing.push("not restored".to_string());
    }
    let frameworks: BTreeSet<String> = json
        .pointer("/project/frameworks")
        .and_then(Value::as_object)
        .map(|m| m.keys().map(|k| k.to_ascii_lowercase()).collect())
        .unwrap_or_default();
    for tfm in &project.target_frameworks {
        if !frameworks.contains(tfm) {
            out.missing.push(format!("{tfm} not restored"));
        }
    }
    out.package_folders = json
        .get("packageFolders")
        .and_then(Value::as_object)
        .map(|m| m.keys().map(PathBuf::from).collect())
        .unwrap_or_default();
    if let Some(libraries) = json.get("libraries").and_then(Value::as_object) {
        for (key, lib) in libraries {
            if lib.get("type").and_then(Value::as_str) != Some("package") {
                continue;
            }
            let Some(path) = lib.get("path").and_then(Value::as_str) else {
                continue;
            };
            let (id, version) = key.split_once('/').unwrap_or((key.as_str(), ""));
            let lower_version = path.rsplit('/').next().unwrap_or(version).to_ascii_lowercase();
            let lower_id = id.to_ascii_lowercase();
            let installed = out.package_folders.iter().any(|folder| {
                let pkg = path.split('/').fold(folder.clone(), |p, part| p.join(part));
                pkg.join(".nupkg.metadata").is_file()
                    || pkg.join(format!("{lower_id}.{lower_version}.nupkg.sha512")).is_file()
            });
            if !installed {
                out.missing.push(format!("{id} {version}"));
            }
        }
    }
    out
}

/// Legacy `packages.config`: `<id>.<version>/` below the solution's `packages` folder (or the
/// nearest `packages` folder above the project).
pub(super) fn check_packages_config(
    root: &Path,
    project: &CsProject,
    solution_dir: Option<&str>,
    out: &mut ProjectRestore,
) {
    let config = relpath::native(root, &project.dir).join("packages.config");
    let Some(el) = relpath::read_text(&config).and_then(|t| xml::parse(&t)) else {
        return;
    };
    let mut folders: Vec<PathBuf> = Vec::new();
    if let Some(s) = solution_dir {
        folders.push(relpath::native(root, s).join("packages"));
    }
    let mut dir = project.dir.clone();
    loop {
        folders.push(relpath::native(root, &dir).join("packages"));
        if dir.is_empty() {
            break;
        }
        dir = relpath::parent(&dir).to_string();
    }
    let folder = folders.iter().find(|f| f.is_dir()).cloned();
    for pkg in el.children_named("package") {
        let (Some(id), Some(version)) = (pkg.attr("id"), pkg.attr("version")) else {
            continue;
        };
        let present = folder
            .as_ref()
            .is_some_and(|f| f.join(format!("{id}.{version}")).is_dir());
        if !present {
            out.missing.push(format!("{id} {version}"));
        }
    }
    if let Some(f) = folder {
        if !out.package_folders.contains(&f) {
            out.package_folders.push(f);
        }
    }
}

/// The restore hint for this repository and OS.
pub fn restore_hint(project: &DotnetProject, platform: &Platform, legacy_only: bool) -> String {
    if legacy_only {
        return "nuget restore".to_string();
    }
    if project.windows_targeting() && platform.os != Os::Windows {
        "dotnet restore -p:EnableWindowsTargeting=true".to_string()
    } else {
        "dotnet restore".to_string()
    }
}

/// Restore outputs of the repository (project scan included).
pub fn deps(cx: &DetectContext<'_>, toolchain: Option<&Toolchain>) -> DepsReport {
    let mut report = deps_for(cx, &project(cx));
    // The SDK's shared frameworks (compiled runtime assemblies: their metadata and method
    // bodies are read for attribute chains; reference packs carry no method bodies).
    if let Some(shared) = toolchain.map(|t| t.root.join("shared")).filter(|s| s.is_dir()) {
        if !report.roots.is_empty() {
            report.roots.push(LibraryRoot {
                path: shared,
                kind: LibraryKind::Stdlib,
                ecosystem: EcosystemId::Dotnet,
                layout: "toolchain_stdlib",
                version: toolchain.and_then(|t| t.version.as_ref()).map(|v| v.text.clone()),
            });
        }
    }
    report
}

/// Restore outputs of the required projects (missing = `DepsMissing`), sub-projects as
/// status notes, NuGet package folders as library roots, fingerprint = every assets file.
pub fn deps_for(cx: &DetectContext<'_>, project: &DotnetProject) -> DepsReport {
    let mut report = DepsReport::none_declared();
    if project.required.is_empty() {
        return report;
    }
    let solution_dir = project.chosen_solution().map(|s| relpath::parent(&s.rel).to_string());
    let mut fp = blake3::Hasher::new();
    let mut folders: Vec<PathBuf> = Vec::new();
    let mut legacy_only = true;
    for p in project.required_projects() {
        let restore = project_restore(cx.root, p, solution_dir.as_deref());
        fp.update(p.rel.as_bytes());
        if let Some(bytes) = restore.assets.as_ref().and_then(|a| fs::read(a).ok()) {
            fp.update(blake3::hash(&bytes).as_bytes());
        }
        for m in &restore.missing {
            fp.update(m.as_bytes());
        }
        if !restore.missing.is_empty() {
            if p.sdk_style {
                legacy_only = false;
            }
            let shown: Vec<&str> = restore.missing.iter().take(3).map(String::as_str).collect();
            report.missing.push(format!("{} ({})", p.rel, shown.join(", ")));
        }
        for f in restore.package_folders {
            if !folders.contains(&f) {
                folders.push(f);
            }
        }
    }
    let sub_reason = match project.chosen_solution() {
        Some(s) => format!("not part of {}", s.rel),
        None => "not part of the opened projects".to_string(),
    };
    for i in &project.subprojects {
        let Some(p) = project.projects.get(*i) else {
            continue;
        };
        report.subprojects.push(SubProject {
            dir: p.dir.clone(),
            reason: sub_reason.clone(),
        });
        if !project_restore(cx.root, p, solution_dir.as_deref())
            .missing
            .is_empty()
        {
            report
                .notes
                .push(format!("sub-project {}: dependencies not installed (dotnet restore)", p.rel));
        }
    }
    report.status = if report.missing.is_empty() {
        DepsStatus::Installed
    } else {
        DepsStatus::Missing
    };
    report.hint = restore_hint(project, cx.platform, legacy_only && !report.missing.is_empty());
    report.roots = folders
        .iter()
        .filter(|f| f.is_dir())
        .map(|f| LibraryRoot {
            path: f.clone(),
            kind: LibraryKind::Dependency,
            ecosystem: EcosystemId::Dotnet,
            layout: "nuget_packages",
            version: None,
        })
        .collect();
    report.fingerprint = hex(fp);
    report
}
