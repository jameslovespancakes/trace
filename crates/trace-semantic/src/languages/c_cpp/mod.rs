//! C / C++ setup hooks (owner native): clangd with a real compile database.
//!
//! **Compile database** (`trace_env::cfamily::build_system`, always written to
//! `{outside}/cdb/compile_commands.json`, clangd `--compile-commands-dir={outside}/cdb`):
//! * an existing `compile_commands.json` (repository or `--env`): copied with the repository
//!   root rewritten to the workspace mirror;
//! * **CMake** (approval: configure runs the project's CMake scripts): `cmake [--preset <first
//!   visible configure preset>] -S <mirror>/<dir> -B {outside}/cbuild/<compiler id> -G Ninja |
//!   NMake Makefiles | MinGW Makefiles | Unix Makefiles -DCMAKE_EXPORT_COMPILE_COMMANDS=ON
//!   -DCMAKE_C_COMPILER=.. -DCMAKE_CXX_COMPILER=.. -DFETCHCONTENT_FULLY_DISCONNECTED=ON
//!   -DVCPKG_MANIFEST_INSTALL=OFF -DCPM_USE_LOCAL_PACKAGES=ON` (+ the vcpkg / Conan toolchain
//!   file when installed), MSVC inside the Visual Studio build environment built from the
//!   installation layout (`trace_env::cfamily::msvc_env`; no script runs); skipped when the
//!   stamp of the CMake inputs is unchanged;
//! * **Meson** (approval): `meson setup {outside}/cbuild/<id>-meson <mirror>/<dir>
//!   --wrap-mode=nodownload` (Ninja backend);
//! * **no build system** (plain Makefiles, autotools, loose files, Visual Studio projects on
//!   Windows): trace writes the database from syntax facts - per file `-x c` / `-x c++` /
//!   `-x c-header` / `-x c++-header` (header languages decided by `trace_syntax::header`) and
//!   the include directories the quoted `#include` facts resolve to. No approval (nothing of
//!   the project runs); a status note says so.
//!
//! Configure failures: libraries CMake / Meson / pkg-config could not find ->
//! `DepsMissing` (hint from the package manager the project declares, else the missing names);
//! anything else -> `BuildFailed` with the log. `.vcxproj`/`.sln`-only projects off Windows ->
//! `Unsupported`. No compiler -> `ToolchainMissing` (per-OS advice). `--query-driver` names the
//! detected gcc/clang (never MSVC); `VCToolsInstallDir` points clang's MSVC driver at the
//! standard library headers. Linux / Windows on aarch64 have no clangd release: a clangd of
//! the system is used (`server_executable`), else `ServerUnavailable` with advice.
//!
//! **Background index** (`--background-index`, shards persisted in `{outside}/cdb/.cache`):
//! without it clangd answers call targets from the documents it has open, so a one-file update
//! resolved a callee to its header prototype where a full index found the definition.
//! [`Hooks::warm_up`] waits for it after the first documents opened, bounded three ways: the
//! index ended (`backgroundIndexProgress` end); it stalled (no end and no new or grown shard
//! for [`INDEX_STALL`]: queries go on with the index as it is); the readiness limit
//! (`ready_timeout_secs`) -> `ServerTimeout`. Never an unbounded wait.
//!
//! **Outside the build**: a C/C++ source (never a header) that the compile database of a real
//! build (an existing `compile_commands.json`, CMake, Meson) does not list is not compiled on
//! this machine: [`Hooks::outside_build_file`] says so before any request (clangd would only
//! guess its flags). **Inactive regions**: clangd reports the `#if` branches the build does
//! not compile (`AnswerPolicy::inactive_regions`); calls there are `inactive_code`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::Value;
use trace_core::fingerprint::PartsHasher;
use trace_core::setup_error::SetupError;
use trace_core::Language;
use trace_env::cfamily::{BuildSystem, Compiler, CompilerKind};
use trace_env::os::{Os, Platform};
use trace_env::EcosystemId;

use crate::backends::fntype::FnTypeRoute;
use crate::languages::{
    default_prepared, AnswerPolicy, Prepared, Server, SetupContext, WarmUpContext, WorkspaceContext,
};
use crate::languages::{detect_context, read_context};
use crate::registry::{BuildSpec, BuildWhen, Recipe};
use crate::setup::{deps_error, require_approval, require_server, Collect};
use trace_env::lookup::compose_path;

mod database;
mod index;

use self::database::*;
use self::index::*;

pub struct Hooks;

/// Reason of a source file the compile database does not list.
pub const NOT_IN_DATABASE: &str = "not in the compile database";

/// The compile database's repository-relative sources, next to it in `{outside}/cdb`
/// (written by `prepare_workspace`, read by `outside_build_file`).
pub const SOURCES_FILE: &str = "trace-sources.json";

/// Backend-private data of a C/C++ preflight.
#[derive(Clone, Debug)]
pub struct CData {
    pub mode: BuildSystem,
    pub compiler: Compiler,
    /// The inspected repository root (existing databases are rebased from it).
    pub repo_root: PathBuf,
    pub cmake: Option<PathBuf>,
    pub ninja: Option<PathBuf>,
    pub meson: Option<PathBuf>,
    /// vcpkg / Conan CMake toolchain file.
    pub toolchain_file: Option<PathBuf>,
    pub vcpkg_installed: Option<PathBuf>,
    /// vcpkg.json / conanfile declared (hint of a configure failure).
    pub package_hint: Option<String>,
    pub empty_submodules: bool,
    pub platform: Platform,
    /// The backend's state directory `{outside}` (the compile database is in `cdb/`).
    pub state_dir: PathBuf,
    /// Bound of the background index wait (the entry's `ready_timeout_secs`).
    pub index_limit: Duration,
    /// Memo of the database's source list.
    pub sources: SourceMemo,
}

/// One memoised source list and the size / modification time of the file it was read from.
#[derive(Debug)]
struct MemoEntry {
    len: u64,
    modified: Option<SystemTime>,
    sources: Arc<BTreeSet<String>>,
}

/// Memo of the compile database's source list ([`SOURCES_FILE`]); read again when the file
/// changed (a new configure rewrites it).
#[derive(Clone, Debug, Default)]
pub struct SourceMemo(Arc<Mutex<Option<MemoEntry>>>);

impl SourceMemo {
    /// The source keys ([`source_key`]) listed in `file`; `None` when it cannot be read.
    pub fn load(&self, file: &Path) -> Option<Arc<BTreeSet<String>>> {
        let meta = std::fs::metadata(file).ok()?;
        let (len, modified) = (meta.len(), meta.modified().ok());
        let mut memo = self.0.lock().ok()?;
        if let Some(entry) = memo.as_ref().filter(|e| e.len == len && e.modified == modified) {
            return Some(Arc::clone(&entry.sources));
        }
        let list: Vec<String> = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
        let sources: Arc<BTreeSet<String>> = Arc::new(list.into_iter().collect());
        *memo = Some(MemoEntry {
            len,
            modified,
            sources: Arc::clone(&sources),
        });
        Some(sources)
    }
}

fn cdata(prepared: &Prepared) -> Option<&CData> {
    prepared.data.as_deref()?.downcast_ref::<CData>()
}

/// C++ when the backend analyses C++ files, else C (the language errors name).
fn project_language(languages: &[Language]) -> Language {
    if languages.contains(&Language::Cpp) {
        Language::Cpp
    } else {
        Language::C
    }
}

/// Whether `mode` compiles from a real build's database (not one trace generated from every
/// file).
fn has_database(mode: &BuildSystem) -> bool {
    matches!(mode, BuildSystem::CompileCommands(_) | BuildSystem::CMake(_) | BuildSystem::Meson(_))
}

/// A C/C++ translation unit by its extension (headers are compiled only through the sources
/// that include them, so they are never outside the build).
pub fn is_source_path(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let Some((_, ext)) = name.rsplit_once('.') else {
        return false;
    };
    matches!(ext.to_ascii_lowercase().as_str(), "c" | "cc" | "cpp" | "cxx" | "c++")
}

/// Comparable form of a repository-relative path (`/`-separated; ASCII case folded on Windows,
/// whose file names are case-insensitive).
pub fn source_key(path: &str) -> String {
    let text = path.replace('\\', "/");
    if cfg!(windows) {
        text.to_ascii_lowercase()
    } else {
        text
    }
}

impl Server for Hooks {
    fn preflight(&self, cx: &SetupContext<'_>) -> Result<Prepared, SetupError> {
        let language = project_language(cx.languages);
        let mut collect = Collect::default();
        // Compilers are searched through the execute context; build files, package trees and
        // compile databases are only read (read context, the repository included).
        let dcx = detect_context(cx, EcosystemId::CFamily);
        let readable = trace_core::paths::forbidden_roots();
        let read = read_context(cx, EcosystemId::CFamily, &readable);
        let mode = trace_env::cfamily::build_system(read.root, cx.files, read.env_override);

        // Platform: Visual Studio projects need Windows.
        if mode == BuildSystem::VisualStudioOnly && cx.platform.os != Os::Windows {
            return Err(SetupError::Unsupported {
                language,
                first: format!("This {} project builds only with Visual Studio.", language.display_name()),
                second: Some("Run trace on Windows to analyze it.".into()),
            });
        }

        // Server (+ a system clangd where no release exists).
        let mut server_executable = None;
        let has_artifact = match cx.entry.install.as_ref().map(|i| &i.recipe) {
            Some(Recipe::Archive { artifacts, .. }) => {
                crate::install::platform_select::artifact_for(artifacts, cx.platform).is_some()
            }
            _ => true,
        };
        if has_artifact {
            collect.check(require_server(cx));
        } else {
            match trace_env::cfamily::system_clangd(cx.vars, cx.platform) {
                Some(p) => server_executable = Some(p),
                None => collect.push(SetupError::ServerUnavailable {
                    language,
                    platform: cx.platform.display(),
                    advice: Some(
                        "Install clangd with your system package manager and run trace again.".into(),
                    ),
                }),
            }
        }

        // Toolchain: a compiler (always: clangd needs its standard headers) and the build tool.
        let compiler = trace_env::cfamily::compilers(cx.vars, cx.platform)
            .into_iter()
            .find(|c| dcx.allowed(&c.cc));
        if compiler.is_none() {
            collect.push(SetupError::ToolchainMissing {
                language,
                needs: "a C/C++ compiler".into(),
                install: compiler_advice(cx.platform.os).into(),
            });
        }
        let tool = |name: &str| trace_env::cfamily::build_tool(name, cx.vars, cx.platform, compiler.as_ref());
        let (mut cmake, mut ninja, mut meson) = (None, tool("ninja"), None);
        match &mode {
            BuildSystem::CMake(_) => {
                cmake = tool("cmake");
                if cmake.is_none() {
                    collect.push(SetupError::ToolchainMissing {
                        language,
                        needs: "CMake".into(),
                        install: "Install it from https://cmake.org/download".into(),
                    });
                }
            }
            BuildSystem::Meson(_) => {
                meson = tool("meson");
                if meson.is_none() {
                    collect.push(SetupError::ToolchainMissing {
                        language,
                        needs: "Meson".into(),
                        install: "Install it from https://mesonbuild.com".into(),
                    });
                }
                if ninja.is_none() {
                    collect.push(SetupError::ToolchainMissing {
                        language,
                        needs: "Ninja (Meson's build backend)".into(),
                        install: "Install it from https://ninja-build.org".into(),
                    });
                }
            }
            _ => ninja = None,
        }

        // Dependencies and approval (only a configure uses them).
        let configures = matches!(mode, BuildSystem::CMake(_) | BuildSystem::Meson(_));
        let toolchain = compiler
            .as_ref()
            .map(|c| trace_env::cfamily::compiler_toolchain(c, cx.vars, cx.platform));
        let deps = trace_env::cfamily::deps(&read, toolchain.as_ref());
        if configures {
            if let Some(e) = deps_error(language, &deps) {
                collect.push(e);
            }
            let build = BuildSpec {
                tool: if matches!(mode, BuildSystem::Meson(_)) {
                    "Meson"
                } else {
                    "CMake"
                }
                .into(),
                runs: "this project's build scripts".into(),
                when: BuildWhen::DecidedByHooks,
            };
            // Named after the project's language (a C++ project is not "C", which
            // `require_approval` would say: the backend's first language).
            if let Err(SetupError::BuildNotAllowed { tool, runs, .. }) = require_approval(cx, &build) {
                collect.push(SetupError::BuildNotAllowed { language, tool, runs });
            }
        }

        let Some(compiler) = compiler.filter(|_| collect.is_empty()) else {
            return collect.finish(default_prepared(cx));
        };
        let mut prepared = default_prepared(cx);
        let compiler_dir = compiler.cc.parent().map(Path::to_path_buf).unwrap_or_default();
        prepared
            .vars
            .insert("toolchain".into(), compiler_dir.display().to_string());
        let query_driver = if compiler.kind == CompilerKind::Msvc {
            String::new()
        } else {
            format!("{},{}", compiler.cc.display(), compiler.cxx.display())
        };
        prepared.vars.insert("query_driver".into(), query_driver);
        if let Some(p) = &server_executable {
            prepared
                .vars
                .insert("server_executable".into(), p.display().to_string());
        }
        if let Some(dir) = trace_env::cfamily::msvc_tools_dir(cx.vars, cx.platform) {
            prepared
                .env
                .insert("VCToolsInstallDir".into(), dir.display().to_string());
        }
        prepared
            .env
            .insert("PATH".into(), compose_path(&[compiler_dir], cx.vars, cx.platform));
        prepared.library_roots = deps.roots.clone();
        prepared.runs_project_code = configures;
        let mut fp = PartsHasher::new();
        fp.text(&format!("{mode:?}"))
            .text(&compiler.id())
            .text(&compiler.cc.display().to_string())
            .text(&deps.fingerprint);
        for name in ["CMakeLists.txt", "CMakePresets.json", "meson.build", "meson_options.txt"] {
            fp.text(&String::from_utf8_lossy(&std::fs::read(dcx.root.join(name)).unwrap_or_default()));
        }
        if let BuildSystem::CompileCommands(dir) = &mode {
            fp.text(&String::from_utf8_lossy(
                &std::fs::read(dir.join("compile_commands.json")).unwrap_or_default(),
            ));
        }
        prepared.fingerprint = fp.finish().hex_prefix(32);
        prepared.status.push(format!(
            "compiler {} {} ({})",
            compiler.kind.as_str(),
            compiler.version.as_ref().map(|v| v.text.as_str()).unwrap_or(""),
            compiler.cc.display()
        ));
        prepared.status.push(match &mode {
            BuildSystem::CompileCommands(dir) => format!("compile database: {}", dir.display()),
            BuildSystem::CMake(dir) => format!("compile database: CMake configure of {}", or_root(dir)),
            BuildSystem::Meson(dir) => format!("compile database: Meson setup of {}", or_root(dir)),
            BuildSystem::VisualStudioOnly | BuildSystem::None => {
                "compile database: generated by trace from the source files (no CMake or Meson build)".into()
            }
        });
        prepared.status.extend(deps.notes.iter().cloned());
        if let Some(p) = &server_executable {
            prepared.status.push(format!("clangd of the system: {}", p.display()));
        }
        let package_hint = if dcx.root.join("vcpkg.json").is_file() {
            Some("vcpkg install".to_string())
        } else if dcx.root.join("conanfile.txt").is_file() || dcx.root.join("conanfile.py").is_file() {
            Some("conan install . --build=missing".to_string())
        } else {
            None
        };
        let vcpkg_installed = trace_env::cfamily::vcpkg_installed(dcx.root, cx.vars);
        let toolchain_file = vcpkg_installed
            .as_ref()
            .and_then(|_| trace_env::cfamily::vcpkg_toolchain_file(cx.vars))
            .or_else(|| trace_env::cfamily::conan_toolchain_file(dcx.root));
        prepared.data = Some(Arc::new(CData {
            mode,
            compiler,
            repo_root: dcx.root.to_path_buf(),
            cmake,
            ninja,
            meson,
            toolchain_file,
            vcpkg_installed,
            package_hint,
            empty_submodules: !trace_env::cfamily::empty_submodules(dcx.root).is_empty(),
            platform: cx.platform.clone(),
            // The workspace's state directory (`Snapshot::outside_dir` of this backend).
            state_dir: cx
                .repo
                .workspaces_dir
                .join(crate::snapshot::sanitize(&cx.entry.id))
                .join("state"),
            index_limit: Duration::from_secs(cx.entry.ready_timeout_secs().max(1)),
            sources: SourceMemo::default(),
        }));
        prepared.toolchain = toolchain;
        Ok(prepared)
    }

    fn prepare_workspace(&self, cx: &WorkspaceContext<'_>) -> Result<(), SetupError> {
        let Some(data) = cx.prepared.data.as_deref().and_then(|d| d.downcast_ref::<CData>()) else {
            return Ok(());
        };
        let language = cx
            .prepared
            .languages
            .iter()
            .copied()
            .find(|l| *l == Language::Cpp)
            .unwrap_or(Language::C);
        let failed = |what: String| SetupError::BuildFailed {
            language,
            what,
            log: cx.log.to_path_buf(),
        };
        let cdb_dir = cx.outside.join("cdb");
        std::fs::create_dir_all(&cdb_dir)
            .map_err(|e| failed(format!("creating {} failed: {e}", cdb_dir.display())))?;
        let target = cdb_dir.join("compile_commands.json");
        let database: Value = match &data.mode {
            BuildSystem::CompileCommands(dir) => {
                let text = std::fs::read_to_string(dir.join("compile_commands.json"))
                    .map_err(|e| failed(format!("reading compile_commands.json failed: {e}")))?;
                let value: Value = serde_json::from_str(&text)
                    .map_err(|e| failed(format!("compile_commands.json is not valid JSON: {e}")))?;
                rebase_database(&value, &data.repo_root, cx.workspace)
            }
            BuildSystem::CMake(dir) => {
                let build = cmake_build_dir(cx.outside, &data.compiler);
                configure_cmake(cx, data, &cx.workspace.join(dir), &build, language)?;
                read_database(&build).map_err(failed)?
            }
            BuildSystem::Meson(dir) => {
                let build = cx
                    .outside
                    .join("cbuild")
                    .join(format!("{}-meson", data.compiler.id()));
                configure_meson(cx, data, &cx.workspace.join(dir), &build, language)?;
                read_database(&build).map_err(failed)?
            }
            BuildSystem::VisualStudioOnly | BuildSystem::None => {
                generated_database(cx.files, cx.workspace, &data.compiler)
            }
        };
        let bytes = serde_json::to_vec_pretty(&database).unwrap_or_default();
        std::fs::write(&target, bytes)
            .map_err(|e| failed(format!("writing the compile database failed: {e}")))?;
        // The sources a real build compiles (`outside_build_file`); a generated database
        // lists every file, so it needs no list.
        let list = cdb_dir.join(SOURCES_FILE);
        if has_database(&data.mode) {
            let sources: Vec<String> = database_sources(&database, cx.workspace).into_iter().collect();
            let bytes = serde_json::to_vec(&sources).unwrap_or_default();
            std::fs::write(&list, bytes).map_err(|e| failed(format!("writing {SOURCES_FILE} failed: {e}")))
        } else {
            let _ = std::fs::remove_file(&list);
            Ok(())
        }
    }

    /// Wait for clangd's background index (module docs): its end, a stall, or the limit.
    fn warm_up(&self, cx: &mut WarmUpContext<'_, '_>) -> Result<(), SetupError> {
        let Some(limit) = cdata(cx.prepared).map(|d| d.index_limit) else {
            return Ok(());
        };
        let language = project_language(&cx.prepared.languages);
        let shards = cx
            .snapshot
            .outside_dir()
            .join("cdb")
            .join(".cache")
            .join("clangd")
            .join("index");
        let wait = IndexWait {
            grace: INDEX_GRACE,
            stall: INDEX_STALL,
            limit,
        };
        wait_background_index(&mut *cx.client, &shards, &wait, language)
    }

    /// A C/C++ source the compile database of a real build does not list (module docs).
    fn outside_build_file(&self, path: &str, prepared: &Prepared) -> Option<String> {
        let data = cdata(prepared)?;
        if !has_database(&data.mode) || !is_source_path(path) {
            return None;
        }
        let sources = data.sources.load(&data.state_dir.join("cdb").join(SOURCES_FILE))?;
        (!sources.contains(&source_key(path))).then(|| NOT_IN_DATABASE.to_string())
    }

    fn answer_policy(&self) -> AnswerPolicy {
        AnswerPolicy {
            inactive_regions: true,
            ..AnswerPolicy::default()
        }
    }

    fn fn_type_route(&self, _language: Language) -> FnTypeRoute {
        FnTypeRoute::Declaration
    }
}

fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")
}

fn or_root(dir: &str) -> &str {
    if dir.is_empty() {
        "the repository root"
    } else {
        dir
    }
}

/// Per-OS advice for a missing C/C++ compiler.
fn compiler_advice(os: Os) -> &'static str {
    match os {
        Os::Windows => "Install the Visual Studio Build Tools from https://visualstudio.microsoft.com/visual-cpp-build-tools/",
        Os::Linux => "Install gcc or clang with your package manager",
        Os::MacOs => "Install the Xcode Command Line Tools (xcode-select --install)",
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/languages/c_cpp/mod.rs"]
mod tests;
