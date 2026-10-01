//! Ecosystem installs with the user's toolchain INTO the tools folder (owner install, PLAN
//! decision 4): `go install` (gopls), pinned R binary packages (R languageserver), GHCup
//! (HLS for the project's GHC), Coursier (Metals jars + launcher) and toolchain-provided
//! servers (verify only). Never into the
//! project, never globally, never at analysis time. Every command runs with a small
//! allow-listed environment (no secrets), in a scratch directory under the tools folder, with a
//! timeout; its output goes to `<tools>/logs/<id>-<version>.log`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use trace_core::setup_error::{InstallFailure, SetupError};
use trace_env::os::{self, EnvVars, Platform, Version, VersionReq};
use trace_env::{DetectContext, EcosystemId, Toolchain, ToolchainStatus};

use super::archive::{self, Budget, ExtractOptions};
use super::fetch::Expected;
use super::platform_select;
use super::{command_timeout, InstalledTool, Installer, Job, StepError};
use crate::registry::{Artifact, GoInstallVersion, PinnedFile, RBinary};

/// Variables passed to install commands (case-insensitive on Windows). Never a secret.
const PASS_VARS: &[&str] = &[
    "PATH",
    "SYSTEMROOT",
    // Windows known-folder lookups return nothing without it.
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "PROCESSOR_ARCHITECTURE",
    "NUMBER_OF_PROCESSORS",
    "HOME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "LOCALAPPDATA",
    "APPDATA",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

/// The allow-listed environment plus TEMP/TMP/TMPDIR = `tmp`.
pub(crate) fn base_env(vars: &EnvVars, tmp: &Path) -> BTreeMap<String, OsString> {
    let mut env = BTreeMap::new();
    for key in PASS_VARS {
        if let Some(v) = vars.get(key) {
            env.insert((*key).to_string(), v.to_os_string());
        }
    }
    for key in ["TEMP", "TMP", "TMPDIR"] {
        env.insert(key.to_string(), tmp.as_os_str().to_os_string());
    }
    env
}

/// `dirs` in front of the environment's PATH.
fn prepend_path(env: &mut BTreeMap<String, OsString>, dirs: &[PathBuf]) {
    let existing = env.get("PATH").cloned().unwrap_or_default();
    let mut all: Vec<PathBuf> = dirs.to_vec();
    all.extend(std::env::split_paths(&existing));
    if let Ok(joined) = std::env::join_paths(all) {
        env.insert("PATH".to_string(), joined);
    }
}

/// Run `program args` with exactly `env`, in `cwd`, appending the command line and its output
/// to `log`. Returns (success, output); a timeout kills the command.
pub(crate) fn run_logged(
    program: &Path,
    args: &[OsString],
    env: &BTreeMap<String, OsString>,
    cwd: &Path,
    log: &Path,
    timeout: Duration,
) -> std::io::Result<(bool, String)> {
    if let Some(parent) = log.parent() {
        fs::create_dir_all(parent)?;
    }
    // No trace stdio for the command, its tree stoppable as a whole (crate::procs).
    let mut cmd = crate::procs::command(program);
    cmd.args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let out_reader = bounded_reader(child.stdout.take());
    let err_reader = bounded_reader(child.stderr.take());
    // A timeout stops the whole tree; after a normal exit, what it left in its tree too.
    let status = crate::procs::wait_bounded(&mut child, timeout)?;
    let mut output = String::from_utf8_lossy(&join_output(out_reader)).into_owned();
    output.push_str(&String::from_utf8_lossy(&join_output(err_reader)));
    let mut f = fs::OpenOptions::new().create(true).append(true).open(log)?;
    let line: Vec<String> = std::iter::once(program.display().to_string())
        .chain(args.iter().map(|a| a.to_string_lossy().into_owned()))
        .collect();
    writeln!(f, "$ {}", line.join(" "))?;
    writeln!(f, "{output}")?;
    let ok = match status {
        Some(s) => {
            writeln!(f, "(exit: {s})")?;
            s.success()
        }
        None => {
            writeln!(f, "(timed out after {} s)", timeout.as_secs())?;
            false
        }
    };
    Ok((ok, output))
}

/// Output kept per stream of an install command (the rest is read and dropped).
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
/// How long the output readers may still run after the command ended (a process that left
/// the command's tree may hold its pipes open).
const OUTPUT_GRACE: Duration = Duration::from_secs(5);

/// Read one output stream to its end in a thread, keeping at most [`MAX_OUTPUT_BYTES`].
fn bounded_reader<R: Read + Send + 'static>(stream: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let Some(mut stream) = stream else {
            return kept;
        };
        let mut chunk = [0u8; 64 * 1024];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = MAX_OUTPUT_BYTES.saturating_sub(kept.len());
                    kept.extend_from_slice(&chunk[..n.min(room)]);
                }
            }
        }
        kept
    })
}

/// The output a reader kept; empty when the stream is still held open after the grace (the
/// reader thread then ends with its holder).
fn join_output(reader: std::thread::JoinHandle<Vec<u8>>) -> Vec<u8> {
    let until = Instant::now() + OUTPUT_GRACE;
    while !reader.is_finished() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    if reader.is_finished() {
        reader.join().unwrap_or_default()
    } else {
        Vec::new()
    }
}

fn os_args(items: &[&dyn AsRef<std::ffi::OsStr>]) -> Vec<OsString> {
    items.iter().map(|a| a.as_ref().to_os_string()).collect()
}

/// A program of a detected toolchain: its recorded executable, else `<root>/bin[/x64]` or
/// `<root>`.
pub(crate) fn toolchain_exe(tc: &Toolchain, names: &[&str], platform: &Platform) -> Option<PathBuf> {
    for name in names {
        if let Some(p) = tc.executables.get(*name).filter(|p| p.is_file()) {
            return Some(p.clone());
        }
    }
    let dirs = [tc.root.join("bin"), tc.root.join("bin").join("x64"), tc.root.clone()];
    os::find_executable(names, &dirs, platform)
}

/// Default "needs / install" texts when the entry declares no toolchain spec.
fn default_toolchain_text(eco: EcosystemId) -> (&'static str, &'static str) {
    match eco {
        EcosystemId::Go => ("Go", "Install it from https://go.dev/dl"),
        EcosystemId::R => ("R", "Install it from https://cloud.r-project.org"),
        EcosystemId::Haskell => ("GHC", "Install it with GHCup from https://www.haskell.org/ghcup"),
        _ => ("its toolchain", "Install it"),
    }
}

/// Last path segment of a Go module / package path (`golang.org/x/tools/gopls` -> gopls).
fn go_binary_name(module: &str) -> &str {
    let last = module.rsplit('/').next().unwrap_or(module);
    // `.../v2` major-version suffixes name the directory above.
    if last.len() > 1 && last.starts_with('v') && last[1..].bytes().all(|b| b.is_ascii_digit()) {
        module.rsplit('/').nth(1).unwrap_or(last)
    } else {
        last
    }
}

/// Go module cache path escaping (upper-case letters -> `!` + lower case).
pub(crate) fn escape_module_path(module: &str) -> String {
    let mut out = String::with_capacity(module.len());
    for c in module.chars() {
        if c.is_ascii_uppercase() {
            out.push('!');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The pinned gopls for a Go version: the entry with the highest `min_go` the Go satisfies
/// (unknown Go version: the most compatible entry).
pub(crate) fn choose_go_version<'a>(
    versions: &'a [GoInstallVersion],
    go: Option<&Version>,
) -> Option<&'a GoInstallVersion> {
    let mut sorted: Vec<(&GoInstallVersion, Version)> = versions
        .iter()
        .filter_map(|v| Version::parse(&v.min_go).map(|m| (v, m)))
        .collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    match go {
        Some(go) => sorted
            .iter()
            .find(|(_, min)| VersionReq::at_least(min.clone()).matches(go))
            .map(|(v, _)| *v),
        None => sorted.last().map(|(v, _)| *v),
    }
}

fn first_line_version(path: &Path) -> Option<Version> {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| t.lines().next().and_then(Version::parse))
}

impl Installer<'_> {
    /// The user's toolchain for `eco` (a toolchain too old for the project still installs
    /// servers), else the §1.2 `NeedsToolchain` text of the entry (or the default text).
    fn toolchain(&self, job: &Job<'_>, eco: EcosystemId) -> Result<Toolchain, StepError> {
        let root = self
            .repo_root
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.tools_dir.clone());
        let cx = DetectContext {
            root: &root,
            platform: self.platform,
            vars: self.vars,
            env_override: None,
            forbidden: &[],
            files: &[],
        };
        match eco.toolchain(&cx) {
            ToolchainStatus::Found(t) => Ok(t),
            ToolchainStatus::TooOld { found, .. } => Ok(found),
            ToolchainStatus::Missing { .. } | ToolchainStatus::NotNeeded => {
                Err(self.needs_toolchain(job, eco, None))
            }
        }
    }

    fn needs_toolchain(&self, job: &Job<'_>, eco: EcosystemId, needs: Option<String>) -> StepError {
        let (default_needs, default_install) = default_toolchain_text(eco);
        let spec = job
            .entry
            .and_then(|e| e.toolchain.as_ref())
            .filter(|t| EcosystemId::parse(&t.ecosystem) == Some(eco));
        let needs = needs
            .or_else(|| spec.map(|t| t.needs.clone()))
            .unwrap_or_else(|| default_needs.to_string());
        let install = spec
            .map(|t| t.install.clone())
            .unwrap_or_else(|| default_install.to_string());
        self.needs(job, needs, install)
    }

    fn needs(&self, job: &Job<'_>, needs: String, install: String) -> StepError {
        StepError::Setup(SetupError::Install {
            language: Some(job.language),
            failure: InstallFailure::NeedsToolchain {
                what: job.spec.display.clone(),
                needs,
                install,
            },
        })
    }

    /// Fresh log for a job (the previous attempt's output is replaced).
    fn fresh_log(&self, job: &Job<'_>) -> PathBuf {
        let log = self.log_path(job);
        if let Some(parent) = log.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&log, b"");
        log
    }

    /// Download `url` and copy it to `dir/name` (tools that need the real file name).
    fn download_named(
        &mut self,
        job: &Job<'_>,
        version: &str,
        url: &str,
        sha256: &str,
        dir: &Path,
        name: &str,
    ) -> Result<PathBuf, StepError> {
        let file = self.download(job, version, url, Expected::Sha256(sha256.to_string()))?;
        let rel = crate::registry::safe_relative(name)
            .filter(|r| r.components().count() == 1)
            .ok_or_else(|| StepError::Failed(format!("unsafe file name {name:?}")))?;
        fs::create_dir_all(dir)?;
        let target = dir.join(rel);
        fs::copy(&file, &target)?;
        let _ = fs::remove_file(&file);
        Ok(target)
    }

    // ----------------------------------------------------------------------------- go

    pub(super) fn go_install(
        &mut self,
        job: &Job<'_>,
        module: &str,
        versions: &[GoInstallVersion],
    ) -> Result<InstalledTool, StepError> {
        let tc = self.toolchain(job, EcosystemId::Go)?;
        let go = toolchain_exe(&tc, &["go"], self.platform)
            .ok_or_else(|| self.needs_toolchain(job, EcosystemId::Go, None))?;
        let go_version = tc
            .version
            .clone()
            .or_else(|| first_line_version(&tc.root.join("VERSION")))
            .or_else(|| {
                os::toolchain_output(&go, &["env", "GOVERSION"]).and_then(|t| Version::parse(t.trim()))
            });
        let chosen = choose_go_version(versions, go_version.as_ref()).ok_or_else(|| {
            let lowest = versions
                .iter()
                .filter_map(|v| Version::parse(&v.min_go))
                .min()
                .map(|v| v.text)
                .unwrap_or_default();
            self.needs_toolchain(job, EcosystemId::Go, Some(format!("Go {lowest} or newer")))
        })?;
        let version = chosen.version.clone();
        let exe_name = archive::single_file_name(&format!("bin/{}", go_binary_name(module)), self.platform);
        if self.manifest.has_version(&self.tools_dir, &job.spec.id, &version) {
            let dir = self.version_dir(&job.spec.id, &version)?;
            if dir.join(&exe_name).is_file() {
                return Ok(self.already_at(job, &version, dir));
            }
        }
        self.emit(job, &version, "start", 0, None);
        let log = self.fresh_log(job);
        let stage = self.staging()?;
        let work = self.work_dir()?;
        self.need_mb = 600;
        let result = (|| {
            let tmp = work.join("tmp");
            fs::create_dir_all(&tmp)?;
            let mut env = base_env(self.vars, &tmp);
            if let Some(bin) = go.parent() {
                prepend_path(&mut env, &[bin.to_path_buf()]);
            }
            for (k, v) in [
                ("GOBIN", stage.join("bin").into_os_string()),
                ("GOPATH", work.join("gopath").into_os_string()),
                ("GOMODCACHE", work.join("mod").into_os_string()),
                ("GOCACHE", work.join("cache").into_os_string()),
            ] {
                env.insert(k.to_string(), v);
            }
            for (k, v) in [
                ("GOTOOLCHAIN", "local"),
                ("GOFLAGS", ""),
                ("CGO_ENABLED", "0"),
                ("GOTELEMETRY", "off"),
                ("GOENV", "off"),
                ("GOWORK", "off"),
            ] {
                env.insert(k.to_string(), OsString::from(v));
            }
            self.emit(job, &version, "build", 0, None);
            let target = format!("{module}@{version}");
            let (ok, _) =
                run_logged(&go, &os_args(&[&"install", &target]), &env, &work, &log, command_timeout())?;
            if !ok {
                return Err(StepError::FailedLog(log.clone()));
            }
            if !chosen.h1.is_empty() {
                let ziphash = work
                    .join("mod")
                    .join("cache")
                    .join("download")
                    .join(escape_module_path(module))
                    .join("@v")
                    .join(format!("{version}.ziphash"));
                let got = fs::read_to_string(&ziphash).unwrap_or_default();
                if got.trim() != chosen.h1.trim() {
                    return Err(StepError::Checksum(format!(
                        "{target}: module hash {} (expected {})",
                        got.trim(),
                        chosen.h1
                    )));
                }
            }
            let exe = stage.join(&exe_name);
            if !exe.is_file() {
                return Err(StepError::Failed(format!("go install did not produce {}", exe.display())));
            }
            archive::set_executable(&exe)?;
            // The module cache is read-only; `go clean -modcache` empties it first.
            let _ =
                run_logged(&go, &os_args(&[&"clean", &"-modcache"]), &env, &work, &log, command_timeout());
            let files = Installer::file_hashes(&stage, &[(exe_name.clone(), exe)]);
            self.commit(
                job,
                &version,
                &stage,
                None,
                "go_install",
                vec![format!("go install {target}")],
                files,
            )
        })();
        let _ = archive::remove_tree(&work);
        if result.is_err() {
            let _ = archive::remove_tree(&stage);
        }
        let dir = result?;
        Ok(self.installed(job, &version, dir))
    }

    // ------------------------------------------------------------------------------- R

    pub(super) fn r_package(
        &mut self,
        job: &Job<'_>,
        package: &str,
        files: &[RBinary],
    ) -> Result<InstalledTool, StepError> {
        let tc = self.toolchain(job, EcosystemId::R)?;
        let r = toolchain_exe(&tc, &["R"], self.platform)
            .ok_or_else(|| self.needs_toolchain(job, EcosystemId::R, None))?;
        let r_version = tc
            .version
            .clone()
            .or_else(|| {
                os::toolchain_output(&r, &["--version"])
                    .and_then(|t| t.lines().next().and_then(Version::parse))
            })
            .ok_or_else(|| self.needs_toolchain(job, EcosystemId::R, None))?;
        let minor = platform_select::r_minor(&r_version);
        let distro = self.platform.linux_distro();
        let chosen = platform_select::r_binaries_for(files, self.platform, distro.as_deref(), &minor);
        if chosen.is_empty() {
            let minors = platform_select::r_minors_for(files, self.platform, distro.as_deref());
            let advice = match minors.as_slice() {
                [] => None,
                [one] => Some(format!("Install R {one} and run trace again.")),
                [rest @ .., last] => {
                    Some(format!("Install R {} or {last} and run trace again.", rest.join(", ")))
                }
            };
            return Err(StepError::Unavailable { advice });
        }
        let variant = format!("R-{minor}");
        let version = job.spec.version.clone();
        if self
            .manifest
            .has_variant(&self.tools_dir, &job.spec.id, &version, &variant)
        {
            let dir = self.version_dir(&job.spec.id, &version)?;
            return Ok(self.already_at(job, &version, dir));
        }
        self.emit(job, &version, "start", 0, None);
        let log = self.fresh_log(job);
        let stage = self.staging()?;
        let work = self.work_dir()?;
        let result = (|| {
            let mut local = Vec::new();
            for f in &chosen {
                // The file name from the URL path without its query (Posit binaries are
                // requested with `?r_version=..&arch=..`; `?` is invalid in Windows names).
                let name = f
                    .url
                    .split(['?', '#'])
                    .next()
                    .unwrap_or(&f.url)
                    .rsplit('/')
                    .next()
                    .filter(|n| !n.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{}_{}.tar.gz", f.package, f.version));
                local.push(self.download_named(
                    job,
                    &version,
                    &f.url,
                    &f.sha256,
                    &work.join("pkgs"),
                    &name,
                )?);
            }
            let tmp = work.join("tmp");
            fs::create_dir_all(&tmp)?;
            let empty = work.join("empty");
            fs::write(&empty, b"")?;
            let mut env = base_env(self.vars, &tmp);
            for key in ["R_LIBS", "R_LIBS_USER", "R_LIBS_SITE"] {
                env.insert(key.into(), stage.clone().into_os_string());
            }
            for key in ["R_ENVIRON_USER", "R_PROFILE_USER", "R_ENVIRON", "R_PROFILE"] {
                env.insert(key.into(), empty.clone().into_os_string());
            }
            let library = {
                let mut s = OsString::from("--library=");
                s.push(stage.as_os_str());
                s
            };
            let mut args = os_args(&[&"CMD", &"INSTALL", &library]);
            args.extend(local.iter().map(|f| f.clone().into_os_string()));
            self.emit(job, &version, "build", 0, None);
            let (ok, _) = run_logged(&r, &args, &env, &work, &log, command_timeout())?;
            if !ok || !stage.join(package).is_dir() {
                return Err(StepError::FailedLog(log.clone()));
            }
            self.commit(
                job,
                &version,
                &stage,
                Some(&variant),
                "r_package",
                chosen.iter().map(|f| f.url.clone()).collect(),
                BTreeMap::new(),
            )
        })();
        let _ = archive::remove_tree(&work);
        if result.is_err() {
            let _ = archive::remove_tree(&stage);
        }
        let dir = result?;
        Ok(self.installed(job, &version, dir))
    }

    // --------------------------------------------------------------------------- ghcup

    pub(super) fn ghcup(
        &mut self,
        job: &Job<'_>,
        hls_version: &str,
        supported_ghc: &[String],
    ) -> Result<InstalledTool, StepError> {
        let tc = self.toolchain(job, EcosystemId::Haskell)?;
        let ghc = tc
            .version
            .clone()
            .or_else(|| {
                toolchain_exe(&tc, &["ghc"], self.platform)
                    .and_then(|g| os::toolchain_output(&g, &["--numeric-version"]))
                    .and_then(|t| Version::parse(t.trim()))
            })
            .ok_or_else(|| self.needs_toolchain(job, EcosystemId::Haskell, None))?;
        let ghc_text = ghc.parts.iter().map(u64::to_string).collect::<Vec<_>>().join(".");
        if !supported_ghc.iter().any(|s| s == &ghc_text) {
            let list = match supported_ghc {
                [] => String::new(),
                [one] => one.clone(),
                [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
            };
            let suggest = supported_ghc.last().cloned().unwrap_or_default();
            return Err(StepError::Setup(SetupError::Unsupported {
                language: job.language,
                first: format!("The Haskell language server does not support GHC {ghc_text}."),
                second: Some(format!("Use GHC {list} (ghcup install ghc {suggest}) and run trace again.")),
            }));
        }
        let name = format!("haskell-language-server-{ghc_text}");
        let platform = self.platform;
        let find_in = |dir: &Path| -> Option<PathBuf> {
            os::find_executable(&[name.as_str()], &[dir.join("bin"), dir.to_path_buf()], platform)
        };
        // 1. Installed into trace's tools folder before.
        if let Some(dir) = self.manifest.tool_dir(&self.tools_dir, &job.spec.id) {
            if find_in(&dir).is_some() {
                let version = self
                    .manifest
                    .tools
                    .get(&job.spec.id)
                    .map(|t| t.version.clone())
                    .unwrap_or_default();
                return Ok(self.already_at(job, &version, dir));
            }
        }
        // 2. Already in the user's GHCup (the toolchain ships it).
        let ghcup_dirs = trace_env::haskell::ghcup_install_dirs(&tc, self.vars, self.platform);
        for dir in &ghcup_dirs {
            for candidate in [dir.clone(), dir.join("hls").join(hls_version)] {
                if let Some(exe) = find_in(&candidate) {
                    return Ok(InstalledTool {
                        id: job.spec.id.clone(),
                        version: hls_version.to_string(),
                        dir: exe.parent().map(Path::to_path_buf).unwrap_or(candidate),
                        already: true,
                    });
                }
            }
        }
        // 3. `ghcup install hls <v> --isolate <tools dir>` with the user's GHCup.
        let ghcup = toolchain_exe(&tc, &["ghcup"], self.platform)
            .or_else(|| {
                ghcup_dirs
                    .iter()
                    .find_map(|d| os::find_executable(&["ghcup"], &[d.join("bin")], self.platform))
            })
            .or_else(|| trace_env::lookup::on_path(&["ghcup"], self.vars, self.platform))
            .ok_or_else(|| {
                self.needs(
                    job,
                    "GHCup".to_string(),
                    "Install it from https://www.haskell.org/ghcup".to_string(),
                )
            })?;
        let version = hls_version.to_string();
        self.emit(job, &version, "start", 0, None);
        let log = self.fresh_log(job);
        let stage = self.staging()?;
        let work = self.work_dir()?;
        self.need_mb = 2_000;
        let result = (|| {
            let tmp = work.join("tmp");
            fs::create_dir_all(&tmp)?;
            let mut env = base_env(self.vars, &tmp);
            for key in [
                "GHCUP_INSTALL_BASE_PREFIX",
                "GHCUP_USE_XDG_DIRS",
                "XDG_DATA_HOME",
                "XDG_BIN_HOME",
                "XDG_CACHE_HOME",
                "XDG_CONFIG_HOME",
            ] {
                if let Some(v) = self.vars.get(key) {
                    env.insert(key.into(), v.to_os_string());
                }
            }
            self.emit(job, &version, "build", 0, None);
            let args = os_args(&[&"install", &"hls", &"--isolate", &stage, &version]);
            let (ok, _) = run_logged(&ghcup, &args, &env, &work, &log, command_timeout())?;
            if !ok {
                return Err(StepError::FailedLog(log.clone()));
            }
            let exe = find_in(&stage)
                .or_else(|| find_named(&stage, &name, self.platform))
                .ok_or_else(|| StepError::Failed(format!("ghcup did not install {name}")))?;
            let files = Installer::file_hashes(&stage, &[(name.clone(), exe)]);
            self.commit(
                job,
                &version,
                &stage,
                None,
                "ghcup",
                vec![format!("ghcup install hls {version}")],
                files,
            )
        })();
        let _ = archive::remove_tree(&work);
        if result.is_err() {
            let _ = archive::remove_tree(&stage);
        }
        let dir = result?;
        Ok(self.installed(job, &version, dir))
    }

    // ------------------------------------------------------------------------ coursier

    pub(super) fn coursier(
        &mut self,
        job: &Job<'_>,
        launcher: &[Artifact],
        fetch: &[String],
        lock: &[PinnedFile],
    ) -> Result<InstalledTool, StepError> {
        let version = job.spec.version.clone();
        if self.manifest.has_version(&self.tools_dir, &job.spec.id, &version) {
            let dir = self.version_dir(&job.spec.id, &version)?;
            return Ok(self.already_at(job, &version, dir));
        }
        let launcher_artifact = platform_select::artifact_for(launcher, self.platform);
        if launcher_artifact.is_none() && !fetch.is_empty() {
            return Err(StepError::Unavailable {
                advice: platform_select::unavailable_advice(launcher, self.platform),
            });
        }
        self.emit(job, &version, "start", 0, None);
        let log = self.fresh_log(job);
        let stage = self.staging()?;
        let work = self.work_dir()?;
        let final_dir = self.version_dir(&job.spec.id, &version)?;
        let result = (|| {
            // The pinned jar closure (sha256 each), each at its URL's Coursier cache path
            // (`<tool>/cache/https/<host>/<path>`: lock names are `group:artifact`, never file
            // names; the server hooks and later `cs fetch --cache` runs read that cache), and a
            // Java argfile with the classpath at its final location
            // (`java @<tool>/classpath.txt <main class>`).
            let mut classpath = Vec::new();
            for f in lock {
                let rel = crate::languages::scala::cache_path(Path::new("cache"), &f.url)
                    .ok_or_else(|| StepError::Failed(format!("unsupported lock url {:?}", f.url)))?;
                let file = self.download(job, &version, &f.url, Expected::Sha256(f.sha256.clone()))?;
                let target = stage.join(&rel);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&file, &target)?;
                let _ = fs::remove_file(&file);
                classpath.push(final_dir.join(&rel).to_string_lossy().replace('\\', "/"));
            }
            let sep = self.platform.path_list_sep().to_string();
            fs::write(stage.join("classpath.txt"), format!("-cp\n\"{}\"\n", classpath.join(&sep)))?;
            let mut sources: Vec<String> = lock.iter().map(|f| f.url.clone()).collect();
            // The pinned coursier launcher (used by install extras, e.g. per-Scala parts).
            if let Some(a) = launcher_artifact {
                let file = self.download(job, &version, &a.url, Expected::Sha256(a.sha256.clone()))?;
                let cs_dir = stage.join("cs");
                fs::create_dir_all(&cs_dir)?;
                let single = archive::single_file_name("cs", self.platform);
                let opts = ExtractOptions {
                    strip: a.strip,
                    strip_prefix: a.strip_prefix.as_deref(),
                    subdir: a.subdir.as_deref(),
                    single_file: Some(&single),
                };
                archive::extract(&file, &cs_dir, &opts, &mut Budget::default())?;
                let _ = fs::remove_file(&file);
                let cs = cs_dir.join(&single);
                if !cs.is_file() {
                    // Zipped launchers carry their own name (cs-x86_64-pc-win32.exe).
                    let found = find_named_prefix(&cs_dir, "cs")
                        .ok_or_else(|| StepError::Failed("the coursier launcher has no program".into()))?;
                    fs::rename(found, &cs)?;
                }
                archive::set_executable(&cs)?;
                sources.push(a.url.clone());
                if !fetch.is_empty() {
                    let tmp = work.join("tmp");
                    fs::create_dir_all(&tmp)?;
                    let mut env = base_env(self.vars, &tmp);
                    env.insert("COURSIER_CACHE".into(), stage.join("cache").into_os_string());
                    let mut args = os_args(&[&"fetch", &"--cache", &stage.join("cache")]);
                    args.extend(fetch.iter().map(OsString::from));
                    self.emit(job, &version, "build", 0, None);
                    let (ok, _) = run_logged(&cs, &args, &env, &work, &log, command_timeout())?;
                    if !ok {
                        return Err(StepError::FailedLog(log.clone()));
                    }
                    sources.extend(fetch.iter().map(|c| format!("cs fetch {c}")));
                }
            }
            self.commit(job, &version, &stage, None, "coursier", sources, BTreeMap::new())
        })();
        let _ = archive::remove_tree(&work);
        if result.is_err() {
            let _ = archive::remove_tree(&stage);
        }
        let dir = result?;
        Ok(self.installed(job, &version, dir))
    }

    // ---------------------------------------------------------------------- toolchain

    pub(super) fn toolchain_recipe(
        &mut self,
        job: &Job<'_>,
        ecosystem: &str,
    ) -> Result<InstalledTool, StepError> {
        let eco = EcosystemId::parse(ecosystem)
            .ok_or_else(|| StepError::Failed(format!("unknown ecosystem {ecosystem}")))?;
        let tc = self.toolchain(job, eco)?;
        Ok(InstalledTool {
            id: job.spec.id.clone(),
            version: tc
                .version
                .as_ref()
                .map(|v| v.text.clone())
                .unwrap_or_else(|| job.spec.version.clone()),
            dir: tc.root,
            already: true,
        })
    }

    fn already_at(&self, job: &Job<'_>, version: &str, dir: PathBuf) -> InstalledTool {
        InstalledTool {
            id: job.spec.id.clone(),
            version: version.to_string(),
            dir,
            already: true,
        }
    }
}

/// A file named `name` (`.exe` on Windows) anywhere below `dir` (bounded walk).
fn find_named(dir: &Path, name: &str, platform: &Platform) -> Option<PathBuf> {
    let want = platform.exe(name);
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(read) = fs::read_dir(&d) else { continue };
        for entry in read.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_file() && entry.file_name().to_string_lossy() == want {
                return Some(path);
            }
            if ft.is_dir() && depth < 6 {
                stack.push((path, depth + 1));
            }
        }
    }
    None
}

/// The first regular file directly in `dir` whose name starts with `prefix`.
fn find_named_prefix(dir: &Path, prefix: &str) -> Option<PathBuf> {
    let mut names: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| e.path())
        .collect();
    names.sort();
    names.into_iter().next()
}

#[cfg(test)]
#[path = "../../tests/unit/install/ecosystem.rs"]
mod tests;
