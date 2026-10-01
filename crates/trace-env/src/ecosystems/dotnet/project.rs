//! The .NET project model: `*.csproj` properties (with `Directory.Build.props`), solutions
//! and the chosen solution.

use super::*;

/// One `*.csproj` read structurally (unconditional properties only; never evaluated).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CsProject {
    /// "src/App/App.csproj"
    pub rel: String,
    /// "src/App" ("" at the root).
    pub dir: String,
    /// `<Project Sdk="...">` (or `<Sdk>` / `<Import Sdk>`); false = the old .NET Framework format.
    pub sdk_style: bool,
    /// Lower-case target framework aliases ("net8.0", "net8.0-ios").
    pub target_frameworks: Vec<String>,
    pub use_maui: bool,
    /// `UseWPF` / `UseWindowsForms` or a `-windows` target framework.
    pub windows_only: bool,
    /// Unconditional `MSBuildProjectExtensionsPath` / `BaseIntermediateOutputPath` (relative
    /// to `dir`, `/`-separated); None = "obj".
    pub extensions_path: Option<String>,
    /// Referenced C# projects (relative paths).
    pub project_references: Vec<String>,
    /// A legacy `packages.config` next to the project.
    pub packages_config: bool,
}

/// One solution file and the C# projects it lists (relative paths).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Solution {
    pub rel: String,
    pub projects: Vec<String>,
}

/// The repository's C# projects and what Roslyn opens.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DotnetProject {
    pub projects: Vec<CsProject>,
    pub solutions: Vec<Solution>,
    /// Index into `solutions` of the solution Roslyn opens (`solution/open`).
    pub chosen: Option<usize>,
    /// Indices into `projects` analysed at index time.
    pub required: Vec<usize>,
    /// Indices into `projects` outside the chosen solution (sub-projects, set up on first use).
    pub subprojects: Vec<usize>,
    /// global.json nearest to the opened solution (or the first required project).
    pub global_json: Option<GlobalJson>,
}

impl DotnetProject {
    pub fn required_projects(&self) -> impl Iterator<Item = &CsProject> + '_ {
        self.required.iter().filter_map(|i| self.projects.get(*i))
    }

    pub fn chosen_solution(&self) -> Option<&Solution> {
        self.chosen.and_then(|i| self.solutions.get(i))
    }

    /// Required projects in the old (non-SDK) project format.
    pub fn legacy_projects(&self) -> Vec<&CsProject> {
        self.required_projects().filter(|p| !p.sdk_style).collect()
    }

    /// Some required project targets Windows only (needs `EnableWindowsTargeting` elsewhere).
    pub fn windows_targeting(&self) -> bool {
        self.required_projects().any(|p| p.windows_only)
    }

    /// Workload ids the required projects need (sorted).
    pub fn workloads(&self) -> Vec<String> {
        let mut out = BTreeSet::new();
        for p in self.required_projects() {
            out.extend(workload_ids(&p.target_frameworks, p.use_maui));
        }
        out.into_iter().collect()
    }

    /// Highest `netX.Y` major a required project targets (net5.0 and newer).
    pub(crate) fn highest_net_major(&self) -> Option<(u64, String)> {
        let mut best: Option<(u64, String)> = None;
        for p in self.required_projects() {
            for tfm in &p.target_frameworks {
                let base = tfm.split('-').next().unwrap_or(tfm);
                let Some(rest) = base.strip_prefix("net") else {
                    continue;
                };
                if !rest.contains('.') {
                    continue; // net48, net472: .NET Framework monikers
                }
                let Some(v) = Version::parse(rest) else {
                    continue;
                };
                let major = v.parts.first().copied().unwrap_or(0);
                if major >= 5 && best.as_ref().is_none_or(|(m, _)| major > *m) {
                    best = Some((major, p.rel.clone()));
                }
            }
        }
        best
    }
}

/// Workload ids for target frameworks (`net8.0-ios17.0` -> "ios") and `UseMaui` ("maui").
pub(crate) fn workload_ids(tfms: &[String], use_maui: bool) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for tfm in tfms {
        let Some((_, platform)) = tfm.split_once('-') else {
            continue;
        };
        let name: String = platform
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect::<String>()
            .to_ascii_lowercase();
        if ["ios", "android", "maccatalyst", "macos", "tvos", "tizen"].contains(&name.as_str()) {
            out.insert(name);
        }
    }
    if use_maui {
        out.insert("maui".to_string());
    }
    out
}

/// Directory names never entered while looking for projects.
pub(super) fn skip_project_dir(name: &str) -> bool {
    name.starts_with('.')
        || ["bin", "obj", "node_modules", "packages", "TestResults"]
            .iter()
            .any(|s| s.eq_ignore_ascii_case(name))
}

/// Unconditional properties of `PropertyGroup`s without a `Condition` (element local name ->
/// text), later groups winning.
pub(super) fn unconditional_properties(project: &Element, out: &mut BTreeMap<String, String>) {
    for group in project.children_named("PropertyGroup") {
        if group.attr("Condition").is_some() {
            continue;
        }
        for prop in &group.children {
            if prop.attr("Condition").is_some() {
                continue;
            }
            out.insert(prop.local_name().to_string(), prop.text.clone());
        }
    }
}

/// Unconditional properties of the nearest `Directory.Build.props` above `dir`.
pub(super) fn directory_build_props(root: &Path, dir: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut current = dir.to_string();
    loop {
        let rel = if current.is_empty() {
            "Directory.Build.props".to_string()
        } else {
            format!("{current}/Directory.Build.props")
        };
        if let Some(el) = relpath::read_text(&relpath::native(root, &rel)).and_then(|t| xml::parse(&t)) {
            unconditional_properties(&el, &mut out);
            return out;
        }
        if current.is_empty() {
            return out;
        }
        current = relpath::parent(&current).to_string();
    }
}

/// A property value MSBuild must evaluate (`$(...)`, `@(...)`, `%(...)`) is not used.
pub(super) fn literal(value: &str) -> Option<&str> {
    let v = value.trim();
    (!v.is_empty() && !v.contains("$(") && !v.contains("@(") && !v.contains("%(")).then_some(v)
}

/// Read one project file structurally. `None` when it is not a readable MSBuild project.
pub(crate) fn read_project(root: &Path, rel: &str) -> Option<CsProject> {
    let el = xml::parse(&relpath::read_text(&relpath::native(root, rel))?)?;
    if el.local_name() != "Project" {
        return None;
    }
    let dir = relpath::parent(rel).to_string();
    let sdk_style = el.attr("Sdk").is_some()
        || el.child("Sdk").is_some()
        || el.children_named("Import").any(|i| i.attr("Sdk").is_some());
    let mut props = directory_build_props(root, &dir);
    unconditional_properties(&el, &mut props);
    let is_true = |key: &str| props.get(key).is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
    let mut target_frameworks: Vec<String> = Vec::new();
    for key in ["TargetFrameworks", "TargetFramework"] {
        if let Some(v) = props.get(key).and_then(|v| literal(v)) {
            target_frameworks = v
                .split(';')
                .map(|t| t.trim().to_ascii_lowercase())
                .filter(|t| !t.is_empty())
                .collect();
            break;
        }
    }
    let extensions_path = ["MSBuildProjectExtensionsPath", "BaseIntermediateOutputPath"]
        .iter()
        .find_map(|k| props.get(*k).and_then(|v| literal(v)))
        .map(|v| v.replace('\\', "/").trim_end_matches('/').to_string());
    let mut project_references = Vec::new();
    for group in el.children_named("ItemGroup") {
        if group.attr("Condition").is_some() {
            continue;
        }
        for item in group.children_named("ProjectReference") {
            if item.attr("Condition").is_some() {
                continue;
            }
            if let Some(target) = item.attr("Include").and_then(literal).and_then(|i| join_rel(&dir, i)) {
                if target.to_ascii_lowercase().ends_with(".csproj") {
                    project_references.push(target);
                }
            }
        }
    }
    let windows_only = is_true("UseWPF")
        || is_true("UseWindowsForms")
        || target_frameworks.iter().any(|t| t.contains("-windows"));
    let packages_config = relpath::native(root, &dir).join("packages.config").is_file();
    Some(CsProject {
        rel: rel.to_string(),
        dir,
        sdk_style,
        use_maui: is_true("UseMaui"),
        target_frameworks,
        windows_only,
        extensions_path,
        project_references,
        packages_config,
    })
}

/// The C# projects a `.sln` (line format) or `.slnx` (XML) lists, as relative paths.
pub(crate) fn read_solution(root: &Path, rel: &str) -> Option<Solution> {
    let text = relpath::read_text(&relpath::native(root, rel))?;
    let dir = relpath::parent(rel);
    let mut projects = Vec::new();
    if rel.to_ascii_lowercase().ends_with(".slnx") {
        let el = xml::parse(&text)?;
        let mut stack = vec![&el];
        while let Some(e) = stack.pop() {
            for c in &e.children {
                if c.local_name() == "Project" {
                    if let Some(p) = c.attr("Path") {
                        projects.extend(join_rel(dir, p));
                    }
                }
                stack.push(c);
            }
        }
    } else {
        // `Project("{TYPE-GUID}") = "Name", "relative\path.csproj", "{GUID}"`
        for line in text.lines() {
            let line = line.trim();
            if !line.starts_with("Project(") {
                continue;
            }
            let Some((_, rhs)) = line.split_once('=') else {
                continue;
            };
            let fields: Vec<&str> = rhs.split(',').map(|f| f.trim().trim_matches('"')).collect();
            if let Some(path) = fields.get(1) {
                projects.extend(join_rel(dir, path));
            }
        }
    }
    projects.retain(|p| p.to_ascii_lowercase().ends_with(".csproj"));
    projects.sort();
    projects.dedup();
    Some(Solution {
        rel: rel.to_string(),
        projects,
    })
}

/// The solution Roslyn opens: the one listing most of the repository's C# projects (ties:
/// the shallower one, then the first path). Solutions without any existing C# project are
/// never chosen. None -> `project/open` with every project.
pub(crate) fn choose_solution(solutions: &[Solution], projects: &[CsProject]) -> Option<usize> {
    let existing: BTreeSet<String> = projects.iter().map(|p| p.rel.to_ascii_lowercase()).collect();
    let mut best: Option<(usize, usize, usize)> = None; // (covered, depth, index)
    for (i, s) in solutions.iter().enumerate() {
        let covered = s
            .projects
            .iter()
            .filter(|p| existing.contains(&p.to_ascii_lowercase()))
            .count();
        if covered == 0 {
            continue;
        }
        let depth = s.rel.matches('/').count();
        let better = match best {
            None => true,
            Some((c, d, _)) => covered > c || (covered == c && depth < d),
        };
        if better {
            best = Some((covered, depth, i));
        }
    }
    best.map(|(_, _, i)| i)
}

/// Scan the repository: projects, solutions, the solution choice, required projects and
/// sub-projects, and the governing global.json.
pub fn project(cx: &DetectContext<'_>) -> DotnetProject {
    let allowed = |p: &Path| cx.allowed(p);
    let files = walk_files(cx.root, &allowed, &skip_project_dir, &|name: &str| {
        let lower = name.to_ascii_lowercase();
        lower.ends_with(".csproj") || lower.ends_with(".sln") || lower.ends_with(".slnx")
    });
    let mut projects = Vec::new();
    let mut solutions = Vec::new();
    for rel in &files {
        if rel.to_ascii_lowercase().ends_with(".csproj") {
            projects.extend(read_project(cx.root, rel));
        } else {
            solutions.extend(read_solution(cx.root, rel));
        }
    }
    let chosen = choose_solution(&solutions, &projects);
    let index: BTreeMap<String, usize> = projects
        .iter()
        .enumerate()
        .map(|(i, p)| (p.rel.to_ascii_lowercase(), i))
        .collect();
    let mut required: BTreeSet<usize> = BTreeSet::new();
    match chosen.and_then(|i| solutions.get(i)) {
        Some(s) => {
            let mut queue: Vec<usize> = s
                .projects
                .iter()
                .filter_map(|p| index.get(&p.to_ascii_lowercase()).copied())
                .collect();
            while let Some(i) = queue.pop() {
                if !required.insert(i) {
                    continue;
                }
                for r in &projects[i].project_references {
                    if let Some(j) = index.get(&r.to_ascii_lowercase()) {
                        queue.push(*j);
                    }
                }
            }
        }
        None => required.extend(0..projects.len()),
    }
    let subprojects: Vec<usize> = (0..projects.len()).filter(|i| !required.contains(i)).collect();
    let anchor = chosen
        .and_then(|i| solutions.get(i))
        .map(|s| relpath::parent(&s.rel).to_string())
        .or_else(|| required.iter().next().map(|i| projects[*i].dir.clone()))
        .unwrap_or_default();
    let global_json = find_global_json(cx.root, &anchor);
    DotnetProject {
        projects,
        solutions,
        chosen,
        required: required.into_iter().collect(),
        subprojects,
        global_json,
    }
}
