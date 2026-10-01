//! The static dependency check: the cabal plan against the store, the package index, stack
//! snapshots; Template Haskell detection.

use super::*;

/// The dependency check of the served project.
#[derive(Clone, Debug, PartialEq)]
pub struct HaskellDeps {
    pub report: DepsReport,
    /// The plan.json checked (the project's own, planned for this GHC).
    pub plan: Option<PathBuf>,
    /// No usable plan: the setup plans the build (approved) and checks the same closure.
    pub plan_needed: bool,
    /// cabal's Hackage package list is needed but missing (`cabal update`).
    pub index_missing: bool,
    /// Store directories of this compiler.
    pub store_dirs: Vec<PathBuf>,
    /// Store packages the checked closure uses.
    pub store_units: usize,
}

/// The dependency report.
pub fn deps(cx: &DetectContext<'_>, _toolchain: Option<&Toolchain>) -> DepsReport {
    let setup = detect(cx);
    haskell_deps(cx, &setup).report
}

/// The full dependency check (module docs).
pub fn haskell_deps(cx: &DetectContext<'_>, setup: &HaskellSetup) -> HaskellDeps {
    let mut out = HaskellDeps {
        report: DepsReport::none_declared(),
        plan: None,
        plan_needed: false,
        index_missing: false,
        store_dirs: Vec::new(),
        store_units: 0,
    };
    out.report.subprojects = setup.pending.clone();
    let Some(project) = &setup.project else {
        return out;
    };
    let dir = cx.root.join(&project.dir);
    let mut fp = PartsHasher::new();
    fp.text(project.tool.as_str()).text(&project.dir);
    for p in &project.packages {
        for f in markers(&cx.root.join(p)).cabal_files {
            fp.text(&relpath::read_small(&cx.root.join(p).join(&f), MAX_FILE_BYTES).unwrap_or_default());
        }
    }
    for name in ["cabal.project", "cabal.project.local", "cabal.project.freeze", "stack.yaml"] {
        fp.text(&relpath::read_small(&dir.join(name), MAX_FILE_BYTES).unwrap_or_default());
    }
    match project.tool {
        BuildTool::Direct => {}
        BuildTool::Stack => {
            let installed = fs::read_dir(dir.join(".stack-work").join("install"))
                .map(|mut rd| rd.next().is_some())
                .unwrap_or(false);
            out.report.hint = DEPS_HINT_STACK.to_string();
            out.report.status = if installed {
                DepsStatus::Installed
            } else {
                out.report.missing.push("the Stack snapshot packages".into());
                DepsStatus::Missing
            };
        }
        BuildTool::Cabal => {
            out.report.hint = DEPS_HINT_CABAL.to_string();
            let compiler_id = setup.ghc_version.as_ref().map(|v| format!("ghc-{}", version_text(v)));
            if let (Some(store), Some(cid)) = (&setup.store_dir, &compiler_id) {
                out.store_dirs = store_compiler_dirs(store, cid);
            }
            let plan_path = dir.join("dist-newstyle").join("cache").join("plan.json");
            let plan = relpath::read_small(&plan_path, MAX_FILE_BYTES)
                .and_then(|t| serde_json::from_str::<Value>(&t).ok());
            let usable = plan.as_ref().filter(|p| {
                compiler_id.is_some()
                    && p.get("compiler-id").and_then(Value::as_str) == compiler_id.as_deref()
            });
            match usable {
                Some(plan) => {
                    let (missing, units) = plan_missing(plan, &out.store_dirs);
                    fp.text(
                        &serde_json::to_string(plan.get("install-plan").unwrap_or(&Value::Null))
                            .unwrap_or_default(),
                    );
                    out.store_units = units;
                    out.plan = Some(plan_path);
                    out.index_missing = units > 0 && !package_index_present(setup.package_cache.as_deref());
                    out.report.status = if missing.is_empty() {
                        DepsStatus::Installed
                    } else {
                        DepsStatus::Missing
                    };
                    out.report.missing = missing;
                }
                None => {
                    out.plan_needed = true;
                    out.index_missing = !package_index_present(setup.package_cache.as_deref());
                    out.report.status = DepsStatus::Installed;
                    out.report
                        .notes
                        .push("dependencies are checked against the cabal build plan trace computes".into());
                }
            }
            for d in &out.store_dirs {
                out.report.roots.push(LibraryRoot {
                    path: d.clone(),
                    kind: LibraryKind::Dependency,
                    ecosystem: EcosystemId::Haskell,
                    layout: "cabal_store",
                    version: None,
                });
            }
        }
    }
    fp.text(&out.report.missing.join(","));
    out.report.fingerprint = fp.finish().hex_prefix(32);
    out
}

/// Store directories of a compiler: `<store>/<compiler-id>` (cabal < 3.12) and
/// `<store>/<compiler-id>-<abi>` (cabal >= 3.12).
pub(crate) fn store_compiler_dirs(store: &Path, compiler_id: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(store)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n == compiler_id || n.strip_prefix(compiler_id).is_some_and(|r| r.starts_with('-'))
                })
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// The Hackage package list exists in cabal's package cache.
pub(crate) fn package_index_present(cache: Option<&Path>) -> bool {
    let Some(cache) = cache else { return false };
    let hackage = cache.join("hackage.haskell.org");
    ["01-index.tar", "01-index.cache", "00-index.tar", "00-index.cache"]
        .iter()
        .any(|f| hackage.join(f).is_file())
}

/// Missing store units of a `plan.json` closure: every unit reachable (`depends`,
/// `exe-depends`, per component) from the local units that is a `global` unit not present in
/// any `store_dirs`. Returns the missing `name-version`s (sorted) and the number of store
/// units in the closure. Units outside the closure are not checked (the plan lists unbuilt
/// alternatives too); unit ids are compared as strings (Windows shortens them).
pub fn plan_missing(plan: &Value, store_dirs: &[PathBuf]) -> (Vec<String>, usize) {
    let units: Vec<&Value> = plan
        .get("install-plan")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let by_id: BTreeMap<&str, &Value> = units
        .iter()
        .filter_map(|u| Some((u.get("id")?.as_str()?, *u)))
        .collect();
    let deps_of = |u: &Value| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut push_list = |v: Option<&Value>| {
            for d in v.and_then(Value::as_array).into_iter().flatten() {
                if let Some(s) = d.as_str() {
                    out.push(s.to_string());
                }
            }
        };
        push_list(u.get("depends"));
        push_list(u.get("exe-depends"));
        if let Some(components) = u.get("components").and_then(Value::as_object) {
            for c in components.values() {
                push_list(c.get("depends"));
                push_list(c.get("exe-depends"));
            }
        }
        out
    };
    let mut queue: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for u in &units {
        let style = u.get("style").and_then(Value::as_str).unwrap_or_default();
        if matches!(style, "local" | "inplace") {
            if let Some(id) = u.get("id").and_then(Value::as_str) {
                seen.insert(id.to_string());
            }
            queue.extend(deps_of(u));
        }
    }
    let mut missing: BTreeSet<String> = BTreeSet::new();
    let mut store_units = 0usize;
    while let Some(id) = queue.pop() {
        if !seen.insert(id.clone()) || seen.len() > units.len() + 1 {
            continue;
        }
        let Some(unit) = by_id.get(id.as_str()) else { continue };
        let kind = unit.get("type").and_then(Value::as_str).unwrap_or_default();
        let style = unit.get("style").and_then(Value::as_str).unwrap_or_default();
        if kind == "configured" && style == "global" {
            store_units += 1;
            if !store_dirs.iter().any(|d| d.join(&id).is_dir()) {
                let name = unit.get("pkg-name").and_then(Value::as_str).unwrap_or(&id);
                let version = unit.get("pkg-version").and_then(Value::as_str).unwrap_or_default();
                missing.insert(if version.is_empty() {
                    name.to_string()
                } else {
                    format!("{name}-{version}")
                });
            }
        }
        queue.extend(deps_of(unit));
    }
    (missing.into_iter().collect(), store_units)
}

pub(super) fn looks_like_store(dir: &Path) -> bool {
    fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok).take(200).any(|e| {
                e.file_name().to_string_lossy().starts_with("ghc-") && e.path().join("package.db").is_dir()
            })
        })
        .unwrap_or(false)
}

/// `--env` accepts a cabal directory (with `store/`) or a cabal store.
pub(crate) fn accepts_env_path(path: &Path) -> bool {
    (path.join("store").is_dir() && (path.join("packages").is_dir() || path.join("config").is_file()))
        || looks_like_store(path)
}

/// Whether one pragma of a Haskell module makes compiling it run code: the `LANGUAGE`
/// extensions `TemplateHaskell` / `QuasiQuotes` (splices run at compile time;
/// `TemplateHaskellQuotes` only builds syntax and runs nothing), or `OPTIONS_GHC` /
/// `OPTIONS` flags `-XTemplateHaskell`, `-XQuasiQuotes` and `-fplugin` (a compiler plugin
/// runs during type checking). `text` is the pragma node's text (`{-# ... #-}`).
pub(crate) fn pragma_runs_code(text: &str) -> bool {
    let inner = text.trim().trim_start_matches("{-#").trim_end_matches("#-}");
    let mut words = inner
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty());
    let Some(kind) = words.next() else { return false };
    const RUNS: [&str; 2] = ["TemplateHaskell", "QuasiQuotes"];
    match kind.to_ascii_uppercase().as_str() {
        "LANGUAGE" => words.any(|w| RUNS.contains(&w)),
        "OPTIONS_GHC" | "OPTIONS" => words.any(|w| {
            w.strip_prefix("-X").is_some_and(|ext| RUNS.contains(&ext))
                || w == "-fplugin"
                || w.starts_with("-fplugin=")
        }),
        _ => false,
    }
}

/// A Haskell source uses Template Haskell / quasi-quotes or a compiler plugin (a pragma
/// node of the syntax tree, [`pragma_runs_code`]): compiling it runs project code.
pub fn uses_template_haskell(source: &str) -> bool {
    let Ok(tree) = trace_syntax::parse_tree(Language::Haskell, source.as_bytes()) else {
        return false;
    };
    let bytes = source.as_bytes();
    let mut cursor = tree.walk();
    let mut visited = 0usize;
    loop {
        let node = cursor.node();
        visited += 1;
        if node.kind() == "pragma" && pragma_runs_code(node.utf8_text(bytes).unwrap_or_default()) {
            return true;
        }
        // Pragmas sit at the top of a module: stop after the header.
        if visited > 20_000 {
            return false;
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return false;
            }
        }
    }
}

pub(super) fn canonical(p: &Path) -> PathBuf {
    fs::canonicalize(p)
        .map(trace_core::inventory::strip_verbatim)
        .unwrap_or_else(|_| p.to_path_buf())
}

/// A Windows known folder (`APPDATA` / `LOCALAPPDATA`): the XDG base directories of the
/// `directory` package cabal uses on Windows. None on other systems.
pub(super) fn windows_known(cx: &DetectContext<'_>, var: &str) -> Option<PathBuf> {
    (cx.platform.os == Os::Windows).then(|| cx.vars.path(var)).flatten()
}
