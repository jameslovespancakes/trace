//! Declared dependencies: pyproject (PEP 621, Poetry, PDM, uv), requirements files,
//! setup.cfg, a literal setup.py, Pipfile and environment.yml, read structurally.

use super::*;

/// Which part of the project a requirement belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Group {
    /// Needed to run the project: missing -> `DepsMissing`.
    Runtime,
    /// Test / dev groups: missing -> a status line.
    Dev,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Declared {
    pub(super) group: Group,
    /// PEP 503-normalised name.
    pub(super) name: String,
    /// The requirement as written (for messages).
    pub(super) raw: String,
}

/// The manifests of one project directory.
#[derive(Default)]
pub(super) struct Manifests {
    /// Project name (skipped in the check).
    pub(super) name: Option<String>,
    pub(super) requirements: Vec<Requirement>,
    /// uv workspace member directories (relative, `/`).
    pub(super) members: Vec<String>,
    /// Lower bound of `requires-python` ("3.9").
    pub(super) requires_python: Option<String>,
    /// Manifest bytes for the fingerprint.
    pub(super) bytes: Vec<u8>,
    /// Some manifest declares the project (used for the "declared" decision).
    pub(super) declares: bool,
}

/// One requirement before marker evaluation.
#[derive(Clone, Debug)]
pub(super) struct Requirement {
    pub(super) group: Group,
    pub(super) text: String,
}

impl Manifests {
    pub(super) fn read(dir: &Path) -> Manifests {
        let mut m = Manifests::default();
        if let Ok(text) = fs::read_to_string(dir.join("pyproject.toml")) {
            m.bytes.extend_from_slice(text.as_bytes());
            if let Some(doc) = toml_value(&text) {
                m.pyproject(&doc);
            }
        }
        let req = dir.join("requirements.txt");
        if req.is_file() {
            m.declares = true;
            let mut seen = BTreeSet::new();
            for line in requirement_lines(&req, 0, &mut seen, &mut m.bytes) {
                m.requirements.push(Requirement {
                    group: Group::Runtime,
                    text: line,
                });
            }
        }
        for (_, path) in dev_requirement_files(dir) {
            let mut seen = BTreeSet::new();
            for line in requirement_lines(&path, 0, &mut seen, &mut m.bytes) {
                m.requirements.push(Requirement {
                    group: Group::Dev,
                    text: line,
                });
            }
        }
        if let Ok(text) = fs::read_to_string(dir.join("setup.cfg")) {
            m.bytes.extend_from_slice(text.as_bytes());
            let ini = trace_core::formats::ini::sections(&text);
            if let Some(options) = ini.get("options") {
                if let Some(value) = options.get("install_requires") {
                    m.declares = true;
                    for line in value.lines().map(str::trim).filter(|l| !l.is_empty()) {
                        m.requirements.push(Requirement {
                            group: Group::Runtime,
                            text: line.to_string(),
                        });
                    }
                }
            }
            if m.name.is_none() {
                m.name = ini
                    .get("metadata")
                    .and_then(|s| s.get("name"))
                    .map(|n| n.trim().to_string());
            }
        }
        if let Ok(bytes) = fs::read(dir.join("setup.py")) {
            m.bytes.extend_from_slice(&bytes);
            let reqs = setup_py_install_requires(&bytes);
            if !reqs.is_empty() {
                m.declares = true;
            }
            for r in reqs {
                m.requirements.push(Requirement {
                    group: Group::Runtime,
                    text: r,
                });
            }
        }
        if let Ok(text) = fs::read_to_string(dir.join("Pipfile")) {
            m.bytes.extend_from_slice(text.as_bytes());
            if let Some(doc) = toml_value(&text) {
                for (section, group) in [("packages", Group::Runtime), ("dev-packages", Group::Dev)] {
                    if let Some(table) = doc.get(section).and_then(Value::as_object) {
                        m.declares = true;
                        for (name, spec) in table {
                            m.requirements.push(Requirement {
                                group,
                                text: pipfile_requirement(name, spec),
                            });
                        }
                    }
                }
            }
        }
        m.requirements.retain(|r| !r.text.trim().is_empty());
        m
    }

    fn pyproject(&mut self, doc: &Value) {
        let project = doc.get("project");
        if let Some(p) = project {
            self.declares = true;
            self.name = p.get("name").and_then(Value::as_str).map(str::to_string);
            for r in string_list(p.get("dependencies")) {
                self.requirements.push(Requirement {
                    group: Group::Runtime,
                    text: r,
                });
            }
            if let Some(spec) = p.get("requires-python").and_then(Value::as_str) {
                self.requires_python = lower_bound(spec);
            }
        }
        // PEP 735 dependency groups (entries may be `{include-group = "x"}` tables).
        if let Some(groups) = doc.get("dependency-groups").and_then(Value::as_object) {
            for list in groups.values() {
                for r in string_list(Some(list)) {
                    self.requirements.push(Requirement {
                        group: Group::Dev,
                        text: r,
                    });
                }
            }
        }
        let tool = doc.get("tool");
        if let Some(poetry) = tool.and_then(|t| t.get("poetry")) {
            self.declares = true;
            if self.name.is_none() {
                self.name = poetry.get("name").and_then(Value::as_str).map(str::to_string);
            }
            if let Some(deps) = poetry.get("dependencies").and_then(Value::as_object) {
                for (name, spec) in deps {
                    if name.eq_ignore_ascii_case("python") {
                        if self.requires_python.is_none() {
                            self.requires_python = spec.as_str().and_then(lower_bound);
                        }
                        continue;
                    }
                    if spec.get("optional").and_then(Value::as_bool) == Some(true) {
                        continue;
                    }
                    self.requirements.push(Requirement {
                        group: Group::Runtime,
                        text: poetry_requirement(name, spec),
                    });
                }
            }
            if let Some(deps) = poetry.get("dev-dependencies").and_then(Value::as_object) {
                for (name, spec) in deps {
                    self.requirements.push(Requirement {
                        group: Group::Dev,
                        text: poetry_requirement(name, spec),
                    });
                }
            }
            if let Some(groups) = poetry.get("group").and_then(Value::as_object) {
                for group in groups.values() {
                    if let Some(deps) = group.get("dependencies").and_then(Value::as_object) {
                        for (name, spec) in deps {
                            self.requirements.push(Requirement {
                                group: Group::Dev,
                                text: poetry_requirement(name, spec),
                            });
                        }
                    }
                }
            }
        }
        if let Some(pdm) = tool
            .and_then(|t| t.get("pdm"))
            .and_then(|p| p.get("dev-dependencies"))
        {
            if let Some(groups) = pdm.as_object() {
                for list in groups.values() {
                    for r in string_list(Some(list)) {
                        self.requirements.push(Requirement {
                            group: Group::Dev,
                            text: r,
                        });
                    }
                }
            }
        }
        if let Some(members) = tool
            .and_then(|t| t.get("uv"))
            .and_then(|u| u.get("workspace"))
            .and_then(|w| w.get("members"))
        {
            self.members = string_list(Some(members));
        }
    }
}

pub(super) fn string_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

/// Poetry `name = "^1.2"` / `{version = "...", markers = "..."}` -> a PEP 508 line (the
/// version is irrelevant for the installed check; markers and `python` constraints kept).
pub(super) fn poetry_requirement(name: &str, spec: &Value) -> String {
    match spec.get("markers").and_then(Value::as_str) {
        Some(markers) => format!("{name}; {markers}"),
        None => name.to_string(),
    }
}

/// Pipfile `name = "*"` / `{version = "...", markers = "..."}`.
pub(super) fn pipfile_requirement(name: &str, spec: &Value) -> String {
    let mut markers = Vec::new();
    if let Some(m) = spec.get("markers").and_then(Value::as_str) {
        markers.push(m.to_string());
    }
    if let Some(p) = spec.get("sys_platform").and_then(Value::as_str) {
        markers.push(format!("sys_platform {p}"));
    }
    if markers.is_empty() {
        name.to_string()
    } else {
        format!("{name}; {}", markers.join(" and "))
    }
}

/// `>=3.9,<4` -> "3.9"; `^3.10` / `~=3.8` / `==3.11.*` -> the named minor.
pub(super) fn lower_bound(spec: &str) -> Option<String> {
    for part in spec.split([',', '|']) {
        let part = part.trim();
        for op in [">=", "~=", "==", "^", "~", ">"] {
            if let Some(rest) = part.strip_prefix(op) {
                let v = rest.trim().trim_end_matches(".*");
                if let Some(mm) = major_minor(v) {
                    return Some(mm);
                }
                if op != ">" {
                    if let Some(major) = v
                        .split('.')
                        .next()
                        .filter(|m| m.chars().all(|c| c.is_ascii_digit()) && !m.is_empty())
                    {
                        return Some(format!("{major}.0"));
                    }
                }
            }
        }
    }
    None
}

/// `requirements-dev.txt`, `dev-requirements.txt`, `requirements/*.txt` ... (test/dev).
pub(super) fn dev_requirement_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = entries(dir)
        .into_iter()
        .filter(|(n, p)| {
            p.is_file() && n.ends_with(".txt") && n != "requirements.txt" && n.contains("requirements")
        })
        .collect();
    out.extend(
        entries(&dir.join("requirements"))
            .into_iter()
            .filter(|(n, p)| p.is_file() && n.ends_with(".txt")),
    );
    out
}

/// Requirement lines of a requirements file, following `-r` includes (bounded depth).
/// Options, editable installs, URLs and paths are skipped (they are not index packages).
pub(super) fn requirement_lines(
    path: &Path,
    depth: usize,
    seen: &mut BTreeSet<PathBuf>,
    bytes: &mut Vec<u8>,
) -> Vec<String> {
    let mut out = Vec::new();
    if depth > 5 || !seen.insert(path.to_path_buf()) {
        return out;
    }
    let Ok(text) = fs::read_to_string(path) else {
        return out;
    };
    bytes.extend_from_slice(text.as_bytes());
    let mut logical = String::new();
    for raw in text.lines() {
        // Line continuations.
        if let Some(stripped) = raw.strip_suffix('\\') {
            logical.push_str(stripped);
            continue;
        }
        logical.push_str(raw);
        let line = std::mem::take(&mut logical);
        let line = strip_requirement_comment(&line);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        for flag in ["-r ", "--requirement ", "--requirement="] {
            if let Some(rest) = line.strip_prefix(flag) {
                let target = path.parent().unwrap_or(Path::new("")).join(rest.trim());
                out.extend(requirement_lines(&target, depth + 1, seen, bytes));
            }
        }
        if line.starts_with('-') {
            continue;
        }
        out.push(line.to_string());
    }
    out
}

/// A ` #` comment ends a requirement line (a `#` inside a URL fragment is not preceded by
/// whitespace).
pub(super) fn strip_requirement_comment(line: &str) -> String {
    if line.trim_start().starts_with('#') {
        return String::new();
    }
    match line.find(" #").or_else(|| line.find("\t#")) {
        Some(i) => line[..i].to_string(),
        None => line.to_string(),
    }
}

/// `setup(install_requires=[...])` with a literal list of strings, read from the syntax tree
/// (never run). Non-literal values give nothing.
pub(super) fn setup_py_install_requires(source: &[u8]) -> Vec<String> {
    let Ok(tree) = trace_syntax::parse_tree(Language::Python, source) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "keyword_argument" {
            let name = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or_default();
            if name == "install_requires" {
                if let Some(value) = node.child_by_field_name("value") {
                    if matches!(value.kind(), "list" | "tuple") {
                        for i in 0..value.named_child_count() {
                            let Some(item) = value.named_child(i as _) else {
                                continue;
                            };
                            if item.kind() != "string" {
                                continue;
                            }
                            let text = item.utf8_text(source).unwrap_or_default();
                            if let Some(s) = python_string_literal(text) {
                                out.push(s);
                            }
                        }
                    }
                }
                continue;
            }
        }
        for i in (0..node.child_count()).rev() {
            if let Some(child) = node.child(i as _) {
                stack.push(child);
            }
        }
    }
    out
}

/// The value of a plain Python string token (`"x"`, `'x'`, `r"x"`, `"""x"""`); f-strings and
/// byte strings are not requirement literals.
pub(super) fn python_string_literal(text: &str) -> Option<String> {
    if text.starts_with(['f', 'F', 'b', 'B']) {
        return None;
    }
    let body = text.trim_start_matches(['r', 'R', 'u', 'U']);
    for q in ["\"\"\"", "'''", "\"", "'"] {
        if let Some(inner) = body.strip_prefix(q).and_then(|b| b.strip_suffix(q)) {
            return Some(inner.to_string());
        }
    }
    None
}
