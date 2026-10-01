//! Test conventions (SPEC §9.6; PLAN decision 14, DESIGN §1.15). Syntax facts only; tests are
//! never run.
//!
//! No test runner is named in code: runner defaults come from the `test_conventions` section of
//! the embedded `assets/library/<lang>.json` tables (data; every row says why it cannot be
//! derived). The order of evidence for a test FILE:
//! 1. the project's own runner configuration ([`TestConfig`], read with structured parsers
//!    from the configuration files: pytest / Jest / Vitest / Mocha settings, build-tool test
//!    source sets) - it replaces that runner's default rows;
//! 2. rows marked `language_spec` (toolchain rules: Go `*_test.go`, Rust `tests/*.rs`) always
//!    apply;
//! 3. rows of a runner (`activated_by`) apply when the project declares that runner, or when
//!    the project's manifests of that ecosystem were not read (unknown: the defaults stand).
//!
//! A test DECLARATION is a callable in a test file whose name matches a row `pattern`
//! (`test*`, `Test*`, `test_*`), or a declaration carrying an attribute / annotation /
//! decorator that a row names (`symbol` rows by their simple name; C# attribute classes
//! without their `Attribute` suffix, the language's attribute naming rule; `#[..]` patterns).
//!
//! Globs: `*` (within a segment), `**` (any number of segments), `?`, `[..]` classes and
//! `{a,b}` alternatives. A glob without `/` matches the file name; a glob with `/` matches the
//! path from any directory boundary (sub-projects of a monorepo).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use trace_core::facts::Declaration;
use trace_core::Language;

/// String values of a JSON string or an array of strings (`Option<&serde_json::Value>` from
/// trace-core's structured readers; a macro so this crate needs no JSON dependency).
macro_rules! json_strings {
    ($v:expr) => {{
        let mut out: Vec<String> = Vec::new();
        if let Some(v) = $v {
            if let Some(s) = v.as_str() {
                out.push(s.to_string());
            } else if let Some(items) = v.as_array() {
                out.extend(items.iter().filter_map(|x| x.as_str().map(str::to_string)));
            }
        }
        out
    }};
}

/// The embedded table files (only `test_conventions` is read here and `syntax_conventions` in
/// `crate::boundary`; the tables themselves belong to trace-library).
pub(crate) const TABLE_FILES: [(&str, &str); 13] = [
    ("python", include_str!("../../../assets/library/python.json")),
    ("javascript", include_str!("../../../assets/library/javascript.json")),
    ("rust", include_str!("../../../assets/library/rust.json")),
    ("go", include_str!("../../../assets/library/go.json")),
    ("java", include_str!("../../../assets/library/java.json")),
    ("c", include_str!("../../../assets/library/c.json")),
    ("cpp", include_str!("../../../assets/library/cpp.json")),
    ("csharp", include_str!("../../../assets/library/csharp.json")),
    ("php", include_str!("../../../assets/library/php.json")),
    ("bash", include_str!("../../../assets/library/bash.json")),
    ("scala", include_str!("../../../assets/library/scala.json")),
    ("r", include_str!("../../../assets/library/r.json")),
    ("haskell", include_str!("../../../assets/library/haskell.json")),
];

/// One `test_conventions` row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Row {
    glob: Option<String>,
    pattern: Option<String>,
    symbol: Option<String>,
    activated_by: Option<String>,
    /// `why_not_derivable == "language_spec"`: a toolchain rule, always applies.
    language_spec: bool,
}

/// Rows per table key.
fn rows() -> &'static BTreeMap<&'static str, Vec<Row>> {
    static ROWS: OnceLock<BTreeMap<&'static str, Vec<Row>>> = OnceLock::new();
    ROWS.get_or_init(|| {
        let mut out = BTreeMap::new();
        for (key, text) in TABLE_FILES {
            let Some(doc) = trace_core::formats::jsonc::parse(text) else {
                continue;
            };
            let list: Vec<Row> = doc
                .get("test_conventions")
                .and_then(|v| v.as_array())
                .map(|rows| {
                    rows.iter()
                        .map(|r| {
                            let s = |k: &str| r.get(k).and_then(|v| v.as_str()).map(str::to_string);
                            Row {
                                glob: s("glob"),
                                pattern: s("pattern"),
                                symbol: s("symbol"),
                                activated_by: s("activated_by"),
                                language_spec: s("why_not_derivable").as_deref() == Some("language_spec"),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            out.insert(key, list);
        }
        out
    })
}

/// The project's own test-runner configuration: runner test globs from runner settings,
/// build-tool test source directories and the packages the project declares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TestConfig {
    /// Runner (the `activated_by` name of its rows) -> (directory of the settings file,
    /// test file globs relative to it). A runner listed here uses these globs INSTEAD of
    /// its default rows.
    runner_globs: BTreeMap<String, Vec<(String, String)>>,
    /// Test source directories of build tools (`<module>/src/test` of Maven / Gradle
    /// projects, `testSourceDirectory`), repository-relative, without a trailing `/`.
    source_dirs: BTreeSet<String>,
    /// Declared packages per table key whose manifests were read.
    declared: BTreeMap<&'static str, BTreeSet<String>>,
}

impl TestConfig {
    /// Read the runner configuration from `(repository-relative path, bytes)` of the
    /// repository's configuration files (unknown files are ignored).
    pub fn from_files(files: &[(&str, &[u8])]) -> TestConfig {
        let mut cfg = TestConfig::default();
        let mut sorted: Vec<&(&str, &[u8])> = files.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));
        for (path, bytes) in sorted {
            let path = path.replace('\\', "/");
            let (dir, name) = match path.rsplit_once('/') {
                Some((d, n)) => (d.to_string(), n.to_string()),
                None => (String::new(), path.clone()),
            };
            let text = String::from_utf8_lossy(bytes);
            cfg.read_file(&dir, &name, &text);
        }
        cfg
    }

    fn declare(&mut self, keys: &[&'static str], names: impl IntoIterator<Item = String>) {
        let names: Vec<String> = names.into_iter().filter(|n| !n.is_empty()).collect();
        for key in keys {
            self.declared.entry(*key).or_default().extend(names.iter().cloned());
        }
    }

    fn runner(&mut self, runner: &str, dir: &str, globs: Vec<String>) {
        let usable: Vec<(String, String)> = globs
            .into_iter()
            .filter(|g| supported_glob(g))
            .map(|g| (dir.to_string(), g.trim_start_matches("./").to_string()))
            .collect();
        if !usable.is_empty() {
            self.runner_globs
                .entry(runner.to_string())
                .or_default()
                .extend(usable);
        }
    }

    fn read_file(&mut self, dir: &str, name: &str, text: &str) {
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "package.json" => {
                let Some(doc) = trace_core::formats::jsonc::parse(text) else { return };
                let mut names = Vec::new();
                for section in ["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"]
                {
                    if let Some(map) = doc.get(section).and_then(|v| v.as_object()) {
                        names.extend(map.keys().cloned());
                    }
                }
                self.declare(&["javascript"], names);
                if let Some(jest) = doc.get("jest") {
                    self.runner("jest", dir, json_strings!(jest.get("testMatch")));
                }
                if let Some(mocha) = doc.get("mocha") {
                    self.runner("mocha", dir, json_strings!(mocha.get("spec")));
                }
            }
            "jest.config.json" => {
                if let Some(doc) = trace_core::formats::jsonc::parse(text) {
                    self.runner("jest", dir, json_strings!(doc.get("testMatch")));
                }
            }
            ".mocharc.json" | ".mocharc.jsonc" => {
                if let Some(doc) = trace_core::formats::jsonc::parse(text) {
                    self.runner("mocha", dir, json_strings!(doc.get("spec")));
                }
            }
            ".mocharc.yml" | ".mocharc.yaml" => {
                if let Some(doc) = trace_core::formats::yaml::parse(text) {
                    self.runner("mocha", dir, json_strings!(doc.get("spec")));
                }
            }
            "requirements.txt"
            | "requirements-dev.txt"
            | "requirements_dev.txt"
            | "dev-requirements.txt"
            | "test-requirements.txt"
            | "requirements-test.txt" => {
                let names: Vec<String> = text.lines().filter_map(requirement_name).collect();
                self.declare(&["python"], names);
            }
            "pyproject.toml" => {
                let Some(doc) = trace_core::formats::toml_value(text) else { return };
                let mut names = Vec::new();
                let project = doc.get("project");
                for spec in json_strings!(project.and_then(|p| p.get("dependencies"))) {
                    names.extend(requirement_name(&spec));
                }
                for table in [
                    project.and_then(|p| p.get("optional-dependencies")),
                    doc.get("dependency-groups"),
                ] {
                    if let Some(groups) = table.and_then(|t| t.as_object()) {
                        for list in groups.values() {
                            for spec in json_strings!(Some(list)) {
                                names.extend(requirement_name(&spec));
                            }
                        }
                    }
                }
                if let Some(poetry) = doc.get("tool").and_then(|t| t.get("poetry")) {
                    for key in ["dependencies", "dev-dependencies"] {
                        if let Some(map) = poetry.get(key).and_then(|v| v.as_object()) {
                            names.extend(map.keys().cloned());
                        }
                    }
                    if let Some(groups) = poetry.get("group").and_then(|v| v.as_object()) {
                        for g in groups.values() {
                            if let Some(map) = g.get("dependencies").and_then(|v| v.as_object()) {
                                names.extend(map.keys().cloned());
                            }
                        }
                    }
                }
                self.declare(&["python"], names);
                if let Some(opts) = doc
                    .get("tool")
                    .and_then(|t| t.get("pytest"))
                    .and_then(|p| p.get("ini_options"))
                {
                    let split = |v: Vec<String>| -> Vec<String> {
                        v.iter()
                            .flat_map(|s| s.split_whitespace().map(str::to_string))
                            .collect()
                    };
                    let files = split(json_strings!(opts.get("python_files")));
                    self.pytest(dir, files, split(json_strings!(opts.get("testpaths"))));
                }
            }
            "pytest.ini" | "tox.ini" | "setup.cfg" => {
                let section = if lower == "setup.cfg" {
                    "tool:pytest"
                } else {
                    "pytest"
                };
                let ini = ini_section(text, section);
                if ini.is_empty() && lower != "pytest.ini" {
                    return;
                }
                let files: Vec<String> = ini
                    .get("python_files")
                    .map(|v| v.split_whitespace().map(str::to_string).collect())
                    .unwrap_or_default();
                let paths: Vec<String> = ini
                    .get("testpaths")
                    .map(|v| v.split_whitespace().map(str::to_string).collect())
                    .unwrap_or_default();
                self.pytest(dir, files, paths);
            }
            "composer.json" => {
                let Some(doc) = trace_core::formats::jsonc::parse(text) else { return };
                let mut names = Vec::new();
                for section in ["require", "require-dev"] {
                    if let Some(map) = doc.get(section).and_then(|v| v.as_object()) {
                        names.extend(map.keys().cloned());
                    }
                }
                self.declare(&["php"], names);
            }
            "pom.xml" => {
                let Some(root) = trace_core::formats::xml::parse(text) else { return };
                let mut names = Vec::new();
                collect_maven_dependencies(&root, &mut names);
                self.declare(&["java", "scala"], names);
                let custom = root
                    .child("build")
                    .and_then(|b| b.child("testSourceDirectory"))
                    .map(|e| e.text.trim().trim_start_matches("./").to_string())
                    .filter(|t| !t.is_empty() && !t.contains("${"));
                // Maven's standard directory layout (super-POM): `src/test/<language>`.
                let test_dir = custom.unwrap_or_else(|| "src/test".to_string());
                self.source_dirs.insert(join(dir, &test_dir));
            }
            "build.gradle" | "build.gradle.kts" => {
                // Gradle's JVM source-set convention: `src/test/<language>`. Build scripts are
                // programs (never read): the JVM runner defaults stand.
                self.source_dirs.insert(join(dir, "src/test"));
            }
            "description" => {
                let mut names = Vec::new();
                for field in ["Imports", "Depends", "Suggests", "LinkingTo"] {
                    if let Some(v) = dcf_field(text, field) {
                        names.extend(
                            v.split(',')
                                .map(|p| p.split('(').next().unwrap_or(p).trim().to_string())
                                .filter(|p| !p.is_empty()),
                        );
                    }
                }
                self.declare(&["r"], names);
            }
            _ if lower.ends_with(".csproj") || lower.ends_with(".fsproj") || lower.ends_with(".vbproj") => {
                let Some(root) = trace_core::formats::xml::parse(text) else { return };
                let mut names = Vec::new();
                collect_package_references(&root, &mut names);
                self.declare(&["csharp"], names);
            }
            _ if is_js_config(&lower, "jest.config") => {
                self.runner("jest", dir, js_config_strings(&lower, text, "testMatch", None));
            }
            _ if is_js_config(&lower, "vitest.config") || is_js_config(&lower, "vite.config") => {
                self.runner("vitest", dir, js_config_strings(&lower, text, "include", Some("test")));
            }
            _ if is_js_config(&lower, ".mocharc") => {
                self.runner("mocha", dir, js_config_strings(&lower, text, "spec", None));
            }
            _ => {}
        }
    }

    /// pytest settings: `python_files` globs (default: the runner's rows) under `testpaths`.
    fn pytest(&mut self, dir: &str, files: Vec<String>, testpaths: Vec<String>) {
        let files = if files.is_empty() {
            rows()
                .get("python")
                .map(|rs| {
                    rs.iter()
                        .filter(|r| r.activated_by.as_deref() == Some("pytest"))
                        .filter_map(|r| r.glob.clone())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            files
        };
        let mut globs = Vec::new();
        for f in &files {
            if testpaths.is_empty() {
                globs.push(format!("**/{f}"));
            } else {
                for p in &testpaths {
                    let p = p.trim_matches('/');
                    globs.push(format!("{p}/**/{f}"));
                }
            }
        }
        self.runner("pytest", dir, globs);
    }

    /// Whether a row of runner `activated_by` for table `key` applies.
    fn runner_active(&self, key: &str, runner: &str) -> bool {
        if self.runner_globs.contains_key(runner) {
            // The runner's own configuration replaces its defaults.
            return false;
        }
        match self.declared.get(key) {
            // The ecosystem's manifests were read: the runner must be declared.
            Some(names) => names.iter().any(|n| same_package(n, runner)),
            // Unknown: the runner's defaults stand.
            None => true,
        }
    }
}

/// Package names compare case-insensitively with `-`, `_`, `.` equivalent (PEP 503 style;
/// exact names of other ecosystems compare equal under it too).
fn same_package(a: &str, b: &str) -> bool {
    let norm = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if matches!(c, '_' | '.') {
                    '-'
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect()
    };
    norm(a) == norm(b)
}

fn join(dir: &str, rel: &str) -> String {
    let rel = rel.trim_matches('/');
    if dir.is_empty() {
        rel.to_string()
    } else {
        format!("{dir}/{rel}")
    }
}

/// PEP 508 requirement -> distribution name (`pytest>=7; python_version>"3"` -> `pytest`).
fn requirement_name(line: &str) -> Option<String> {
    let line = line.split('#').next().unwrap_or("").trim();
    if line.is_empty() || line.starts_with('-') {
        return None;
    }
    let name: String = line
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    (!name.is_empty()).then_some(name)
}

/// `key = value` entries of one INI section (continuation lines indented).
fn ini_section(text: &str, section: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut inside = false;
    let mut last: Option<String> = None;
    for raw in text.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            inside = trimmed[1..trimmed.len() - 1].trim() == section;
            last = None;
            continue;
        }
        if !inside {
            continue;
        }
        let continuation = line.starts_with(' ') || line.starts_with('\t');
        match (continuation, &last) {
            (true, Some(key)) => {
                if let Some(v) = out.get_mut(key) {
                    v.push(' ');
                    v.push_str(trimmed);
                }
            }
            _ => {
                if let Some((k, v)) = trimmed.split_once('=').or_else(|| trimmed.split_once(':')) {
                    let key = k.trim().to_string();
                    out.insert(key.clone(), v.trim().to_string());
                    last = Some(key);
                }
            }
        }
    }
    out
}

/// A field of a Debian control file (R `DESCRIPTION`), continuation lines joined.
fn dcf_field(text: &str, field: &str) -> Option<String> {
    let mut value: Option<String> = None;
    for line in text.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(v) = value.as_mut() {
                v.push(' ');
                v.push_str(line.trim());
            }
            continue;
        }
        if value.is_some() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.trim() == field {
                value = Some(v.trim().to_string());
            }
        }
    }
    value
}

/// `groupId:artifactId` of every `<dependency>` of a POM.
fn collect_maven_dependencies(e: &trace_core::formats::xml::Element, out: &mut Vec<String>) {
    if e.local_name() == "dependency" {
        if let (Some(g), Some(a)) = (e.child("groupId"), e.child("artifactId")) {
            out.push(format!("{}:{}", g.text.trim(), a.text.trim()));
        }
    }
    for c in &e.children {
        collect_maven_dependencies(c, out);
    }
}

/// `Include` of every MSBuild `<PackageReference>`.
fn collect_package_references(e: &trace_core::formats::xml::Element, out: &mut Vec<String>) {
    if e.local_name() == "PackageReference" {
        if let Some(i) = e.attr("Include").or_else(|| e.attr("Update")) {
            out.push(i.to_string());
        }
    }
    for c in &e.children {
        collect_package_references(c, out);
    }
}

/// Whether `name` is `<stem>.{js,cjs,mjs,ts,cts,mts}`.
fn is_js_config(name: &str, stem: &str) -> bool {
    name.strip_prefix(stem)
        .and_then(|rest| rest.strip_prefix('.'))
        .is_some_and(|ext| matches!(ext, "js" | "cjs" | "mjs" | "ts" | "cts" | "mts"))
}

/// String literals of property `key` (inside property `parent` when given) of a JavaScript /
/// TypeScript configuration module, read from its syntax tree.
fn js_config_strings(name: &str, text: &str, key: &str, parent: Option<&str>) -> Vec<String> {
    let language = if name.ends_with("ts") {
        Language::TypeScript
    } else {
        Language::JavaScript
    };
    let Ok(tree) = crate::parse_tree(language, text.as_bytes()) else {
        return Vec::new();
    };
    let src = text.as_bytes();
    let key_of = |pair: tree_sitter::Node| -> Option<String> {
        let k = pair.child_by_field_name("key")?;
        let t = crate::node::text(k, src);
        Some(t.trim_matches(|c| c == '"' || c == '\'' || c == '`').to_string())
    };
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    let mut seen = 0usize;
    while let Some(node) = stack.pop() {
        seen += 1;
        if seen > 200_000 {
            break;
        }
        if node.kind() == "pair" && key_of(node).as_deref() == Some(key) {
            let under_parent = match parent {
                None => true,
                Some(p) => {
                    let mut cur = node.parent();
                    let mut found = false;
                    while let Some(c) = cur {
                        if c.kind() == "pair" {
                            found = key_of(c).as_deref() == Some(p);
                            break;
                        }
                        cur = c.parent();
                    }
                    found
                }
            };
            if under_parent {
                if let Some(v) = node.child_by_field_name("value") {
                    let items = if v.kind() == "array" {
                        crate::node::named_children(v)
                    } else {
                        vec![v]
                    };
                    for item in items {
                        if item.kind() == "string" {
                            let t = crate::node::text(item, src);
                            out.push(t.trim_matches(|c| c == '"' || c == '\'').to_string());
                        }
                    }
                }
            }
        }
        let mut cursor = node.walk();
        let children: Vec<tree_sitter::Node<'_>> = node.named_children(&mut cursor).collect();
        stack.extend(children);
    }
    out.sort();
    out.dedup();
    out
}

/// Whether trace's glob matcher reads `glob` (extended globs `?(..)`, `+(..)`, `@(..)`, `!(..)`
/// and negations are not read: such settings leave the runner's defaults in place).
fn supported_glob(glob: &str) -> bool {
    !glob.is_empty()
        && !glob.starts_with('!')
        && !["?(", "+(", "@(", "!(", "*("].iter().any(|x| glob.contains(x))
}

/// Expand `{a,b}` alternatives (nested; bounded to 64 alternatives).
fn expand_braces(glob: &str) -> Vec<String> {
    let bytes = glob.as_bytes();
    let Some(open) = glob.find('{') else {
        return vec![glob.to_string()];
    };
    let mut depth = 0;
    let mut close = None;
    let mut commas = Vec::new();
    for (i, b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            b',' if depth == 1 => commas.push(i),
            _ => {}
        }
    }
    let Some(close) = close else {
        return vec![glob.to_string()];
    };
    let (head, tail) = (&glob[..open], &glob[close + 1..]);
    let mut starts = vec![open + 1];
    starts.extend(commas.iter().map(|c| c + 1));
    let mut ends = commas.clone();
    ends.push(close);
    let mut out = Vec::new();
    for (s, e) in starts.into_iter().zip(ends) {
        for rest in expand_braces(&format!("{head}{}{tail}", &glob[s..e])) {
            if out.len() >= 64 {
                return out;
            }
            out.push(rest);
        }
    }
    out
}

/// One path segment against one glob segment (`*`, `?`, `[..]`).
fn segment_match(glob: &[char], name: &[char]) -> bool {
    match glob.first() {
        None => name.is_empty(),
        Some('*') => (0..=name.len()).any(|k| segment_match(&glob[1..], &name[k..])),
        Some('?') => !name.is_empty() && segment_match(&glob[1..], &name[1..]),
        Some('[') => {
            let Some(end) = glob.iter().position(|c| *c == ']') else {
                return name.first() == Some(&'[') && segment_match(&glob[1..], &name[1..]);
            };
            let Some(c) = name.first() else { return false };
            let class = &glob[1..end];
            let (negate, class) = match class.first() {
                Some('!') | Some('^') => (true, &class[1..]),
                _ => (false, class),
            };
            let mut hit = false;
            let mut i = 0;
            while i < class.len() {
                if i + 2 < class.len() && class[i + 1] == '-' {
                    hit |= class[i] <= *c && *c <= class[i + 2];
                    i += 3;
                } else {
                    hit |= class[i] == *c;
                    i += 1;
                }
            }
            hit != negate && segment_match(&glob[end + 1..], &name[1..])
        }
        Some(g) => name.first() == Some(g) && segment_match(&glob[1..], &name[1..]),
    }
}

fn segments_match(glob: &[&str], path: &[&str]) -> bool {
    match glob.first() {
        None => path.is_empty(),
        Some(&"**") => (0..=path.len()).any(|k| segments_match(&glob[1..], &path[k..])),
        Some(g) => {
            !path.is_empty() && {
                let gc: Vec<char> = g.chars().collect();
                let pc: Vec<char> = path[0].chars().collect();
                segment_match(&gc, &pc) && segments_match(&glob[1..], &path[1..])
            }
        }
    }
}

/// Whether `glob` matches the whole repository-relative `path`.
pub fn glob_matches(glob: &str, path: &str) -> bool {
    let path = path.replace('\\', "/");
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    expand_braces(glob).iter().any(|g| {
        let gs: Vec<&str> = g.split('/').filter(|s| !s.is_empty()).collect();
        segments_match(&gs, &segs)
    })
}

/// Whether a convention glob matches `path`: by file name when the glob has no `/`, else from
/// any directory boundary of the path. Returns the directory prefix the match started at
/// (`""` for the repository root).
pub fn glob_matches_within(glob: &str, path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let name = segs.last().copied()?;
    let expanded = expand_braces(glob);
    for g in &expanded {
        let gs: Vec<&str> = g.split('/').filter(|s| !s.is_empty()).collect();
        if !g.contains('/') {
            let gc: Vec<char> = g.chars().collect();
            let nc: Vec<char> = name.chars().collect();
            if segment_match(&gc, &nc) {
                return Some(segs[..segs.len() - 1].join("/"));
            }
            continue;
        }
        for start in 0..segs.len() {
            if segments_match(&gs, &segs[start..]) {
                return Some(segs[..start].join("/"));
            }
        }
    }
    None
}

/// Whether `path` lies under directory `dir` (repository-relative; `""` = everywhere).
fn under(dir: &str, path: &str) -> Option<String> {
    if dir.is_empty() {
        return Some(path.to_string());
    }
    path.strip_prefix(dir)
        .and_then(|r| r.strip_prefix('/'))
        .map(str::to_string)
}

/// Framework imports come from convention data, not repository paths or benchmark names.
/// This only enables syntax candidates; it never claims runner collection or execution.
pub(crate) fn is_test_call(
    language: Language,
    callee: &str,
    qualified: Option<&str>,
    test_file: bool,
) -> bool {
    let Some(key) = crate::languages::syntax(language).and_then(|s| s.library_table) else {
        return false;
    };
    rows().get(key).is_some_and(|rows| {
        rows.iter().any(|row| {
            let Some((module, member)) = row.symbol.as_deref().and_then(|s| s.rsplit_once('.')) else {
                return false;
            };
            match qualified {
                Some(path) => {
                    (path == module || path.strip_prefix(module).is_some_and(|rest| rest.starts_with('.')))
                        && Some(member) == path.rsplit('.').next()
                }
                // Conventional global test APIs are heuristics confined to test files.
                None => test_file && member == callee && member != "default",
            }
        })
    })
}

pub fn is_test_path(path: &str, language: Language, project: &TestConfig) -> bool {
    let path = path.replace('\\', "/");
    // Build-tool test source sets.
    if project.source_dirs.iter().any(|d| path.starts_with(&format!("{d}/"))) {
        return true;
    }
    let Some(key) = crate::languages::syntax(language).and_then(|s| s.library_table) else {
        return false;
    };
    let Some(rows) = rows().get(key) else {
        return false;
    };
    // The project's runner settings (only runners that have rows for this language).
    for (runner, globs) in &project.runner_globs {
        if !rows
            .iter()
            .any(|r| r.activated_by.as_deref() == Some(runner.as_str()))
        {
            continue;
        }
        for (dir, glob) in globs {
            if let Some(rest) = under(dir, &path) {
                if glob_matches(glob, &rest)
                    || glob_matches_within(glob, &rest).is_some_and(|_| !glob.contains('/'))
                {
                    return true;
                }
            }
        }
    }
    rows.iter().filter(|r| r.glob.is_some()).any(|r| {
        let active = r.language_spec
            || match r.activated_by.as_deref() {
                Some(runner) => project.runner_active(key, runner),
                None => true,
            };
        active
            && r.glob
                .as_deref()
                .is_some_and(|g| glob_matches_within(g, &path).is_some())
    })
}

/// Last path segment of an attribute spelling without arguments: `tokio::test` -> `test`,
/// `pytest.mark.parametrize(...)` -> `parametrize`.
fn attribute_name(spelling: &str) -> &str {
    let bare = spelling.split('(').next().unwrap_or(spelling).trim();
    let bare = bare.trim_start_matches(['@', '#', '[']).trim_end_matches(']');
    let after_colons = bare.rsplit("::").next().unwrap_or(bare);
    after_colons.rsplit('.').next().unwrap_or(after_colons).trim()
}

/// Attribute names and test-function name patterns of every table's rows.
struct DeclarationRules {
    attributes: BTreeSet<String>,
    name_patterns: BTreeSet<String>,
}

fn declaration_rules() -> &'static DeclarationRules {
    static RULES: OnceLock<DeclarationRules> = OnceLock::new();
    RULES.get_or_init(|| {
        let mut attributes = BTreeSet::new();
        let mut name_patterns = BTreeSet::new();
        for (key, list) in rows() {
            for r in list {
                if let Some(sym) = &r.symbol {
                    let simple = attribute_name(sym);
                    // C#: attribute classes are written without their `Attribute` suffix.
                    let simple = if *key == "csharp" {
                        simple
                            .strip_suffix("Attribute")
                            .filter(|s| !s.is_empty())
                            .unwrap_or(simple)
                    } else {
                        simple
                    };
                    // A symbol row naming a base class (`TestCase`) marks tests by its pattern.
                    if r.pattern.is_none() {
                        attributes.insert(simple.to_string());
                    }
                }
                if let Some(p) = &r.pattern {
                    if p.starts_with("#[") || p.starts_with('@') {
                        attributes.insert(attribute_name(p).to_string());
                    } else if p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '*')
                        && p.contains('*')
                    {
                        name_patterns.insert(p.clone());
                    }
                }
            }
        }
        DeclarationRules {
            attributes,
            name_patterns,
        }
    })
}

/// Whether a declaration is a test function/method by the rows' name patterns (in a test
/// file) or attribute rows (anywhere).
pub(crate) fn is_test_declaration(in_test_file: bool, decl: &Declaration) -> bool {
    if !decl.kind.is_callable() || decl.is_stub {
        return false;
    }
    let rules = declaration_rules();
    if in_test_file
        && rules.name_patterns.iter().any(|p| {
            let gc: Vec<char> = p.chars().collect();
            let nc: Vec<char> = decl.name.chars().collect();
            segment_match(&gc, &nc)
        })
    {
        return true;
    }
    decl.decorators
        .iter()
        .any(|d| rules.attributes.contains(attribute_name(d)))
}

#[cfg(test)]
#[path = "../tests/unit/testing.rs"]
mod tests;
