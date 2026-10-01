//! Haskell projects: cabal / stack projects, packages, the primary project and the pending
//! sub-projects.

use super::*;

#[derive(Default, Clone)]
pub(super) struct DirMarkers {
    cabal_project: bool,
    stack_yaml: bool,
    pub(super) cabal_files: Vec<String>,
    package_yaml: bool,
}

pub(super) fn markers(dir: &Path) -> DirMarkers {
    let mut m = DirMarkers::default();
    let Ok(rd) = fs::read_dir(dir) else { return m };
    for e in rd.filter_map(Result::ok).take(5000) {
        let name = e.file_name().to_string_lossy().into_owned();
        if !e.path().is_file() {
            continue;
        }
        match name.as_str() {
            "cabal.project" => m.cabal_project = true,
            "stack.yaml" => m.stack_yaml = true,
            "package.yaml" => m.package_yaml = true,
            n if n.ends_with(".cabal") && n.len() > ".cabal".len() => m.cabal_files.push(name.clone()),
            _ => {}
        }
    }
    m.cabal_files.sort();
    m
}

/// Directories skipped when looking for projects (build outputs, dependency trees).
pub(super) fn skipped(rel_dir: &str) -> bool {
    rel_dir
        .split('/')
        .any(|s| matches!(s, "dist-newstyle" | ".stack-work" | "dist" | ".git"))
}

/// The primary project and the pending sub-projects (module docs).
pub(crate) fn project_layout(cx: &DetectContext<'_>) -> (Option<HaskellProject>, Vec<SubProject>) {
    let files: Vec<String> = cx
        .files
        .iter()
        .filter(|(_, l)| *l == Language::Haskell)
        .map(|(p, _)| p.replace('\\', "/"))
        .collect();
    // Every ancestor directory of a Haskell file (and the root).
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    dirs.insert(String::new());
    for f in &files {
        let mut d = relpath::parent(f).to_string();
        loop {
            if !skipped(&d) {
                dirs.insert(d.clone());
            }
            if d.is_empty() {
                break;
            }
            d = relpath::parent(&d).to_string();
        }
    }
    let marks: BTreeMap<String, DirMarkers> = dirs
        .iter()
        .map(|d| (d.clone(), markers(&cx.root.join(d))))
        .filter(|(_, m)| m.cabal_project || m.stack_yaml || !m.cabal_files.is_empty() || m.package_yaml)
        .collect();
    let project_roots: Vec<&String> = marks
        .iter()
        .filter(|(_, m)| m.cabal_project || m.stack_yaml)
        .map(|(d, _)| d)
        .collect();
    let packages: Vec<&String> = marks
        .iter()
        .filter(|(_, m)| !m.cabal_files.is_empty() || m.package_yaml)
        .map(|(d, _)| d)
        .collect();
    // Every project: explicit project roots, plus packages outside all of them.
    let mut projects: BTreeSet<String> = project_roots.iter().map(|d| (*d).clone()).collect();
    for p in &packages {
        if !project_roots.iter().any(|r| relpath::within(p, r)) {
            projects.insert((*p).clone());
        }
    }
    if projects.is_empty() {
        let project = (!files.is_empty()).then(|| HaskellProject {
            dir: String::new(),
            tool: BuildTool::Direct,
            own_cradle: false,
            package_yaml_only: false,
            packages: Vec::new(),
        });
        return (project, Vec::new());
    }
    let count = |d: &str| files.iter().filter(|f| relpath::within(f, d)).count();
    let primary = if projects.contains("") {
        String::new()
    } else {
        projects
            .iter()
            .max_by(|a, b| {
                count(a)
                    .cmp(&count(b))
                    .then_with(|| b.split('/').count().cmp(&a.split('/').count()))
                    .then_with(|| b.cmp(a))
            })
            .cloned()
            .unwrap_or_default()
    };
    let m = marks.get(&primary).cloned().unwrap_or_default();
    let root_dir = cx.root.join(&primary);
    // Served packages: the `packages:` entries of cabal.project / stack.yaml (default `.`).
    let entries: Vec<String> = if m.cabal_project {
        let fields = relpath::read_small(&root_dir.join("cabal.project"), MAX_FILE_BYTES)
            .map(|t| read_cabal_fields(&t))
            .unwrap_or_default();
        fields
            .get("packages")
            .map(|v| {
                v.split([' ', ',', '\n', '\t'])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_else(|| vec![".".to_string()])
    } else if m.stack_yaml {
        relpath::read_small(&root_dir.join("stack.yaml"), MAX_FILE_BYTES)
            .and_then(|t| trace_core::formats::yaml::parse(&t))
            .and_then(|d| d.get("packages").and_then(Value::as_array).cloned())
            .map(|items| items.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_else(|| vec![".".to_string()])
    } else {
        vec![".".to_string()]
    };
    let mut served: Vec<String> = Vec::new();
    for p in packages.iter().filter(|p| relpath::within(p, &primary)) {
        let rel_in_project = p
            .strip_prefix(primary.as_str())
            .unwrap_or(p)
            .trim_start_matches('/')
            .to_string();
        let cabal_files = marks.get(*p).map(|mk| mk.cabal_files.clone()).unwrap_or_default();
        if entries
            .iter()
            .any(|e| package_entry_matches(e, &rel_in_project, &cabal_files))
        {
            served.push((*p).clone());
        }
    }
    let tool = build_tool(cx, &primary, &m);
    let own_cradle = matches!(cradle_of(&root_dir), Some(BuildTool::Cabal | BuildTool::Stack));
    let project = HaskellProject {
        dir: primary.clone(),
        tool,
        own_cradle,
        package_yaml_only: served.iter().all(|p| {
            marks
                .get(p)
                .is_some_and(|mk| mk.cabal_files.is_empty() && mk.package_yaml)
        }) && !served.is_empty(),
        packages: served.clone(),
    };
    // Pending: other projects and unlisted packages; loose files outside the served packages
    // grouped by their outermost directory that holds no served package.
    let mut pending: BTreeMap<String, String> = BTreeMap::new();
    for p in projects.iter().filter(|p| **p != primary) {
        if !relpath::within(p, &primary) || !served.iter().any(|s| relpath::within(s, p)) {
            pending.insert(p.clone(), format!("separate Haskell project ({p})"));
        }
    }
    for p in packages
        .iter()
        .filter(|p| relpath::within(p, &primary) && !served.contains(p))
    {
        pending
            .entry((*p).clone())
            .or_insert_with(|| format!("Haskell package not listed in the project ({p})"));
    }
    for f in &files {
        if served.iter().any(|s| relpath::within(f, s)) || pending.keys().any(|d| relpath::within(f, d)) {
            continue;
        }
        // The outermost ancestor directory below the primary project that holds no served
        // package; files directly in a served package's ancestor stay with the server.
        let mut dir = relpath::parent(f).to_string();
        let mut chosen: Option<String> = None;
        while relpath::within(&dir, &primary) && dir != primary {
            if !served.iter().any(|s| relpath::within(s, &dir)) {
                chosen = Some(dir.clone());
            }
            dir = relpath::parent(&dir).to_string();
        }
        if let Some(d) = chosen {
            pending.insert(d.clone(), format!("Haskell files outside the project's packages ({d})"));
        }
    }
    let pending = pending
        .into_iter()
        .map(|(dir, reason)| SubProject { dir, reason })
        .collect();
    (Some(project), pending)
}

/// A `packages:` entry (`.`, `./`, `lib/`, `*/`, `lib/*.cabal`) against a package directory
/// relative to the project (`""` = the project directory) and its `.cabal` files.
pub(crate) fn package_entry_matches(entry: &str, package_dir: &str, cabal_files: &[String]) -> bool {
    let e = entry.trim().trim_start_matches("./").trim_end_matches('/');
    let e = if e == "." { "" } else { e };
    let glob = |pattern: &str, text: &str| -> bool {
        globset::Glob::new(pattern)
            .map(|g| g.compile_matcher().is_match(text))
            .unwrap_or(false)
    };
    if e.ends_with(".cabal") {
        return cabal_files.iter().any(|f| {
            let full = if package_dir.is_empty() {
                f.clone()
            } else {
                format!("{package_dir}/{f}")
            };
            glob(e, &full)
        });
    }
    if e.is_empty() {
        return package_dir.is_empty();
    }
    e == package_dir || glob(e, package_dir)
}

/// The build tool of the primary project (module docs).
pub(super) fn build_tool(cx: &DetectContext<'_>, dir: &str, m: &DirMarkers) -> BuildTool {
    let root = cx.root.join(dir);
    if let Some(t) = cradle_of(&root) {
        return t;
    }
    let cabal_project = m.cabal_project
        || fs::read_dir(&root)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .any(|e| e.file_name().to_string_lossy().starts_with("cabal.project"))
            })
            .unwrap_or(false);
    if cabal_project || root.join("dist-newstyle").is_dir() {
        return BuildTool::Cabal;
    }
    let ghcup = ghcup_dirs(cx.vars, cx.platform);
    let stack_installed = || find_tool("stack", ghcup.as_ref(), cx.vars, cx.platform).is_some();
    if m.stack_yaml && root.join(".stack-work").is_dir() && stack_installed() {
        return BuildTool::Stack;
    }
    if m.cabal_files.is_empty() && m.package_yaml && stack_installed() {
        return BuildTool::Stack;
    }
    BuildTool::Cabal
}

/// The cradle of a project's own `hie.yaml` when it is a cabal / stack cradle.
pub(crate) fn cradle_of(project_dir: &Path) -> Option<BuildTool> {
    let doc = relpath::read_small(&project_dir.join("hie.yaml"), MAX_FILE_BYTES)
        .and_then(|t| trace_core::formats::yaml::parse(&t))?;
    let cradle = doc.get("cradle")?.as_object()?;
    if cradle.contains_key("cabal") {
        Some(BuildTool::Cabal)
    } else if cradle.contains_key("stack") {
        Some(BuildTool::Stack)
    } else {
        None
    }
}

/// Top-level `field: value` entries of a cabal-format file (`cabal.project`, cabal's
/// `config`): `--` comments dropped, indented continuation lines appended, section headers
/// end a field.
pub(crate) fn read_cabal_fields(text: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut current: Option<String> = None;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let content = line.split("--").next().unwrap_or_default();
        if content.trim().is_empty() {
            continue;
        }
        let indented = content.starts_with(' ') || content.starts_with('\t');
        if indented {
            if let Some(key) = &current {
                if let Some(v) = out.get_mut(key) {
                    v.push('\n');
                    v.push_str(content.trim());
                }
            }
            continue;
        }
        match content.split_once(':') {
            Some((key, value)) if !key.trim().contains(' ') => {
                let key = key.trim().to_ascii_lowercase();
                out.entry(key.clone()).or_insert_with(|| value.trim().to_string());
                current = Some(key);
            }
            _ => current = None,
        }
    }
    out
}
