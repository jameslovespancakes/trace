//! R: the R toolchain, the project's library paths and the declared package
//! dependencies, found statically. Nothing of the project runs and R itself is never started:
//! `DESCRIPTION` files are DCF (read with [`parse_dcf`]), `renv.lock` is JSON, `NAMESPACE`
//! files and R sources are read with the tree-sitter-r syntax tree, `Rversion.h` with the C
//! syntax tree.
//!
//! **Toolchain** (order: project pins -> `--env` R home -> PATH -> standard install
//! locations): `DESCRIPTION` `Depends: R (>= x)` is a requirement (`TooOld { source:
//! "DESCRIPTION" }`), `renv.lock` `R.Version` selects among the installed Rs. An R home has
//! `library/base/DESCRIPTION` (its `Version` is the R version) or `include/Rversion.h`.
//! Standard locations: Windows `%ProgramFiles%\R\R-*`, `%LOCALAPPDATA%\Programs\R\R-*`
//! (registry-free); Linux `/usr/lib/R`, `/usr/lib64/R`, `/usr/local/lib/R`, `/opt/R/*/lib/R`;
//! macOS `/Library/Frameworks/R.framework/Versions/*/Resources`, Homebrew `Cellar/r/*/lib/R`.
//!
//! **Library paths** (R's own order): `--env` (a library directory), the renv project library
//! (`renv/library/[<platform>/]R-<x.y>/<triplet>`; renv isolates, so then only the renv library
//! and the system library count), `R_LIBS`, `R_LIBS_USER`, `R_LIBS_SITE`, the default user
//! library, the site libraries and `<R_HOME>/library` (base + recommended packages).
//!
//! **Dependencies**: `DESCRIPTION` `Depends` / `Imports` / `LinkingTo` (without `R` and the
//! base packages), else every package of `renv.lock`; each must have `<lib>/<pkg>/DESCRIPTION`
//! with a version meeting its constraint. Hint: `renv::restore()` with a `renv.lock`, else
//! `install.packages(c(...))` naming up to five missing packages.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use trace_core::Language;

use crate::lookup;
use crate::os::{self, Os, Platform, Version, VersionReq};
use crate::syn::Syn;
use crate::{
    hex, subdirs, DepsReport, DepsStatus, DetectContext, EcosystemId, LibraryKind, LibraryRoot, Origin,
    Toolchain, ToolchainStatus,
};

/// Hint of projects with a `renv.lock`.
pub(crate) const RENV_HINT: &str = "renv::restore()";
/// Packages with priority "base": they ship with every R.
pub const BASE_PACKAGES: &[&str] = &[
    "base",
    "compiler",
    "datasets",
    "graphics",
    "grDevices",
    "grid",
    "methods",
    "parallel",
    "splines",
    "stats",
    "stats4",
    "tcltk",
    "tools",
    "utils",
];
/// Packages attached in every R session (search order, most recently attached first).
pub const DEFAULT_ATTACHED: &[&str] =
    &["stats", "graphics", "grDevices", "utils", "datasets", "methods", "base"];

// ---------------------------------------------------------------------------------------------
// DCF (DESCRIPTION)
// ---------------------------------------------------------------------------------------------

/// Fields of the first record of a Debian Control File (`Key: value`, continuation lines start
/// with whitespace).
pub(crate) fn parse_dcf(text: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut key: Option<String> = None;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            if out.is_empty() {
                continue;
            }
            break;
        }
        if line.starts_with([' ', '\t']) {
            if let Some(k) = &key {
                if let Some(v) = out.get_mut(k) {
                    v.push(' ');
                    v.push_str(line.trim());
                }
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_string();
            out.insert(k.clone(), v.trim().to_string());
            key = Some(k);
        }
    }
    out
}

/// One declared package dependency (`pkg (>= 1.2.0)`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Dependency {
    pub name: String,
    /// `>=`, `>`, `==`, `<=`, `<`, `!=`.
    pub op: Option<String>,
    pub version: Option<String>,
    /// `Depends`, `Imports`, `LinkingTo`, `renv.lock`.
    pub field: String,
}

impl Dependency {
    /// Whether `installed` meets the constraint (no constraint: always).
    pub fn accepts(&self, installed: &Version) -> bool {
        use std::cmp::Ordering;
        let (Some(op), Some(v)) = (&self.op, self.version.as_deref().and_then(r_version)) else {
            return true;
        };
        let o = cmp_numeric(installed, &v);
        match op.as_str() {
            ">=" => o != Ordering::Less,
            ">" => o == Ordering::Greater,
            "==" | "=" => o == Ordering::Equal,
            "<=" => o != Ordering::Greater,
            "<" => o == Ordering::Less,
            "!=" => o != Ordering::Equal,
            _ => true,
        }
    }
}

/// An R package version (`1.0-2` is `1.0.2`: R treats `-` like `.`).
pub fn r_version(text: &str) -> Option<Version> {
    Version::parse(&text.trim().replace('-', "."))
}

/// Numeric comparison of the version parts (missing parts are 0).
fn cmp_numeric(a: &Version, b: &Version) -> std::cmp::Ordering {
    let n = a.parts.len().max(b.parts.len());
    (0..n)
        .map(|i| {
            a.parts
                .get(i)
                .copied()
                .unwrap_or(0)
                .cmp(&b.parts.get(i).copied().unwrap_or(0))
        })
        .find(|o| o.is_ne())
        .unwrap_or(std::cmp::Ordering::Equal)
}

/// `pkg (>= 1.0), other` -> dependencies of `field`.
pub(crate) fn parse_dependency_list(value: &str, field: &str) -> Vec<Dependency> {
    let mut out = Vec::new();
    for part in value.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (name, constraint) = match part.split_once('(') {
            Some((n, rest)) => (n.trim(), Some(rest.trim_end_matches(')').trim())),
            None => (part, None),
        };
        if name.is_empty() {
            continue;
        }
        let (op, version) = match constraint {
            Some(c) => {
                let op: String = c
                    .chars()
                    .take_while(|ch| matches!(ch, '<' | '>' | '=' | '!'))
                    .collect();
                let v = c[op.len()..].trim().to_string();
                (Some(if op.is_empty() { "==".to_string() } else { op }), Some(v))
            }
            None => (None, None),
        };
        out.push(Dependency {
            name: name.to_string(),
            op,
            version,
            field: field.to_string(),
        });
    }
    out
}

/// `Depends` (without `R`), `Imports`, `LinkingTo` of a DESCRIPTION, without base packages.
pub(crate) fn description_dependencies(fields: &BTreeMap<String, String>) -> Vec<Dependency> {
    let mut out = Vec::new();
    for field in ["Depends", "Imports", "LinkingTo"] {
        if let Some(v) = fields.get(field) {
            out.extend(
                parse_dependency_list(v, field)
                    .into_iter()
                    .filter(|d| d.name != "R" && !BASE_PACKAGES.contains(&d.name.as_str())),
            );
        }
    }
    out
}

/// The `R (>= x)` requirement of a DESCRIPTION `Depends` field.
pub(crate) fn description_r_requirement(fields: &BTreeMap<String, String>) -> Option<VersionReq> {
    let dep = parse_dependency_list(fields.get("Depends")?, "Depends")
        .into_iter()
        .find(|d| d.name == "R")?;
    let v = Version::parse(dep.version.as_deref()?)?;
    match dep.op.as_deref() {
        Some(">=") | Some(">") => Some(VersionReq::at_least(v)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Toolchain
// ---------------------------------------------------------------------------------------------

/// "4.6" from "4.6.1".
pub fn r_minor(v: &Version) -> String {
    format!("{}.{}", v.parts.first().copied().unwrap_or(0), v.parts.get(1).copied().unwrap_or(0))
}

/// An R home: `library/base/DESCRIPTION` or `include/Rversion.h`.
pub(crate) fn is_r_home(dir: &Path) -> bool {
    dir.join("library").join("base").join("DESCRIPTION").is_file()
        || dir.join("include").join("Rversion.h").is_file()
}

/// The R version of an R home (base package version, else `Rversion.h`).
pub(crate) fn home_version(home: &Path) -> Option<Version> {
    let base = fs::read_to_string(home.join("library").join("base").join("DESCRIPTION")).ok();
    if let Some(v) = base.and_then(|t| parse_dcf(&t).get("Version").and_then(|v| Version::parse(v))) {
        return Some(v);
    }
    let header = fs::read(home.join("include").join("Rversion.h")).ok()?;
    rversion_header(&header)
}

/// `#define R_MAJOR "4"` + `#define R_MINOR "6.1"` read from the C syntax tree.
pub(crate) fn rversion_header(source: &[u8]) -> Option<Version> {
    let syn = Syn::parse(Language::C, source)?;
    let mut major = None;
    let mut minor = None;
    for i in 0..syn.len() {
        if syn.kind(i) != "preproc_def" {
            continue;
        }
        let (Some(name), Some(value)) = (syn.field(i, "name"), syn.field(i, "value")) else {
            continue;
        };
        let value = syn.text(value).trim().trim_matches('"').to_string();
        match syn.text(name) {
            "R_MAJOR" => major = Some(value),
            "R_MINOR" => minor = Some(value),
            _ => {}
        }
    }
    Version::parse(&format!("{}.{}", major?, minor?))
}

/// `Rscript` of an R home (Windows `bin/x64/Rscript.exe`, else `bin/Rscript`).
pub(crate) fn rscript(home: &Path, platform: &Platform) -> Option<PathBuf> {
    [home.join("bin").join("x64"), home.join("bin")]
        .into_iter()
        .map(|d| d.join(platform.exe("Rscript")))
        .find(|p| p.is_file())
}

/// `R` of an R home (Windows `bin/x64/R.exe`, else `bin/R`).
pub(crate) fn r_exe(home: &Path, platform: &Platform) -> Option<PathBuf> {
    [home.join("bin").join("x64"), home.join("bin")]
        .into_iter()
        .map(|d| d.join(platform.exe("R")))
        .find(|p| p.is_file())
}

fn toolchain_of(home: PathBuf, origin: Origin, platform: &Platform) -> Toolchain {
    let version = home_version(&home);
    let mut executables = BTreeMap::new();
    if let Some(p) = rscript(&home, platform) {
        executables.insert("Rscript".to_string(), p);
    }
    if let Some(p) = r_exe(&home, platform) {
        executables.insert("R".to_string(), p);
    }
    let mut facts = BTreeMap::new();
    if let Some(v) = &version {
        facts.insert("r_minor".to_string(), r_minor(v));
    }
    facts.insert("library".to_string(), home.join("library").display().to_string());
    let bin = [home.join("bin").join("x64"), home.join("bin")]
        .into_iter()
        .find(|d| d.join(platform.exe("Rscript")).is_file())
        .unwrap_or_else(|| home.join("bin"));
    facts.insert("bindir".to_string(), bin.display().to_string());
    Toolchain {
        id: "r",
        root: home,
        version,
        executables,
        origin,
        facts,
    }
}

/// The R home of an `R` / `Rscript` found on PATH (`<home>/bin[/x64]/R`); symlinks are
/// followed outside Windows (`/usr/local/bin/R` -> the framework).
fn home_of_exe(exe: &Path, platform: &Platform) -> Option<PathBuf> {
    let exe = if platform.os == Os::Windows {
        exe.to_path_buf()
    } else {
        fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf())
    };
    let mut bin = exe.parent()?.to_path_buf();
    if matches!(bin.file_name().and_then(|n| n.to_str()), Some("x64" | "i386")) {
        bin = bin.parent()?.to_path_buf();
    }
    let home = bin.parent()?.to_path_buf();
    is_r_home(&home).then_some(home)
}

/// Standard R homes per OS (newest first within each location).
fn standard_homes(vars: &os::EnvVars, platform: &Platform) -> Vec<PathBuf> {
    let versioned = |dir: &Path, prefix: &str| -> Vec<PathBuf> {
        os::versioned_children(dir, prefix)
            .into_iter()
            .map(|(_, p)| p)
            .collect()
    };
    let mut out = Vec::new();
    match platform.os {
        Os::Windows => {
            for key in ["ProgramW6432", "ProgramFiles"] {
                if let Some(pf) = vars.path(key) {
                    out.extend(versioned(&pf.join("R"), "R-"));
                }
            }
            if let Some(local) = vars.path("LOCALAPPDATA") {
                out.extend(versioned(&local.join("Programs").join("R"), "R-"));
            }
        }
        Os::Linux => {
            for d in ["/usr/lib/R", "/usr/lib64/R", "/usr/local/lib/R", "/usr/local/lib64/R"] {
                out.push(PathBuf::from(d));
            }
            out.extend(
                versioned(Path::new("/opt/R"), "")
                    .into_iter()
                    .map(|p| p.join("lib").join("R")),
            );
        }
        Os::MacOs => {
            out.extend(
                versioned(Path::new("/Library/Frameworks/R.framework/Versions"), "")
                    .into_iter()
                    .map(|p| p.join("Resources")),
            );
            for cellar in ["/opt/homebrew/Cellar/r", "/usr/local/Cellar/r"] {
                out.extend(
                    versioned(Path::new(cellar), "")
                        .into_iter()
                        .map(|p| p.join("lib").join("R")),
                );
            }
            out.extend(
                versioned(Path::new("/opt/R"), "")
                    .into_iter()
                    .map(|p| p.join("lib").join("R")),
            );
        }
    }
    out
}

/// The R version `renv.lock` records (`{"R": {"Version": "4.4.1"}}`).
pub(crate) fn renv_r_version(root: &Path) -> Option<Version> {
    let text = fs::read_to_string(root.join("renv.lock")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    Version::parse(v.get("R")?.get("Version")?.as_str()?)
}

/// The R for this project (see the module docs for the order).
pub fn toolchain(cx: &DetectContext<'_>) -> ToolchainStatus {
    let mut candidates: Vec<(PathBuf, Origin)> = Vec::new();
    let push = |home: PathBuf, origin: Origin, out: &mut Vec<(PathBuf, Origin)>| {
        let key = home.to_string_lossy().replace('\\', "/").to_lowercase();
        if cx.allowed(&home)
            && is_r_home(&home)
            && !out
                .iter()
                .any(|(h, _)| h.to_string_lossy().replace('\\', "/").to_lowercase() == key)
        {
            out.push((home, origin));
        }
    };
    if let Some(path) = cx.env_override.filter(|p| is_r_home(p)) {
        push(path.to_path_buf(), Origin::Override, &mut candidates);
    }
    if let Some(exe) = lookup::on_path(&["Rscript", "R"], cx.vars, cx.platform) {
        if let Some(home) = home_of_exe(&exe, cx.platform) {
            push(home, Origin::Path, &mut candidates);
        }
    }
    for home in standard_homes(cx.vars, cx.platform) {
        push(home, Origin::StandardLocation, &mut candidates);
    }
    let searched = vec!["PATH (Rscript, R)".to_string(), "standard R install locations".to_string()];
    if candidates.is_empty() {
        return ToolchainStatus::Missing { searched };
    }
    let requirement = fs::read_to_string(cx.root.join("DESCRIPTION"))
        .ok()
        .and_then(|t| description_r_requirement(&parse_dcf(&t)));
    let pin = renv_r_version(cx.root);
    let versions: Vec<Option<Version>> = candidates.iter().map(|(h, _)| home_version(h)).collect();
    let satisfies = |v: &Option<Version>| match (&requirement, v) {
        (Some(req), Some(v)) => req.matches(v),
        _ => true,
    };
    let pinned = |v: &Option<Version>| match (&pin, v) {
        (Some(p), Some(v)) => r_minor(p) == r_minor(v),
        _ => false,
    };
    // A user-chosen R (`--env`) always wins; pins select among the others.
    let pick = if candidates[0].1 == Origin::Override {
        satisfies(&versions[0]).then_some((0, Origin::Override))
    } else {
        (0..candidates.len())
            .find(|i| pinned(&versions[*i]) && satisfies(&versions[*i]))
            .map(|i| (i, Origin::Pin))
            .or_else(|| {
                (0..candidates.len())
                    .find(|i| satisfies(&versions[*i]))
                    .map(|i| (i, candidates[i].1))
            })
    };
    match (pick, requirement) {
        (Some((i, origin)), _) => {
            ToolchainStatus::Found(toolchain_of(candidates[i].0.clone(), origin, cx.platform))
        }
        (None, Some(needed)) => {
            let (home, origin) = candidates[0].clone();
            ToolchainStatus::TooOld {
                found: toolchain_of(home, origin, cx.platform),
                needed,
                source: "DESCRIPTION".into(),
            }
        }
        // Without a requirement every candidate satisfies it, so a pick exists.
        (None, None) => {
            ToolchainStatus::Found(toolchain_of(candidates[0].0.clone(), candidates[0].1, cx.platform))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Library paths and dependencies
// ---------------------------------------------------------------------------------------------

/// An installed package directory: `DESCRIPTION` + `Meta/`.
fn is_installed_package(dir: &Path) -> bool {
    dir.join("DESCRIPTION").is_file() && dir.join("Meta").is_dir()
}

/// A library directory: at least one child is an installed package.
pub(crate) fn accepts_env_path(path: &Path) -> bool {
    subdirs(path).iter().take(2000).any(|(_, p)| is_installed_package(p))
}

/// The renv project library for R `minor` (renv >= 1.0 `renv/library/<platform>/R-<x.y>/<triplet>`,
/// older `renv/library/R-<x.y>/<triplet>`).
pub(crate) fn renv_library(root: &Path, minor: &str) -> Option<PathBuf> {
    let base = root.join("renv").join("library");
    let want = format!("R-{minor}");
    let mut found: Vec<PathBuf> = Vec::new();
    for (name, dir) in subdirs(&base) {
        if name == want {
            found.extend(subdirs(&dir).into_iter().map(|(_, p)| p));
        } else {
            for (inner, idir) in subdirs(&dir) {
                if inner == want {
                    found.extend(subdirs(&idir).into_iter().map(|(_, p)| p));
                }
            }
        }
    }
    found.sort();
    found.into_iter().next()
}

/// Everything the R preflight needs, computed once.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RSetup {
    /// Project library directories in R's order, without the system library.
    pub libraries: Vec<PathBuf>,
    /// `<R_HOME>/library`.
    pub system_library: Option<PathBuf>,
    /// The renv library in use.
    pub renv_library: Option<PathBuf>,
    /// The project has a `renv.lock`.
    pub renv: bool,
    /// Declared dependencies (DESCRIPTION, else renv.lock).
    pub declared: Vec<Dependency>,
    /// `--env` points to no R library.
    pub env_not_found: Option<PathBuf>,
    pub deps: DepsReport,
}

/// Declared packages against the library paths.
pub fn deps(cx: &DetectContext<'_>, toolchain: Option<&Toolchain>) -> DepsReport {
    setup(cx, toolchain).deps
}

fn split_list(v: &std::ffi::OsStr) -> Vec<PathBuf> {
    std::env::split_paths(v)
        .filter(|p| p.is_absolute() && !p.to_string_lossy().contains('%'))
        .collect()
}

/// Library paths, the declared dependencies and the dependency report.
pub fn setup(cx: &DetectContext<'_>, toolchain: Option<&Toolchain>) -> RSetup {
    let root = cx.root;
    let version = toolchain.and_then(|t| t.version.clone());
    let minor = version.as_ref().map(r_minor);
    let system_library = toolchain.map(|t| t.root.join("library")).filter(|p| p.is_dir());
    let renv = root.join("renv.lock").is_file();
    let renv_lib = minor.as_deref().and_then(|m| renv_library(root, m));

    let mut env_not_found = None;
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(path) = cx.env_override {
        if accepts_env_path(path) {
            dirs.push(path.to_path_buf());
        } else if !is_r_home(path) {
            env_not_found = Some(path.to_path_buf());
        }
    }
    if let Some(lib) = &renv_lib {
        dirs.push(lib.clone());
    } else {
        for key in ["R_LIBS", "R_LIBS_USER", "R_LIBS_SITE"] {
            if let Some(v) = cx.vars.get(key) {
                dirs.extend(split_list(v));
            }
        }
        let home = os::home_dir(cx.vars, cx.platform);
        if let Some(m) = &minor {
            match cx.platform.os {
                Os::Windows => {
                    if let Some(local) = cx.vars.path("LOCALAPPDATA") {
                        dirs.push(local.join("R").join("win-library").join(m));
                    }
                    if let Some(h) = &home {
                        dirs.push(h.join("Documents").join("R").join("win-library").join(m));
                    }
                }
                Os::Linux => {
                    if let Some(h) = &home {
                        for (name, dir) in subdirs(&h.join("R")) {
                            if name.ends_with("-library") {
                                dirs.push(dir.join(m));
                            }
                        }
                    }
                }
                Os::MacOs => {
                    if let Some(h) = &home {
                        let lib = h.join("Library").join("R");
                        for arch in ["arm64", "x86_64"] {
                            dirs.push(lib.join(arch).join(m).join("library"));
                        }
                        dirs.push(lib.join(m).join("library"));
                    }
                }
            }
        }
        if let Some(t) = toolchain {
            dirs.push(t.root.join("site-library"));
        }
        if cx.platform.os == Os::Linux {
            dirs.push(PathBuf::from("/usr/local/lib/R/site-library"));
            dirs.push(PathBuf::from("/usr/lib/R/site-library"));
        }
    }
    let mut libraries: Vec<PathBuf> = Vec::new();
    for d in dirs {
        if d.is_dir() && cx.allowed(&d) && !libraries.contains(&d) && Some(&d) != system_library.as_ref() {
            libraries.push(d);
        }
    }

    let description = fs::read_to_string(root.join("DESCRIPTION"))
        .ok()
        .map(|t| parse_dcf(&t));
    let declared: Vec<Dependency> = match &description {
        Some(fields) => description_dependencies(fields),
        None if renv => renv_packages(root),
        None => Vec::new(),
    };

    let mut all_libs: Vec<&PathBuf> = libraries.iter().collect();
    if let Some(s) = &system_library {
        all_libs.push(s);
    }
    let mut fp = blake3::Hasher::new();
    fp.update(b"r-deps-v1\0");
    for l in &all_libs {
        fp.update(l.to_string_lossy().as_bytes());
        fp.update(b"\0");
    }
    for f in ["DESCRIPTION", "renv.lock", "NAMESPACE"] {
        fp.update(&fs::read(root.join(f)).unwrap_or_default());
    }
    let mut missing: BTreeSet<String> = BTreeSet::new();
    for dep in &declared {
        let installed = all_libs.iter().find_map(|lib| {
            let desc = lib.join(&dep.name).join("DESCRIPTION");
            let text = fs::read_to_string(desc).ok()?;
            parse_dcf(&text).get("Version").and_then(|v| r_version(v))
        });
        match &installed {
            Some(v) if dep.accepts(v) => {
                fp.update(dep.name.as_bytes());
                fp.update(v.text.as_bytes());
            }
            _ => {
                missing.insert(dep.name.clone());
            }
        }
    }
    let missing: Vec<String> = missing.into_iter().collect();
    let status = if declared.is_empty() {
        DepsStatus::NoneDeclared
    } else if missing.is_empty() {
        DepsStatus::Installed
    } else {
        DepsStatus::Missing
    };
    let hint = if renv {
        RENV_HINT.to_string()
    } else {
        install_hint(&missing)
    };
    let mut roots: Vec<LibraryRoot> = libraries
        .iter()
        .map(|p| LibraryRoot {
            path: p.clone(),
            kind: LibraryKind::Dependency,
            ecosystem: EcosystemId::R,
            layout: "r_library",
            version: None,
        })
        .collect();
    if let Some(s) = &system_library {
        roots.push(LibraryRoot {
            path: s.clone(),
            kind: LibraryKind::Stdlib,
            ecosystem: EcosystemId::R,
            layout: "r_library",
            version: version.as_ref().map(|v| v.text.clone()),
        });
    }
    let mut notes = Vec::new();
    if let Some(lib) = &renv_lib {
        notes.push(format!("R library: renv ({})", lib.display()));
    } else if renv {
        notes.push("renv.lock without a renv library for this R".to_string());
    }
    RSetup {
        libraries,
        system_library,
        renv_library: renv_lib,
        renv,
        declared,
        env_not_found,
        deps: DepsReport {
            status,
            missing,
            hint,
            roots,
            fingerprint: hex(fp),
            subprojects: Vec::new(),
            notes,
        },
    }
}

/// `install.packages(c("a", "b"))` with up to five names (`and N more` after them).
pub fn install_hint(missing: &[String]) -> String {
    if missing.is_empty() {
        return "install.packages(...)".to_string();
    }
    let shown: Vec<String> = missing.iter().take(5).map(|n| format!("\"{n}\"")).collect();
    let call = if shown.len() == 1 {
        format!("install.packages({})", shown[0])
    } else {
        format!("install.packages(c({}))", shown.join(", "))
    };
    if missing.len() > 5 {
        format!("{call} and {} more", missing.len() - 5)
    } else {
        call
    }
}

/// Every package of `renv.lock` (`Packages` object).
fn renv_packages(root: &Path) -> Vec<Dependency> {
    let Ok(text) = fs::read_to_string(root.join("renv.lock")) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let Some(map) = v.get("Packages").and_then(Value::as_object) else {
        return Vec::new();
    };
    map.iter()
        .map(|(name, p)| Dependency {
            name: p.get("Package").and_then(Value::as_str).unwrap_or(name).to_string(),
            op: Some(">=".into()),
            version: p.get("Version").and_then(Value::as_str).map(str::to_string),
            field: "renv.lock".into(),
        })
        .filter(|d| !BASE_PACKAGES.contains(&d.name.as_str()))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// NAMESPACE directives and package symbols (syntax trees; used to name deparsed server targets)
// ---------------------------------------------------------------------------------------------

/// Directives of a `NAMESPACE` file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Namespace {
    /// `import(pkg)` in file order.
    pub imports: Vec<String>,
    /// `importFrom(pkg, sym, ...)`: (pkg, sym) in file order.
    pub import_from: Vec<(String, String)>,
    /// `export(name)`.
    pub exports: Vec<String>,
    /// An `exportPattern(...)` exists (the exports cannot be listed statically).
    pub export_pattern: bool,
}

/// Unnamed argument values of a call (identifiers and strings).
fn call_arguments(syn: &Syn<'_>, call: usize) -> (Vec<String>, Vec<(String, usize)>) {
    let mut positional = Vec::new();
    let mut named = Vec::new();
    let Some(args) = syn.field(call, "arguments") else {
        return (positional, named);
    };
    for arg in syn.named_children(args) {
        if syn.kind(arg) != "argument" {
            continue;
        }
        let Some(value) = syn.field(arg, "value") else { continue };
        match syn.field(arg, "name") {
            Some(n) => named.push((syn.text(n).to_string(), value)),
            None => {
                if let Some(v) = syn.literal(value) {
                    positional.push(v);
                }
            }
        }
    }
    (positional, named)
}

/// Read `NAMESPACE` directives from the R syntax tree (never evaluated).
pub fn read_namespace(source: &[u8]) -> Namespace {
    let mut ns = Namespace::default();
    let Some(syn) = Syn::parse(Language::R, source) else {
        return ns;
    };
    for i in 0..syn.len() {
        if syn.kind(i) != "call" {
            continue;
        }
        let Some(function) = syn.field(i, "function") else { continue };
        if syn.kind(function) != "identifier" {
            continue;
        }
        let (positional, _) = call_arguments(&syn, i);
        match syn.text(function) {
            "import" => ns.imports.extend(positional),
            "importFrom" => {
                if let Some((pkg, syms)) = positional.split_first() {
                    ns.import_from.extend(syms.iter().map(|s| (pkg.clone(), s.clone())));
                }
            }
            "export" => ns.exports.extend(positional),
            "exportPattern" => ns.export_pattern = true,
            _ => {}
        }
    }
    ns
}

/// Explicit `pkg::sym` / `pkg:::sym` uses of an R source.
pub fn qualified_symbols(source: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(syn) = Syn::parse(Language::R, source) else {
        return out;
    };
    for i in 0..syn.len() {
        if syn.kind(i) != "namespace_operator" {
            continue;
        }
        let (Some(lhs), Some(rhs)) = (syn.field(i, "lhs"), syn.field(i, "rhs")) else {
            continue;
        };
        if let (Some(pkg), Some(sym)) = (syn.literal(lhs), syn.literal(rhs)) {
            out.push((pkg, sym));
        }
    }
    out
}

/// Packages attached by `library(pkg)` / `require(pkg)` calls of an R source, in order
/// (`character.only = TRUE` calls name no package statically and are skipped).
pub fn attached_packages(source: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let Some(syn) = Syn::parse(Language::R, source) else {
        return out;
    };
    for i in 0..syn.len() {
        if syn.kind(i) != "call" {
            continue;
        }
        let Some(function) = syn.field(i, "function") else { continue };
        if !matches!(syn.text(function), "library" | "require") {
            continue;
        }
        let (positional, named) = call_arguments(&syn, i);
        let character_only = named
            .iter()
            .any(|(n, v)| n == "character.only" && matches!(syn.text(*v), "TRUE" | "T"));
        if character_only {
            continue;
        }
        if let Some(pkg) = positional.into_iter().next() {
            out.push(pkg);
        }
    }
    out
}

/// Documented topics of an installed package (`help/AnIndex`: `topic<TAB>file` lines).
pub(crate) fn help_topics(package_dir: &Path) -> BTreeSet<String> {
    fs::read_to_string(package_dir.join("help").join("AnIndex"))
        .map(|t| {
            t.lines()
                .filter_map(|l| l.split('\t').next())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Exported names of an installed package: `NAMESPACE` `export()` names, plus the documented
/// topics when the package exports by pattern (or has no NAMESPACE file, like `base`).
pub fn package_exports(package_dir: &Path) -> BTreeSet<String> {
    let ns = fs::read(package_dir.join("NAMESPACE"))
        .ok()
        .map(|b| read_namespace(&b));
    let mut out: BTreeSet<String> = ns
        .as_ref()
        .map(|n| n.exports.iter().cloned().collect())
        .unwrap_or_default();
    if ns.as_ref().is_none_or(|n| n.export_pattern) {
        out.extend(help_topics(package_dir));
    }
    out
}

/// The installed directory of `package` in the first library holding it.
pub fn package_dir(libraries: &[PathBuf], package: &str) -> Option<PathBuf> {
    libraries
        .iter()
        .map(|l| l.join(package))
        .find(|p| p.join("DESCRIPTION").is_file())
}

/// Installed version of a package directory.
pub fn package_version(package_dir: &Path) -> Option<String> {
    let text = fs::read_to_string(package_dir.join("DESCRIPTION")).ok()?;
    parse_dcf(&text).get("Version").cloned()
}

/// The R ecosystem ([`crate::Ecosystem`]).
pub struct R;

impl crate::Ecosystem for R {
    fn id(&self) -> crate::EcosystemId {
        crate::EcosystemId::R
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
#[path = "../../tests/unit/ecosystems/r.rs"]
mod tests;
