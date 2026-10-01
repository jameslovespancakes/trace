//! Pending languages and sub-projects: code languages used only in test / fixture / example
//! code or packaging files are set up on first use, with the reason their files carry.

use std::collections::{BTreeMap, BTreeSet};

use trace_core::model::Index;
use trace_core::repo_settings::RepoSettings;
use trace_core::{Language, SupportLevel};

use crate::languages::rules;

/// Number of files not analysed yet (pending languages and sub-projects, set up on first use).
pub fn pending_files(index: &Index) -> usize {
    index
        .files
        .iter()
        .filter(|f| f.support == SupportLevel::Pending)
        .count()
}

/// Directory names whose files are fixtures, test data or examples of a repository, not its
/// product code (compared case-insensitively).
const NON_PRODUCT_DIRS: &[&str] = &[
    "fixtures",
    "fixture",
    "testdata",
    "test-data",
    "test_data",
    "examples",
    "example",
];

/// Build configuration written in a code language (`AnalysisRules::build_scripts`, e.g. Mill
/// builds): a Java project must not need the Scala server (and approval) for its build
/// scripts alone.
fn is_build_script(path: &str, language: Language) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    rules(language).build_scripts.contains(&name)
}

/// A file that is not product code by its location: a test path
/// (`trace_syntax::is_test_path`), a file under a fixtures / testdata / examples directory,
/// or under a build tool's test source set (a directory named `<set>Test` / `<set>Tests`,
/// e.g. Gradle's `jvmTest`, `androidTest`, `integrationTests`).
fn is_non_product_location(path: &str, language: Language) -> bool {
    if is_build_script(path, language) {
        return true;
    }
    if trace_syntax::is_test_path(path, language, &trace_syntax::testing::TestConfig::default()) {
        return true;
    }
    let mut segments: Vec<&str> = path.split('/').collect();
    segments.pop();
    // Below a JVM source root (`src/<set>/java|scala|groovy/`) the directories are the
    // package path (`com/example/...`), not project layout: they never mark non-product code.
    if let Some(root) = segments
        .windows(3)
        .position(|w| w[0] == "src" && matches!(w[2], "java" | "scala" | "groovy"))
    {
        // Maven / Gradle standard layout: `src/test/<lang>` (and `src/<target>Test/<lang>`)
        // is a test source set even without a build file naming it.
        if segments[root + 1] == "test" {
            return true;
        }
        segments.truncate(root + 3);
    }
    segments.iter().any(|s| {
        NON_PRODUCT_DIRS.iter().any(|d| s.eq_ignore_ascii_case(d))
            || (s.len() > 4 && (s.ends_with("Test") || s.ends_with("Tests")))
    })
}

/// Pending languages (DESIGN §1.13): code languages whose every file is test / fixture /
/// example code ([`is_non_product_location`]) while another code language has more product
/// files than this language has files, and code languages used only in packaging files
/// ([`is_packaging_location`]: formulas, installer and release scripts) while another code
/// language has product files. They are not set up at index time; the first query that
/// needs one of their files sets them up. A language alone in a repository is never
/// pending. Test files of product languages are always analysed.
pub fn pending_languages(files: &[(&str, Language)]) -> BTreeSet<Language> {
    let mut total: BTreeMap<Language, usize> = BTreeMap::new();
    let mut product: BTreeMap<Language, usize> = BTreeMap::new();
    let mut scripts: BTreeMap<Language, usize> = BTreeMap::new();
    let mut packaging: BTreeMap<Language, usize> = BTreeMap::new();
    for &(path, language) in files.iter().filter(|(_, l)| l.is_code()) {
        *total.entry(language).or_insert(0) += 1;
        if is_build_script(path, language) {
            *scripts.entry(language).or_insert(0) += 1;
        }
        if is_packaging_location(path, language) {
            *packaging.entry(language).or_insert(0) += 1;
        } else if !is_non_product_location(path, language) {
            *product.entry(language).or_insert(0) += 1;
        }
    }
    let product_of = |l: Language| product.get(&l).copied().unwrap_or(0);
    let mut out = BTreeSet::new();
    for (&language, &count) in &total {
        // Only build scripts (Mill): build configuration, never product.
        let scripts_only = scripts.get(&language) == Some(&count);
        let dominated = total
            .keys()
            .any(|&other| other != language && product_of(other) > count);
        // Only packaging files while the repository has product code in another language.
        let packaging_only = packaging.get(&language) == Some(&count)
            && total.keys().any(|&other| other != language && product_of(other) > 0);
        if product_of(language) == 0 && (dominated || scripts_only || packaging_only) {
            out.insert(language);
        }
    }
    out
}

/// Folders whose files package, distribute or containerize the repository (compared
/// case-insensitively, at any depth): package-manager recipes (`Formula`,
/// `HomebrewFormula`, `debian`, `rpm`, `snap`), packaging, container and installer folders.
/// Hidden folders (`.github`, `.circleci`, `.devcontainer`, ...) are never inventoried.
const PACKAGING_DIRS: &[&str] = &[
    "packaging",
    "Formula",
    "HomebrewFormula",
    "debian",
    "rpm",
    "snap",
    "docker",
    "installer",
    "installers",
];

/// Sub-folders of a `pkg/` folder that name a packaging system (`pkg/brew/`, `pkg/deb/`,
/// ...). Other `pkg/` folders hold code packages (a common source layout), never packaging.
const PKG_SYSTEMS: &[&str] = &[
    "brew",
    "homebrew",
    "deb",
    "debian",
    "rpm",
    "snap",
    "aur",
    "arch",
    "archlinux",
    "chocolatey",
    "choco",
    "scoop",
    "winget",
    "flatpak",
    "nix",
    "msi",
    "docker",
];

/// A packaging file by location or role (I-09; a location / role rule, never a repository
/// name): a file below a packaging folder ([`PACKAGING_DIRS`], `pkg/<packaging system>/`), a
/// script of a script language in the root `ci/` folder, or a recipe by role - `PKGBUILD`,
/// `Dockerfile*`, and installer / release / publish scripts (`install*`, `release*`,
/// `publish*`) at the root. Packaging files are inventoried and listed; a language used only
/// in packaging files is never required at index time ([`pending_languages`]).
pub(crate) fn is_packaging_location(path: &str, language: Language) -> bool {
    let segments: Vec<&str> = path.split('/').collect();
    let Some((name, dirs)) = segments.split_last() else {
        return false;
    };
    if dirs
        .iter()
        .any(|d| PACKAGING_DIRS.iter().any(|p| d.eq_ignore_ascii_case(p)))
    {
        return true;
    }
    if dirs
        .windows(2)
        .any(|w| w[0].eq_ignore_ascii_case("pkg") && PKG_SYSTEMS.iter().any(|p| w[1].eq_ignore_ascii_case(p)))
    {
        return true;
    }
    if dirs.first().is_some_and(|d| d.eq_ignore_ascii_case("ci")) && rules(language).ci_scripts {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    if lower == "pkgbuild" || lower.starts_with("dockerfile") {
        return true;
    }
    dirs.is_empty() && ["install", "release", "publish"].iter().any(|p| lower.starts_with(p))
}

/// Code languages whose every file is a packaging file ([`is_packaging_location`]).
fn packaging_only_languages(files: &[(&str, Language)]) -> BTreeSet<Language> {
    let mut all: BTreeMap<Language, bool> = BTreeMap::new();
    for &(path, language) in files.iter().filter(|(_, l)| l.is_code()) {
        let packaging = is_packaging_location(path, language);
        all.entry(language)
            .and_modify(|only| *only &= packaging)
            .or_insert(packaging);
    }
    all.into_iter().filter(|(_, only)| *only).map(|(l, _)| l).collect()
}

/// Why the files of a pending language are not analysed yet (status, completeness).
pub(crate) fn pending_language_reason(language: Language) -> String {
    format!(
        "{} is only used in tests, fixtures, examples or build scripts here; it is set up when a query needs one of these files",
        language.display_name()
    )
}

/// Why the files of a language used only in packaging files are not analysed yet (status,
/// completeness): they are listed as packaging, never required at index time.
pub(crate) fn packaging_language_reason(language: Language) -> String {
    format!(
        "{} is only used in packaging files here (installers, release scripts, package recipes); it is set up when a query needs one of these files",
        language.display_name()
    )
}

/// Whether `path` lies in the sub-project directory `dir` (relative, `/`-separated).
fn in_dir(path: &str, dir: &str) -> bool {
    let dir = dir.trim_end_matches('/');
    !dir.is_empty() && path.len() > dir.len() && path.starts_with(dir) && path.as_bytes()[dir.len()] == b'/'
}

/// The pending reason of every file (pending languages, then the sub-projects each backend's
/// preflight reported in `Prepared::pending_dirs`, except those a query already set up).
pub(super) fn pending_reasons(
    files: &[(&str, Language)],
    pending: &BTreeSet<Language>,
    prepared: &[(Vec<Language>, &BTreeMap<String, String>)],
    settings: &RepoSettings,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let packaging = packaging_only_languages(files);
    for &(path, language) in files {
        if pending.contains(&language) {
            let reason = if packaging.contains(&language) {
                packaging_language_reason(language)
            } else {
                pending_language_reason(language)
            };
            out.insert(path.to_string(), reason);
            continue;
        }
        let dirs = prepared
            .iter()
            .find(|(languages, _)| languages.contains(&language))
            .map(|(_, dirs)| *dirs);
        if let Some((dir, reason)) = dirs.and_then(|dirs| {
            dirs.iter()
                .filter(|(dir, _)| !settings.dir_ready(dir))
                .find(|(dir, _)| in_dir(path, dir))
        }) {
            out.insert(path.to_string(), sub_project_reason(dir, reason));
        }
    }
    out
}

/// Prefix of the pending reason of a sub-project file ([`sub_project_reason`]).
const SUB_PROJECT: &str = "sub-project ";

/// "sub-project {dir}: {reason}" (the reason a preflight gave in `Prepared::pending_dirs`).
fn sub_project_reason(dir: &str, reason: &str) -> String {
    format!("{SUB_PROJECT}{}: {reason}", dir.trim_end_matches('/'))
}

/// The sub-project directory of a pending reason, `None` for a pending language.
pub(crate) fn pending_dir(reason: &str) -> Option<&str> {
    reason
        .strip_prefix(SUB_PROJECT)
        .and_then(|rest| rest.split_once(": "))
        .map(|(dir, _)| dir)
}

/// Pending languages of `files` that no query set up yet.
pub(super) fn pending_now(files: &[(&str, Language)], settings: &RepoSettings) -> BTreeSet<Language> {
    pending_languages(files)
        .into_iter()
        .filter(|l| !settings.ready_languages.contains(l))
        .collect()
}

#[cfg(test)]
#[path = "../../tests/unit/pipeline/pending.rs"]
mod tests;
