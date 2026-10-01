//! Haskell setup hooks (owner haskell; DESIGN §4.9).
//!
//! haskell-language-server comes with the toolchain (PLAN decision 4): the binary built for
//! exactly the project's GHC (`haskell-language-server-<ghc>`), found in trace's tools folder
//! (`trace status --install haskell` runs `ghcup install hls`), GHCup or PATH, and launched
//! directly (`--lsp`; never the wrapper, which would run the cradle a second time).
//!
//! **Preflight** (every independent failure collected):
//! 1. toolchain (`trace_env::haskell`): GHC (the version `with-compiler:` / `stack.yaml` / a
//!    pin asks for), cabal (or Stack for Stack projects);
//! 2. server: HLS for that GHC; a GHC no HLS release supports is an error naming the
//!    supported ones;
//! 3. dependencies: the `plan.json` closure in the cabal store (or Stack's install dir), the
//!    Hackage package list;
//! 4. approval: cabal configures packages (`Setup.hs`, configure scripts, preprocessors) and
//!    GHC runs Template Haskell splices -> `trace index --allow-build` (loose files only when
//!    they use Template Haskell / quasi-quotes).
//!
//! **Prepared**: `server_executable` = the HLS binary; `hie.yaml` (cabal / stack / direct
//! cradle, unless the project has its own cabal or stack cradle) and `cabal.project.local`
//! with `offline: True` generated into the workspace (never the user's project); ghcide and
//! hie-bios caches under `{outside}/hls`; PATH = GHC, cabal, GHCup (+ MSYS2 on Windows).
//! **prepare_workspace**: without a usable `plan.json`, plans the build with
//! `cabal build all --dry-run --offline --builddir={outside}/plan` (approved) and checks the
//! plan's closure against the store. **check_loaded**: `Cabal-7125` (offline refusal) ->
//! dependency error; a missing package list -> `cabal update` error; a failed cradle ->
//! build error (on Windows: the POSIX-only `unix` package -> run on Linux/macOS).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use trace_core::fingerprint::PartsHasher;
use trace_core::model::Diagnostic;
use trace_core::setup_error::SetupError;
use trace_core::Language;
use trace_env::haskell::{self, version_text, BuildTool, DEPS_HINT_CABAL, DEPS_HINT_STACK};
use trace_env::os::{Os, Platform};
use trace_env::EcosystemId;

use super::{build_step_timeout, detect_context, run_step, toolchain_spec, Step};
use super::{
    default_prepared, ExternalLocation, LoadedContext, Prepared, Server, SetupContext, WorkspaceContext,
};
use crate::backends::fntype::FnTypeRoute;
use crate::registry::{BuildSpec, BuildWhen, Recipe};
use crate::setup::{deps_error, require_approval, Collect, STATUS_DEPENDENCIES, STATUS_TOOLCHAIN};
use trace_env::lookup::compose_path;

pub struct Hooks;

/// Install id of HLS in the tools folder.
pub const INSTALL_ID: &str = "haskell-language-server";

/// After the first documents are opened: how long HLS may take to begin its cradle / typecheck
/// progress, and how long all progress must stay ended (its tokens follow each other with
/// gaps of seconds on a busy machine: the next cradle load begins after the typecheck).
const LOAD_GRACE: Duration = Duration::from_secs(10);
const LOAD_SETTLE: Duration = Duration::from_secs(10);

/// The pinned HLS release (GHCup "recommended").
pub const PINNED_HLS: &str = "2.14.0.0";

/// GHCup's recommended GHC, suggested when the project's GHC is not supported.
pub const RECOMMENDED_GHC: &str = "9.10.3";

/// The GHC versions the pinned HLS release supports (also in the registry recipe).
pub const SUPPORTED_GHC: [&str; 6] = ["9.6.7", "9.8.4", "9.10.3", "9.12.2", "9.12.4", "9.14.1"];

const GHCUP_INSTALL: &str = "Install it with GHCup from https://www.haskell.org/ghcup/";

/// Typed data for the workspace hooks.
#[derive(Clone, Debug, Default)]
pub struct HaskellData {
    /// The served project (relative, `""` = repository root).
    pub project: String,
    /// "cabal" | "stack" | "direct".
    pub tool: &'static str,
    /// No usable plan.json: plan the build in the workspace (approved step).
    pub plan_needed: bool,
    pub store_dirs: Vec<PathBuf>,
    pub cabal: Option<PathBuf>,
    /// PATH of build steps (GHC, cabal, GHCup, MSYS2, the user's PATH).
    pub path: String,
    pub cabal_dir: Option<PathBuf>,
    /// ghcide / hie-bios caches ([`short_cache_dir`]).
    pub cache_dir: PathBuf,
}

/// A short per-repository cache directory (`<cache home>/hb/<repository key>`) for the
/// language server's build caches (Windows path limit, see the preflight).
fn short_cache_dir(repo: &trace_core::paths::RepoPaths) -> PathBuf {
    repo.home.join("hb").join(&repo.key)
}

/// The server error for a GHC without an installed HLS: not supported by any pinned HLS
/// release, or supported but not installed.
pub fn server_error(ghc: &str, supported: &[String]) -> SetupError {
    let language = Language::Haskell;
    if supported.iter().any(|s| s == ghc) {
        return SetupError::ToolchainMissing {
            language,
            needs: format!("the Haskell language server for GHC {ghc}"),
            install: "Install it: ghcup install hls".into(),
        };
    }
    let list = match supported {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    };
    let suggest = if supported.iter().any(|s| s == RECOMMENDED_GHC) {
        RECOMMENDED_GHC.to_string()
    } else {
        supported.last().cloned().unwrap_or_default()
    };
    SetupError::Unsupported {
        language,
        first: format!("The Haskell language server does not support GHC {ghc}."),
        second: Some(format!("Use GHC {list} (ghcup install ghc {suggest}) and run trace again.")),
    }
}

/// The package-list error (`cabal update` never runs on the user's behalf).
pub fn package_list_error() -> SetupError {
    SetupError::Unsupported {
        language: Language::Haskell,
        first: "The Haskell package list is missing. Run cabal update and run trace again.".into(),
        second: None,
    }
}

/// The generated workspace files: `hie.yaml` (unless the project keeps its own cabal / stack
/// cradle) and, for cabal, `cabal.project.local` = the project's own + `offline: True`.
pub fn generated_files(
    project_dir: &str,
    tool: BuildTool,
    own_cradle: bool,
    existing_local: Option<&str>,
) -> Vec<(String, Vec<u8>)> {
    let at = |name: &str| {
        if project_dir.is_empty() {
            name.to_string()
        } else {
            format!("{project_dir}/{name}")
        }
    };
    let mut out = Vec::new();
    if !own_cradle {
        let cradle = match tool {
            BuildTool::Cabal => "cradle:\n  cabal:\n",
            BuildTool::Stack => "cradle:\n  stack:\n",
            BuildTool::Direct => "cradle:\n  direct:\n    arguments: []\n",
        };
        out.push((at("hie.yaml"), cradle.as_bytes().to_vec()));
    }
    if tool == BuildTool::Cabal {
        let mut text = String::new();
        if let Some(existing) = existing_local {
            // The project's own settings stay; a field `offline:` of its own is replaced.
            let mut skipping = false;
            for line in existing.lines() {
                let top_level = !line.starts_with(' ') && !line.starts_with('\t');
                if top_level {
                    skipping = line
                        .split_once(':')
                        .is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case("offline"));
                }
                if !skipping {
                    text.push_str(line);
                    text.push('\n');
                }
            }
        }
        text.push_str("-- written by trace: never download while analysing\noffline: True\n");
        out.push((at("cabal.project.local"), text.into_bytes()));
    }
    out
}

/// The `unix` package named in a cabal / GHC error text (POSIX-only; no Windows build).
fn mentions_unix_package(text: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.'))
        .any(|w| {
            w == "unix"
                || w.strip_prefix("unix-")
                    .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        })
}

/// Map cabal / hie-bios failure texts to setup errors (`None`: no failure).
fn failure_from_texts(
    texts: &[String],
    cradle_failed: bool,
    tool: &str,
    os: Os,
    log: &Path,
) -> Option<SetupError> {
    let language = Language::Haskell;
    let hint = if tool == "stack" {
        DEPS_HINT_STACK
    } else {
        DEPS_HINT_CABAL
    };
    // `Cabal-7125` alone is any failed build step ("Failed to build X"); only the offline
    // refusal means a package that is not installed.
    if texts
        .iter()
        .any(|t| t.to_ascii_lowercase().contains("refusing to download"))
    {
        return Some(SetupError::DepsMissing {
            language,
            hint: hint.into(),
        });
    }
    if texts.iter().any(|t| {
        let l = t.to_ascii_lowercase();
        l.contains("package list for") && l.contains("does not exist")
    }) {
        return Some(package_list_error());
    }
    if !cradle_failed {
        return None;
    }
    if os == Os::Windows && texts.iter().any(|t| mentions_unix_package(t)) {
        return Some(SetupError::Unsupported {
            language,
            first: "This Haskell project could not be built on Windows (it uses the unix package).".into(),
            second: Some("Run trace on Linux or macOS to analyze it.".into()),
        });
    }
    Some(SetupError::BuildFailed {
        language,
        what: format!("{tool} could not load the project"),
        log: log.to_path_buf(),
    })
}

/// A cradle / cabal load failure message.
fn is_cradle_failure(text: &str) -> bool {
    let l = text.to_ascii_lowercase();
    l.contains("failed to load cradle")
        || l.contains("failed to parse result of calling cabal")
        || l.contains("failed to parse result of calling stack")
        || l.contains("cradle failure")
        || l.contains("cradleerror")
        || (text.contains("Cabal-7125") && l.contains("failed to build"))
}

fn supported_ghc(cx: &SetupContext<'_>) -> (Vec<String>, String) {
    match cx.entry.install.as_ref().map(|i| &i.recipe) {
        Some(Recipe::Ghcup {
            hls_version,
            supported_ghc,
        }) => (supported_ghc.clone(), hls_version.clone()),
        _ => (SUPPORTED_GHC.iter().map(|s| s.to_string()).collect(), PINNED_HLS.to_string()),
    }
}

impl Server for Hooks {
    fn preflight(&self, cx: &SetupContext<'_>) -> Result<Prepared, SetupError> {
        let language = Language::Haskell;
        let mut collect = Collect::default();
        let dcx = detect_context(cx, EcosystemId::Haskell);
        let spec = toolchain_spec(
            cx,
            "haskell",
            "GHC (with cabal)",
            "Install them with GHCup from https://www.haskell.org/ghcup/",
        );
        let setup = haskell::detect(&dcx);
        let project = setup.project.clone();
        let tool = project.as_ref().map_or(BuildTool::Direct, |p| p.tool);
        // 1. toolchain.
        if project.as_ref().is_some_and(|p| p.package_yaml_only) && setup.stack.is_none() {
            collect.push(SetupError::ToolchainMissing {
                language,
                needs: "Stack (the project has only a package.yaml)".into(),
                install: GHCUP_INSTALL.into(),
            });
        }
        match (&setup.wanted, &setup.ghc) {
            (Some((v, source)), None) => {
                let text = version_text(v);
                collect.push(SetupError::ToolchainMissing {
                    language,
                    needs: format!("GHC {text} ({source})"),
                    install: format!("Install it: ghcup install ghc {text}"),
                });
            }
            (None, None) => collect.push(SetupError::ToolchainMissing {
                language,
                needs: spec.needs.clone(),
                install: spec.install.clone(),
            }),
            _ => {}
        }
        let ghc_found = setup.ghc.is_some();
        match tool {
            BuildTool::Cabal if setup.cabal.is_none() && (ghc_found || setup.wanted.is_some()) => {
                collect.push(SetupError::ToolchainMissing {
                    language,
                    needs: "cabal".into(),
                    install: GHCUP_INSTALL.into(),
                });
            }
            BuildTool::Stack if setup.stack.is_none() => collect.push(SetupError::ToolchainMissing {
                language,
                needs: "Stack".into(),
                install: GHCUP_INSTALL.into(),
            }),
            _ => {}
        }
        // 2. server: HLS for exactly this GHC.
        let (supported, pinned) = supported_ghc(cx);
        let install_id = cx.entry.install.as_ref().map(|i| i.id.as_str()).unwrap_or(INSTALL_ID);
        let ghc_text = setup.ghc_version.as_ref().map(version_text);
        let mut hls: Option<PathBuf> = None;
        if let (Some(ghc), true) = (&ghc_text, ghc_found) {
            let tools_dirs: Vec<PathBuf> = cx.tools.tool_dir(install_id).into_iter().collect();
            let dirs = haskell::hls_dirs(setup.ghcup.as_ref(), &pinned, &tools_dirs, cx.vars);
            hls = haskell::find_hls(&dirs, ghc, cx.platform).filter(|p| dcx.allowed(p));
            if hls.is_none() {
                collect.push(server_error(ghc, &supported));
            }
        }
        // 3. dependencies.
        let hdeps = haskell::haskell_deps(&dcx, &setup);
        if let Some(e) = deps_error(language, &hdeps.report) {
            collect.push(e);
        }
        if tool == BuildTool::Cabal && hdeps.index_missing {
            collect.push(package_list_error());
        }
        // 4. approval.
        let mut build = cx.entry.requires_build.clone().unwrap_or(BuildSpec {
            tool: "cabal".into(),
            runs: "this project's Setup scripts and Template Haskell code".into(),
            when: BuildWhen::DecidedByHooks,
        });
        let needs_approval = match tool {
            BuildTool::Cabal => project.is_some(),
            BuildTool::Stack => {
                build.tool = "Stack".into();
                true
            }
            BuildTool::Direct => {
                build.tool = "GHC".into();
                build.runs = "this project's Template Haskell code".into();
                cx.files
                    .iter()
                    .filter(|(_, l)| *l == Language::Haskell)
                    .take(5000)
                    .any(|(path, _)| {
                        let file = cx.repo.root.join(path);
                        std::fs::metadata(&file).is_ok_and(|m| m.len() <= 4 * 1024 * 1024)
                            && std::fs::read_to_string(&file)
                                .is_ok_and(|s| haskell::uses_template_haskell(&s))
                    })
            }
        };
        if needs_approval {
            collect.check(require_approval(cx, &build));
        }
        let toolchain = haskell::haskell_toolchain(&setup);
        let (Some(toolchain), Some(hls), Some(ghc_text)) =
            (toolchain.filter(|_| collect.is_empty()), hls, ghc_text)
        else {
            return collect.finish(default_prepared(cx));
        };

        let mut prepared = default_prepared(cx);
        let project = project.unwrap_or(haskell::HaskellProject {
            dir: String::new(),
            tool,
            own_cradle: false,
            package_yaml_only: false,
            packages: Vec::new(),
        });
        prepared
            .vars
            .insert("server_executable".into(), hls.display().to_string());
        prepared
            .vars
            .insert("toolchain".into(), toolchain.root.display().to_string());
        prepared.vars.insert("toolchain:ghc".into(), ghc_text.clone());
        let mut first: Vec<PathBuf> = Vec::new();
        for exe in [setup.ghc.as_ref(), setup.cabal.as_ref(), setup.stack.as_ref()]
            .into_iter()
            .flatten()
        {
            if let Some(parent) = exe.parent() {
                first.push(parent.to_path_buf());
            }
        }
        if let Some(g) = &setup.ghcup {
            first.push(g.bin.clone());
        }
        first.extend(setup.msys_dirs.iter().cloned());
        let path = compose_path(&first, cx.vars, cx.platform);
        prepared.env.insert("PATH".into(), path.clone());
        // hie-bios builds each project in `dist-<dir>-<hash>/build/<arch>/<ghc>/<package>/
        // package.conf.inplace/...`: under the deep workspace state dir that passes Windows'
        // 260-character path limit of GHC's tools (ghc-pkg: "openBinaryTempFileWithDefault
        // Permissions: invalid argument", the cradle fails). Both caches live in a short
        // per-repository directory of trace's cache home instead.
        let cache_dir = short_cache_dir(cx.repo);
        prepared
            .env
            .insert("HIE_BIOS_CACHE_DIR".into(), cache_dir.join("hie-bios").display().to_string());
        if !setup.cabal_xdg {
            // ghcide + hie-bios caches in trace's cache (cabal's own directories are not
            // under XDG_CACHE_HOME in this layout).
            prepared
                .env
                .insert("XDG_CACHE_HOME".into(), cache_dir.display().to_string());
            if let Some(d) = &setup.cabal_dir {
                prepared.env.insert("CABAL_DIR".into(), d.display().to_string());
            }
        }
        let own_local =
            std::fs::read_to_string(cx.repo.root.join(&project.dir).join("cabal.project.local")).ok();
        prepared.generated = generated_files(&project.dir, tool, project.own_cradle, own_local.as_deref());
        prepared.library_roots = hdeps.report.roots.clone();
        prepared.runs_project_code = needs_approval;
        for sub in &setup.pending {
            prepared.pending_dirs.insert(sub.dir.clone(), sub.reason.clone());
        }
        let hls_version = haskell::hls_version_of(&hls).unwrap_or_else(|| pinned.clone());
        let mut fp = PartsHasher::new();
        fp.text(&toolchain.root.display().to_string())
            .text(&ghc_text)
            .text(&hls.display().to_string())
            .text(&hls_version)
            .text(tool.as_str())
            .text(&project.dir)
            .text(&hdeps.report.fingerprint)
            .text(if needs_approval { "build" } else { "no-build" });
        for (name, bytes) in &prepared.generated {
            fp.text(name).part(bytes);
        }
        prepared.fingerprint = fp.finish().hex_prefix(32);
        let build_tool = match tool {
            BuildTool::Cabal => setup
                .cabal
                .as_ref()
                .map(|c| format!(", cabal ({})", c.display()))
                .unwrap_or_default(),
            BuildTool::Stack => setup
                .stack
                .as_ref()
                .map(|s| format!(", Stack ({})", s.display()))
                .unwrap_or_default(),
            BuildTool::Direct => String::new(),
        };
        prepared
            .status
            .push(format!("{STATUS_TOOLCHAIN}GHC {ghc_text} ({}){build_tool}", toolchain.root.display()));
        prepared.status.push(format!(
            "server: haskell-language-server {hls_version} for GHC {ghc_text} ({})",
            hls.display()
        ));
        let deps_line = match tool {
            BuildTool::Direct => "none declared".to_string(),
            BuildTool::Stack => "installed (Stack)".to_string(),
            BuildTool::Cabal if hdeps.plan_needed => {
                "checked against the cabal plan when trace indexes".to_string()
            }
            BuildTool::Cabal => format!("installed ({} store packages)", hdeps.store_units),
        };
        prepared.status.push(format!("{STATUS_DEPENDENCIES}{deps_line}"));
        prepared.status.extend(hdeps.report.notes.iter().cloned());
        prepared.data = Some(Arc::new(HaskellData {
            project: project.dir.clone(),
            tool: tool.as_str(),
            plan_needed: tool == BuildTool::Cabal && hdeps.plan_needed,
            store_dirs: hdeps.store_dirs.clone(),
            cabal: setup.cabal.clone(),
            path,
            cabal_dir: setup.cabal_dir.clone().filter(|_| !setup.cabal_xdg),
            cache_dir,
        }));
        prepared.toolchain = Some(toolchain);
        Ok(prepared)
    }

    fn prepare_workspace(&self, cx: &WorkspaceContext<'_>) -> Result<(), SetupError> {
        let language = Language::Haskell;
        let Some(data) = cx
            .prepared
            .data
            .as_ref()
            .and_then(|d| d.downcast_ref::<HaskellData>())
        else {
            return Ok(());
        };
        for dir in ["hls", "tmp"] {
            let _ = std::fs::create_dir_all(cx.outside.join(dir));
        }
        let _ = std::fs::create_dir_all(data.cache_dir.join("hie-bios"));
        let (true, Some(cabal)) = (data.plan_needed, &data.cabal) else {
            return Ok(());
        };
        // Plan the build (solver only, offline) into trace's state dir.
        let builddir = cx.outside.join("plan");
        let tmp = cx.outside.join("tmp");
        let mut set: Vec<(&str, String)> = vec![
            ("PATH", data.path.clone()),
            ("TEMP", tmp.display().to_string()),
            ("TMP", tmp.display().to_string()),
            ("TMPDIR", tmp.display().to_string()),
        ];
        if let Some(d) = &data.cabal_dir {
            set.push(("CABAL_DIR", d.display().to_string()));
        }
        let env = crate::tools::clean_env(
            &[
                "CABAL_DIR",
                "CABAL_CONFIG",
                "GHCUP_INSTALL_BASE_PREFIX",
                "GHCUP_USE_XDG_DIRS",
                "XDG_CONFIG_HOME",
                "XDG_DATA_HOME",
                "XDG_STATE_HOME",
                "XDG_CACHE_HOME",
            ],
            &set,
        );
        let cwd = cx.workspace.join(&data.project);
        let step = Step {
            program: cabal,
            args: vec![
                "build".into(),
                "all".into(),
                "--dry-run".into(),
                "--offline".into(),
                format!("--builddir={}", builddir.display()),
            ],
            cwd: &cwd,
            env,
            timeout: build_step_timeout(),
            quiet_stdout: false,
        };
        let outcome = run_step(&step, cx.log).map_err(|e| SetupError::BuildFailed {
            language,
            what: format!("cabal could not plan the build: {e}"),
            log: cx.log.to_path_buf(),
        })?;
        if !outcome.success {
            let texts = vec![outcome.output];
            return Err(failure_from_texts(&texts, true, "cabal", Platform::current().os, cx.log).unwrap_or(
                SetupError::BuildFailed {
                    language,
                    what: "cabal could not plan the build".into(),
                    log: cx.log.to_path_buf(),
                },
            ));
        }
        let plan = std::fs::read_to_string(builddir.join("cache").join("plan.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .ok_or_else(|| SetupError::BuildFailed {
                language,
                what: "cabal wrote no build plan".into(),
                log: cx.log.to_path_buf(),
            })?;
        let (missing, _) = haskell::plan_missing(&plan, &data.store_dirs);
        if !missing.is_empty() {
            return Err(SetupError::DepsMissing {
                language,
                hint: DEPS_HINT_CABAL.into(),
            });
        }
        Ok(())
    }

    /// HLS loads the cabal cradle and typechecks only after `didOpen` (nothing runs during the
    /// readiness wait), one cradle call per not-yet-known file: without this wait the first
    /// requests queue behind the whole load and time out. The load's failures (cradle, missing
    /// packages) are checked like `check_loaded`.
    fn warm_up(&self, cx: &mut super::WarmUpContext<'_, '_>) -> Result<(), SetupError> {
        let language = Language::Haskell;
        let log = cx.client.log_path().to_path_buf();
        cx.client
            .wait_progress_after_open(LOAD_GRACE, LOAD_SETTLE)
            .map_err(|e| match e {
                crate::SemanticError::Setup(setup) => setup,
                _ => SetupError::ServerCrashed {
                    language,
                    log: log.clone(),
                },
            })?;
        let log_messages = cx.client.log_messages().to_vec();
        let notifications = cx.client.notifications().to_vec();
        let diagnostics = cx.client.diagnostics().to_vec();
        self.check_loaded(&LoadedContext {
            prepared: cx.prepared,
            log_messages: &log_messages,
            notifications: &notifications,
            diagnostics: &diagnostics,
            log: &log,
        })
        .map(|_| ())
    }

    fn check_loaded(&self, cx: &LoadedContext<'_>) -> Result<Vec<Diagnostic>, SetupError> {
        let tool = cx
            .prepared
            .data
            .as_ref()
            .and_then(|d| d.downcast_ref::<HaskellData>())
            .map_or("cabal", |d| d.tool);
        let mut texts: Vec<String> = cx.log_messages.iter().map(|(_, t)| t.clone()).collect();
        let mut cradle_failed = cx
            .log_messages
            .iter()
            .any(|(kind, t)| *kind == 1 && is_cradle_failure(t));
        for (_, params) in cx.diagnostics {
            for d in params
                .get("diagnostics")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let message = d.get("message").and_then(Value::as_str).unwrap_or_default();
                let severity = d.get("severity").and_then(Value::as_u64).unwrap_or(1);
                if severity == 1 && is_cradle_failure(message) {
                    cradle_failed = true;
                }
                texts.push(message.to_string());
            }
        }
        match failure_from_texts(&texts, cradle_failed, tool, Platform::current().os, cx.log) {
            Some(e) => {
                // The server's own words next to its log (the error names only the fix).
                let evidence: Vec<&str> = texts
                    .iter()
                    .map(String::as_str)
                    .filter(|t| {
                        let l = t.to_ascii_lowercase();
                        t.contains("Cabal-7125")
                            || l.contains("failed to build")
                            || l.contains("refusing to download")
                            || l.contains("package list for")
                            || is_cradle_failure(t)
                    })
                    .take(5)
                    .collect();
                if !evidence.is_empty() {
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(cx.log) {
                        let _ =
                            writeln!(f, "\ntrace: the Haskell load failed:\n{}", evidence.join("\n---\n"));
                    }
                }
                Err(e)
            }
            None => Ok(Vec::new()),
        }
    }

    fn external_location(&self, uri: &str, prepared: &Prepared) -> Option<ExternalLocation> {
        let path = crate::lsp::uri_to_path(uri).ok()?;
        for root in &prepared.library_roots {
            let Ok(rest) = path.strip_prefix(&root.path) else { continue };
            // `<store>/<compiler>/<name>-<version>-<hash>/...`
            let unit = rest.components().next()?.as_os_str().to_string_lossy().into_owned();
            let mut parts: Vec<&str> = unit.split('-').collect();
            if parts.len() >= 3 {
                parts.pop();
            }
            let version = parts
                .last()
                .filter(|v| v.starts_with(|c: char| c.is_ascii_digit()))
                .map(|v| v.to_string());
            if version.is_some() {
                parts.pop();
            }
            return Some(ExternalLocation {
                path: path.display().to_string(),
                line: 0,
                column: 0,
                package: parts.join("-"),
                version,
                stdlib: false,
                readable: path.is_file(),
                symbol: None,
            });
        }
        None
    }

    fn fn_type_route(&self, _language: Language) -> FnTypeRoute {
        FnTypeRoute::Declaration
    }
}

#[cfg(test)]
#[path = "../../tests/unit/languages/haskell.rs"]
mod tests;
