//! Scala setup hooks (owner jvm): Metals 1.6.9 on the trace-managed JDK 21.
//!
//! * **Install** (`trace status --install scala`, the only time anything is downloaded): the
//!   Coursier recipe fetches the pinned Metals jars (sha256 lock) into
//!   `<tools>/metals/<v>/cache` (Coursier cache layout). The install extras of this file then
//!   fetch the parts that depend on the repository into the same cache: the Bloop build
//!   server and its sbt plugin, per Scala version of the build the presentation compiler
//!   (`mtags_<v>` / `scala3-presentation-compiler_3`), SemanticDB (Scala 2) and the Zinc
//!   compiler bridge, the `mtags` of the Scala version sbt itself uses (build files) and the
//!   Java SemanticDB compiler plugin Metals configures for Java sources of the build.
//!   A `parts.json` next to the cache records what was fetched.
//! * **Analysis never downloads**: Metals runs with `COURSIER_MODE=offline` and its own
//!   Coursier cache (the tools cache); the build (sbt + Bloop) reads the user's Coursier cache
//!   offline (`-Dsbt.offline=true`) and finds the server parts through a local file
//!   repository (`COURSIER_REPOSITORIES`, and an sbt resolver in the trace-owned sbt global
//!   base). A missing part is the error "The Scala language server needs its files for
//!   Scala 2.13.18." / "Install them: trace status --install scala".
//! * **Toolchain**: the user's JDK 17+ and the build tool's launcher (sbt, Mill, Scala CLI).
//!   Maven/Gradle Scala builds and build-less Scala files are refused with a clear error.
//! * **Approval**: sbt/Mill run the build definition (`bloopInstall`), Scala CLI its directives.
//! * **Windows / Unix sockets**: Bloop listens on a Unix socket in its daemon folder, which
//!   Metals takes from the OS data folder: on Linux / macOS below Metals' user home (a short,
//!   trace-owned home under the trace cache folder, `XDG_*` pointed there too), on Windows the
//!   user's known folder LocalAppData (`%LOCALAPPDATA%\ScalaCli\data`; Windows answers it from
//!   the user profile, neither the environment nor `user.home` moves it). The socket path must
//!   stay short, else an error ([`socket_error`] checks the path Bloop really uses).
//! * **Nothing outlives trace**: sbt runs in batch mode without its server; Bloop is a child
//!   of the Metals JVM and stops with the session's process tree (`crate::procs`).
//! * **Readiness** (`warm_up`): the registry entry waits for nothing; the hook follows the
//!   build import and the workspace index in the client's log messages and `.metals/metals.log`
//!   of the workspace (the lines of this Metals process only), stops at once with the build /
//!   dependency error when the import failed, then waits until the first compile of the open
//!   files ended (Metals' `Compiling` progress) so answers never depend on the compile state.
//!   Changes settle the same way (`settle_changes`).
//! * **Answers**: Metals answers "request cancelled" while it compiles (retried);
//!   `.metals/readonly/**` and `jar:` locations are external libraries; files that only belong
//!   to another Scala version of a cross-built sbt project (`src/<set>/scala-3`, `scala-2.12`,
//!   `scala-2.13+` ... not matching the imported build's Scala version) are outside the build
//!   before any request (`outside_build_file`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use trace_core::model::Diagnostic;
use trace_core::paths::RepoPaths;
use trace_core::setup_error::SetupError;
use trace_core::Language;
use trace_env::jvm::{self, BuildSystem, JvmSetup};
use trace_env::os::{self, EnvVars, Os, Platform};
use trace_env::{EcosystemId, Toolchain, ToolchainStatus};

use crate::backends::fntype::FnTypeRoute;
use crate::languages::jvm::{
    approval, classify_load, ensure_state_dirs, file_uri, is_language_stdlib, jar_library, jar_location,
    jdk_error, jdk_status, load_texts, path_text, pending_dirs, percent_decode, prepared_fingerprint,
    write_state_file, LoadOutcome,
};
use crate::languages::{
    first_line, read_context, AnswerPolicy, ExternalLocation, InstallExtra, InstallExtraContext,
    LoadedContext, Prepared, Server, SettleContext, SetupContext, WarmUpContext, WorkspaceContext,
    WorkspaceMode,
};
use crate::registry::Registry;
use crate::setup::{deps_error, require_runtimes, require_server, Collect};

mod install;
mod metals_log;

use self::install::*;
use self::metals_log::*;

pub(crate) use self::install::cache_path;

pub struct Hooks;

/// Metals needs a JDK 17 or newer for the build (Bloop 2 runs on 17+).
pub const SCALA_MIN_JDK: u32 = 17;
/// Server-side parts pinned with Metals 1.6.9.
pub const METALS_VERSION: &str = "1.6.9";
pub const BLOOP_VERSION: &str = "2.1.2";
pub const SEMANTICDB_VERSION: &str = "4.17.0";
/// The Java SemanticDB compiler plugin Metals 1.6.9 configures for Java sources of the build.
pub const SEMANTICDB_JAVAC_VERSION: &str = "0.12.3";
/// Longest Unix-socket path accepted (AF_UNIX: 108 bytes on Linux/Windows, 104 on macOS).
pub const MAX_SOCKET_PATH: usize = 100;
/// The tools folder id of Metals.
pub const METALS_TOOL: &str = "metals";
const PARTS_FILE: &str = "parts.json";
const SBT_RESOLVER_FILE: &str = "trace-metals.sbt";
/// Coursier cache of the sbt plugins Metals adds (`<tools>/metals/<v>/sbt-plugins`).
const SBT_PLUGINS_CACHE: &str = "sbt-plugins";

/// What the install extras fetched (`<tools>/metals/<v>/parts.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Parts {
    pub schema: u32,
    pub bloop: Option<String>,
    /// Zinc version of the Bloop server (compiler bridges are fetched for it).
    pub zinc: Option<String>,
    /// Project Scala versions whose parts are installed.
    pub scala: BTreeSet<String>,
    /// sbt version -> the Scala version sbt itself runs (its build files' mtags).
    pub sbt: BTreeMap<String, String>,
    /// Java SemanticDB compiler plugin version (Java sources of the build).
    pub semanticdb_javac: Option<String>,
}

impl Parts {
    pub fn load(metals_dir: &Path) -> Parts {
        std::fs::read(metals_dir.join(PARTS_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save(&self, metals_dir: &Path) -> std::io::Result<()> {
        let mut out = self.clone();
        out.schema = 1;
        let bytes = serde_json::to_vec_pretty(&out).map_err(std::io::Error::other)?;
        std::fs::write(metals_dir.join(PARTS_FILE), bytes)
    }
}

/// Backend-private data of a Scala preflight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScalaData {
    pub build: BuildSystem,
    /// Java argument file of the Metals launch (`@{outside}/metals.args`).
    pub argfile: String,
    /// Short user home of the Metals JVM (Bloop socket folder).
    pub short_home: PathBuf,
    /// sbt global plugin settings (`<short home>/.sbt/1.0/plugins/trace-metals.sbt`, the
    /// trace-owned sbt global base): the resolver of Metals' sbt-bloop plugin (sbt builds).
    pub sbt_global_plugin: Option<String>,
    /// The Scala version the import builds (sbt: the default `scalaVersion`; other Scala
    /// versions of a cross build are not imported).
    pub build_scala: Option<String>,
}

impl Server for Hooks {
    fn preflight(&self, cx: &SetupContext<'_>) -> Result<Prepared, SetupError> {
        let language = Language::Scala;
        let readable = trace_core::paths::forbidden_roots();
        let dcx = read_context(cx, EcosystemId::Jvm, &readable);
        let setup = jvm::setup(&dcx);
        let systems = scala_systems(&setup);
        // Project shape: without an sbt / Mill / Scala CLI build nothing else matters.
        let Some(build) = systems.first().copied() else {
            return Err(no_build_error(&setup));
        };
        let mut c = Collect::default();

        // 1. Platform: the Bloop socket path.
        let short_home = short_home(cx.repo);
        if let Some(e) = socket_error(&short_home, cx.platform, cx.vars) {
            c.push(e);
        }
        // 2. Toolchain: JDK 17+ and the build tool's launcher.
        let status = setup.select_jdk(SCALA_MIN_JDK);
        if let Some(e) = jdk_error(language, &setup, SCALA_MIN_JDK, &status) {
            c.push(e);
        }
        if let Some(e) = launcher_error(&setup, build) {
            c.push(e);
        }
        // 3. Server: Metals jars (lock) + the JDK 21 runtime, then the repository's parts.
        c.check(require_server(cx));
        c.check(require_runtimes(cx));
        let metals_dir = cx.tools.tool_dir(METALS_TOOL);
        let mut classpath = Vec::new();
        if let Some(dir) = &metals_dir {
            match lock_classpath(cx.entry, dir) {
                Some(cp) => classpath = cp,
                None => c.push(SetupError::ServerMissing { language }),
            }
            let missing = missing_parts(dir, &setup, build);
            if !missing.is_empty() {
                c.push(parts_error(&missing));
            }
        }
        // 4. Dependencies (static: sbt version and plugins).
        let report = setup.deps_report(&systems);
        if let Some(e) = deps_error(language, &report) {
            c.push(e);
        }
        // 5. Approval.
        c.check(approval(cx, build));

        let jdk = match status {
            ToolchainStatus::Found(t) => Some(t),
            _ => None,
        };
        let prepared = scala_prepared(
            cx,
            &setup,
            &systems,
            jdk,
            report.roots,
            metals_dir.as_deref(),
            &classpath,
            short_home,
        );
        c.finish(prepared)
    }

    fn prepare_workspace(&self, cx: &WorkspaceContext<'_>) -> Result<(), SetupError> {
        let language = Language::Scala;
        ensure_state_dirs(cx, language)?;
        let Some(data) = scala_data(cx.prepared) else {
            return Ok(());
        };
        let failed = |path: &Path, e: std::io::Error| SetupError::BuildFailed {
            language,
            what: format!("creating {} failed: {e}", path.display()),
            log: cx.log.to_path_buf(),
        };
        std::fs::create_dir_all(&data.short_home).map_err(|e| failed(&data.short_home, e))?;
        // The readiness wait reads this process's metals.log only: the previous process's
        // lines move aside (the workspace is trace's mirror; `.metals/` is never synced). A
        // log that cannot move is still read correctly: only the lines after the last
        // "Started: Metals" line count (`MetalsLog`).
        let metals = cx.workspace.join(".metals");
        let current = metals.join("metals.log");
        if current.is_file() {
            let _ = std::fs::rename(&current, metals.join(PREVIOUS_LOG));
        }
        if let Some(text) = &data.sbt_global_plugin {
            let dir = sbt_global_plugins_dir(&data.short_home);
            std::fs::create_dir_all(&dir).map_err(|e| failed(&dir, e))?;
            let file = dir.join(SBT_RESOLVER_FILE);
            std::fs::write(&file, text).map_err(|e| failed(&file, e))?;
        }
        write_state_file(cx, language, "metals.args", data.argfile.as_bytes())
    }

    fn warm_up(&self, cx: &mut WarmUpContext<'_, '_>) -> Result<(), SetupError> {
        wait_for_metals(cx.client, cx.prepared, &cx.snapshot.dir, metals_ready_limit())
    }

    fn settle_changes(&self, cx: &mut SettleContext<'_>) -> Result<(), SetupError> {
        // Metals may compile after a change (a file watcher event): wait until that ended.
        let log = cx.client.log_path().to_path_buf();
        cx.client
            .wait_progress_after_open(SETTLE_GRACE, SETTLE_QUIET)
            .map_err(|e| match e {
                crate::SemanticError::Setup(setup) => setup,
                _ => SetupError::ServerCrashed {
                    language: Language::Scala,
                    log,
                },
            })
    }

    fn check_loaded(&self, cx: &LoadedContext<'_>) -> Result<Vec<Diagnostic>, SetupError> {
        classify_metals(cx.prepared, cx.log_messages, cx.notifications, cx.diagnostics, cx.log)
    }

    fn external_location(&self, uri: &str, _prepared: &Prepared) -> Option<ExternalLocation> {
        metals_readonly_location(uri).or_else(|| jar_location(uri))
    }

    fn answer_policy(&self) -> AnswerPolicy {
        AnswerPolicy {
            retry_cancelled: 3,
            ..AnswerPolicy::default()
        }
    }

    fn fn_type_route(&self, _language: Language) -> FnTypeRoute {
        FnTypeRoute::Label
    }

    fn install_extras(&self, repo_root: Option<&Path>) -> Vec<InstallExtra> {
        install_extras_for(repo_root)
    }

    fn run_install_extra(
        &self,
        extra: &InstallExtra,
        cx: &InstallExtraContext<'_>,
    ) -> Result<(), SetupError> {
        run_extra(extra, cx)
    }

    fn outside_build(&self, path: &str, error: &str) -> Option<String> {
        outside_build_error(path, error)
    }

    fn outside_build_file(&self, path: &str, prepared: &Prepared) -> Option<String> {
        let data = scala_data(prepared)?;
        if data.build != BuildSystem::Sbt {
            return None;
        }
        let version = data.build_scala.as_deref()?;
        let dir = scala_version_dir(path)?;
        match scala_dir_matches(dir.trim_start_matches("scala-"), version) {
            Some(false) => {
                Some(format!("only built for {} (the build imports Scala {version})", version_dir_label(dir)))
            }
            _ => None,
        }
    }
}

/// The load checks of a Metals process (parts, dependencies, build import) over its log
/// messages, notifications and diagnostics; notes for the run's diagnostics.
fn classify_metals(
    prepared: &Prepared,
    log_messages: &[(u8, String)],
    notifications: &[(String, Value)],
    diagnostics: &[(String, Value)],
    log: &Path,
) -> Result<Vec<Diagnostic>, SetupError> {
    let build = scala_data(prepared).map_or(BuildSystem::Sbt, |d| d.build);
    let (tool_notes, texts) =
        split_tool_artifact_messages(load_texts(log_messages, notifications, diagnostics));
    let parts_failure = texts.iter().find(|t| {
        let l = t.to_ascii_lowercase();
        (l.contains("mtags") || l.contains("presentation-compiler") || l.contains("semanticdb-scalac"))
            && (l.contains("could not")
                || l.contains("failed to")
                || l.contains("not found")
                || l.contains("offline"))
    });
    if let Some(t) = parts_failure {
        return Err(SetupError::Unsupported {
            language: Language::Scala,
            first: format!("The Scala language server is missing some of its files ({}).", first_line(t)),
            second: Some("Install them: trace status --install scala".to_string()),
        });
    }
    match classify_load(&texts) {
        LoadOutcome::DepsMissing => Err(SetupError::DepsMissing {
            language: Language::Scala,
            hint: build.hint().to_string(),
        }),
        LoadOutcome::BuildFailed(message) => Err(SetupError::BuildFailed {
            language: Language::Scala,
            what: format!("{} import failed: {message}", build.tool()),
            log: log.to_path_buf(),
        }),
        LoadOutcome::Ok(notes) => Ok(notes
            .into_iter()
            .chain(tool_notes)
            .map(|n| Diagnostic::new("build_warning", None, n))
            .collect()),
    }
}

/// A request error that means "no build target for this file" -> the reason (Metals answers
/// files of another Scala version this way when the static rule could not decide).
fn outside_build_error(path: &str, error: &str) -> Option<String> {
    let lower = error.to_ascii_lowercase();
    if !(lower.contains("no build target") || lower.contains("not part of any build target")) {
        return None;
    }
    let dir = path
        .replace('\\', "/")
        .split('/')
        .find(|s| {
            s.strip_prefix("scala-")
                .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
        })
        .map(str::to_string)?;
    Some(format!("only built for {} (the build imports its default Scala version)", version_dir_label(&dir)))
}

// ---------------------------------------------------------------------------------------------
// Files of another Scala version (sbt cross builds)
// ---------------------------------------------------------------------------------------------

/// The version-specific source folder of a repository-relative path: `src/<set>/scala-<v>`
/// (sbt's cross-version source folders: `scala-2.13`, `scala-3`, plus the `scala-2.13+` /
/// `scala-2.13-` conventions builds add) or `target/scala-<v>` (generated sources of one Scala
/// version). A module folder that happens to be named `scala-3` is not one.
fn scala_version_dir(path: &str) -> Option<&str> {
    let segs: Vec<&str> = path.split(['/', '\\']).collect();
    // The file name itself is never the folder.
    let dirs = segs.len().checked_sub(1)?;
    (0..dirs).find_map(|i| {
        let seg = segs[i];
        let spec = seg.strip_prefix("scala-")?;
        if !spec.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        let source_set = i >= 2 && segs[i - 2] == "src";
        let target = i >= 1 && segs[i - 1] == "target";
        (source_set || target).then_some(seg)
    })
}

/// Leading numbers of a version text (`2.13.18` -> [2, 13, 18], `3.8.0-RC1` -> [3, 8, 0]).
fn version_numbers(text: &str) -> Vec<u64> {
    text.split('.')
        .map_while(|part| {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<u64>().ok()
        })
        .collect()
}

/// Whether a `scala-<spec>` folder is compiled for Scala `version`: `2.13` / `3` (the version
/// starts with it; `3.x` alike), `2.13+` (that version or newer), `2.13-` (older; the boundary
/// itself is undecided because builds use the suffix both inclusively and exclusively).
/// `None` when the rule cannot decide (never claim "outside" then).
fn scala_dir_matches(spec: &str, version: &str) -> Option<bool> {
    let v = version_numbers(version);
    if v.is_empty() {
        return None;
    }
    let (base, suffix) = if let Some(b) = spec.strip_suffix('+') {
        (b, Some('+'))
    } else if let Some(b) = spec.strip_suffix('-') {
        (b, Some('-'))
    } else {
        (spec.strip_suffix(".x").unwrap_or(spec), None)
    };
    // Only plain dotted numbers are version folders.
    if base.is_empty()
        || !base
            .split('.')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let b = version_numbers(base);
    let prefix: Vec<u64> = v.iter().copied().take(b.len()).collect();
    match suffix {
        Some('+') => Some(prefix >= b),
        Some(_) => match prefix.cmp(&b) {
            std::cmp::Ordering::Less => Some(true),
            std::cmp::Ordering::Greater => Some(false),
            std::cmp::Ordering::Equal => None,
        },
        None => Some(prefix == b),
    }
}

/// Messages about artifacts of the servers' own tools that analysis never uses: Bloop's
/// Scala.js / Scala Native linker bridges ("Could not resolve platform artifacts", needed only
/// to link and run JS/Native code) and Metals' Java SemanticDB plugin for standalone Java
/// files. They are not the project's dependencies: notes, never the dependency error.
const TOOL_ARTIFACT_MESSAGES: &[&str] = &["could not resolve platform artifacts", "semanticdb-javac"];

/// (first lines of the tool-artifact messages as notes, every other text).
fn split_tool_artifact_messages(texts: Vec<String>) -> (Vec<String>, Vec<String>) {
    let (tool, rest): (Vec<String>, Vec<String>) = texts.into_iter().partition(|t| {
        let l = t.to_ascii_lowercase();
        TOOL_ARTIFACT_MESSAGES.iter().any(|m| l.contains(m))
    });
    (tool.iter().map(|t| first_line(t)).collect(), rest)
}

/// "scala-3" -> "Scala 3", "scala-2.13+" -> "Scala 2.13 and newer", "scala-2.12-" -> "Scala
/// 2.12 and older".
fn version_dir_label(dir: &str) -> String {
    let v = dir.trim_start_matches("scala-");
    if let Some(base) = v.strip_suffix('+') {
        format!("Scala {base} and newer")
    } else if let Some(base) = v.strip_suffix('-') {
        format!("Scala {base} and older")
    } else {
        format!("Scala {v}")
    }
}

/// sbt, Mill and Scala CLI builds (the builds Metals imports here).
fn scala_systems(setup: &JvmSetup) -> Vec<BuildSystem> {
    setup
        .project
        .systems()
        .into_iter()
        .filter(|s| matches!(s, BuildSystem::Sbt | BuildSystem::Mill | BuildSystem::ScalaCli))
        .collect()
}

fn no_build_error(setup: &JvmSetup) -> SetupError {
    let other = setup
        .project
        .maven_roots
        .first()
        .or_else(|| setup.project.gradle_roots.first());
    match other {
        Some(dir) => SetupError::Unsupported {
            language: Language::Scala,
            first: format!(
                "Scala projects built with Maven or Gradle are not supported yet ({}).",
                if dir.is_empty() { "." } else { dir.as_str() }
            ),
            second: Some("sbt, Mill and Scala CLI projects work.".to_string()),
        },
        None => SetupError::Unsupported {
            language: Language::Scala,
            first: "These Scala files have no build (sbt, Mill or Scala CLI).".to_string(),
            second: Some("Add a build.sbt or a project.scala file and run trace again.".to_string()),
        },
    }
}

fn launcher_error(setup: &JvmSetup, build: BuildSystem) -> Option<SetupError> {
    let (found, needs, install) = match build {
        BuildSystem::Sbt => {
            (setup.sbt.is_some(), "sbt", "Install it from https://www.scala-sbt.org/download")
        }
        BuildSystem::Mill => (setup.mill.is_some(), "Mill", "Install it from https://mill-build.org"),
        BuildSystem::ScalaCli => (
            setup.scala_cli.is_some(),
            "Scala CLI",
            "Install it from https://scala-cli.virtuslab.org/install",
        ),
        _ => (true, "", ""),
    };
    (!found).then(|| SetupError::ToolchainMissing {
        language: Language::Scala,
        needs: needs.to_string(),
        install: install.to_string(),
    })
}

/// The short user home of the Metals JVM (Bloop's socket lives below it).
pub fn short_home(repo: &RepoPaths) -> PathBuf {
    repo.home.join("jh")
}

/// Bloop's daemon socket where Metals puts it: the OS data folder of Metals' project
/// directories (`ScalaCli`) + `bloop/daemon/socket`.
/// * Windows: the user's known folder LocalAppData (`%LOCALAPPDATA%`, the user's value: Metals
///   asks Windows for the known folder, which neither the environment of the Metals process nor
///   its `user.home` moves), `ScalaCli\data`.
/// * Linux: `$XDG_DATA_HOME/scalacli` of the Metals process = the short home's
///   `.local/share/scalacli` (the preflight sets it; `user.home` is the short home).
/// * macOS: `<user.home>/Library/Application Support/ScalaCli` (the short home).
pub fn bloop_socket_path(short_home: &Path, p: &Platform, vars: &EnvVars) -> PathBuf {
    let data = match p.os {
        Os::Windows => os::data_local_dir(vars, p)
            .or_else(|| os::home_dir(vars, p).map(|h| h.join("AppData").join("Local")))
            .unwrap_or_else(|| short_home.join("AppData").join("Local"))
            .join("ScalaCli")
            .join("data"),
        Os::Linux => short_home.join(".local").join("share").join("scalacli"),
        Os::MacOs => short_home
            .join("Library")
            .join("Application Support")
            .join("ScalaCli"),
    };
    data.join("bloop").join("daemon").join("socket")
}

/// Linux: Metals (and the Bloop daemon folder it chooses) read the XDG folders first; they
/// stay below the trace-owned short home like `user.home`. macOS / Windows do not read them.
fn metals_data_env(short_home: &Path, p: &Platform) -> Vec<(&'static str, String)> {
    if p.os != Os::Linux {
        return Vec::new();
    }
    vec![
        ("XDG_DATA_HOME", path_text(&short_home.join(".local").join("share"))),
        ("XDG_CACHE_HOME", path_text(&short_home.join(".cache"))),
        ("XDG_CONFIG_HOME", path_text(&short_home.join(".config"))),
    ]
}

/// "The Scala build server cannot start: its folder path is too long (<path>)." / how to
/// shorten it: the trace cache folder (Linux, macOS), the Windows user profile (Windows).
pub fn socket_error(short_home: &Path, p: &Platform, vars: &EnvVars) -> Option<SetupError> {
    let socket = bloop_socket_path(short_home, p, vars);
    let second = match p.os {
        Os::Windows => "Bloop keeps it in your Windows user profile; Scala analysis needs a user folder with a shorter path.",
        _ => "Set a shorter trace cache folder (TRACE_CACHE_DIR) and run trace again.",
    };
    (socket.to_string_lossy().len() > MAX_SOCKET_PATH).then(|| SetupError::Unsupported {
        language: Language::Scala,
        first: format!(
            "The Scala build server cannot start: its folder path is too long ({}).",
            socket.display()
        ),
        second: Some(second.to_string()),
    })
}

/// Coordinates of the per-Scala-version parts.
fn scala_coordinates(version: &str) -> Vec<String> {
    if version.starts_with("3.") {
        vec![format!("org.scala-lang:scala3-presentation-compiler_3:{version}")]
    } else {
        vec![
            format!("org.scalameta:mtags_{version}:{METALS_VERSION}"),
            format!("org.scalameta:semanticdb-scalac_{version}:{SEMANTICDB_VERSION}"),
        ]
    }
}

/// The Zinc compiler bridge of a Scala version (Scala 3 ships its own sbt bridge).
fn bridge_coordinate(version: &str, zinc: Option<&str>) -> Option<String> {
    if version.starts_with("3.") {
        return Some(format!("org.scala-lang:scala3-sbt-bridge:{version}"));
    }
    let zinc = zinc?;
    let mut parts = version.split('.');
    let binary = format!("{}.{}", parts.next()?, parts.next()?);
    Some(format!("org.scala-sbt:compiler-bridge_{binary}:{zinc}"))
}

fn scala_data(prepared: &Prepared) -> Option<&ScalaData> {
    prepared.data.as_ref()?.downcast_ref::<ScalaData>()
}

/// The Metals version of the embedded registry (the install record).
fn metals_install_version() -> String {
    Registry::builtin()
        .entry("lsp:metals")
        .and_then(|e| e.install.as_ref().map(|i| i.version.clone()))
        .unwrap_or_else(|| METALS_VERSION.to_string())
}

/// `<tools>/<id>/<version>` per MANIFEST, else the pinned version folder when it exists.
fn installed_dir(tools_dir: &Path, id: &str, version: &str) -> Option<PathBuf> {
    crate::install::manifest::Manifest::load(tools_dir)
        .tool_dir(tools_dir, id)
        .or_else(|| {
            let dir = tools_dir.join(id).join(version);
            dir.is_dir().then_some(dir)
        })
}

/// Java argument file of the Metals JVM: its short user home, its own Coursier cache (the
/// tools cache, read offline) and the pinned class path.
pub fn metals_argfile(short_home: &Path, metals_cache: &Path, classpath: &[PathBuf], p: &Platform) -> String {
    let quote = |text: String| format!("\"{}\"", text.replace('\\', "/").replace('"', "\\\""));
    let cp = classpath
        .iter()
        .map(|c| c.display().to_string().replace('\\', "/"))
        .collect::<Vec<_>>()
        .join(&p.path_list_sep().to_string());
    format!(
        "-Duser.home={}\n-Dcoursier.cache={}\n-cp {}\n",
        quote(short_home.display().to_string()),
        quote(metals_cache.display().to_string()),
        quote(cp)
    )
}

/// The sbt global plugin settings: every build level resolves Metals' sbt-bloop plugin from
/// the tools folder's sbt plugin cache (a local file repository; nothing is downloaded).
pub fn sbt_resolver(plugin_uri: &str) -> String {
    format!(
        "// Written by trace into its own sbt global base: Metals' build-server plugin\n// comes from trace's tools folder; nothing is downloaded.\nresolvers += \"trace-metals-parts\" at \"{plugin_uri}\"\n"
    )
}

#[allow(clippy::too_many_arguments)] // one call site; the inputs are the preflight's results
fn scala_prepared(
    cx: &SetupContext<'_>,
    setup: &JvmSetup,
    systems: &[BuildSystem],
    jdk: Option<Toolchain>,
    library_roots: Vec<trace_env::LibraryRoot>,
    metals_dir: Option<&Path>,
    classpath: &[PathBuf],
    short_home: PathBuf,
) -> Prepared {
    let build = systems.first().copied().unwrap_or(BuildSystem::Sbt);
    let project = &setup.project;
    let jdk_home = jdk.as_ref().map(|t| path_text(&t.root)).unwrap_or_default();
    let mut vars = BTreeMap::new();
    vars.insert("jdk".to_string(), jdk_home.clone());
    let launcher = |t: &Option<Toolchain>, id: &str| {
        t.as_ref()
            .and_then(|t| t.executables.get(id))
            .map_or(Value::Null, |p| Value::String(path_text(p)))
    };
    let mut json_vars = BTreeMap::new();
    json_vars.insert("sbt_script".to_string(), launcher(&setup.sbt, "sbt"));
    json_vars.insert("mill_script".to_string(), launcher(&setup.mill, "mill"));
    json_vars.insert("scala_cli".to_string(), launcher(&setup.scala_cli, "scala-cli"));

    let metals_cache = metals_dir.map(|d| d.join("cache")).unwrap_or_default();
    let parts_repo = metals_dir.map(central_base).unwrap_or_default();
    let parts_uri = file_uri(&parts_repo);
    let plugin_uri = file_uri(&metals_dir.map(sbt_plugin_base).unwrap_or_default());
    let mut env = BTreeMap::new();
    if !jdk_home.is_empty() {
        env.insert("JAVA_HOME".to_string(), jdk_home.clone());
    }
    env.insert("COURSIER_MODE".to_string(), "offline".to_string());
    // No COURSIER_CACHE variable: it would override Metals' own `-Dcoursier.cache` (the tools
    // cache holding Bloop and the presentation compilers; versions such as `0.34.0+44` are
    // only found through a cache, never as a raw file repository). sbt gets the user's cache
    // (the project's dependencies) as a property instead.
    env.insert("COURSIER_REPOSITORIES".to_string(), format!("ivy2Local|central|{parts_uri}"));
    // sbt runs offline against the user's installed sbt boot folder and Ivy home; its user
    // home and global base are the short, trace-owned home (sockets, global plugins).
    env.insert(
        "SBT_OPTS".to_string(),
        [
            "-Dsbt.offline=true".to_string(),
            // Batch runs (bloopInstall) only: no sbt server that could outlive the import.
            "-Dsbt.server.autostart=false".to_string(),
            format!("-Duser.home={}", path_text(&short_home)),
            format!("-Dsbt.boot.directory={}", path_text(&setup.sbt_boot)),
            format!("-Dsbt.ivy.home={}", path_text(&setup.ivy_home)),
            format!("-Dsbt.global.base={}", path_text(&short_home.join(".sbt").join("1.0"))),
            format!("-Dcoursier.cache={}", path_text(&setup.coursier_cache)),
        ]
        .join(" "),
    );
    // Metals adds sbt-bloop to the build (`project/metals.sbt`) and to every meta-build level
    // it exports (it writes two levels below the deepest one with .sbt files, so files in the
    // project copy chase it). A global plugin's resolvers are injected into every meta-build
    // level: the resolver lives in the trace-owned sbt global base instead.
    let sbt_global_plugin = (build == BuildSystem::Sbt).then(|| sbt_resolver(&plugin_uri));
    for (key, value) in metals_data_env(&short_home, cx.platform) {
        env.insert(key.to_string(), value);
    }
    // The Scala version the import builds: sbt imports the default `scalaVersion` only.
    let build_scala = (build == BuildSystem::Sbt)
        .then(|| project.scala_versions.first().cloned())
        .flatten();
    let argfile = metals_argfile(&short_home, &metals_cache, classpath, cx.platform);
    let mut status = Vec::new();
    status.extend(jdk_status(jdk.as_ref()));
    status.push(format!(
        "{} build (Bloop), Scala {}",
        build.tool(),
        if project.scala_versions.is_empty() {
            "version from the build".to_string()
        } else {
            project.scala_versions.join(", ")
        }
    ));
    let approved = if cx.settings.allow_build {
        "allowed"
    } else {
        "not allowed"
    };
    let parts = metals_dir.map(Parts::load).unwrap_or_default();
    let parts_text = serde_json::to_string(&parts).unwrap_or_default();
    let fingerprint = prepared_fingerprint(&[
        "scala-1",
        setup.fingerprint.as_str(),
        jdk_home.as_str(),
        build.tool(),
        approved,
        parts_text.as_str(),
    ]);
    Prepared {
        backend: cx.entry.id.clone(),
        languages: cx.languages.to_vec(),
        vars,
        json_vars,
        env,
        workspace: WorkspaceMode::Mirror,
        generated: Vec::new(),
        library_roots,
        toolchain: jdk,
        runs_project_code: true,
        pending_dirs: pending_dirs(setup),
        fingerprint,
        status,
        data: Some(Arc::new(ScalaData {
            build,
            argfile,
            short_home,
            sbt_global_plugin,
            build_scala,
        })),
    }
}

/// `.metals/readonly/dependencies/<jar>/<path>` (library sources Metals extracts) and
/// `.metals/readonly/src.zip/...` (JDK sources): external, readable source files.
fn metals_readonly_location(uri: &str) -> Option<ExternalLocation> {
    let decoded = percent_decode(uri).replace('\\', "/");
    let (_, rest) = decoded.split_once("/.metals/readonly/")?;
    let path = decoded
        .strip_prefix("file://")
        .map(|p| {
            // file:///C:/x -> C:/x ; file:///x -> /x
            let p = p.trim_start_matches('/');
            if p.as_bytes().get(1) == Some(&b':') {
                p.to_string()
            } else {
                format!("/{p}")
            }
        })
        .unwrap_or_else(|| decoded.clone());
    let jar = rest
        .strip_prefix("dependencies/")
        .and_then(|dep| dep.split('/').next())
        .unwrap_or("");
    let (package, version, stdlib) = if jar.is_empty() || jar == "src.zip" {
        // JDK sources (`src.zip`).
        ("jdk".to_string(), None, true)
    } else {
        let (package, version) = jar_library(&jar.replace("-sources.jar", ".jar"));
        let stdlib = is_language_stdlib(&package)
            || package == "scala-library"
            || package.starts_with("scala3-library");
        (package, version, stdlib)
    };
    Some(ExternalLocation {
        path,
        line: 0,
        column: 0,
        package,
        version,
        stdlib,
        readable: true,
        symbol: None,
    })
}

#[cfg(test)]
#[path = "../../../tests/unit/languages/scala/mod.rs"]
mod tests;
