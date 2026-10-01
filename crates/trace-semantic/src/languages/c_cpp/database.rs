//! The compilation database: CMake / Meson configure steps (approved builds), the database
//! rebased into the workspace, and a generated database from syntax includes when the
//! project has none.

use crate::backend::SemanticFile;
use crate::languages::WorkspaceContext;
use crate::languages::{build_step_timeout, run_step, Step};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use trace_core::fingerprint::PartsHasher;
use trace_core::setup_error::SetupError;
use trace_core::Language;
use trace_env::cfamily::{Compiler, CompilerKind};
use trace_env::os::{EnvVars, Os};

use super::*;

/// Build files hashed into the configure stamp at most (bounded walk).
pub(super) const MAX_STAMP_ENTRIES: usize = 20_000;

/// `{outside}/cbuild/<compiler id>`: the CMake build directory (never inside the repository
/// or its mirror).
pub fn cmake_build_dir(outside: &Path, compiler: &Compiler) -> PathBuf {
    outside.join("cbuild").join(compiler.id())
}

/// The CMake generator: Ninja when found, else the Makefile generator of the platform.
pub fn cmake_generator(ninja: Option<&Path>, compiler: &Compiler, os: Os) -> &'static str {
    if ninja.is_some() {
        "Ninja"
    } else if os == Os::Windows && compiler.kind == CompilerKind::Msvc {
        "NMake Makefiles"
    } else if os == Os::Windows {
        "MinGW Makefiles"
    } else {
        "Unix Makefiles"
    }
}

/// The first non-hidden configure preset of `<src>/CMakePresets.json`.
pub(super) fn first_preset(src: &Path) -> Option<String> {
    let text = std::fs::read_to_string(src.join("CMakePresets.json")).ok()?;
    let value = trace_core::formats::jsonc::parse(&text)?;
    value
        .get("configurePresets")?
        .as_array()?
        .iter()
        .find(|p| !p.get("hidden").and_then(Value::as_bool).unwrap_or(false))
        .and_then(|p| p.get("name")?.as_str().map(str::to_string))
}

/// Inputs of one CMake configure.
pub struct CmakePlan<'a> {
    pub src: &'a Path,
    pub build: &'a Path,
    pub generator: &'a str,
    pub compiler: &'a Compiler,
    pub preset: Option<&'a str>,
    pub ninja: Option<&'a Path>,
    pub toolchain_file: Option<&'a Path>,
    pub vcpkg_installed: Option<&'a Path>,
}

/// Arguments of the CMake configure (module docs).
pub fn cmake_args(plan: &CmakePlan<'_>) -> Vec<String> {
    let CmakePlan {
        src,
        build,
        generator,
        compiler,
        preset,
        ninja,
        toolchain_file,
        vcpkg_installed,
    } = *plan;
    let mut args = Vec::new();
    if let Some(p) = preset {
        args.push("--preset".to_string());
        args.push(p.to_string());
    }
    args.extend([
        "-S".to_string(),
        src.display().to_string(),
        "-B".to_string(),
        build.display().to_string(),
        "-G".to_string(),
        generator.to_string(),
        "-DCMAKE_EXPORT_COMPILE_COMMANDS=ON".to_string(),
        format!("-DCMAKE_C_COMPILER={}", slash(&compiler.cc)),
        format!("-DCMAKE_CXX_COMPILER={}", slash(&compiler.cxx)),
        "-DFETCHCONTENT_FULLY_DISCONNECTED=ON".to_string(),
        "-DVCPKG_MANIFEST_INSTALL=OFF".to_string(),
        "-DCPM_USE_LOCAL_PACKAGES=ON".to_string(),
    ]);
    if let (Some(n), "Ninja") = (ninja, generator) {
        args.push(format!("-DCMAKE_MAKE_PROGRAM={}", slash(n)));
    }
    if let Some(f) = toolchain_file {
        args.push(format!("-DCMAKE_TOOLCHAIN_FILE={}", slash(f)));
    }
    if let Some(d) = vcpkg_installed {
        args.push(format!("-DVCPKG_INSTALLED_DIR={}", slash(d)));
    }
    args
}

/// CMake paths with forward slashes (backslashes are escapes in CMake strings).
pub(super) fn slash(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// The environment of a configure: the Visual Studio build environment for MSVC
/// (`trace_env::cfamily::msvc_env`, built from the installation layout), else the
/// allow-listed environment with the compiler first on PATH.
pub(super) fn configure_env(
    cx: &WorkspaceContext<'_>,
    data: &CData,
    language: Language,
) -> Result<BTreeMap<String, String>, SetupError> {
    let passthrough = ["VCPKG_ROOT", "PKG_CONFIG_PATH", "CMAKE_PREFIX_PATH"];
    let path = cx.prepared.env.get("PATH").cloned().unwrap_or_default();
    if data.compiler.kind == CompilerKind::Msvc {
        let msvc =
            trace_env::cfamily::msvc_env(&data.compiler, &EnvVars::from_process(), &data.platform, &path)
                .ok_or_else(|| {
                    SetupError::BuildFailed {
            language,
            what: "the Visual Studio build environment is incomplete (no Windows SDK with headers was found)"
                .into(),
            log: cx.log.to_path_buf(),
        }
                })?;
        let set: Vec<(&str, String)> = msvc.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        return Ok(crate::tools::clean_env(&passthrough, &set));
    }
    Ok(crate::tools::clean_env(&passthrough, &[("PATH", path)]))
}

/// Run the CMake configure unless its stamp is unchanged.
pub(super) fn configure_cmake(
    cx: &WorkspaceContext<'_>,
    data: &CData,
    src: &Path,
    build: &Path,
    language: Language,
) -> Result<(), SetupError> {
    let failed = |what: &str| SetupError::BuildFailed {
        language,
        what: what.to_string(),
        log: cx.log.to_path_buf(),
    };
    let Some(cmake) = &data.cmake else {
        return Err(failed("CMake was not found"));
    };
    let generator = cmake_generator(data.ninja.as_deref(), &data.compiler, data.platform.os);
    let preset = first_preset(src);
    let args = cmake_args(&CmakePlan {
        src,
        build,
        generator,
        compiler: &data.compiler,
        preset: preset.as_deref(),
        ninja: data.ninja.as_deref(),
        toolchain_file: data.toolchain_file.as_deref(),
        vcpkg_installed: data.vcpkg_installed.as_deref(),
    });
    let stamp = stamp_of(src, &args);
    let stamp_file = build.join("trace-configure.stamp");
    if build.join("compile_commands.json").is_file()
        && std::fs::read_to_string(&stamp_file).ok().as_deref() == Some(stamp.as_str())
    {
        return Ok(());
    }
    std::fs::create_dir_all(build)
        .map_err(|e| failed(&format!("creating the build directory failed: {e}")))?;
    let env = configure_env(cx, data, language)?;
    let step = Step {
        program: cmake,
        args,
        cwd: build,
        env,
        timeout: build_step_timeout(),
        quiet_stdout: false,
    };
    let outcome = run_step(&step, cx.log).map_err(|e| failed(&format!("CMake could not start: {e}")))?;
    if outcome.timed_out {
        return Err(failed("CMake configure did not finish in 10 minutes"));
    }
    if !outcome.success {
        return Err(configure_error(&outcome.output, data, language, cx.log, "CMake configure failed"));
    }
    let _ = std::fs::write(&stamp_file, stamp);
    Ok(())
}

/// Run `meson setup` (reconfigure when the build directory exists).
pub(super) fn configure_meson(
    cx: &WorkspaceContext<'_>,
    data: &CData,
    src: &Path,
    build: &Path,
    language: Language,
) -> Result<(), SetupError> {
    let failed = |what: &str| SetupError::BuildFailed {
        language,
        what: what.to_string(),
        log: cx.log.to_path_buf(),
    };
    let Some(meson) = &data.meson else {
        return Err(failed("Meson was not found"));
    };
    let mut args = vec![
        "setup".to_string(),
        build.display().to_string(),
        src.display().to_string(),
        "--wrap-mode=nodownload".to_string(),
        "--backend=ninja".to_string(),
    ];
    let stamp = stamp_of(src, &args);
    let stamp_file = build.join("trace-configure.stamp");
    if build.join("compile_commands.json").is_file()
        && std::fs::read_to_string(&stamp_file).ok().as_deref() == Some(stamp.as_str())
    {
        return Ok(());
    }
    if build.join("meson-private").is_dir() {
        args.push("--reconfigure".into());
    }
    let mut env = configure_env(cx, data, language)?;
    let cc = data.compiler.cc.display().to_string();
    let cxx = data.compiler.cxx.display().to_string();
    env.insert("CC".into(), cc);
    env.insert("CXX".into(), cxx);
    if let Some(n) = &data.ninja {
        env.insert("NINJA".into(), n.display().to_string());
    }
    let temp = std::env::temp_dir();
    let step = Step {
        program: meson,
        args,
        cwd: &temp,
        env,
        timeout: build_step_timeout(),
        quiet_stdout: false,
    };
    let outcome = run_step(&step, cx.log).map_err(|e| failed(&format!("Meson could not start: {e}")))?;
    if outcome.timed_out {
        return Err(failed("Meson setup did not finish in 10 minutes"));
    }
    if !outcome.success {
        return Err(configure_error(&outcome.output, data, language, cx.log, "Meson setup failed"));
    }
    let _ = std::fs::write(&stamp_file, stamp);
    Ok(())
}

/// The database a configure wrote into `build`.
pub(super) fn read_database(build: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(build.join("compile_commands.json"))
        .map_err(|_| "the configure wrote no compile_commands.json".to_string())?;
    serde_json::from_str(&text).map_err(|e| format!("compile_commands.json is not valid JSON: {e}"))
}

/// Stamp of a configure: its arguments plus every CMake / Meson input below `src` (bounded).
pub(super) fn stamp_of(src: &Path, args: &[String]) -> String {
    let mut h = PartsHasher::new();
    h.text(&args.join("\u{1}"));
    let mut stack = vec![src.to_path_buf()];
    let mut seen = 0usize;
    let mut inputs: Vec<PathBuf> = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            seen += 1;
            if seen > MAX_STAMP_ENTRIES {
                break;
            }
            let name = e.file_name().to_string_lossy().to_string();
            let path = e.path();
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_dir() {
                if !name.starts_with('.')
                    && !["build", "out", "node_modules", "target"].contains(&name.as_str())
                {
                    stack.push(path);
                }
            } else if name == "CMakeLists.txt"
                || name.ends_with(".cmake")
                || name == "CMakePresets.json"
                || name == "CMakeUserPresets.json"
                || name == "meson.build"
                || name == "meson_options.txt"
                || name == "meson.options"
                || name == "vcpkg.json"
                || name.ends_with(".wrap")
            {
                inputs.push(path);
            }
        }
    }
    inputs.sort();
    for path in inputs {
        h.text(&path.display().to_string());
        h.text(&String::from_utf8_lossy(&std::fs::read(&path).unwrap_or_default()));
    }
    h.finish().hex_prefix(32)
}

/// A failed configure (tool output) -> `DepsMissing` when packages were not found, else
/// `BuildFailed`.
pub(super) fn configure_error(
    output: &str,
    data: &CData,
    language: Language,
    log: &Path,
    what: &str,
) -> SetupError {
    let missing = missing_packages(output);
    if !missing.is_empty() {
        let hint = match (&data.package_hint, data.empty_submodules) {
            (Some(h), _) => h.clone(),
            (None, true) => "git submodule update --init".to_string(),
            (None, false) => {
                format!("install the libraries the configure could not find: {}", missing.join(", "))
            }
        };
        return SetupError::DepsMissing { language, hint };
    }
    SetupError::BuildFailed {
        language,
        what: what.to_string(),
        log: log.to_path_buf(),
    }
}

/// Package names a CMake / Meson / pkg-config log reports as not found.
pub fn missing_packages(output: &str) -> Vec<String> {
    let mut out = BTreeSet::new();
    let quoted_after = |line: &str, marker: &str, quote: char| -> Option<String> {
        let rest = &line[line.find(marker)? + marker.len()..];
        let rest = rest.strip_prefix(quote)?;
        Some(rest[..rest.find(quote)?].to_string())
    };
    for line in output.lines() {
        let t = line.trim();
        if let Some(n) = quoted_after(t, "provided by ", '"') {
            out.insert(n);
        } else if let Some(rest) = t.split("Could NOT find ").nth(1) {
            if let Some(n) = rest.split([' ', ':', '(']).next().filter(|n| !n.is_empty()) {
                out.insert(n.to_string());
            }
        } else if let Some(n) =
            quoted_after(t, "No package ", '\'').or_else(|| quoted_after(t, "Package ", '\''))
        {
            if t.contains("found") {
                out.insert(n);
            }
        } else if t.starts_with("Dependency") && t.contains("not found") {
            let name = quoted_after(t, "Dependency ", '"').or_else(|| {
                t.strip_prefix("Dependency ")
                    .and_then(|r| r.split_whitespace().next())
                    .map(str::to_string)
            });
            out.extend(name);
        }
    }
    out.into_iter().collect()
}

/// Rewrite an existing database from the repository root to the workspace (every string of
/// `directory` / `file` / `command` / `arguments` / `output`).
pub fn rebase_database(value: &Value, from: &Path, to: &Path) -> Value {
    let from_text = from.display().to_string();
    let mut variants: Vec<String> = vec![from_text.clone(), from_text.replace('\\', "/")];
    let mut chars = from_text.chars();
    if let (Some(first), true) = (chars.next(), from_text.get(1..2) == Some(":")) {
        let rest: String = chars.collect();
        for v in [
            format!("{}{rest}", first.to_ascii_lowercase()),
            format!("{}{rest}", first.to_ascii_uppercase()),
        ] {
            variants.push(v.clone());
            variants.push(v.replace('\\', "/"));
        }
    }
    variants.sort_by_key(|v| std::cmp::Reverse(v.len()));
    variants.dedup();
    let to_text = to.display().to_string();
    fn walk(v: &Value, variants: &[String], to: &str) -> Value {
        match v {
            Value::String(s) => {
                let mut out = s.clone();
                for variant in variants {
                    if !variant.is_empty() && out.contains(variant.as_str()) {
                        out = out.replace(variant.as_str(), to);
                    }
                }
                Value::String(out)
            }
            Value::Array(items) => Value::Array(items.iter().map(|i| walk(i, variants, to)).collect()),
            Value::Object(map) => {
                Value::Object(map.iter().map(|(k, v)| (k.clone(), walk(v, variants, to))).collect())
            }
            other => other.clone(),
        }
    }
    walk(value, &variants, &to_text)
}

/// Comparable text of a path: `/`-separated, without the `\\?\` prefix, `.` / `..` segments
/// resolved lexically (never above the first segment), no trailing `/`.
pub(super) fn path_text(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    let text = text.strip_prefix("//?/").unwrap_or(&text);
    let mut parts: Vec<&str> = Vec::new();
    for (i, segment) in text.split('/').enumerate() {
        match segment {
            "." => {}
            "" if i > 0 => {}
            ".." if parts.len() > 1 => {
                parts.pop();
            }
            _ => parts.push(segment),
        }
    }
    parts.join("/")
}

/// `text` relative to `root` (both [`path_text`]; ASCII case folded on Windows), or None when
/// it is not below it.
pub(super) fn below(text: &str, root: &str) -> Option<String> {
    let (t, r) = if cfg!(windows) {
        (text.to_ascii_lowercase(), root.to_ascii_lowercase())
    } else {
        (text.to_string(), root.to_string())
    };
    let rest = t.strip_prefix(r.as_str())?.strip_prefix('/')?;
    (!rest.is_empty()).then(|| text[text.len() - rest.len()..].to_string())
}

/// The [`source_key`]s of the files a compile database compiles that lie in `workspace`
/// (each entry's `file`, relative to its `directory`; entries outside the workspace, such as
/// generated sources in the build directory, are not repository files).
pub fn database_sources(database: &Value, workspace: &Path) -> BTreeSet<String> {
    let mut roots = vec![path_text(workspace)];
    if let Ok(real) = std::fs::canonicalize(workspace) {
        let real = path_text(&real);
        if !roots.contains(&real) {
            roots.push(real);
        }
    }
    let mut out = BTreeSet::new();
    for entry in database.as_array().into_iter().flatten() {
        let Some(file) = entry.get("file").and_then(Value::as_str) else {
            continue;
        };
        let file = Path::new(file);
        let full = if file.is_absolute() {
            file.to_path_buf()
        } else {
            match entry.get("directory").and_then(Value::as_str) {
                Some(dir) => Path::new(dir).join(file),
                None => continue,
            }
        };
        let text = path_text(&full);
        if let Some(rel) = roots.iter().find_map(|root| below(&text, root)) {
            out.insert(source_key(&rel));
        }
    }
    out
}

/// Include directories (relative, `.` = root) the quoted `#include` facts of the partition
/// resolve to: `D` such that `D/<target>` is a file of the partition.
pub fn include_dirs(files: &[&SemanticFile<'_>]) -> BTreeSet<String> {
    let paths: Vec<&str> = files.iter().map(|f| f.path).collect();
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for f in files {
        if !matches!(f.language, Language::C | Language::Cpp) {
            continue;
        }
        for imp in &f.facts.imports {
            let inc = imp
                .target
                .trim_matches(|c| c == '"' || c == '<' || c == '>')
                .replace('\\', "/");
            if inc.is_empty() || inc.starts_with('/') || inc.split('/').any(|s| s == "..") {
                continue;
            }
            let suffix = format!("/{inc}");
            for p in &paths {
                if *p == inc {
                    dirs.insert(".".to_string());
                } else if let Some(prefix) = p.strip_suffix(suffix.as_str()) {
                    dirs.insert(prefix.to_string());
                }
            }
        }
    }
    dirs
}

/// `-x` language of a file (headers per their decided language).
pub(super) fn x_language(path: &str, language: Language) -> &'static str {
    let lower = path.to_ascii_lowercase();
    let header = [".h", ".hh", ".hpp", ".hxx", ".h++", ".inl", ".ipp", ".tpp"]
        .iter()
        .any(|e| lower.ends_with(e));
    match (language, header) {
        (Language::Cpp, true) => "c++-header",
        (Language::Cpp, false) => "c++",
        (_, true) => "c-header",
        (_, false) => "c",
    }
}

/// The compile database trace writes without a build system (module docs).
pub fn generated_database(files: &[&SemanticFile<'_>], workspace: &Path, compiler: &Compiler) -> Value {
    let includes: Vec<String> = include_dirs(files)
        .into_iter()
        .map(|d| {
            let dir = if d == "." {
                workspace.to_path_buf()
            } else {
                workspace.join(&d)
            };
            format!("-I{}", dir.display())
        })
        .collect();
    let mut entries = Vec::new();
    for f in files {
        if !matches!(f.language, Language::C | Language::Cpp) {
            continue;
        }
        let x = x_language(f.path, f.language);
        let driver = match (compiler.kind, f.language) {
            (CompilerKind::Msvc, Language::Cpp) => "clang++".to_string(),
            (CompilerKind::Msvc, _) => "clang".to_string(),
            (_, Language::Cpp) => compiler.cxx.display().to_string(),
            _ => compiler.cc.display().to_string(),
        };
        let file = workspace.join(f.path).display().to_string();
        let mut arguments = vec![driver, "-x".to_string(), x.to_string()];
        arguments.extend(includes.iter().cloned());
        arguments.push("-c".into());
        arguments.push(file.clone());
        entries.push(json!({
            "directory": workspace.display().to_string(),
            "file": file,
            "arguments": arguments,
        }));
    }
    Value::Array(entries)
}
