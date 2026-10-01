use super::*;
use crate::backend::SemanticFile;
use crate::languages::{Prepared, Server, SetupContext};
use crate::test_support::facts::{facts_with_imports, sfile};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use trace_core::facts::FileFacts;
use trace_core::setup_error::SetupError;
use trace_core::Language;
use trace_env::cfamily::{BuildSystem, Compiler, CompilerKind};
use trace_env::os::{EnvVars, Os, Platform};

fn gcc() -> Compiler {
    Compiler {
        kind: CompilerKind::Gcc,
        cc: PathBuf::from("/usr/bin/gcc"),
        cxx: PathBuf::from("/usr/bin/g++"),
        version: trace_env::os::Version::parse("13.2.0"),
        vs_install: None,
    }
}

#[test]
fn rule_cmake_build_dir_is_outside_the_repo() {
    let workspace = Path::new("/cache/workspaces/lsp_clangd/tree");
    let outside = Path::new("/cache/workspaces/lsp_clangd/state");
    let build = cmake_build_dir(outside, &gcc());
    assert!(build.starts_with(outside));
    assert!(!build.starts_with(workspace));
    assert_eq!(build, outside.join("cbuild").join("gcc-13.2.0"));
    let compiler = gcc();
    let args = cmake_args(&CmakePlan {
        src: workspace,
        build: &build,
        generator: "Ninja",
        compiler: &compiler,
        preset: None,
        ninja: Some(Path::new("/usr/bin/ninja")),
        toolchain_file: None,
        vcpkg_installed: None,
    });
    let pos = |a: &str| args.iter().position(|x| x == a).unwrap();
    assert_eq!(args[pos("-S") + 1], workspace.display().to_string());
    assert_eq!(args[pos("-B") + 1], build.display().to_string());
    assert_eq!(args[pos("-G") + 1], "Ninja");
    for flag in [
        "-DCMAKE_EXPORT_COMPILE_COMMANDS=ON",
        "-DFETCHCONTENT_FULLY_DISCONNECTED=ON",
        "-DVCPKG_MANIFEST_INSTALL=OFF",
        "-DCPM_USE_LOCAL_PACKAGES=ON",
        "-DCMAKE_C_COMPILER=/usr/bin/gcc",
    ] {
        assert!(args.iter().any(|a| a == flag), "{flag}");
    }
    assert_eq!(cmake_generator(None, &gcc(), Os::Linux), "Unix Makefiles");
    let msvc = Compiler {
        kind: CompilerKind::Msvc,
        ..gcc()
    };
    assert_eq!(cmake_generator(None, &msvc, Os::Windows), "NMake Makefiles");
}

#[test]
fn rule_generated_compile_database_without_build_system() {
    let a = facts_with_imports(Language::Cpp, &["fmt/core.h", "gtest/gtest.h", "../evil.h", "<vector>"]);
    let none = FileFacts::default();
    let files = [
        sfile("test/os-test.cc", Language::Cpp, &a),
        sfile("include/fmt/core.h", Language::Cpp, &none),
        sfile("test/gtest/gtest/gtest.h", Language::Cpp, &none),
        sfile("src/legacy.c", Language::C, &none),
        sfile("main.go", Language::Go, &none),
    ];
    let refs: Vec<&SemanticFile<'_>> = files.iter().collect();
    assert_eq!(include_dirs(&refs).into_iter().collect::<Vec<_>>(), vec!["include", "test/gtest"]);
    let ws = Path::new("/ws");
    let db = generated_database(&refs, ws, &gcc());
    let entries = db.as_array().unwrap();
    assert_eq!(entries.len(), 4, "only C and C++ files");
    let args = |i: usize| -> Vec<String> {
        entries[i]["arguments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(args(0)[..3], ["/usr/bin/g++", "-x", "c++"]);
    assert_eq!(args(1)[2], "c++-header", "a C++ header");
    assert_eq!(args(3)[..3], ["/usr/bin/gcc", "-x", "c"]);
    assert!(args(0).contains(&format!("-I{}", ws.join("include").display())));
    assert_eq!(entries[0]["directory"], ws.display().to_string());
}

#[test]
fn rule_existing_database_is_rebased_into_the_workspace() {
    let db = json!([{
        "directory": "/home/u/proj/build",
        "file": "/home/u/proj/src/a.cc",
        "command": "c++ -I/home/u/proj/include -c /home/u/proj/src/a.cc"
    }]);
    let out = rebase_database(&db, Path::new("/home/u/proj"), Path::new("/ws"));
    assert_eq!(out[0]["directory"], "/ws/build");
    assert_eq!(out[0]["command"], "c++ -I/ws/include -c /ws/src/a.cc");
}

#[test]
fn rule_configure_log_missing_package_is_a_dependency_error() {
    let log = "CMake Error at CMakeLists.txt:10 (find_package):\n  Could not find a package configuration file provided by \"fmt\" with any of\n-- Could NOT find ZLIB (missing: ZLIB_LIBRARY)\nDependency \"glib-2.0\" not found, tried pkgconfig\n";
    assert_eq!(missing_packages(log), vec!["ZLIB", "fmt", "glib-2.0"]);
    let data = CData {
        mode: BuildSystem::CMake(String::new()),
        compiler: gcc(),
        repo_root: PathBuf::from("/r"),
        cmake: None,
        ninja: None,
        meson: None,
        toolchain_file: None,
        vcpkg_installed: None,
        package_hint: Some("vcpkg install".into()),
        empty_submodules: false,
        platform: Platform::current(),
        state_dir: PathBuf::from("/state"),
        index_limit: Duration::from_secs(600),
        sources: SourceMemo::default(),
    };
    let e = configure_error(log, &data, Language::Cpp, Path::new("log"), "CMake configure failed");
    assert_eq!(
            e.to_string(),
            "Dependencies not installed. Install your project's dependencies (vcpkg install)\n       or point trace to them: trace index --env <path>"
        );
    let other = configure_error(
        "CMake Error: syntax",
        &data,
        Language::Cpp,
        Path::new("log"),
        "CMake configure failed",
    );
    assert_eq!(other.kind(), "build_failed");
}

#[test]
fn rule_visual_studio_only_project_is_unsupported_off_windows() {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("trace-fixtures-vcx-{}", uuid::Uuid::new_v4().simple()));
    let root = dir.join("repo");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("app.vcxproj"), "<Project/>").unwrap();
    std::fs::write(root.join("src/a.cpp"), "int main() {}\n").unwrap();
    let repo = trace_core::paths::RepoPaths::resolve_in(&root, &dir.join("home")).unwrap();
    let registry = crate::registry::Registry::builtin();
    let entry = registry.entry("lsp:clangd").unwrap();
    let tools = crate::test_support::setup::tool_env(None);
    let settings = trace_core::repo_settings::RepoSettings::default();
    let files = [("src/a.cpp", Language::Cpp)];
    let facts = |_: &str| -> Option<&FileFacts> { None };
    let platform = Platform {
        os: Os::Linux,
        arch: trace_env::os::Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    };
    let vars = EnvVars::default();
    let cx = SetupContext {
        repo: &repo,
        entry,
        languages: &[Language::Cpp],
        files: &files,
        facts: &facts,
        settings: &settings,
        tools: &tools,
        platform: &platform,
        vars: &vars,
        report_only: true,
    };
    let err = Hooks.preflight(&cx).unwrap_err();
    assert_eq!(
        err.lines(),
        vec![
            "This C++ project builds only with Visual Studio.".to_string(),
            "       Run trace on Windows to analyze it.".to_string()
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rule_cmake_approval_error_names_the_projects_language() {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("trace-fixtures-cmake-{}", uuid::Uuid::new_v4().simple()));
    let root = dir.join("repo");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("CMakeLists.txt"), "project(x CXX)\n").unwrap();
    std::fs::write(root.join("src/a.cc"), "int main() {}\n").unwrap();
    std::fs::write(root.join("src/b.c"), "int f(void) { return 0; }\n").unwrap();
    let repo = trace_core::paths::RepoPaths::resolve_in(&root, &dir.join("home")).unwrap();
    let registry = crate::registry::Registry::builtin();
    let entry = registry.entry("lsp:clangd").unwrap();
    let tools = crate::test_support::setup::tool_env(None);
    let settings = trace_core::repo_settings::RepoSettings::default();
    let files = [("src/a.cc", Language::Cpp), ("src/b.c", Language::C)];
    let facts = |_: &str| -> Option<&FileFacts> { None };
    let platform = Platform::current();
    let vars = EnvVars::default();
    let cx = SetupContext {
        repo: &repo,
        entry,
        languages: &[Language::C, Language::Cpp],
        files: &files,
        facts: &facts,
        settings: &settings,
        tools: &tools,
        platform: &platform,
        vars: &vars,
        report_only: true,
    };
    let err = Hooks.preflight(&cx).unwrap_err();
    let items = match err {
        SetupError::Several { items } => items,
        one => vec![one],
    };
    let approval: Vec<&SetupError> = items
        .iter()
        .filter(|e| matches!(e, SetupError::BuildNotAllowed { .. }))
        .collect();
    assert_eq!(approval.len(), 1, "{items:?}");
    assert!(
        matches!(
            approval[0],
            SetupError::BuildNotAllowed {
                language: Language::Cpp,
                ..
            }
        ),
        "{approval:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn wait() -> IndexWait {
    IndexWait {
        grace: Duration::from_secs(10),
        stall: Duration::from_secs(120),
        limit: Duration::from_secs(600),
    }
}

fn shards(files: u64) -> ShardStats {
    ShardStats {
        files,
        bytes: files * 100,
        newest: None,
    }
}

/// Rule: the background index wait ends when the index ends, when it stalls (no progress
/// change and no shard change for the stall time) or at the limit (`ServerTimeout`); it
/// never waits without a bound.
#[test]
fn rule_background_index_wait_stops_on_stall() {
    use super::IndexProgress::{Ended, NotBegun, Running};
    let w = wait();
    let t0 = Instant::now();
    let s = |secs: u64| t0 + Duration::from_secs(secs);

    // Ended: ready at once.
    let mut watch = IndexWatch::new(t0, shards(0));
    assert_eq!(watch.observe(s(1), Running, shards(0), &w), IndexVerdict::Wait);
    assert_eq!(watch.observe(s(30), Running, shards(5), &w), IndexVerdict::Wait);
    assert_eq!(watch.observe(s(31), Ended, shards(5), &w), IndexVerdict::Ready);

    // Stall: the progress stays open, the shards stopped changing.
    let mut watch = IndexWatch::new(t0, shards(0));
    assert_eq!(watch.observe(s(1), Running, shards(0), &w), IndexVerdict::Wait);
    assert_eq!(watch.observe(s(60), Running, shards(40), &w), IndexVerdict::Wait);
    assert_eq!(watch.observe(s(179), Running, shards(40), &w), IndexVerdict::Wait);
    assert_eq!(watch.observe(s(180), Running, shards(40), &w), IndexVerdict::Stalled);

    // Steady progress beyond the limit: the timeout error, never an endless wait.
    let mut watch = IndexWatch::new(t0, shards(0));
    assert_eq!(watch.observe(s(1), Running, shards(1), &w), IndexVerdict::Wait);
    let mut verdict = IndexVerdict::Wait;
    for i in 1..=12 {
        verdict = watch.observe(s(i * 50), Running, shards(i + 1), &w);
        if verdict != IndexVerdict::Wait {
            break;
        }
    }
    assert_eq!(verdict, IndexVerdict::TimedOut);

    // Nothing begun and nothing written within the grace: nothing to index.
    let mut watch = IndexWatch::new(t0, shards(3));
    assert_eq!(watch.observe(s(5), NotBegun, shards(3), &w), IndexVerdict::Wait);
    assert_eq!(watch.observe(s(10), NotBegun, shards(3), &w), IndexVerdict::Ready);

    // Shards written although no begin was seen: waited for like a running index.
    let mut watch = IndexWatch::new(t0, shards(0));
    assert_eq!(watch.observe(s(5), NotBegun, shards(2), &w), IndexVerdict::Wait);
    assert_eq!(watch.observe(s(20), NotBegun, shards(2), &w), IndexVerdict::Wait);
    assert_eq!(watch.observe(s(125), NotBegun, shards(2), &w), IndexVerdict::Stalled);
}

/// Rule: the index progress is the last begin / end of clangd's background index token;
/// other tokens and notifications do not count.
#[test]
fn rule_background_index_progress_follows_its_token() {
    let progress = |token: &str, kind: &str| {
        ("$/progress".to_string(), json!({"token": token, "value": {"kind": kind}}))
    };
    let other = ("textDocument/inactiveRegions".to_string(), json!({}));
    assert_eq!(background_index_progress(std::slice::from_ref(&other)), IndexProgress::NotBegun);
    let running = [progress(BACKGROUND_INDEX_TOKEN, "begin"), progress("other", "end"), other];
    assert_eq!(background_index_progress(&running), IndexProgress::Running);
    let ended = [
        progress(BACKGROUND_INDEX_TOKEN, "begin"),
        progress(BACKGROUND_INDEX_TOKEN, "end"),
    ];
    assert_eq!(background_index_progress(&ended), IndexProgress::Ended);
    let again = [
        progress(BACKGROUND_INDEX_TOKEN, "begin"),
        progress(BACKGROUND_INDEX_TOKEN, "end"),
        progress(BACKGROUND_INDEX_TOKEN, "begin"),
    ];
    assert_eq!(background_index_progress(&again), IndexProgress::Running);
}

/// Rule: clangd runs its background index (persisted outside the repository), reports
/// inactive preprocessor regions, and its start does not wait on progress (the bounded
/// warm-up does).
#[test]
fn rule_clangd_background_index_and_inactive_regions_are_enabled() {
    let registry = crate::registry::Registry::builtin();
    let entry = registry.entry("lsp:clangd").unwrap();
    assert!(entry.args.iter().any(|a| a == "--background-index=true"), "{:?}", entry.args);
    assert!(entry.args.iter().any(|a| a == "--compile-commands-dir={outside}/cdb"));
    assert_eq!(entry.ready, crate::registry::ReadySpec::None);
    assert!(Hooks.answer_policy().inactive_regions);
}

fn cdata_for(mode: BuildSystem, state_dir: &Path) -> CData {
    CData {
        mode,
        compiler: gcc(),
        repo_root: PathBuf::from("/r"),
        cmake: None,
        ninja: None,
        meson: None,
        toolchain_file: None,
        vcpkg_installed: None,
        package_hint: None,
        empty_submodules: false,
        platform: Platform::current(),
        state_dir: state_dir.to_path_buf(),
        index_limit: Duration::from_secs(600),
        sources: SourceMemo::default(),
    }
}

/// Rule: a C/C++ source the compile database of a real build does not list is outside the
/// build (no request is made for it); headers, listed sources and trace's generated
/// database (every file) never are.
#[test]
fn rule_source_outside_compile_database_is_outside_build() {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("trace-fixtures-cdb-{}", uuid::Uuid::new_v4().simple()));
    let workspace = dir.join("tree");
    let state = dir.join("state");
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    std::fs::create_dir_all(state.join("cdb")).unwrap();
    let build = dir.join("state").join("cbuild");
    let database = json!([
        {"directory": build.display().to_string(),
         "file": workspace.join("src").join("a.cc").display().to_string()},
        {"directory": workspace.join("src").display().to_string(), "file": "../lib/./b.c"},
        {"directory": build.display().to_string(),
         "file": build.join("generated.cc").display().to_string()}
    ]);
    let sources = database_sources(&database, &workspace);
    assert_eq!(
        sources.iter().cloned().collect::<Vec<_>>(),
        vec![source_key("lib/b.c"), source_key("src/a.cc")],
        "relative entries resolve against their directory; build-directory files are no repository files"
    );
    let list: Vec<String> = sources.into_iter().collect();
    std::fs::write(state.join("cdb").join(SOURCES_FILE), serde_json::to_vec(&list).unwrap()).unwrap();

    let prepared = |mode: BuildSystem| Prepared {
        data: Some(Arc::new(cdata_for(mode, &state))),
        ..Prepared::default()
    };
    let cmake = prepared(BuildSystem::CMake(String::new()));
    assert_eq!(Hooks.outside_build_file("src/a.cc", &cmake), None);
    assert_eq!(Hooks.outside_build_file("lib/b.c", &cmake), None);
    assert_eq!(Hooks.outside_build_file("test/fuzzing/fuzz.cc", &cmake).as_deref(), Some(NOT_IN_DATABASE));
    assert_eq!(Hooks.outside_build_file("include/x.h", &cmake), None, "headers never");
    assert_eq!(Hooks.outside_build_file("include/x.hpp", &cmake), None, "headers never");
    let generated = prepared(BuildSystem::None);
    assert_eq!(Hooks.outside_build_file("test/fuzzing/fuzz.cc", &generated), None);
    // No list (nothing prepared yet): the file is analysed as usual.
    let empty = dir.join("empty-state");
    let unprepared = Prepared {
        data: Some(Arc::new(cdata_for(BuildSystem::CMake(String::new()), &empty))),
        ..Prepared::default()
    };
    assert_eq!(Hooks.outside_build_file("test/fuzzing/fuzz.cc", &unprepared), None);
    assert!(is_source_path("a/B.CPP") && is_source_path("x.cxx") && !is_source_path("x.inl"));
    let _ = std::fs::remove_dir_all(&dir);
}
