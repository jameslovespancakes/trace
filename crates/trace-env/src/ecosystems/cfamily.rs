//! C / C++: compilers, build tools (CMake, Ninja, Meson), the project's build system and its
//! declared package managers. Read-only directory listings; a compiler binary
//! is asked for `--version` only when no directory names its version.
//!
//! * **Compilers** ([`compilers`], preference order): Windows - MSVC (Visual Studio
//!   installations under Program Files and the installer's instance `state.json` files, newest
//!   `VC/Tools/MSVC/<ver>` with `cl.exe`), LLVM (`<Program Files>/LLVM/bin`), MinGW gcc (PATH,
//!   MSYS2 `ucrt64`/`mingw64`/`clang64`, WinLibs); Linux - `cc`/`c++`, gcc, clang on PATH,
//!   `/usr/bin`, `/usr/lib/llvm-NN/bin`; macOS - Command Line Tools, Xcode, Homebrew.
//! * **Build system** ([`build_system`]): `--env` compile database -> an existing
//!   `compile_commands.json` (root, `build/`, `out/`, `cmake-build-*/`) -> CMake -> Meson ->
//!   Visual Studio projects only -> none (trace generates the database from syntax facts).
//! * **Dependencies** ([`deps`]): `vcpkg.json` needs an installed tree (`vcpkg_installed/` or
//!   `$VCPKG_ROOT/installed`), a Conan file needs `conan_toolchain.cmake`; empty git submodule
//!   directories are status notes. Libraries CMake / Meson cannot find show up at configure
//!   time (`trace_semantic` `languages/c_cpp/`).
//!
//! Moved from `trace-semantic/src/generic.rs` (Foundation): [`msvc_tools_dir`] / `newest_msvc`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use trace_core::Language;

use crate::lookup;
use crate::os::{self, EnvVars, Os, Platform, Version};
use crate::relpath;
use crate::{
    subdirs, DepsReport, DepsStatus, DetectContext, EcosystemId, LibraryKind, LibraryRoot, Origin, Toolchain,
    ToolchainStatus,
};

// ---------------------------------------------------------------------------------------------
// Compilers
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerKind {
    Msvc,
    Gcc,
    Clang,
}

impl CompilerKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            CompilerKind::Msvc => "msvc",
            CompilerKind::Gcc => "gcc",
            CompilerKind::Clang => "clang",
        }
    }
}

/// One installed C/C++ compiler pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compiler {
    pub kind: CompilerKind,
    pub cc: PathBuf,
    pub cxx: PathBuf,
    pub version: Option<Version>,
    /// MSVC: the Visual Studio installation (the directory holding `VC/Tools/MSVC`).
    pub vs_install: Option<PathBuf>,
}

impl Compiler {
    /// Build-directory id: `msvc-14.44.35207`, `gcc-15.2.0`, `clang` (no version known).
    pub fn id(&self) -> String {
        match &self.version {
            Some(v) => format!("{}-{}", self.kind.as_str(), v.text),
            None => self.kind.as_str().to_string(),
        }
    }
}

/// Installed compilers, preferred first (module docs).
pub fn compilers(vars: &EnvVars, p: &Platform) -> Vec<Compiler> {
    let mut out: Vec<Compiler> = Vec::new();
    let path_dirs = os::path_dirs(vars);
    match p.os {
        Os::Windows => {
            out.extend(msvc_compilers(vars, p));
            for pf in program_files(vars) {
                let bin = pf.join("LLVM").join("bin");
                out.extend(pair(&bin, "clang", "clang++", CompilerKind::Clang, p));
            }
            let mut gcc_dirs = path_dirs.clone();
            gcc_dirs.extend(
                ["ucrt64", "mingw64", "clang64"]
                    .iter()
                    .map(|d| PathBuf::from(r"C:\msys64").join(d).join("bin")),
            );
            if let Some(local) = vars.path("LOCALAPPDATA") {
                let packages = local.join("Microsoft").join("WinGet").join("Packages");
                for (name, dir) in subdirs(&packages) {
                    if name.starts_with("BrechtSanders.WinLibs") {
                        gcc_dirs.push(dir.join("mingw64").join("bin"));
                    }
                }
            }
            for dir in gcc_dirs {
                out.extend(pair(&dir, "gcc", "g++", CompilerKind::Gcc, p));
            }
        }
        Os::Linux => {
            let mut dirs = path_dirs.clone();
            dirs.push(PathBuf::from("/usr/bin"));
            for dir in &dirs {
                out.extend(pair(dir, "cc", "c++", CompilerKind::Gcc, p));
                out.extend(pair(dir, "gcc", "g++", CompilerKind::Gcc, p));
                out.extend(pair(dir, "clang", "clang++", CompilerKind::Clang, p));
            }
            for (_, dir) in os::versioned_children(Path::new("/usr/lib"), "llvm-") {
                out.extend(pair(&dir.join("bin"), "clang", "clang++", CompilerKind::Clang, p));
            }
        }
        Os::MacOs => {
            let clt = PathBuf::from("/Library/Developer/CommandLineTools/usr/bin");
            let xcode = PathBuf::from(
                "/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin",
            );
            for dir in [
                clt,
                xcode,
                PathBuf::from("/opt/homebrew/opt/llvm/bin"),
                PathBuf::from("/usr/local/opt/llvm/bin"),
            ] {
                out.extend(pair(&dir, "clang", "clang++", CompilerKind::Clang, p));
            }
            for dir in path_dirs {
                out.extend(pair(&dir, "gcc", "g++", CompilerKind::Gcc, p));
            }
        }
    }
    // Deduplicate by the C compiler's real path.
    let mut seen: Vec<PathBuf> = Vec::new();
    out.retain(|c| {
        let real = std::fs::canonicalize(&c.cc).unwrap_or_else(|_| c.cc.clone());
        if seen.contains(&real) {
            false
        } else {
            seen.push(real);
            true
        }
    });
    out
}

/// `<dir>/<cc>` + `<dir>/<cxx>` when both exist; `cc` resolved to gcc or clang by its target.
fn pair(dir: &Path, cc: &str, cxx: &str, kind: CompilerKind, p: &Platform) -> Option<Compiler> {
    let cc_path = os::find_executable(&[cc], &[dir.to_path_buf()], p)?;
    let cxx_path = os::find_executable(&[cxx], &[dir.to_path_buf()], p)?;
    let real = std::fs::canonicalize(&cc_path).unwrap_or_else(|_| cc_path.clone());
    let real_name = real
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let kind = if real_name.contains("clang") {
        CompilerKind::Clang
    } else {
        kind
    };
    let version = version_from_dirs(dir, kind).or_else(|| {
        let out = os::toolchain_output(&cc_path, &["--version"])?;
        let first = out.lines().next()?;
        first
            .split_whitespace()
            .filter(|w| w.starts_with(|c: char| c.is_ascii_digit()) && w.contains('.'))
            .find_map(Version::parse)
    });
    Some(Compiler {
        kind,
        cc: cc_path,
        cxx: cxx_path,
        version,
        vs_install: None,
    })
}

/// gcc: `<bin>/../lib/gcc/<triple>/<version>`; clang: `<bin>/../lib/clang/<major>`.
fn version_from_dirs(bin: &Path, kind: CompilerKind) -> Option<Version> {
    let lib = bin.parent()?.join("lib");
    match kind {
        CompilerKind::Gcc => subdirs(&lib.join("gcc"))
            .into_iter()
            .flat_map(|(_, triple)| os::versioned_children(&triple, ""))
            .map(|(v, _)| v)
            .max(),
        CompilerKind::Clang => os::versioned_children(&lib.join("clang"), "")
            .into_iter()
            .map(|(v, _)| v)
            .next(),
        CompilerKind::Msvc => None,
    }
}

fn program_files(vars: &EnvVars) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|k| vars.path(k))
        .collect();
    if roots.is_empty() {
        roots.push(PathBuf::from(r"C:\Program Files"));
        roots.push(PathBuf::from(r"C:\Program Files (x86)"));
    }
    roots
}

/// Visual Studio installations: `<Program Files>/Microsoft Visual Studio/<year>/<edition>` and
/// the installer's `<ProgramData>/Microsoft/VisualStudio/Packages/_Instances/*/state.json`.
pub(crate) fn vs_installations(vars: &EnvVars) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for pf in program_files(vars) {
        for (_, year) in subdirs(&pf.join("Microsoft Visual Studio")) {
            for (_, edition) in subdirs(&year) {
                if edition.join("VC").join("Tools").join("MSVC").is_dir() && !out.contains(&edition) {
                    out.push(edition);
                }
            }
        }
    }
    let program_data = vars
        .path("ProgramData")
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    let instances = program_data
        .join("Microsoft")
        .join("VisualStudio")
        .join("Packages")
        .join("_Instances");
    for (_, dir) in subdirs(&instances) {
        let Ok(text) = std::fs::read_to_string(dir.join("state.json")) else {
            continue;
        };
        let Some(path) = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("installationPath")?.as_str().map(PathBuf::from))
        else {
            continue;
        };
        if path.join("VC").join("Tools").join("MSVC").is_dir() && !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// MSVC compilers (newest toolset per installation with a `cl.exe` for this host).
fn msvc_compilers(vars: &EnvVars, p: &Platform) -> Vec<Compiler> {
    let (host, target) = if p.arch == os::Arch::Aarch64 {
        ("Hostarm64", "arm64")
    } else {
        ("Hostx64", "x64")
    };
    let mut out = Vec::new();
    for vs in vs_installations(vars) {
        let tools = vs.join("VC").join("Tools").join("MSVC");
        let default_version = std::fs::read_to_string(
            vs.join("VC")
                .join("Auxiliary")
                .join("Build")
                .join("Microsoft.VCToolsVersion.default.txt"),
        )
        .ok()
        .map(|t| t.trim().to_string());
        let mut versions = os::versioned_children(&tools, "");
        if let Some(d) = &default_version {
            versions.sort_by_key(|(v, _)| v.text != *d);
        }
        for (version, dir) in versions {
            let cl = dir.join("bin").join(host).join(target).join("cl.exe");
            if cl.is_file() {
                out.push(Compiler {
                    kind: CompilerKind::Msvc,
                    cc: cl.clone(),
                    cxx: cl,
                    version: Some(version),
                    vs_install: Some(vs.clone()),
                });
                break;
            }
        }
    }
    out
}

/// A build tool (`cmake`, `ninja`, `meson`): PATH, then the copies bundled with Visual Studio
/// (CMake, Ninja) and the standard install locations.
pub fn build_tool(name: &str, vars: &EnvVars, p: &Platform, compiler: Option<&Compiler>) -> Option<PathBuf> {
    let mut names = vec![name];
    if name == "ninja" {
        names.push("ninja-build");
    }
    if let Some(found) = lookup::on_path(&names, vars, p) {
        return Some(found);
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(vs) = compiler.and_then(|c| c.vs_install.as_ref()) {
        let base = vs
            .join("Common7")
            .join("IDE")
            .join("CommonExtensions")
            .join("Microsoft")
            .join("CMake");
        dirs.push(base.join("CMake").join("bin"));
        dirs.push(base.join("Ninja"));
    }
    match p.os {
        Os::Windows => {
            for pf in program_files(vars) {
                dirs.push(pf.join("CMake").join("bin"));
                dirs.push(pf.join("Meson"));
            }
        }
        Os::Linux => {
            dirs.extend(["/usr/local/bin", "/usr/bin", "/snap/bin"].map(PathBuf::from));
        }
        Os::MacOs => {
            dirs.extend(
                ["/opt/homebrew/bin", "/usr/local/bin", "/Applications/CMake.app/Contents/bin"]
                    .map(PathBuf::from),
            );
        }
    }
    os::find_executable(&names, &dirs, p)
}

/// The C compiler Rust build scripts / proc macros link with for `host_triple`: MSVC for
/// `*-windows-msvc`, gcc for `*-windows-gnu`, `cc` on Linux, the Command Line Tools on macOS.
/// Returns the directory of the linker (to put on PATH); None when not installed.
pub fn rust_linker_dir(host_triple: &str, vars: &EnvVars, p: &Platform) -> Option<PathBuf> {
    let all = compilers(vars, p);
    let wanted = |c: &&Compiler| {
        if host_triple.ends_with("-windows-msvc") {
            c.kind == CompilerKind::Msvc
        } else if host_triple.ends_with("-windows-gnu") || host_triple.ends_with("-windows-gnullvm") {
            c.kind == CompilerKind::Gcc
        } else {
            true
        }
    };
    all.iter()
        .find(wanted)
        .and_then(|c| c.cc.parent().map(Path::to_path_buf))
}

/// A clangd installed with the system (PATH, `/usr/bin/clangd-NN`, LLVM).
pub fn system_clangd(vars: &EnvVars, p: &Platform) -> Option<PathBuf> {
    if let Some(found) = lookup::on_path(&["clangd"], vars, p) {
        return Some(found);
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    match p.os {
        Os::Linux => {
            if let Some((_, newest)) = os::versioned_children(Path::new("/usr/bin"), "clangd-")
                .into_iter()
                .find(|(_, f)| f.is_file())
            {
                return Some(newest);
            }
            for (_, dir) in os::versioned_children(Path::new("/usr/lib"), "llvm-") {
                dirs.push(dir.join("bin"));
            }
        }
        Os::Windows => {
            for key in ["ProgramFiles", "ProgramFiles(x86)"] {
                if let Some(pf) = vars.path(key) {
                    dirs.push(pf.join("LLVM").join("bin"));
                }
            }
        }
        Os::MacOs => {
            dirs.push(PathBuf::from("/opt/homebrew/opt/llvm/bin"));
            dirs.push(PathBuf::from("/usr/local/opt/llvm/bin"));
        }
    }
    os::find_executable(&["clangd"], &dirs, p)
}

/// The C compiler for cgo (gcc or clang; MSVC is not supported by cgo).
pub fn cgo_compiler(vars: &EnvVars, p: &Platform) -> Option<Compiler> {
    compilers(vars, p).into_iter().find(|c| c.kind != CompilerKind::Msvc)
}

// ---------------------------------------------------------------------------------------------
// Toolchain (ecosystem contract)
// ---------------------------------------------------------------------------------------------

/// `--env`: a compile database (`compile_commands.json` or its directory).
pub(crate) fn accepts_env_path(path: &Path) -> bool {
    path.join("compile_commands.json").is_file()
        || (path.file_name().is_some_and(|n| n == "compile_commands.json") && path.is_file())
}

/// The preferred compiler as a toolchain (facts: `kind`, `cc`, `cxx`, `vs_install`,
/// `cmake`, `ninja`, `meson`).
pub fn toolchain(cx: &DetectContext<'_>) -> ToolchainStatus {
    let all = compilers(cx.vars, cx.platform);
    let Some(c) = all.into_iter().find(|c| cx.allowed(&c.cc)) else {
        return ToolchainStatus::Missing {
            searched: vec!["PATH".into(), "standard install locations".into()],
        };
    };
    ToolchainStatus::Found(compiler_toolchain(&c, cx.vars, cx.platform))
}

/// A compiler as a [`Toolchain`].
pub fn compiler_toolchain(c: &Compiler, vars: &EnvVars, p: &Platform) -> Toolchain {
    let mut facts = BTreeMap::new();
    facts.insert("kind".to_string(), c.kind.as_str().to_string());
    facts.insert("id".to_string(), c.id());
    let mut executables = BTreeMap::new();
    executables.insert("cc".to_string(), c.cc.clone());
    executables.insert("cxx".to_string(), c.cxx.clone());
    if let Some(vs) = &c.vs_install {
        facts.insert("vs_install".to_string(), vs.display().to_string());
    }
    for tool in ["cmake", "ninja", "meson"] {
        if let Some(path) = build_tool(tool, vars, p, Some(c)) {
            executables.insert(tool.to_string(), path);
        }
    }
    let root = c.cc.parent().map(Path::to_path_buf).unwrap_or_else(|| c.cc.clone());
    Toolchain {
        id: "cc",
        root,
        version: c.version.clone(),
        executables,
        origin: Origin::StandardLocation,
        facts,
    }
}

// ---------------------------------------------------------------------------------------------
// Build system
// ---------------------------------------------------------------------------------------------

/// How the compile database is obtained.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildSystem {
    /// An existing `compile_commands.json` in this directory (absolute).
    CompileCommands(PathBuf),
    /// `CMakeLists.txt` in this directory (relative to the root).
    CMake(String),
    /// `meson.build` in this directory (relative to the root).
    Meson(String),
    /// Only Visual Studio projects (`.vcxproj` / `.sln`).
    VisualStudioOnly,
    /// No build system trace can run: trace writes the database from syntax facts.
    None,
}

const C_LANGUAGES: &[Language] = &[Language::C, Language::Cpp];

/// The build system of the repository (module docs).
pub fn build_system(root: &Path, files: &[(&str, Language)], env_override: Option<&Path>) -> BuildSystem {
    if let Some(o) = env_override.filter(|o| accepts_env_path(o)) {
        let dir = if o.is_file() {
            o.parent().map(Path::to_path_buf).unwrap_or_else(|| o.to_path_buf())
        } else {
            o.to_path_buf()
        };
        return BuildSystem::CompileCommands(dir);
    }
    let mut existing = vec![root.to_path_buf(), root.join("build"), root.join("out")];
    for (name, dir) in subdirs(root) {
        if name.starts_with("cmake-build-") {
            existing.push(dir);
        }
    }
    if let Some(dir) = existing
        .into_iter()
        .find(|d| d.join("compile_commands.json").is_file())
    {
        return BuildSystem::CompileCommands(dir);
    }
    let top = |name: &str| -> Option<String> {
        let dirs = relpath::manifest_dirs(root, files, C_LANGUAGES, name);
        dirs.iter()
            .find(|d| !dirs.iter().any(|o| o != *d && relpath::within(d, o)))
            .cloned()
    };
    if let Some(dir) = top("CMakeLists.txt") {
        return BuildSystem::CMake(dir);
    }
    if let Some(dir) = top("meson.build") {
        return BuildSystem::Meson(dir);
    }
    if has_visual_studio_project(root, 3) {
        return BuildSystem::VisualStudioOnly;
    }
    BuildSystem::None
}

/// A `.vcxproj` or `.sln` at the root or up to `depth` levels below.
fn has_visual_studio_project(dir: &Path, depth: usize) -> bool {
    for (name, path) in crate::entries(dir) {
        let lower = name.to_ascii_lowercase();
        if (lower.ends_with(".vcxproj") || lower.ends_with(".sln")) && path.is_file() {
            return true;
        }
        if depth > 0
            && path.is_dir()
            && !name.starts_with('.')
            && !crate::SKIP_DIRS.contains(&name.as_str())
            && has_visual_studio_project(&path, depth - 1)
        {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------------------------
// Dependencies
// ---------------------------------------------------------------------------------------------

/// Installed vcpkg tree for a `vcpkg.json` project: `<root>/vcpkg_installed`,
/// `<root>/<build dir>/vcpkg_installed`, `$VCPKG_ROOT/installed`.
pub fn vcpkg_installed(root: &Path, vars: &EnvVars) -> Option<PathBuf> {
    let mut candidates = vec![root.join("vcpkg_installed")];
    for (name, dir) in subdirs(root) {
        if name.starts_with("build") || name.starts_with("out") || name.starts_with("cmake-build-") {
            candidates.push(dir.join("vcpkg_installed"));
        }
    }
    if let Some(v) = vars.path("VCPKG_ROOT") {
        candidates.push(v.join("installed"));
    }
    candidates.into_iter().find(|c| {
        subdirs(c)
            .iter()
            .any(|(n, d)| n != "vcpkg" && (d.join("include").is_dir() || d.join("lib").is_dir()))
    })
}

/// vcpkg's CMake toolchain file (`$VCPKG_ROOT/scripts/buildsystems/vcpkg.cmake`).
pub fn vcpkg_toolchain_file(vars: &EnvVars) -> Option<PathBuf> {
    let f = vars
        .path("VCPKG_ROOT")?
        .join("scripts")
        .join("buildsystems")
        .join("vcpkg.cmake");
    f.is_file().then_some(f)
}

/// `conan_toolchain.cmake` written by `conan install` below the root (bounded search).
pub fn conan_toolchain_file(root: &Path) -> Option<PathBuf> {
    fn find(dir: &Path, depth: usize) -> Option<PathBuf> {
        let f = dir.join("conan_toolchain.cmake");
        if f.is_file() {
            return Some(f);
        }
        if depth == 0 {
            return None;
        }
        subdirs(dir)
            .into_iter()
            .filter(|(n, _)| !n.starts_with('.') && n != "node_modules" && n != "src")
            .find_map(|(_, d)| find(&d, depth - 1))
    }
    subdirs(root)
        .into_iter()
        .filter(|(n, _)| n.starts_with("build") || n.starts_with("out") || n.starts_with("cmake-build-"))
        .find_map(|(_, d)| find(&d, 3))
        .or_else(|| find(root, 0))
}

/// Git submodule directories of `.gitmodules` that are missing or empty (relative).
pub fn empty_submodules(root: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(root.join(".gitmodules")) else {
        return Vec::new();
    };
    // `.gitmodules` is git-config syntax: `[submodule "x"]` sections with `key = value` lines.
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "path" {
            continue;
        }
        let rel = value.trim().trim_matches('"').replace('\\', "/");
        let dir = root.join(&rel);
        if crate::entries(&dir).is_empty() {
            out.push(rel);
        }
    }
    out.sort();
    out
}

/// The directories the compiler's own headers live in (the C/C++ standard library and the
/// platform SDK): declarations clangd resolves there are library calls (`toolchain_stdlib`).
/// MSVC: `<MSVC>/include` and the Windows SDK's `Include/<version>` (the Universal CRT with
/// `stdio.h`/`stdlib.h`, `um`, `shared`); MinGW / LLVM on Windows: the compiler's prefix
/// (`<prefix>/bin/gcc.exe` -> `<prefix>`); Linux / macOS: `/usr/include`, `/usr/local/include`,
/// the compiler's own header trees (`/usr/lib/gcc`, `/usr/lib/llvm-*`) and the macOS SDKs.
/// Only existing directories, deduplicated.
pub(crate) fn system_header_dirs(t: &Toolchain, vars: &EnvVars, p: &Platform) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let cc = t.executables.get("cc");
    if t.facts.get("kind").is_some_and(|k| k == "msvc") {
        // <MSVC>/bin/Hostx64/x64/cl.exe -> <MSVC>/include
        if let Some(include) = cc.and_then(|cl| cl.ancestors().nth(4)).map(|d| d.join("include")) {
            dirs.push(include);
        }
        if let Some((kits, sdk)) = windows_sdk(vars) {
            dirs.push(kits.join("Include").join(sdk));
        }
    } else if p.os == Os::Windows {
        if let Some(prefix) = cc.and_then(|c| c.parent()).and_then(Path::parent) {
            dirs.push(prefix.to_path_buf());
        }
    } else {
        dirs.push(PathBuf::from("/usr/include"));
        dirs.push(PathBuf::from("/usr/local/include"));
        dirs.push(PathBuf::from("/usr/lib/gcc"));
        for (_, dir) in os::versioned_children(Path::new("/usr/lib"), "llvm-") {
            dirs.push(dir);
        }
        if p.os == Os::MacOs {
            dirs.push(PathBuf::from("/Library/Developer/CommandLineTools/SDKs"));
            dirs.push(PathBuf::from(
                "/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs",
            ));
            dirs.push(PathBuf::from("/Library/Developer/CommandLineTools/usr/include"));
            dirs.push(PathBuf::from("/Library/Developer/CommandLineTools/usr/lib/clang"));
        }
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for d in dirs {
        if d.is_dir() && !out.contains(&d) {
            out.push(d);
        }
    }
    out
}

/// Declared package managers and their installed trees (module docs).
pub fn deps(cx: &DetectContext<'_>, toolchain: Option<&Toolchain>) -> DepsReport {
    let mut report = DepsReport::none_declared();
    let mut h = blake3::Hasher::new();
    let mut missing = Vec::new();
    let mut hints = Vec::new();
    let mut declared = false;
    if cx.root.join("vcpkg.json").is_file() {
        declared = true;
        h.update(&std::fs::read(cx.root.join("vcpkg.json")).unwrap_or_default());
        match vcpkg_installed(cx.root, cx.vars) {
            Some(dir) => {
                h.update(dir.display().to_string().as_bytes());
                report.notes.push(format!("vcpkg packages: {}", dir.display()));
            }
            None => {
                missing.push("vcpkg packages".to_string());
                hints.push("vcpkg install");
            }
        }
    }
    if cx.root.join("conanfile.txt").is_file() || cx.root.join("conanfile.py").is_file() {
        declared = true;
        match conan_toolchain_file(cx.root) {
            Some(f) => {
                h.update(f.display().to_string().as_bytes());
                report.notes.push(format!("conan toolchain: {}", f.display()));
            }
            None => {
                missing.push("conan packages".to_string());
                hints.push("conan install . --build=missing");
            }
        }
    }
    for sub in empty_submodules(cx.root) {
        report
            .notes
            .push(format!("git submodule {sub} is not checked out (git submodule update --init)"));
    }
    if let Some(t) = toolchain {
        let version = t.version.as_ref().map(|v| v.text.clone());
        for path in system_header_dirs(t, cx.vars, cx.platform) {
            report.roots.push(LibraryRoot {
                path,
                kind: LibraryKind::Stdlib,
                ecosystem: EcosystemId::CFamily,
                layout: "toolchain_stdlib",
                version: version.clone(),
            });
        }
    }
    report.fingerprint = crate::hex(h);
    report.hint = hints.join(", ");
    report.status = if !missing.is_empty() {
        DepsStatus::Missing
    } else if declared {
        DepsStatus::Installed
    } else {
        DepsStatus::NoneDeclared
    };
    report.missing = missing;
    report
}

// ---------------------------------------------------------------------------------------------
// MSVC tools directory (Foundation)
// ---------------------------------------------------------------------------------------------

/// The newest `<Program Files>/Microsoft Visual Studio/<year>/<edition>/VC/Tools/MSVC/<version>/`
/// with an `include` directory (Windows only; read-only directory listings; `None` when not
/// installed). clang's MSVC driver reads it from `VCToolsInstallDir` to find the C/C++
/// standard library headers (and then the Windows SDK from the registry); without it clangd
/// parses every `#include <string>` as a fatal error. The returned path ends with a
/// separator, as `VCToolsInstallDir` does.
pub fn msvc_tools_dir(vars: &EnvVars, platform: &Platform) -> Option<PathBuf> {
    if platform.os != Os::Windows {
        return None;
    }
    let mut roots: Vec<PathBuf> = ["ProgramFiles(x86)", "ProgramFiles"]
        .iter()
        .filter_map(|k| vars.get(k).map(PathBuf::from))
        .collect();
    if roots.is_empty() {
        roots.push(PathBuf::from(r"C:\Program Files (x86)"));
        roots.push(PathBuf::from(r"C:\Program Files"));
    }
    newest_msvc(&roots).map(|p| PathBuf::from(format!("{}\\", p.display())))
}

/// Newest `VC/Tools/MSVC/<version>` (numeric version order) under the Visual Studio
/// installations of `roots`.
pub(crate) fn newest_msvc(roots: &[PathBuf]) -> Option<PathBuf> {
    let dirs = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
            .unwrap_or_default()
    };
    let mut best: Option<(Vec<u64>, PathBuf)> = None;
    for root in roots {
        for year in dirs(&root.join("Microsoft Visual Studio")) {
            for edition in dirs(&year) {
                for version in dirs(&edition.join("VC").join("Tools").join("MSVC")) {
                    if !version.join("include").is_dir() {
                        continue;
                    }
                    let key: Vec<u64> = version
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default()
                        .split('.')
                        .map(|part| part.parse().unwrap_or(0))
                        .collect();
                    if best.as_ref().is_none_or(|(k, _)| key > *k) {
                        best = Some((key, version));
                    }
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// The Windows 10/11 SDK: `<Program Files (x86)>/Windows Kits/10` and its newest version with
/// `um/windows.h` headers.
fn windows_sdk(vars: &EnvVars) -> Option<(PathBuf, String)> {
    for pf in program_files(vars) {
        let kits = pf.join("Windows Kits").join("10");
        for (version, dir) in os::versioned_children(&kits.join("Include"), "") {
            if dir.join("um").join("windows.h").is_file() || dir.join("um").join("Windows.h").is_file() {
                return Some((kits.clone(), version.text));
            }
        }
    }
    None
}

/// The build environment Visual Studio's `vcvars64.bat` / `vcvarsarm64.bat` would set for an MSVC
/// `compiler`, built from the installation layout (the MSVC toolset of `compiler.cc`, the
/// newest Windows SDK) instead of running the script: trace never starts `cmd.exe`. The
/// variables CMake and the compiler read: `INCLUDE`, `LIB`, `LIBPATH`, `PATH` (compiler, SDK
/// tools such as `rc.exe` / `mt.exe`, then `base_path`), `VCToolsInstallDir`, `VCINSTALLDIR`,
/// `VSINSTALLDIR`, `WindowsSdkDir`, `WindowsSDKVersion`, `UCRTVersion`,
/// `UniversalCRTSdkDir`, `VCToolsVersion`, `Platform`. `None` for other compilers or when the
/// toolset or the Windows SDK is incomplete.
pub fn msvc_env(
    compiler: &Compiler,
    vars: &EnvVars,
    p: &Platform,
    base_path: &str,
) -> Option<Vec<(String, String)>> {
    if compiler.kind != CompilerKind::Msvc {
        return None;
    }
    let vs = compiler.vs_install.as_ref()?;
    // <vs>/VC/Tools/MSVC/<version>/bin/Host<arch>/<arch>/cl.exe
    let host_bin = compiler.cc.parent()?;
    let tools = host_bin.parent()?.parent()?.parent()?;
    let tools_version = tools.file_name()?.to_str()?.to_string();
    let arch = if p.arch == os::Arch::Aarch64 {
        "arm64"
    } else {
        "x64"
    };
    let (kits, sdk) = windows_sdk(vars)?;
    let dirs = |candidates: Vec<PathBuf>| -> String {
        candidates
            .into_iter()
            .filter(|d| d.is_dir())
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(";")
    };
    let sdk_include = kits.join("Include").join(&sdk);
    let sdk_lib = kits.join("Lib").join(&sdk);
    let include = dirs(vec![
        tools.join("include"),
        tools.join("ATLMFC").join("include"),
        vs.join("VC").join("Auxiliary").join("VS").join("include"),
        sdk_include.join("ucrt"),
        sdk_include.join("um"),
        sdk_include.join("shared"),
        sdk_include.join("winrt"),
        sdk_include.join("cppwinrt"),
    ]);
    let lib = dirs(vec![
        tools.join("lib").join(arch),
        tools.join("ATLMFC").join("lib").join(arch),
        sdk_lib.join("ucrt").join(arch),
        sdk_lib.join("um").join(arch),
    ]);
    let libpath = dirs(vec![
        tools.join("lib").join(arch),
        tools.join("ATLMFC").join("lib").join(arch),
        tools.join("lib").join("x86").join("store").join("references"),
        kits.join("UnionMetadata").join(&sdk),
        kits.join("References").join(&sdk),
    ]);
    let mut path = dirs(vec![
        host_bin.to_path_buf(),
        kits.join("bin").join(&sdk).join(arch),
        kits.join("bin").join(arch),
    ]);
    if !base_path.is_empty() {
        path = format!("{path};{base_path}");
    }
    let with_sep = |d: &Path| format!("{}\\", d.display());
    Some(vec![
        ("INCLUDE".to_string(), include),
        ("LIB".to_string(), lib),
        ("LIBPATH".to_string(), libpath),
        ("PATH".to_string(), path),
        ("VCToolsInstallDir".to_string(), with_sep(tools)),
        ("VCINSTALLDIR".to_string(), with_sep(&vs.join("VC"))),
        ("VSINSTALLDIR".to_string(), with_sep(vs)),
        ("WindowsSdkDir".to_string(), with_sep(&kits)),
        ("UniversalCRTSdkDir".to_string(), with_sep(&kits)),
        ("WindowsSDKVersion".to_string(), format!("{sdk}\\")),
        ("UCRTVersion".to_string(), sdk),
        ("VCToolsVersion".to_string(), tools_version),
        ("Platform".to_string(), arch.to_string()),
    ])
}

/// The CFamily ecosystem ([`crate::Ecosystem`]).
pub(crate) struct CFamily;

impl crate::Ecosystem for CFamily {
    fn id(&self) -> crate::EcosystemId {
        crate::EcosystemId::CFamily
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
#[path = "../../tests/unit/ecosystems/cfamily.rs"]
mod tests;
