//! Installer and tools directory (DESIGN §1.9; owner install).
//!
//! Language servers and the runtimes they run on (portable Node, Temurin JDK 21, the .NET 10
//! runtime) are installed by trace into its per-user tools folder
//! (`trace_core::config::semantic_tools_dir`) with pinned versions + sha256, layout
//! `<tools>/<id>/<version>[/<variant>]/...`, `MANIFEST.json` schema 2 ([`manifest`]).
//! Toolchains are never installed; servers distributed through an ecosystem (gopls,
//! R languageserver, HLS) are built/installed with the user's toolchain INTO the
//! tools folder ([`ecosystem`]).
//!
//! Every install: one installer at a time (`<tools>/.install.lock`, an OS file lock), licence
//! gates answered before anything is downloaded, verified downloads
//! (`<tools>/downloads/<digest>`), unpacking into `<tools>/.staging-<uuid>/` and an atomic
//! rename into place, MANIFEST updated last. One progress line per tool ([`progress_line`],
//! [`ProgressPrinter`]). Failures are `SetupError`s in the approved style (DESIGN §1.2), never
//! a fallback.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use trace_core::setup_error::{InstallFailure, SetupError};
use trace_core::Language;
use trace_env::os::{EnvVars, Platform};

use crate::registry::{BackendEntry, InstallSpec, LicenceGate, Recipe, Registry};

pub mod archive;
pub mod ecosystem;
pub mod fetch;
pub mod manifest;
pub mod npm;
pub mod platform_select;

use archive::{Budget, ExtractOptions};
use fetch::Expected;
use manifest::Manifest;

/// Licence gates (PLAN decision 11).
pub enum LicenceAnswer<'a> {
    /// `--yes`.
    Accepted,
    /// Terminal question (§1.2).
    Ask(&'a mut dyn FnMut(&InstallSpec, &LicenceGate) -> bool),
    /// Non-interactive without `--yes` -> `InstallFailure::LicenceNotAccepted`.
    Refuse,
}

/// One progress event (one line per tool on stderr).
#[derive(Clone, Debug)]
pub struct InstallProgress {
    pub what: String,
    pub product: String,
    pub version: String,
    /// "start" | "download" | "extract" | "build" | "done" | "wait" (another install holds
    /// the lock)
    pub step: &'static str,
    pub done: u64,
    pub total: Option<u64>,
}

pub struct InstallRequest<'a> {
    pub tools_dir: &'a Path,
    pub registry: &'a Registry,
    pub languages: &'a [Language],
    pub repo_root: Option<&'a Path>,
    pub platform: &'a Platform,
    pub vars: &'a EnvVars,
    pub progress: &'a mut dyn FnMut(&InstallProgress),
    pub licences: LicenceAnswer<'a>,
}

#[derive(Clone, Debug, Serialize)]
pub struct InstalledTool {
    pub id: String,
    pub version: String,
    pub dir: PathBuf,
    pub already: bool,
}

/// Longest time an ecosystem install command may run (setting `semantic.install_timeout_secs`).
pub(crate) fn command_timeout() -> Duration {
    Duration::from_secs(trace_core::config::current().semantic.install_timeout_secs)
}

// ---------------------------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------------------------

/// Installs the servers of `languages` plus their runtimes and install extras (idempotent).
/// Errors: `SetupError::Install` / `ServerUnavailable` / `Unsupported`.
pub fn install(req: InstallRequest<'_>) -> Result<Vec<InstalledTool>, SetupError> {
    let InstallRequest {
        tools_dir,
        registry,
        languages,
        repo_root,
        platform,
        vars,
        progress,
        licences,
    } = req;
    run(
        Inputs {
            tools_dir,
            registry,
            repo_root,
            platform,
            vars,
        },
        languages,
        progress,
        licences,
    )
}

/// PLAN decision 10: which of `languages` are default languages whose server, server runtime
/// or install extras are not installed yet (cheap: MANIFEST + tool dirs; no network).
/// Repository-dependent extras are not considered (see [`missing_defaults_at`]).
pub fn missing_defaults(
    tools_dir: &Path,
    registry: &Registry,
    languages: &[Language],
    platform: &Platform,
) -> Vec<Language> {
    missing_defaults_at(tools_dir, registry, languages, platform, None)
}

/// [`missing_defaults`] including the install extras the servers need for the repository at
/// `repo_root` (Rust std crates, gopls by Go minor, ...).
pub fn missing_defaults_at(
    tools_dir: &Path,
    registry: &Registry,
    languages: &[Language],
    platform: &Platform,
    repo_root: Option<&Path>,
) -> Vec<Language> {
    let manifest = Manifest::load(tools_dir);
    let mut out: Vec<Language> = Vec::new();
    for &language in languages {
        if !language.is_default() || out.contains(&language) {
            continue;
        }
        let Some(entry) = registry.entry_for(language) else {
            continue;
        };
        // Never a licence-gated tool (they install only with the user's answer).
        if entry.install.as_ref().is_some_and(|s| s.licence_gate.is_some()) {
            continue;
        }
        let runtime_missing = entry.runtime.iter().any(|id| {
            registry
                .runtime(id)
                .is_some_and(|spec| spec_missing(tools_dir, &manifest, spec, platform))
        });
        let server_missing = entry
            .install
            .as_ref()
            .is_some_and(|spec| spec_missing(tools_dir, &manifest, spec, platform));
        let extras_missing = !runtime_missing
            && !server_missing
            && crate::languages::server_for(&entry.id)
                .install_extras(repo_root)
                .iter()
                .any(|x| !manifest.has_extra(&entry.id, &x.id));
        if runtime_missing || server_missing || extras_missing {
            out.push(language);
        }
    }
    out
}

/// PLAN decision 10: install exactly `missing_defaults_at(..)` for `req.languages` (never a
/// non-default language, never a licence-gated tool; licences are always refused here).
/// Called by the pipeline before preflight; not called when automatic installs are off.
pub fn auto_install(req: InstallRequest<'_>) -> Result<Vec<InstalledTool>, SetupError> {
    let InstallRequest {
        tools_dir,
        registry,
        languages,
        repo_root,
        platform,
        vars,
        progress,
        licences: _,
    } = req;
    let missing = missing_defaults_at(tools_dir, registry, languages, platform, repo_root);
    if missing.is_empty() {
        return Ok(Vec::new());
    }
    run(
        Inputs {
            tools_dir,
            registry,
            repo_root,
            platform,
            vars,
        },
        &missing,
        progress,
        LicenceAnswer::Refuse,
    )
}

/// Whether an install record still has to be installed here (false when nothing can be
/// installed on this platform: the language's preflight reports that).
fn spec_missing(tools_dir: &Path, manifest: &Manifest, spec: &InstallSpec, platform: &Platform) -> bool {
    let installed = manifest.installed_version(tools_dir, &spec.id);
    match &spec.recipe {
        Recipe::Archive { artifacts, .. } => {
            platform_select::artifact_for(artifacts, platform).is_some()
                && installed != Some(spec.version.as_str())
        }
        Recipe::Npm { .. } | Recipe::Coursier { .. } | Recipe::RPackage { .. } => {
            installed != Some(spec.version.as_str())
        }
        Recipe::GoInstall { versions, .. } => {
            !installed.is_some_and(|v| versions.iter().any(|x| x.version == v))
        }
        Recipe::Ghcup { .. } | Recipe::FromToolchain { .. } => false,
    }
}

// ---------------------------------------------------------------------------------------------
// Progress lines (DESIGN §1.2)
// ---------------------------------------------------------------------------------------------

/// `Installing the {what} ({product} {version})...` (or the lock-wait line).
pub fn progress_line(p: &InstallProgress) -> String {
    if p.step == "wait" {
        return "Waiting for another trace install to finish...".to_string();
    }
    if p.version.is_empty() {
        format!("Installing the {} ({})...", p.what, p.product)
    } else {
        format!("Installing the {} ({} {})...", p.what, p.product, p.version)
    }
}

/// Prints install progress: on a terminal one line per tool rewritten in place with the
/// download percentage and ending with ` done`; otherwise exactly one line per tool.
pub struct ProgressPrinter {
    terminal: bool,
    open: Option<String>,
    last_pct: Option<u64>,
}

impl ProgressPrinter {
    pub fn new(terminal: bool) -> ProgressPrinter {
        ProgressPrinter {
            terminal,
            open: None,
            last_pct: None,
        }
    }

    /// For stderr (terminal detection included).
    pub fn stderr() -> ProgressPrinter {
        ProgressPrinter::new(io::stderr().is_terminal())
    }

    /// Print `p` on stderr.
    pub fn event(&mut self, p: &InstallProgress) {
        let mut err = io::stderr().lock();
        let _ = self.write(&mut err, p);
    }

    /// Print `p` on `out`.
    pub fn write(&mut self, out: &mut dyn Write, p: &InstallProgress) -> io::Result<()> {
        let line = progress_line(p);
        match p.step {
            "wait" => {
                self.close(out)?;
                writeln!(out, "{line}")?;
            }
            "start" => {
                self.close(out)?;
                if self.terminal {
                    write!(out, "\r{line}")?;
                    self.open = Some(line);
                    self.last_pct = None;
                } else {
                    writeln!(out, "{line}")?;
                }
            }
            "done" => {
                if let Some(open) = self.open.take() {
                    writeln!(out, "\r{open} done    ")?;
                }
            }
            _ => {
                if let (Some(open), Some(total)) = (&self.open, p.total.filter(|t| *t > 0)) {
                    let pct = (p.done.min(total) * 100) / total;
                    if self.last_pct != Some(pct) {
                        write!(out, "\r{open} {pct}%")?;
                        self.last_pct = Some(pct);
                    }
                }
            }
        }
        out.flush()
    }

    fn close(&mut self, out: &mut dyn Write) -> io::Result<()> {
        if self.open.take().is_some() {
            writeln!(out)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------------------------

/// Why one install step failed (mapped to the §1.2 texts by `Installer::fail`).
#[derive(Debug)]
pub(crate) enum StepError {
    /// Network / HTTP problem (details for the log).
    Download(String),
    /// A download did not match its pinned digest.
    Checksum(String),
    /// The disk is full.
    DiskFull,
    /// Anything else, with details for the log.
    Failed(String),
    /// A command failed; its output is in this log.
    FailedLog(PathBuf),
    /// No build for this computer (second line of `ServerUnavailable`).
    Unavailable { advice: Option<String> },
    /// A complete error already (toolchain missing, unsupported GHC, ...).
    Setup(SetupError),
}

impl From<io::Error> for StepError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::StorageFull {
            StepError::DiskFull
        } else {
            StepError::Failed(e.to_string())
        }
    }
}

impl From<fetch::FetchError> for StepError {
    fn from(e: fetch::FetchError) -> Self {
        match e {
            fetch::FetchError::Download(m) => StepError::Download(m),
            fetch::FetchError::Checksum { .. } => StepError::Checksum(e.to_string()),
            fetch::FetchError::NoDigest(_) => StepError::Failed(e.to_string()),
            fetch::FetchError::Io(io) => io.into(),
        }
    }
}

impl From<archive::ArchiveError> for StepError {
    fn from(e: archive::ArchiveError) -> Self {
        match e {
            archive::ArchiveError::Io(io) => io.into(),
            other => StepError::Failed(other.to_string()),
        }
    }
}

/// Read-only inputs of one installer run.
#[derive(Clone, Copy)]
struct Inputs<'a> {
    tools_dir: &'a Path,
    registry: &'a Registry,
    repo_root: Option<&'a Path>,
    platform: &'a Platform,
    vars: &'a EnvVars,
}

/// One install record to install for a language's entry.
#[derive(Clone, Copy)]
pub(crate) struct Job<'j> {
    pub spec: &'j InstallSpec,
    pub language: Language,
    /// The backend the tool serves.
    pub entry: Option<&'j BackendEntry>,
}

/// The installer state while the lock is held.
pub(crate) struct Installer<'a> {
    pub tools_dir: PathBuf,
    pub repo_root: Option<&'a Path>,
    pub platform: &'a Platform,
    pub vars: &'a EnvVars,
    pub progress: &'a mut dyn FnMut(&InstallProgress),
    pub manifest: Manifest,
    /// Estimated space the current job needs (for `DiskSpace`).
    pub need_mb: u64,
}

fn setup_io(language: Option<Language>, what: &str, dir: &Path) -> SetupError {
    SetupError::Install {
        language,
        failure: InstallFailure::Failed {
            what: what.to_string(),
            log: dir.to_path_buf(),
        },
    }
}

/// The entries to install for `languages` (one per backend, registry order kept per call).
fn plan<'r>(
    registry: &'r Registry,
    languages: &[Language],
    platform: &Platform,
) -> Result<Vec<(Language, &'r BackendEntry)>, SetupError> {
    let mut out: Vec<(Language, &BackendEntry)> = Vec::new();
    for &language in languages {
        let Some(entry) = registry.entry_for(language) else {
            return Err(SetupError::ServerUnavailable {
                language,
                platform: platform.display(),
                advice: None,
            });
        };
        if !out.iter().any(|(_, e)| e.id == entry.id) {
            out.push((language, entry));
        }
    }
    Ok(out)
}

fn run(
    inputs: Inputs<'_>,
    languages: &[Language],
    progress: &mut dyn FnMut(&InstallProgress),
    mut licences: LicenceAnswer<'_>,
) -> Result<Vec<InstalledTool>, SetupError> {
    let plan = plan(inputs.registry, languages, inputs.platform)?;
    if plan.is_empty() {
        return Ok(Vec::new());
    }
    let first = plan.first().map(|(l, _)| *l);
    let tools_dir = std::path::absolute(inputs.tools_dir).unwrap_or_else(|_| inputs.tools_dir.to_path_buf());
    fs::create_dir_all(&tools_dir).map_err(|_| setup_io(first, "language servers", &tools_dir))?;
    let _lock = InstallLock::acquire(&tools_dir, progress)
        .map_err(|_| setup_io(first, "language servers", &tools_dir))?;
    clean_staging(&tools_dir);
    let mut installer = Installer {
        manifest: Manifest::load(&tools_dir),
        tools_dir,
        repo_root: inputs.repo_root,
        platform: inputs.platform,
        vars: inputs.vars,
        progress,
        need_mb: 0,
    };
    installer.licence_gates(&plan, &mut licences)?;
    let mut out = Vec::new();
    let mut done: BTreeSet<String> = BTreeSet::new();
    for (language, entry) in &plan {
        let mut specs: Vec<&InstallSpec> = Vec::new();
        for id in &entry.runtime {
            let spec = inputs.registry.runtime(id).ok_or_else(|| {
                setup_io(
                    Some(*language),
                    &format!("{} runtime \"{id}\"", entry.server.name),
                    &installer.tools_dir,
                )
            })?;
            specs.push(spec);
        }
        specs.extend(entry.install.as_ref());
        for spec in specs {
            if !done.insert(spec.id.clone()) {
                continue;
            }
            let job = Job {
                spec,
                language: *language,
                entry: Some(entry),
            };
            out.push(installer.install_job(&job)?);
        }
        installer.install_extras(*language, entry)?;
    }
    Ok(out)
}

/// `<tools>/.install.lock` held for the whole run (released when dropped).
struct InstallLock {
    _file: fs::File,
}

impl InstallLock {
    fn acquire(tools_dir: &Path, progress: &mut dyn FnMut(&InstallProgress)) -> io::Result<InstallLock> {
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(tools_dir.join(".install.lock"))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                progress(&InstallProgress {
                    what: String::new(),
                    product: String::new(),
                    version: String::new(),
                    step: "wait",
                    done: 0,
                    total: None,
                });
                file.lock()?;
            }
            Err(fs::TryLockError::Error(e)) => return Err(e),
        }
        Ok(InstallLock { _file: file })
    }
}

/// Leftovers of an interrupted install (`.staging-*`, `.work-*`); called with the lock held.
fn clean_staging(tools_dir: &Path) {
    let Ok(read) = fs::read_dir(tools_dir) else {
        return;
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(".staging-") || name.starts_with(".work-") {
            let _ = archive::remove_tree(&entry.path());
        }
    }
    if let Ok(read) = fs::read_dir(tools_dir.join("downloads")) {
        for entry in read.flatten() {
            if entry.file_name().to_string_lossy().contains(".part-") {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// Rename with retries (virus scanners briefly hold freshly written files on Windows).
fn rename_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut last = None;
    for attempt in 0..20 {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                std::thread::sleep(Duration::from_millis(100 * (attempt + 1)));
            }
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("rename failed")))
}

impl Installer<'_> {
    fn emit(&mut self, job: &Job<'_>, version: &str, step: &'static str, done: u64, total: Option<u64>) {
        (self.progress)(&InstallProgress {
            what: job.spec.display.clone(),
            product: job.spec.product.clone(),
            version: version.to_string(),
            step,
            done,
            total,
        });
    }

    /// PLAN decision 11: every gated tool of the plan is answered before any download.
    fn licence_gates(
        &mut self,
        plan: &[(Language, &BackendEntry)],
        licences: &mut LicenceAnswer<'_>,
    ) -> Result<(), SetupError> {
        for (language, entry) in plan {
            let Some(spec) = &entry.install else { continue };
            let Some(gate) = &spec.licence_gate else { continue };
            if self.manifest.licence_accepted(&spec.id, &spec.version)
                || self.manifest.has_version(&self.tools_dir, &spec.id, &spec.version)
            {
                continue;
            }
            let accepted = match licences {
                LicenceAnswer::Accepted => true,
                LicenceAnswer::Ask(ask) => (**ask)(spec, gate),
                LicenceAnswer::Refuse => false,
            };
            if !accepted {
                return Err(SetupError::Install {
                    language: Some(*language),
                    failure: InstallFailure::LicenceNotAccepted {
                        what: spec.display.clone(),
                        url: gate.url.clone(),
                    },
                });
            }
            self.manifest.accept_licence(&spec.id, &spec.version);
            self.manifest
                .save(&self.tools_dir)
                .map_err(|_| setup_io(Some(*language), &spec.display, &self.tools_dir))?;
        }
        Ok(())
    }

    fn install_job(&mut self, job: &Job<'_>) -> Result<InstalledTool, SetupError> {
        self.need_mb = 0;
        let result = match &job.spec.recipe {
            Recipe::Archive {
                artifacts,
                executables,
            } => self.archive_recipe(job, artifacts, executables),
            Recipe::Npm { packages } => self.npm_recipe(job, packages),
            Recipe::GoInstall { module, versions } => self.go_install(job, module, versions),
            Recipe::RPackage { package, files, .. } => self.r_package(job, package, files),
            Recipe::Ghcup {
                hls_version,
                supported_ghc,
            } => self.ghcup(job, hls_version, supported_ghc),
            Recipe::Coursier {
                launcher,
                fetch,
                lock,
                ..
            } => self.coursier(job, launcher, fetch, lock),
            Recipe::FromToolchain { ecosystem } => self.toolchain_recipe(job, ecosystem),
        };
        result.map_err(|e| self.fail(job, e))
    }

    /// The §1.2 error of a failed step (details written to `<tools>/logs/<id>-<version>.log`).
    fn fail(&mut self, job: &Job<'_>, err: StepError) -> SetupError {
        let what = job.spec.display.clone();
        let language = Some(job.language);
        let failure = match err {
            StepError::Setup(e) => return e,
            StepError::Unavailable { advice } => {
                return SetupError::ServerUnavailable {
                    language: job.language,
                    platform: self.platform.display(),
                    advice,
                }
            }
            StepError::Download(detail) => {
                self.write_log(job, &detail);
                InstallFailure::Download { what }
            }
            StepError::Checksum(detail) => {
                self.write_log(job, &detail);
                InstallFailure::Checksum { what }
            }
            StepError::DiskFull => InstallFailure::DiskSpace {
                what,
                needs_mb: if self.need_mb == 0 { 500 } else { self.need_mb },
                dir: self.tools_dir.clone(),
            },
            StepError::Failed(detail) => {
                let log = self.write_log(job, &detail);
                InstallFailure::Failed { what, log }
            }
            StepError::FailedLog(log) => InstallFailure::Failed { what, log },
        };
        SetupError::Install { language, failure }
    }

    /// `<tools>/logs/<id>-<version>.log` of a job.
    pub(crate) fn log_path(&self, job: &Job<'_>) -> PathBuf {
        let name = format!("{}-{}.log", job.spec.id, job.spec.version).replace([':', '/', '\\', ' '], "-");
        self.tools_dir.join("logs").join(name)
    }

    fn write_log(&self, job: &Job<'_>, detail: &str) -> PathBuf {
        let path = self.log_path(job);
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(f, "{detail}");
        }
        path
    }

    /// A fresh staging directory `<tools>/.staging-<uuid>`.
    pub(crate) fn staging(&self) -> Result<PathBuf, StepError> {
        let dir = self
            .tools_dir
            .join(format!(".staging-{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// A fresh scratch directory `<tools>/.work-<uuid>` (removed by the caller).
    pub(crate) fn work_dir(&self) -> Result<PathBuf, StepError> {
        let dir = self
            .tools_dir
            .join(format!(".work-{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// `<tools>/<id>/<version>`.
    pub(crate) fn version_dir(&self, id: &str, version: &str) -> Result<PathBuf, StepError> {
        crate::registry::safe_relative(&format!("{id}/{version}"))
            .map(|rel| self.tools_dir.join(rel))
            .ok_or_else(|| StepError::Failed(format!("unsafe tool path {id}/{version}")))
    }

    /// Download a verified file, reporting progress.
    pub(crate) fn download(
        &mut self,
        job: &Job<'_>,
        version: &str,
        url: &str,
        expected: Expected,
    ) -> Result<PathBuf, StepError> {
        let what = job.spec.display.clone();
        let product = job.spec.product.clone();
        let version = version.to_string();
        let progress = &mut *self.progress;
        let mut on = |done: u64, total: Option<u64>| {
            progress(&InstallProgress {
                what: what.clone(),
                product: product.clone(),
                version: version.clone(),
                step: "download",
                done,
                total,
            });
        };
        let path = fetch::download_verified(&self.tools_dir, url, &expected, &mut on)?;
        let size_mb = fs::metadata(&path).map(|m| m.len() >> 20).unwrap_or(0);
        self.need_mb = self.need_mb.max(size_mb.saturating_mul(3).max(1));
        Ok(path)
    }

    /// Move a finished staging directory into place and record it in MANIFEST.json.
    /// `variant` None: `stage` becomes `<tools>/<id>/<version>` (replacing leftovers);
    /// Some(v): `stage` becomes `<tools>/<id>/<version>/<v>` next to other variants.
    #[allow(clippy::too_many_arguments)] // one call per recipe; a struct would only rename them
    pub(crate) fn commit(
        &mut self,
        job: &Job<'_>,
        version: &str,
        stage: &Path,
        variant: Option<&str>,
        method: &str,
        sources: Vec<String>,
        files: BTreeMap<String, String>,
    ) -> Result<PathBuf, StepError> {
        let id = job.spec.id.clone();
        let dir = self.version_dir(&id, version)?;
        let previous = self.manifest.tools.get(&id).cloned();
        let same_version = previous.as_ref().is_some_and(|t| t.version == version) && dir.is_dir();
        match variant {
            None => {
                if dir.exists() {
                    archive::remove_tree(&dir)?;
                }
                if let Some(parent) = dir.parent() {
                    fs::create_dir_all(parent)?;
                }
                rename_retry(stage, &dir)?;
            }
            Some(v) => {
                if !same_version && dir.exists() {
                    archive::remove_tree(&dir)?;
                }
                fs::create_dir_all(&dir)?;
                let target = dir.join(v);
                if target.exists() {
                    archive::remove_tree(&target)?;
                }
                rename_retry(stage, &target)?;
            }
        }
        let mut tool = match (&previous, variant) {
            (Some(t), Some(_)) if same_version => t.clone(),
            _ => manifest::ManifestTool::default(),
        };
        tool.version = version.to_string();
        tool.platform = self.platform.key();
        tool.method = method.to_string();
        tool.license = job.spec.license.clone();
        tool.installed_at = manifest::now_stamp();
        tool.licence_accepted = self.manifest.licence_accepted(&id, version);
        tool.files.extend(files);
        for s in sources {
            if !tool.sources.contains(&s) {
                tool.sources.push(s);
            }
        }
        if let Some(v) = variant {
            tool.variants.insert(v.to_string());
        }
        self.manifest.tools.insert(id.clone(), tool);
        self.manifest.save(&self.tools_dir)?;
        // The previous version is no longer referenced (best effort: a running server may
        // still hold it open on Windows).
        if let Some(prev) = previous.filter(|p| p.version != version) {
            if let Ok(old) = self.version_dir(&id, &prev.version) {
                let _ = archive::remove_tree(&old);
            }
        }
        Ok(dir)
    }

    /// Relative executable -> sha256 for the manifest.
    pub(crate) fn file_hashes(dir: &Path, found: &[(String, PathBuf)]) -> BTreeMap<String, String> {
        found
            .iter()
            .filter_map(|(_, path)| {
                let rel = path.strip_prefix(dir).ok()?.to_string_lossy().replace('\\', "/");
                Some((rel, fetch::sha256_file(path).ok()?))
            })
            .collect()
    }

    fn already(&self, job: &Job<'_>, version: &str) -> InstalledTool {
        InstalledTool {
            id: job.spec.id.clone(),
            version: version.to_string(),
            dir: self
                .version_dir(&job.spec.id, version)
                .unwrap_or_else(|_| self.tools_dir.clone()),
            already: true,
        }
    }

    fn installed(&mut self, job: &Job<'_>, version: &str, dir: PathBuf) -> InstalledTool {
        self.emit(job, version, "done", 0, None);
        InstalledTool {
            id: job.spec.id.clone(),
            version: version.to_string(),
            dir,
            already: false,
        }
    }

    fn archive_recipe(
        &mut self,
        job: &Job<'_>,
        artifacts: &[crate::registry::Artifact],
        executables: &[String],
    ) -> Result<InstalledTool, StepError> {
        let version = job.spec.version.clone();
        if self.manifest.has_version(&self.tools_dir, &job.spec.id, &version) {
            return Ok(self.already(job, &version));
        }
        let artifact = platform_select::artifact_for(artifacts, self.platform).ok_or_else(|| {
            StepError::Unavailable {
                advice: platform_select::unavailable_advice(artifacts, self.platform),
            }
        })?;
        if let Some(advice) = platform_select::glibc_too_old(artifact, self.platform.glibc().as_ref()) {
            return Err(StepError::Unavailable { advice: Some(advice) });
        }
        self.emit(job, &version, "start", 0, None);
        let file = self.download(job, &version, &artifact.url, Expected::Sha256(artifact.sha256.clone()))?;
        let stage = self.staging()?;
        let result = (|| {
            self.emit(job, &version, "extract", 0, None);
            let single = executables
                .first()
                .map(|e| archive::single_file_name(e, self.platform));
            let opts = ExtractOptions {
                strip: artifact.strip,
                strip_prefix: artifact.strip_prefix.as_deref(),
                subdir: artifact.subdir.as_deref(),
                single_file: single.as_deref(),
            };
            archive::extract(&file, &stage, &opts, &mut Budget::default())?;
            let found = archive::mark_executables(&stage, executables, self.platform)?;
            if !executables.is_empty() && found.is_empty() {
                return Err(StepError::Failed(format!(
                    "{}: none of the declared executables ({}) exist after unpacking",
                    artifact.url,
                    executables.join(", ")
                )));
            }
            let files = Installer::file_hashes(&stage, &found);
            self.commit(job, &version, &stage, None, "archive", vec![artifact.url.clone()], files)
        })();
        if result.is_err() {
            let _ = archive::remove_tree(&stage);
        }
        let dir = result?;
        let _ = fs::remove_file(&file);
        Ok(self.installed(job, &version, dir))
    }

    fn npm_recipe(
        &mut self,
        job: &Job<'_>,
        packages: &[crate::registry::NpmPackage],
    ) -> Result<InstalledTool, StepError> {
        let version = job.spec.version.clone();
        if self.manifest.has_version(&self.tools_dir, &job.spec.id, &version) {
            return Ok(self.already(job, &version));
        }
        self.emit(job, &version, "start", 0, None);
        let stage = self.staging()?;
        let what = job.spec.display.clone();
        let product = job.spec.product.clone();
        let result = (|| {
            let progress = &mut *self.progress;
            let mut on = |done: u64, total: u64| {
                progress(&InstallProgress {
                    what: what.clone(),
                    product: product.clone(),
                    version: version.clone(),
                    step: "download",
                    done,
                    total: Some(total),
                });
            };
            let files = npm::install(
                &self.tools_dir,
                packages,
                &stage,
                self.platform,
                &mut Budget::default(),
                &mut on,
            )?;
            let sources = npm::selected(packages, self.platform)
                .iter()
                .map(|p| p.url.clone())
                .collect();
            let dir = self.commit(job, &version, &stage, None, "npm", sources, BTreeMap::new())?;
            Ok::<_, StepError>((dir, files))
        })();
        match result {
            Ok((dir, files)) => {
                for f in files {
                    let _ = fs::remove_file(f);
                }
                Ok(self.installed(job, &version, dir))
            }
            Err(e) => {
                let _ = archive::remove_tree(&stage);
                Err(e)
            }
        }
    }

    /// `Server::install_extras` for the repository, each run once (recorded per backend
    /// in MANIFEST.json).
    fn install_extras(&mut self, language: Language, entry: &BackendEntry) -> Result<(), SetupError> {
        let hooks = crate::languages::server_for(&entry.id);
        let extras = hooks.install_extras(self.repo_root);
        let display = entry
            .install
            .as_ref()
            .map(|s| s.display.clone())
            .unwrap_or_else(|| format!("{} language server", language.display_name()));
        for extra in extras {
            if self.manifest.has_extra(&entry.id, &extra.id) {
                continue;
            }
            (self.progress)(&InstallProgress {
                what: display.clone(),
                product: extra.id.clone(),
                version: String::new(),
                step: "start",
                done: 0,
                total: None,
            });
            let log = self
                .tools_dir
                .join("logs")
                .join(format!("{}-{}.log", entry.id, extra.id).replace([':', '/', '\\', ' '], "-"));
            if let Some(parent) = log.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let cx = crate::languages::InstallExtraContext {
                tools_dir: &self.tools_dir,
                platform: self.platform,
                vars: self.vars,
                repo_root: self.repo_root,
                log: &log,
            };
            hooks.run_install_extra(&extra, &cx)?;
            // A hook may have recorded tools itself: keep its records.
            self.manifest = Manifest::load(&self.tools_dir);
            self.manifest.record_extra(&entry.id, &extra.id);
            self.manifest
                .save(&self.tools_dir)
                .map_err(|_| setup_io(Some(language), &display, &self.tools_dir))?;
            (self.progress)(&InstallProgress {
                what: display.clone(),
                product: extra.id.clone(),
                version: String::new(),
                step: "done",
                done: 0,
                total: None,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../tests/unit/install/mod.rs"]
mod tests;
