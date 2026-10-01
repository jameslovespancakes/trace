//! Rust setup hooks (owner native): rust-analyzer loads the Cargo workspace with build
//! scripts and procedural macros under the build approval.
//!
//! **Preflight** (collects every independent failure, PLAN decision 15):
//! 1. toolchain (`trace_env::rust::resolve`): a pinned toolchain that is not installed, no
//!    toolchain, or a `rust-version` above the toolchain; then the standard library source
//!    (`rustup component add rust-src`), Cargo, and the C linker build scripts and proc macros
//!    link with (MSVC for `*-windows-msvc`, MinGW gcc for `*-windows-gnu`, `cc`, the Command
//!    Line Tools);
//! 2. server: rust-analyzer installed, plus the standard library's own crates in the Cargo home
//!    (rust-analyzer loads the sysroot as a Cargo workspace; without them `Option`/`Some` do not
//!    resolve) - an install extra of `trace status --install rust` ([`Hooks::install_extras`]);
//! 3. dependencies: the registry packages of `Cargo.lock` the host needs (static);
//! 4. approval: a Cargo project runs build scripts and procedural macros ->
//!    `trace index --allow-build`.
//!
//! **Prepared**: `CARGO` / `RUSTC` absolute, `RUSTUP_TOOLCHAIN` (never the rustup default),
//! `CARGO_HOME` / `RUSTUP_HOME` of the user, `RUST_SRC_PATH`, PATH = toolchain + linker + the
//! user's PATH; initialization options through `{json:ra_build}` (build scripts + proc
//! macros), `{json:ra_metadata_args}` (`--locked` only when every project has a lockfile),
//! `{json:ra_linked_projects}` (the required Cargo manifests; loose `.rs` files without any
//! manifest), `{toolchain}` = sysroot. Nested non-member Cargo projects are pending
//! sub-projects.
//!
//! **prepare_workspace** (approved): the authoritative `cargo metadata --offline [--locked]
//! --filter-platform <host>` per project in the mirror; unresolvable dependencies ->
//! `DepsMissing("cargo fetch")`, anything else -> `BuildFailed` with the log.
//! **check_loaded**: rust-analyzer's final `experimental/serverStatus` health
//! (`rust_analyzer::classify_status`) -> `BuildFailed` / `DepsMissing` / `ServerMissing`.
//! **file_expansions**: attribute procedural macros of crates that depend on a proc-macro
//! crate are expanded (`rust-analyzer/expandMacro`) under the approval (DESIGN §1.15).
//!
//! The module also holds small helpers shared by the native setups (rust, go, c_cpp): the
//! detection context, PATH composition, logged build steps with a timeout and the
//! file URI of a server location.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use trace_core::fingerprint::PartsHasher;
use trace_core::model::Diagnostic;
use trace_core::semantics::ExpandedMacro;
use trace_core::setup_error::{InstallFailure, SetupError};
use trace_core::Language;
use trace_env::os::{EnvVars, Platform};
use trace_env::{DetectContext, EcosystemId, ToolchainStatus};

use super::{
    build_step_timeout, default_prepared, detect_context, first_line, read_context, run_step, toolchain_spec,
    InstallExtra, InstallExtraContext, LoadedContext, Prepared, Server, SetupContext, Step, WorkspaceContext,
};
use crate::backend::SemanticFile;
use crate::backends::fntype::{FnTypeRoute, FnTypeSession};
use crate::backends::rust_analyzer::{self, LoadProblem};
use crate::registry::{BuildSpec, BuildWhen};
use crate::setup::{deps_error, require_approval, require_server, toolchain_error, Collect};

pub struct Hooks;

/// Install extra: the standard library's own crates (`cargo fetch` of the sysroot workspace).
pub const STD_DEPS_EXTRA: &str = "rust-std-deps";

/// Loose `.rs` files linked as standalone files at most (no Cargo manifest anywhere).
const MAX_LOOSE_FILES: usize = 200;

/// Backend-private data of a Rust preflight (`Prepared::data`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RustData {
    /// Build scripts and proc macros run (approval given, Cargo projects present).
    pub expand: bool,
    /// Package directories (relative) that depend on a proc-macro crate.
    pub proc_macro_dirs: Vec<String>,
    /// Required Cargo manifests (relative).
    pub manifests: Vec<String>,
    /// Every project has a Cargo.lock (`--locked`).
    pub locked: bool,
    /// Host triple (`--filter-platform`).
    pub host: String,
}

impl Server for Hooks {
    fn preflight(&self, cx: &SetupContext<'_>) -> Result<Prepared, SetupError> {
        let language = Language::Rust;
        let mut collect = Collect::default();
        // Toolchains are searched through the execute context; the Cargo home, lockfiles and
        // installed crates are only read (read context, the repository included).
        let dcx = detect_context(cx, EcosystemId::Rust);
        let readable = trace_core::paths::forbidden_roots();
        let read = read_context(cx, EcosystemId::Rust, &readable);
        let layout = trace_env::rust::cargo_layout(dcx.root, cx.files);
        let spec = toolchain_spec(cx, "rust", "Rust", "Install it from https://rustup.rs");
        let resolution = trace_env::rust::resolve(&dcx);
        let toolchain = match &resolution.status {
            ToolchainStatus::Found(t) => Some(t.clone()),
            ToolchainStatus::TooOld {
                found,
                needed,
                source,
            } => {
                collect.push(SetupError::ToolchainVersion {
                    language,
                    needs: needed.describe("Rust"),
                    source: source.clone(),
                    tool: "Rust".into(),
                    found: found
                        .version
                        .as_ref()
                        .map(|v| v.text.clone())
                        .unwrap_or_else(|| "unknown".into()),
                    install: "Update it with rustup update".into(),
                });
                None
            }
            status => {
                match &resolution.pinned_missing {
                    Some((channel, source)) => collect.push(SetupError::ToolchainMissing {
                        language,
                        needs: format!("the Rust toolchain {channel} ({source})"),
                        install: format!("Install it: rustup toolchain install {channel}"),
                    }),
                    None => {
                        if let Some(e) = toolchain_error(language, &spec, status) {
                            collect.push(e);
                        }
                    }
                }
                None
            }
        };
        let cargo_home = trace_env::rust::cargo_home(&read);
        let needs_build = !layout.projects.is_empty();
        if let Some(t) = &toolchain {
            if !t.facts.contains_key("rust_src") {
                let install = match t.facts.get("rustup_toolchain") {
                    Some(name) => format!("Install it: rustup component add rust-src --toolchain {name}"),
                    None => "Install it: rustup component add rust-src".to_string(),
                };
                collect.push(SetupError::ToolchainMissing {
                    language,
                    needs: "Rust's standard library source".into(),
                    install,
                });
            }
            if !t.executables.contains_key("cargo") {
                collect.push(SetupError::ToolchainMissing {
                    language,
                    needs: "Cargo".into(),
                    install: "Install it with your Rust toolchain (https://rustup.rs)".into(),
                });
            }
            if needs_build {
                let host = t.facts.get("host").cloned().unwrap_or_default();
                if trace_env::cfamily::rust_linker_dir(&host, cx.vars, cx.platform).is_none() {
                    let (needs, install) = linker_advice(&host);
                    collect.push(SetupError::ToolchainMissing {
                        language,
                        needs: needs.into(),
                        install: install.into(),
                    });
                } else if host.ends_with("-windows-msvc") && msvc_build_env(true, &host, cx, "").is_none() {
                    collect.push(SetupError::ToolchainMissing {
                        language,
                        needs: "the Windows SDK (to link build scripts)".into(),
                        install: "Install it with the Microsoft C++ Build Tools (https://visualstudio.microsoft.com/visual-cpp-build-tools)".into(),
                    });
                }
            }
        }
        collect.check(require_server(cx));
        if let Some(t) = &toolchain {
            if trace_env::rust::std_deps_missing(t, cargo_home.as_deref()).is_some_and(|m| !m.is_empty()) {
                collect.push(SetupError::ServerMissing { language });
            }
        }
        let deps = trace_env::rust::deps(&read, toolchain.as_ref());
        if let Some(e) = deps_error(language, &deps) {
            collect.push(e);
        }
        if needs_build {
            let build = cx.entry.requires_build.clone().unwrap_or(BuildSpec {
                tool: "Cargo".into(),
                runs: "this project's build scripts and procedural macros".into(),
                when: BuildWhen::BuildFiles,
            });
            collect.check(require_approval(cx, &build));
        }
        let Some(toolchain) = toolchain.filter(|_| collect.is_empty()) else {
            return collect.finish(default_prepared(cx));
        };

        let mut prepared = default_prepared(cx);
        let sysroot = toolchain.root.display().to_string();
        let host = toolchain
            .facts
            .get("host")
            .cloned()
            .unwrap_or_else(|| trace_env::rust::host_triple(cx.platform));
        prepared.vars.insert("toolchain".into(), sysroot.clone());
        let locked = layout.projects.iter().all(|p| p.lock);
        let linked: Vec<Value> = if needs_build {
            layout
                .projects
                .iter()
                .map(|p| Value::String(p.manifest.clone()))
                .collect()
        } else {
            cx.files
                .iter()
                .filter(|(_, l)| *l == Language::Rust)
                .take(MAX_LOOSE_FILES)
                .map(|(p, _)| Value::String(p.to_string()))
                .collect()
        };
        prepared.json_vars.insert("ra_build".into(), Value::Bool(needs_build));
        prepared.json_vars.insert(
            "ra_metadata_args".into(),
            if needs_build && locked {
                json!(["--locked"])
            } else {
                json!([])
            },
        );
        prepared
            .json_vars
            .insert("ra_linked_projects".into(), Value::Array(linked));

        let mut path_dirs = vec![toolchain.root.join("bin")];
        if needs_build {
            path_dirs.extend(trace_env::cfamily::rust_linker_dir(&host, cx.vars, cx.platform));
        }
        let path = trace_env::lookup::compose_path(&path_dirs, cx.vars, cx.platform);
        // MSVC hosts: build scripts and proc macros link with link.exe, which needs the Visual
        // Studio build environment (LIB/LIBPATH/INCLUDE of the toolset + Windows SDK), built
        // from the installation layout (no script runs).
        match msvc_build_env(needs_build, &host, cx, &path) {
            Some(vars) => prepared.env.extend(vars),
            None => {
                prepared.env.insert("PATH".into(), path);
            }
        }
        for (key, exe) in [("CARGO", "cargo"), ("RUSTC", "rustc")] {
            if let Some(p) = toolchain.executables.get(exe) {
                prepared.env.insert(key.into(), p.display().to_string());
            }
        }
        for (key, fact) in [
            ("RUSTUP_TOOLCHAIN", "rustup_toolchain"),
            ("RUSTUP_HOME", "rustup_home"),
            ("CARGO_HOME", "cargo_home"),
            ("RUST_SRC_PATH", "rust_src"),
        ] {
            if let Some(v) = toolchain.facts.get(fact) {
                prepared.env.insert(key.into(), v.clone());
            }
        }
        prepared.library_roots = deps.roots.clone();
        prepared.runs_project_code = needs_build;
        for sub in &layout.subprojects {
            prepared.pending_dirs.insert(sub.dir.clone(), sub.reason.clone());
        }
        let proc_macro_dirs = proc_macro_users(&layout, read.root, cargo_home.as_deref());
        let mut fp = PartsHasher::new();
        fp.text(&sysroot)
            .text(&toolchain.version.as_ref().map(|v| v.text.clone()).unwrap_or_default())
            .text(&deps.fingerprint)
            .text(if needs_build { "build" } else { "no-build" })
            .text(if locked { "locked" } else { "unlocked" });
        for p in &layout.projects {
            fp.text(&p.manifest);
        }
        prepared.fingerprint = fp.finish().hex_prefix(32);
        prepared.status.push(format!(
            "toolchain Rust {} ({})",
            toolchain
                .version
                .as_ref()
                .map(|v| v.text.as_str())
                .unwrap_or("unknown"),
            sysroot
        ));
        if !needs_build {
            prepared
                .status
                .push("no Cargo.toml: files are analysed as standalone Rust files".into());
        }
        prepared.status.extend(deps.notes.iter().cloned());
        prepared.data = Some(Arc::new(RustData {
            expand: needs_build,
            proc_macro_dirs,
            manifests: layout.projects.iter().map(|p| p.manifest.clone()).collect(),
            locked,
            host,
        }));
        prepared.toolchain = Some(toolchain);
        Ok(prepared)
    }

    fn prepare_workspace(&self, cx: &WorkspaceContext<'_>) -> Result<(), SetupError> {
        let language = Language::Rust;
        let Some(data) = rust_data(cx.prepared) else {
            return Ok(());
        };
        if !data.expand {
            return Ok(());
        }
        let Some(cargo) = cx.prepared.env.get("CARGO").map(PathBuf::from) else {
            return Ok(());
        };
        let target = cx.outside.join("target");
        let mut set: Vec<(&str, String)> =
            cx.prepared.env.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        set.push(("CARGO_NET_OFFLINE", "true".into()));
        set.push(("CARGO_TARGET_DIR", target.display().to_string()));
        set.push(("RUSTUP_AUTO_INSTALL", "0".into()));
        set.push(("CARGO_TERM_COLOR", "never".into()));
        let env = crate::tools::clean_env(&[], &set);
        for manifest in &data.manifests {
            let mut args = vec![
                "metadata".to_string(),
                "--format-version".into(),
                "1".into(),
                "--offline".into(),
                "--filter-platform".into(),
                data.host.clone(),
                "--manifest-path".into(),
                cx.workspace.join(manifest).display().to_string(),
            ];
            if data.locked {
                args.push("--locked".into());
            }
            let step = Step {
                program: &cargo,
                args,
                cwd: cx.workspace,
                env: env.clone(),
                timeout: build_step_timeout(),
                quiet_stdout: true,
            };
            let failed = |what: &str| SetupError::BuildFailed {
                language,
                what: what.to_string(),
                log: cx.log.to_path_buf(),
            };
            let outcome =
                run_step(&step, cx.log).map_err(|e| failed(&format!("cargo could not start: {e}")))?;
            if outcome.timed_out {
                return Err(failed("cargo metadata did not finish in 10 minutes"));
            }
            if !outcome.success {
                return Err(cargo_metadata_error(&outcome.output, cx.log));
            }
        }
        Ok(())
    }

    fn check_loaded(&self, cx: &LoadedContext<'_>) -> Result<Vec<Diagnostic>, SetupError> {
        let language = Language::Rust;
        let Some(status) = rust_analyzer::last_status(cx.notifications) else {
            return Ok(Vec::new());
        };
        let log = cx.log.to_path_buf();
        match rust_analyzer::classify_status(&status) {
            None if status.health != "ok" => Ok(vec![Diagnostic::new(
                "rust_analyzer_warning",
                None,
                status.message.unwrap_or_default(),
            )]),
            None => Ok(Vec::new()),
            Some(LoadProblem::BuildScripts) => Err(SetupError::BuildFailed {
                language,
                what: "build scripts of some packages failed".into(),
                log,
            }),
            Some(LoadProblem::ProcMacros) => Err(SetupError::BuildFailed {
                language,
                what: "procedural macros could not run".into(),
                log,
            }),
            Some(LoadProblem::Dependencies) => Err(SetupError::DepsMissing {
                language,
                hint: trace_env::rust::DEPS_HINT.into(),
            }),
            Some(LoadProblem::SysrootDependencies) => Err(SetupError::ServerMissing { language }),
            Some(LoadProblem::Workspace(message)) => Err(SetupError::BuildFailed {
                language,
                what: format!("the Cargo workspace did not load: {}", first_line(&message)),
                log,
            }),
        }
    }

    fn fn_type_route(&self, _language: Language) -> FnTypeRoute {
        FnTypeRoute::Declaration
    }

    fn install_extras(&self, repo_root: Option<&Path>) -> Vec<InstallExtra> {
        let vars = EnvVars::from_process();
        let platform = Platform::current();
        let temp = std::env::temp_dir();
        let root = repo_root.unwrap_or(temp.as_path());
        let files: [(&str, Language); 0] = [];
        // The repository's remembered `--env` toolchain (the one the preflight will use).
        let settings = repo_root
            .and_then(|r| trace_core::paths::RepoPaths::resolve(r).ok())
            .and_then(|p| trace_core::repo_settings::RepoSettings::load(&p).ok())
            .unwrap_or_default();
        let dcx = DetectContext {
            root,
            platform: &platform,
            vars: &vars,
            env_override: settings.env_for("rust"),
            forbidden: &[],
            files: &files,
        };
        let ToolchainStatus::Found(t) = trace_env::rust::resolve(&dcx).status else {
            return Vec::new();
        };
        let home = trace_env::rust::cargo_home(&dcx);
        let missing = trace_env::rust::std_deps_missing(&t, home.as_deref()).unwrap_or_default();
        if missing.is_empty() {
            return Vec::new();
        }
        let (Some(cargo), Some(library)) = (t.executables.get("cargo"), t.facts.get("rust_src")) else {
            return Vec::new();
        };
        // One extra per toolchain version: another toolchain has its own std crates.
        let id = match &t.version {
            Some(v) => format!("{STD_DEPS_EXTRA}-{}", v.text),
            None => STD_DEPS_EXTRA.to_string(),
        };
        vec![InstallExtra {
            id,
            coordinates: vec![
                cargo.display().to_string(),
                PathBuf::from(library).join("Cargo.toml").display().to_string(),
                t.facts.get("host").cloned().unwrap_or_default(),
                t.facts.get("rustup_toolchain").cloned().unwrap_or_default(),
                home.map(|h| h.display().to_string()).unwrap_or_default(),
            ],
            reason: format!(
                "the standard library's own crates ({} missing) that rust-analyzer loads with the sysroot",
                missing.len()
            ),
        }]
    }

    fn run_install_extra(
        &self,
        extra: &InstallExtra,
        cx: &InstallExtraContext<'_>,
    ) -> Result<(), SetupError> {
        if !extra.id.starts_with(STD_DEPS_EXTRA) {
            return Ok(());
        }
        let failed = || SetupError::Install {
            language: Some(Language::Rust),
            failure: InstallFailure::Failed {
                what: "Rust language server".into(),
                log: cx.log.to_path_buf(),
            },
        };
        let [cargo, manifest, host, toolchain, home] = extra.coordinates.as_slice() else {
            return Err(failed());
        };
        let mut set: Vec<(&str, String)> = vec![
            ("RUSTC_BOOTSTRAP", "1".into()),
            ("RUSTUP_AUTO_INSTALL", "0".into()),
            ("CARGO_TERM_COLOR", "never".into()),
        ];
        if !toolchain.is_empty() {
            set.push(("RUSTUP_TOOLCHAIN", toolchain.clone()));
        }
        if !home.is_empty() {
            set.push(("CARGO_HOME", home.clone()));
        }
        let env = crate::tools::clean_env(
            &[
                "RUSTUP_HOME",
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "NO_PROXY",
                "SSL_CERT_FILE",
                "CARGO_HTTP_CAINFO",
            ],
            &set,
        );
        let mut args = vec![
            "fetch".to_string(),
            "--locked".into(),
            "--manifest-path".into(),
            manifest.clone(),
        ];
        if !host.is_empty() {
            args.push("--target".into());
            args.push(host.clone());
        }
        let temp = std::env::temp_dir();
        let step = Step {
            program: Path::new(cargo),
            args,
            cwd: &temp,
            env,
            timeout: build_step_timeout(),
            quiet_stdout: false,
        };
        match run_step(&step, cx.log) {
            Ok(o) if o.success => Ok(()),
            _ => Err(failed()),
        }
    }

    fn file_expansions(
        &self,
        file: &SemanticFile<'_>,
        prepared: &Prepared,
        session: &mut dyn FnTypeSession,
        budget: &AtomicU32,
    ) -> (Vec<ExpandedMacro>, Option<Diagnostic>) {
        expansions_for(file, prepared, session, budget)
    }
}

/// Attribute-macro expansions of `file` (module docs; DESIGN §1.15), bounded by `budget`.
pub fn expansions_for(
    file: &SemanticFile<'_>,
    prepared: &Prepared,
    session: &mut dyn FnTypeSession,
    budget: &AtomicU32,
) -> (Vec<ExpandedMacro>, Option<Diagnostic>) {
    let mut out = Vec::new();
    let Some(data) = rust_data(prepared) else {
        return (out, None);
    };
    let path = file.path.replace('\\', "/");
    let in_scope = data
        .proc_macro_dirs
        .iter()
        .any(|d| d.is_empty() || path == *d || path.starts_with(&format!("{d}/")));
    if !data.expand || file.language != Language::Rust || !in_scope {
        return (out, None);
    }
    let targets = rust_analyzer::expansion_targets(file.source);
    if targets.is_empty() {
        return (out, None);
    }
    let Ok(uri) = session.uri_of(file.path) else {
        return (out, None);
    };
    // Every target within the run's budget is asked, in one pipelined batch.
    let mut asked = Vec::with_capacity(targets.len());
    let mut diagnostic = None;
    for target in targets {
        if budget
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |b| b.checked_sub(1))
            .is_err()
        {
            diagnostic = Some(Diagnostic::new(
                "bounded",
                Some(file.path.to_string()),
                format!(
                    "macro expansion stopped after {} expansions in this run",
                    rust_analyzer::MAX_EXPANSIONS_PER_RUN
                ),
            ));
            break;
        }
        asked.push(target);
    }
    let calls = asked
        .iter()
        .map(|target| (rust_analyzer::EXPAND_MACRO.to_string(), rust_analyzer::expand_params(&uri, target)))
        .collect();
    if let Ok(answers) = session.request_many(calls) {
        for (target, answer) in asked.iter().zip(answers) {
            if let Some(text) = answer.ok().as_ref().and_then(rust_analyzer::parse_expansion) {
                out.push(ExpandedMacro {
                    span: target.span,
                    text,
                });
            }
        }
    }
    (out, diagnostic)
}

fn rust_data(prepared: &Prepared) -> Option<&RustData> {
    prepared.data.as_deref()?.downcast_ref::<RustData>()
}

/// Package directories of the required members that depend (directly) on a proc-macro crate:
/// a proc-macro package of the repository, or a registry crate whose manifest in the Cargo
/// home declares `[lib] proc-macro = true` (read from Cargo metadata files; no crate names).
fn proc_macro_users(
    layout: &trace_env::rust::CargoLayout,
    root: &Path,
    cargo_home: Option<&Path>,
) -> Vec<String> {
    let mut proc_macro: BTreeSet<String> = layout
        .packages
        .iter()
        .filter(|p| p.proc_macro)
        .map(|p| p.name.clone())
        .collect();
    let mut versions: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for project in &layout.projects {
        let lock = root.join(&project.dir).join("Cargo.lock");
        if let Ok(text) = std::fs::read_to_string(lock) {
            for p in trace_env::cargo_lock_packages(&text) {
                if p.registry {
                    versions.entry(p.name).or_default().push(p.version);
                }
            }
        }
    }
    let indexes: Vec<PathBuf> = cargo_home
        .map(|h| h.join("registry").join("src"))
        .and_then(|src| std::fs::read_dir(src).ok())
        .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    let required = layout.required_packages();
    let wanted: BTreeSet<&str> = required
        .iter()
        .flat_map(|p| p.dependencies.iter().map(String::as_str))
        .collect();
    for name in wanted {
        let Some(vs) = versions.get(name) else { continue };
        let is_proc_macro = vs.iter().any(|v| {
            indexes.iter().any(|index| {
                std::fs::read_to_string(index.join(format!("{name}-{v}")).join("Cargo.toml"))
                    .ok()
                    .and_then(|t| trace_core::formats::toml_value(&t))
                    .and_then(|m| {
                        let lib = m.get("lib")?;
                        lib.get("proc-macro").or_else(|| lib.get("proc_macro"))?.as_bool()
                    })
                    .unwrap_or(false)
            })
        });
        if is_proc_macro {
            proc_macro.insert(name.to_string());
        }
    }
    let mut dirs: Vec<String> = required
        .iter()
        .filter(|p| p.dependencies.iter().any(|d| proc_macro.contains(d)))
        .map(|p| p.dir.clone())
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

/// `cargo metadata` failure (tool output) -> the setup error.
fn cargo_metadata_error(output: &str, log: &Path) -> SetupError {
    let lower = output.to_ascii_lowercase();
    let offline = [
        "attempting to make an http request, but --offline",
        "failed to select a version",
        "failed to download",
        "no matching package",
        "failed to load source for dependency",
        "failed to get `",
    ]
    .iter()
    .any(|p| lower.contains(p));
    if offline {
        return SetupError::DepsMissing {
            language: Language::Rust,
            hint: trace_env::rust::DEPS_HINT.into(),
        };
    }
    let what = if lower.contains("needs to be updated but --locked")
        || (lower.contains("lock file") && lower.contains("--locked"))
    {
        "Cargo.lock is out of date (run cargo update)".to_string()
    } else if lower.contains("is not supported by the following packages") {
        "a dependency needs a newer Rust toolchain".to_string()
    } else {
        "cargo could not read the workspace".to_string()
    };
    SetupError::BuildFailed {
        language: Language::Rust,
        what,
        log: log.to_path_buf(),
    }
}

/// The Visual Studio build environment for linking on an MSVC host (`None` when no build runs,
/// the host is not MSVC, or the toolset / Windows SDK is incomplete). `path` goes after the
/// toolset directories on PATH.
fn msvc_build_env(
    needs_build: bool,
    host: &str,
    cx: &SetupContext<'_>,
    path: &str,
) -> Option<Vec<(String, String)>> {
    if !needs_build || !host.ends_with("-windows-msvc") {
        return None;
    }
    let compiler = trace_env::cfamily::compilers(cx.vars, cx.platform)
        .into_iter()
        .find(|c| c.kind == trace_env::cfamily::CompilerKind::Msvc)?;
    trace_env::cfamily::msvc_env(&compiler, cx.vars, cx.platform, path)
}

/// (needs, install) of the missing C linker for a host triple.
fn linker_advice(host: &str) -> (&'static str, &'static str) {
    if host.ends_with("-windows-msvc") {
        (
            "a C linker (the Microsoft C++ Build Tools)",
            "Install the Build Tools from https://visualstudio.microsoft.com/visual-cpp-build-tools/",
        )
    } else if host.contains("-windows-") {
        (
            "a C linker (MinGW-w64 gcc)",
            "Install MinGW-w64, for example with MSYS2 from https://www.msys2.org,",
        )
    } else if host.contains("-apple-") {
        ("a C linker (the Xcode Command Line Tools)", "Install them with xcode-select --install")
    } else {
        ("a C linker (cc)", "Install gcc or clang with your package manager")
    }
}

#[cfg(test)]
#[path = "../../tests/unit/languages/rust.rs"]
mod tests;
