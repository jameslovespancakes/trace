//! R setup hooks (owner r): the R languageserver package in an isolated R session.
//!
//! **Preflight** (collects every independent failure):
//! 1. toolchain (`trace_env::r::toolchain`): R, and the `Depends: R (>= x)` of the project's
//!    DESCRIPTION (`ToolchainVersion`);
//! 2. server: languageserver installed for this R minor (`<tools>/r-languageserver/<v>/R-<x.y>`,
//!    binary packages from the pinned Posit snapshot); an R minor / platform / Linux
//!    distribution without pinned binaries is `ServerUnavailable` naming the pinned R minors;
//! 3. dependencies: DESCRIPTION `Depends` / `Imports` / `LinkingTo` (or `renv.lock`) installed
//!    in the library paths (renv aware); `--env` must be an R library;
//! 4. approval, always: the R session loads the namespaces of the project's dependencies
//!    (`.onLoad` code, DLLs) -> `trace index --allow-build`.
//!
//! **Launch** (registry): `<R_HOME>/bin[/x64]/Rscript --no-init-file --no-environ -e
//! languageserver::run()` with `R_LIBS` / `R_LIBS_USER` = the project libraries + trace's
//! library (last, so the project's versions win), empty `R_PROFILE_USER` / `R_ENVIRON_USER`
//! files in the state dir (languageserver's callr workers would otherwise source the project's
//! `.Rprofile`, which may download renv) and `TEMP`/`TMP`/`TMPDIR` in the state dir.
//! **Readiness**: languageserver sends no progress; the client polls `workspace/symbol` with
//! `vars["symbol_poll_query"]` (the 3-character prefix of declared names found in the most
//! files) until the count is non-zero and stable twice.
//!
//! **Library targets**: definitions into installed packages are deparsed into
//! `<tmp>/Rtmp*/<symbol>.R` ("# Generated from function body. ..."); they are named
//! `<pkg>::<symbol>` with the package from the syntax: explicit `pkg::` uses, NAMESPACE
//! `importFrom` / `import` (the installed package's exports), `library()` / `require()`
//! attachments, then R's default packages. A symbol two packages could supply is not named.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use trace_core::facts::FileFacts;
use trace_core::fingerprint::PartsHasher;
use trace_core::model::{Diagnostic, SymbolKind};
use trace_core::setup_error::SetupError;
use trace_core::Language;
use trace_env::r::{
    attached_packages, package_dir, package_exports, package_version, qualified_symbols, r_minor,
    read_namespace, BASE_PACKAGES, DEFAULT_ATTACHED,
};
use trace_env::{EcosystemId, ToolchainStatus};

use super::{
    default_prepared, ExternalLocation, LoadedContext, Prepared, Server, SetupContext, WorkspaceContext,
};
use super::{detect_context, toolchain_spec};
use crate::backends::fntype::FnTypeRoute;
use crate::install::platform_select::{r_binaries_for, r_minors_for};
use crate::registry::{BuildSpec, BuildWhen, Recipe};
use crate::setup::{
    deps_error, require_approval, require_server, toolchain_error, Collect, STATUS_DEPENDENCIES,
};
use trace_env::lookup::compose_path;

pub struct Hooks;

/// Install id when the registry entry has no install record.
const DEFAULT_INSTALL_ID: &str = "r-languageserver";
/// First line of the files languageserver deparses package functions into.
pub const DEPARSED_HEADER: &str = "# Generated from function body. Editing this file has no effect.";
/// R sources larger than this are not read for package qualifiers.
const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;

/// Package names of deparsed targets, computed lazily from the repository (Prepared.data).
#[derive(Debug)]
pub struct RData {
    /// R sources of the repository (absolute, original tree).
    pub r_files: Vec<PathBuf>,
    /// The project's NAMESPACE file, if any.
    pub namespace: Option<PathBuf>,
    /// Library directories in R's order: project libraries, trace's library, the system library.
    pub libraries: Vec<PathBuf>,
    pub system_library: Option<PathBuf>,
    symbols: OnceLock<SymbolSources>,
    exports: Mutex<BTreeMap<String, Arc<BTreeSet<String>>>>,
}

#[derive(Debug, Default)]
struct SymbolSources {
    /// `pkg::sym` uses: sym -> packages.
    explicit: BTreeMap<String, BTreeSet<String>>,
    /// NAMESPACE `importFrom(pkg, sym)` (a later directive wins, like R).
    import_from: BTreeMap<String, String>,
    /// NAMESPACE `import(pkg)` in file order.
    imports: Vec<String>,
    /// `library(pkg)` / `require(pkg)` in file order.
    attached: Vec<String>,
}

impl RData {
    pub fn new(
        r_files: Vec<PathBuf>,
        namespace: Option<PathBuf>,
        libraries: Vec<PathBuf>,
        system_library: Option<PathBuf>,
    ) -> RData {
        RData {
            r_files,
            namespace,
            libraries,
            system_library,
            symbols: OnceLock::new(),
            exports: Mutex::new(BTreeMap::new()),
        }
    }

    fn sources(&self) -> &SymbolSources {
        self.symbols.get_or_init(|| {
            let mut s = SymbolSources::default();
            for file in &self.r_files {
                if fs::metadata(file).map(|m| m.len() > MAX_SOURCE_BYTES).unwrap_or(true) {
                    continue;
                }
                let Ok(bytes) = fs::read(file) else { continue };
                for (pkg, sym) in qualified_symbols(&bytes) {
                    s.explicit.entry(sym).or_default().insert(pkg);
                }
                s.attached.extend(attached_packages(&bytes));
            }
            if let Some(ns) = self.namespace.as_ref().and_then(|p| fs::read(p).ok()) {
                let ns = read_namespace(&ns);
                for (pkg, sym) in ns.import_from {
                    s.import_from.insert(sym, pkg);
                }
                s.imports = ns.imports;
            }
            s
        })
    }

    fn exports_of(&self, package: &str) -> Arc<BTreeSet<String>> {
        if let Ok(cache) = self.exports.lock() {
            if let Some(e) = cache.get(package) {
                return e.clone();
            }
        }
        let exports = Arc::new(
            package_dir(&self.libraries, package)
                .map(|d| package_exports(&d))
                .unwrap_or_default(),
        );
        if let Ok(mut cache) = self.exports.lock() {
            cache.insert(package.to_string(), exports.clone());
        }
        exports
    }

    /// The package that supplies `symbol` to this project, when exactly one can.
    pub fn package_of(&self, symbol: &str) -> Option<String> {
        let s = self.sources();
        let mut candidates: BTreeSet<String> = s.explicit.get(symbol).cloned().unwrap_or_default();
        let provides = |pkg: &str| self.exports_of(pkg).contains(symbol);
        let implicit = s
            .import_from
            .get(symbol)
            .cloned()
            .or_else(|| s.imports.iter().rev().find(|p| provides(p)).cloned())
            .or_else(|| s.attached.iter().rev().find(|p| provides(p)).cloned())
            .or_else(|| DEFAULT_ATTACHED.iter().find(|p| provides(p)).map(|p| p.to_string()));
        candidates.extend(implicit);
        if candidates.len() == 1 {
            candidates.into_iter().next()
        } else {
            None
        }
    }
}

impl Server for Hooks {
    fn preflight(&self, cx: &SetupContext<'_>) -> Result<Prepared, SetupError> {
        let language = Language::R;
        let mut collect = Collect::default();
        let dcx = detect_context(cx, EcosystemId::R);
        let spec = toolchain_spec(cx, "r", "R", "Install it from https://cloud.r-project.org");

        // 1. Toolchain.
        let status = trace_env::r::toolchain(&dcx);
        if let Some(e) = toolchain_error(language, &spec, &status) {
            collect.push(e);
        }
        let toolchain = match status {
            ToolchainStatus::Found(t) => Some(t),
            _ => None,
        };
        let minor = toolchain.as_ref().and_then(|t| t.version.as_ref()).map(r_minor);

        // 2. Server (languageserver for this R minor).
        let install_id = cx
            .entry
            .install
            .as_ref()
            .map(|i| i.id.clone())
            .unwrap_or_else(|| DEFAULT_INSTALL_ID.to_string());
        let tools_library = minor
            .as_ref()
            .and_then(|m| cx.tools.tool_dir(&install_id).map(|d| d.join(format!("R-{m}"))));
        let server_ok = require_server(cx);
        if let Some(m) = &minor {
            if let Some(unavailable) = unavailable_for_minor(cx, m) {
                collect.push(unavailable);
            } else if let Err(e) = server_ok {
                collect.push(e);
            } else if !tools_library
                .as_deref()
                .is_some_and(|l| l.join("languageserver").join("DESCRIPTION").is_file())
            {
                collect.push(SetupError::ServerMissing { language });
            }
        } else if let Err(e) = server_ok {
            collect.push(e);
        }

        // 3. Dependencies (the library paths depend on the R minor).
        let readable = trace_core::paths::forbidden_roots();
        let setup =
            trace_env::r::setup(&super::read_context(cx, EcosystemId::R, &readable), toolchain.as_ref());
        if let Some(path) = &setup.env_not_found {
            collect.push(SetupError::EnvNotFound {
                language: Some(language),
                path: path.clone(),
            });
        } else if toolchain.is_some() {
            if let Some(e) = deps_error(language, &setup.deps) {
                collect.push(e);
            }
        }

        // 4. Approval: the R session always loads the dependencies' namespaces.
        collect.check(require_approval(cx, &build_spec(cx)));

        let toolchain = match toolchain {
            Some(t) if collect.is_empty() => t,
            _ => {
                if collect.is_empty() {
                    collect.push(SetupError::ToolchainMissing {
                        language,
                        needs: spec.needs.clone(),
                        install: spec.install.clone(),
                    });
                }
                return collect.finish(default_prepared(cx));
            }
        };
        let Some(tools_library) = tools_library else {
            return Err(SetupError::ServerMissing { language });
        };

        let mut prepared = default_prepared(cx);
        let sep = cx.platform.path_list_sep().to_string();
        let mut libs: Vec<String> = setup.libraries.iter().map(|p| p.display().to_string()).collect();
        libs.push(tools_library.display().to_string());
        let libs = libs.join(&sep);
        prepared.env.insert("R_LIBS".into(), libs.clone());
        prepared.env.insert("R_LIBS_USER".into(), libs);
        let bindir = toolchain
            .facts
            .get("bindir")
            .map(PathBuf::from)
            .unwrap_or_else(|| toolchain.root.join("bin"));
        prepared
            .env
            .insert("PATH".into(), compose_path(&[bindir], cx.vars, cx.platform));
        prepared
            .vars
            .insert("toolchain".into(), toolchain.root.display().to_string());
        let names = declared_names(cx);
        if let Some(query) = symbol_poll_query(&names) {
            prepared.vars.insert("symbol_poll_query".into(), query);
        }
        prepared.library_roots = setup.deps.roots.clone();
        prepared.runs_project_code = true;
        let version = toolchain
            .version
            .as_ref()
            .map(|v| v.text.clone())
            .unwrap_or_else(|| "unknown".into());
        let mut fp = PartsHasher::new();
        fp.text(&toolchain.root.display().to_string())
            .text(&version)
            .text(&setup.deps.fingerprint)
            .text(&tools_library.display().to_string());
        prepared.fingerprint = fp.finish().hex_prefix(32);
        prepared.status.push(match setup.deps.status {
            trace_env::DepsStatus::Installed => format!(
                "{STATUS_DEPENDENCIES}installed ({} package{} in {} librar{})",
                setup.declared.len(),
                if setup.declared.len() == 1 { "" } else { "s" },
                setup.libraries.len() + usize::from(setup.system_library.is_some()),
                if setup.libraries.len() + usize::from(setup.system_library.is_some()) == 1 {
                    "y"
                } else {
                    "ies"
                }
            ),
            _ => format!("{STATUS_DEPENDENCIES}none declared"),
        });
        prepared.status.extend(setup.deps.notes.iter().cloned());

        let root = cx.repo.root.clone();
        let r_files: Vec<PathBuf> = cx
            .files
            .iter()
            .filter(|(_, l)| *l == Language::R)
            .map(|(p, _)| root.join(p))
            .collect();
        let namespace = Some(root.join("NAMESPACE")).filter(|p| p.is_file());
        let mut libraries = setup.libraries.clone();
        libraries.push(tools_library);
        libraries.extend(setup.system_library.clone());
        prepared.data =
            Some(Arc::new(RData::new(r_files, namespace, libraries, setup.system_library.clone())));
        prepared.toolchain = Some(toolchain);
        Ok(prepared)
    }

    fn prepare_workspace(&self, cx: &WorkspaceContext<'_>) -> Result<(), SetupError> {
        let dir = cx.outside.join("r");
        let failed = |e: std::io::Error| SetupError::BuildFailed {
            language: Language::R,
            what: format!("preparing the R session files failed: {e}"),
            log: cx.log.to_path_buf(),
        };
        fs::create_dir_all(&dir).map_err(failed)?;
        for sub in ["cache", "data", "config"] {
            fs::create_dir_all(dir.join(sub)).map_err(failed)?;
        }
        // Empty profile / environment files: the project's .Rprofile / .Renviron never run
        // (the launcher created directories for these `{outside}` values; they are files).
        for name in ["Rprofile", "Renviron"] {
            let path = dir.join(name);
            if path.is_dir() {
                let _ = fs::remove_dir(&path);
            }
            fs::write(&path, b"").map_err(failed)?;
        }
        Ok(())
    }

    fn check_loaded(&self, cx: &LoadedContext<'_>) -> Result<Vec<Diagnostic>, SetupError> {
        for (_, text) in cx.log_messages {
            if let Some(pkg) = missing_package(text) {
                return Err(if pkg == "languageserver" {
                    SetupError::ServerMissing {
                        language: Language::R,
                    }
                } else {
                    SetupError::DepsMissing {
                        language: Language::R,
                        hint: trace_env::r::install_hint(&[pkg]),
                    }
                });
            }
        }
        Ok(Vec::new())
    }

    fn external_location(&self, uri: &str, prepared: &Prepared) -> Option<ExternalLocation> {
        let data = prepared.data.as_deref()?.downcast_ref::<RData>()?;
        let path = crate::lsp::uri_to_path(uri).ok()?;
        r_location(&path, data)
    }

    fn fn_type_route(&self, _language: Language) -> FnTypeRoute {
        FnTypeRoute::TableOnly
    }
}

fn build_spec(cx: &SetupContext<'_>) -> BuildSpec {
    cx.entry.requires_build.clone().unwrap_or(BuildSpec {
        tool: "an R session".into(),
        runs: "the code of this project's packages".into(),
        when: BuildWhen::Always,
    })
}

/// `ServerUnavailable` when the install record pins no binaries for this platform, Linux
/// distribution and R minor (the advice names the pinned R minors).
fn unavailable_for_minor(cx: &SetupContext<'_>, minor: &str) -> Option<SetupError> {
    let Recipe::RPackage { files, .. } = &cx.entry.install.as_ref()?.recipe else {
        return None;
    };
    let distro = cx.platform.linux_distro();
    if !r_binaries_for(files, cx.platform, distro.as_deref(), minor).is_empty() {
        return None;
    }
    let minors = r_minors_for(files, cx.platform, distro.as_deref());
    let advice = match minors.as_slice() {
        [] => None,
        [one] => Some(format!("Install R {one} and run trace again.")),
        [rest @ .., last] => Some(format!("Install R {} or {last} and run trace again.", rest.join(", "))),
    };
    Some(SetupError::ServerUnavailable {
        language: Language::R,
        platform: match &distro {
            Some(d) => format!("{} ({d}) with R {minor}", cx.platform.display()),
            None => format!("{} with R {minor}", cx.platform.display()),
        },
        advice,
    })
}

/// Declared names per R file (functions first; the synthetic module never).
fn declared_names(cx: &SetupContext<'_>) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    for (path, language) in cx.files {
        if *language != Language::R {
            continue;
        }
        let Some(facts) = (cx.facts)(path) else { continue };
        out.push((path.to_string(), names_of(facts)));
    }
    out
}

fn names_of(facts: &FileFacts) -> Vec<String> {
    facts
        .declarations
        .iter()
        .filter(|d| d.kind != SymbolKind::Module && !d.name.starts_with('<'))
        .map(|d| d.name.clone())
        .collect()
}

/// The `workspace/symbol` readiness query: the 3-character prefix of declared names found in
/// the most files (ties: the smallest prefix), so the count keeps growing while files are
/// still being parsed; shorter names are used whole. None when nothing is declared.
pub fn symbol_poll_query(names_by_file: &[(String, Vec<String>)]) -> Option<String> {
    let mut files_per_prefix: BTreeMap<String, usize> = BTreeMap::new();
    for (_, names) in names_by_file {
        let prefixes: BTreeSet<String> = names
            .iter()
            .filter(|n| !n.is_empty())
            .map(|n| n.chars().take(3).collect::<String>())
            .collect();
        for p in prefixes {
            *files_per_prefix.entry(p).or_default() += 1;
        }
    }
    let best = files_per_prefix.values().copied().max()?;
    files_per_prefix
        .into_iter()
        .find(|(_, count)| *count == best)
        .map(|(prefix, _)| prefix)
}

/// A deparsed package function: `<tmp>/Rtmp*/<symbol>.R` starting with [`DEPARSED_HEADER`].
fn deparsed_symbol(path: &Path) -> Option<String> {
    let parent = path.parent()?.file_name()?.to_string_lossy().to_string();
    if !parent.starts_with("Rtmp") || path.extension().is_none_or(|e| e != "R") {
        return None;
    }
    let file = fs::File::open(path).ok()?;
    let mut first = String::new();
    BufReader::new(file).read_line(&mut first).ok()?;
    if first.trim_end() != DEPARSED_HEADER {
        return None;
    }
    Some(path.file_stem()?.to_string_lossy().to_string())
}

/// A server location in an installed package or a deparsed package function.
fn r_location(path: &Path, data: &RData) -> Option<ExternalLocation> {
    for lib in &data.libraries {
        if let Ok(rest) = path.strip_prefix(lib) {
            let package = rest.components().next()?.as_os_str().to_string_lossy().to_string();
            let dir = lib.join(&package);
            return Some(ExternalLocation {
                path: path.display().to_string(),
                line: 0,
                column: 0,
                version: package_version(&dir),
                stdlib: data.system_library.as_deref() == Some(lib.as_path()),
                readable: path.extension().is_some_and(|e| e == "R" || e == "r"),
                package,
                symbol: None,
            });
        }
    }
    let symbol = deparsed_symbol(path)?;
    let package = data.package_of(&symbol)?;
    let dir = package_dir(&data.libraries, &package);
    let stdlib = BASE_PACKAGES.contains(&package.as_str())
        || match (&dir, &data.system_library) {
            (Some(d), Some(s)) => d.starts_with(s),
            _ => false,
        };
    Some(ExternalLocation {
        path: path.display().to_string(),
        line: 0,
        column: 0,
        version: dir.as_deref().and_then(package_version),
        stdlib,
        readable: true,
        symbol: Some(format!("{package}::{symbol}")),
        package,
    })
}

/// `there is no package called 'x'` (R quotes with ‘’ or '') in a server log line.
fn missing_package(text: &str) -> Option<String> {
    let rest = &text[text.find("there is no package called")? + "there is no package called".len()..];
    let rest = rest.trim_start();
    let mut chars = rest.chars();
    let open = chars.next()?;
    let close = match open {
        '\u{2018}' => '\u{2019}',
        '\'' => '\'',
        '"' => '"',
        _ => return None,
    };
    let body: String = chars.take_while(|c| *c != close).collect();
    (!body.is_empty()).then_some(body)
}

#[cfg(test)]
#[path = "../../tests/unit/languages/r.rs"]
mod tests;
