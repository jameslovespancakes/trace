use super::*;
use crate::registry::{Recipe, Registry};
use trace_core::facts::FileFacts;
use trace_env::os::Os;

#[test]
fn rule_cargo_needs_build_approval() {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("trace-fixtures-rust-{}", uuid::Uuid::new_v4().simple()));
    let root = dir.join("repo");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"app\"\nversion = \"0.1.0\"\n").unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    let repo = trace_core::paths::RepoPaths::resolve_in(&root, &dir.join("home")).unwrap();
    let registry = Registry::builtin();
    let entry = registry.entry("lsp:rust-analyzer").unwrap();
    let tools = crate::test_support::setup::tool_env(None);
    let settings = trace_core::repo_settings::RepoSettings::default();
    let files = [("src/main.rs", Language::Rust)];
    let facts = |_: &str| -> Option<&FileFacts> { None };
    // A Windows platform without Program Files variables: no standard install location
    // can hold a toolchain, whatever machine runs the test.
    let platform = Platform {
        os: Os::Windows,
        arch: trace_env::os::Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    };
    let vars = EnvVars::default();
    let cx = SetupContext {
        repo: &repo,
        entry,
        languages: &[Language::Rust],
        files: &files,
        facts: &facts,
        settings: &settings,
        tools: &tools,
        platform: &platform,
        vars: &vars,
        report_only: true,
    };
    let err = Hooks.preflight(&cx).unwrap_err();
    let text = err.lines().join("\n");
    assert!(
        text.contains("Rust needs Cargo, which runs this project's build scripts and procedural macros."),
        "{text}"
    );
    assert!(text.contains("Only allow this for projects you trust: trace index --allow-build"));
    // Every independent failure is listed at once (toolchain, server, approval).
    assert_eq!(err.kind(), "setup_incomplete");
    assert!(text.contains("Rust is not installed."), "{text}");
    assert!(text.contains("The Rust language server is not installed."), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rule_rust_src_missing_is_an_error() {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("trace-fixtures-rustsrc-{}", uuid::Uuid::new_v4().simple()));
    let root = dir.join("repo");
    let home = dir.join("user");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
    let tc = home.join(".rustup/toolchains/stable-x86_64-unknown-linux-gnu");
    std::fs::create_dir_all(tc.join("bin")).unwrap();
    std::fs::create_dir_all(tc.join("lib/rustlib")).unwrap();
    std::fs::write(tc.join("bin/rustc"), "").unwrap();
    std::fs::write(tc.join("bin/cargo"), "").unwrap();
    std::fs::write(
        tc.join("lib/rustlib/multirust-channel-manifest.toml"),
        "[pkg.rustc]\nversion = \"1.95.0 (x 2026-01-01)\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(home.join(".rustup")).unwrap();
    std::fs::write(home.join(".rustup/settings.toml"), "default_toolchain = \"stable\"\n").unwrap();
    let repo = trace_core::paths::RepoPaths::resolve_in(&root, &dir.join("cache")).unwrap();
    let registry = Registry::builtin();
    let entry = registry.entry("lsp:rust-analyzer").unwrap();
    let tools = crate::test_support::setup::tool_env(None);
    let settings = trace_core::repo_settings::RepoSettings::default();
    let files = [("main.rs", Language::Rust)];
    let facts = |_: &str| -> Option<&FileFacts> { None };
    let platform = Platform {
        os: Os::Linux,
        arch: trace_env::os::Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    };
    let home_text = home.display().to_string();
    let vars = EnvVars::from_pairs(&[("HOME", home_text.as_str())]);
    let cx = SetupContext {
        repo: &repo,
        entry,
        languages: &[Language::Rust],
        files: &files,
        facts: &facts,
        settings: &settings,
        tools: &tools,
        platform: &platform,
        vars: &vars,
        report_only: true,
    };
    let text = Hooks.preflight(&cx).unwrap_err().lines().join("\n");
    assert!(
            text.contains("Rust needs Rust's standard library source, which is not installed. Install it: rustup component add rust-src --toolchain stable-x86_64-unknown-linux-gnu and run trace again."),
            "{text}"
        );
    assert!(!text.contains("--allow-build"), "loose files need no approval: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rule_rust_msrv_above_toolchain_is_an_error() {
    let e = cargo_metadata_error(
        "error: rustc 1.92.0 is not supported by the following packages:\n  grep@0.4.1 requires rustc 1.96",
        Path::new("log.txt"),
    );
    assert_eq!(e.kind(), "build_failed");
    let offline = cargo_metadata_error(
            "error: failed to download `encoding_rs_io v0.1.8`\nattempting to make an HTTP request, but --offline was specified",
            Path::new("log.txt"),
        );
    assert_eq!(
            offline.to_string(),
            "Dependencies not installed. Install your project's dependencies (cargo fetch)\n       or point trace to them: trace index --env <path>"
        );
    // The MSRV check itself happens before launch (trace-env rule
    // `rule_rust_msrv_above_toolchain_is_too_old`); its message:
    let msrv = SetupError::ToolchainVersion {
        language: Language::Rust,
        needs: "Rust 1.96 or newer".into(),
        source: "rust-version in Cargo.toml".into(),
        tool: "Rust".into(),
        found: "1.92.0".into(),
        install: "Update it with rustup update".into(),
    };
    assert_eq!(
            msrv.to_string(),
            "This Rust project needs Rust 1.96 or newer (rust-version in Cargo.toml); the installed Rust is 1.92.0. Update it with rustup update and run trace again."
        );
}

struct FakeSession {
    requests: Vec<(String, Value)>,
}

impl FnTypeSession for FakeSession {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, crate::SemanticError> {
        self.requests.push((method.to_string(), params));
        Ok(json!({"name": "main", "expansion": "fn main() { tokio::runtime::Builder::new_multi_thread(); }"}))
    }
    fn uri_of(&self, rel: &str) -> Result<String, crate::SemanticError> {
        Ok(format!("file:///ws/{rel}"))
    }
    fn read_location(&mut self, _uri: &str) -> Option<(PathBuf, Vec<u8>)> {
        None
    }
}

#[test]
fn rule_attribute_macro_expansion_is_delivered_under_approval() {
    let facts = FileFacts::default();
    let source = b"#[tokio::main]\nasync fn main() {}\n#[inline]\nfn f() {}\n";
    let file = SemanticFile {
        path: "app/src/main.rs",
        language: Language::Rust,
        hash: trace_core::Hash32::of(source),
        source,
        facts: &facts,
    };
    let approved = Prepared {
        data: Some(Arc::new(RustData {
            expand: true,
            proc_macro_dirs: vec!["app".into()],
            ..RustData::default()
        })),
        ..Prepared::default()
    };
    let mut session = FakeSession { requests: Vec::new() };
    let budget = AtomicU32::new(rust_analyzer::MAX_EXPANSIONS_PER_RUN);
    let (expanded, bounded) = expansions_for(&file, &approved, &mut session, &budget);
    assert_eq!(expanded.len(), 1);
    assert!(expanded[0].text.contains("new_multi_thread"));
    assert_eq!(expanded[0].span.start, 0);
    assert!(bounded.is_none());
    assert_eq!(session.requests.len(), 1);
    assert_eq!(session.requests[0].0, "rust-analyzer/expandMacro");
    assert_eq!(budget.load(Ordering::SeqCst), rust_analyzer::MAX_EXPANSIONS_PER_RUN - 1);

    // Without approval (no build), or outside a crate that uses proc macros: no request.
    let not_approved = Prepared {
        data: Some(Arc::new(RustData {
            expand: false,
            proc_macro_dirs: vec!["app".into()],
            ..RustData::default()
        })),
        ..Prepared::default()
    };
    let mut session = FakeSession { requests: Vec::new() };
    assert!(expansions_for(&file, &not_approved, &mut session, &budget)
        .0
        .is_empty());
    let other_crate = Prepared {
        data: Some(Arc::new(RustData {
            expand: true,
            proc_macro_dirs: vec!["lib".into()],
            ..RustData::default()
        })),
        ..Prepared::default()
    };
    assert!(expansions_for(&file, &other_crate, &mut session, &budget)
        .0
        .is_empty());
    assert!(session.requests.is_empty());

    // The run's budget bounds the requests.
    let empty = AtomicU32::new(0);
    let (none, bounded) = expansions_for(&file, &approved, &mut session, &empty);
    assert!(none.is_empty());
    assert_eq!(bounded.unwrap().kind, "bounded");
}

/// Rule: the native preflights READ manifests, lockfiles and installed dependencies inside
/// the repository (read context: only the user's protected roots are off limits) while
/// toolchains are searched through the execute context, which never trusts anything in the
/// repository. A Go module cache inside the repository is read: its modules count as
/// installed.
#[test]
fn rule_native_preflight_reads_manifests_inside_the_repository() {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("trace-fixtures-readctx-{}", uuid::Uuid::new_v4().simple()));
    let root = dir.join("repo");
    let goroot = dir.join("go");
    let cache = root.join(".modcache");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("go.mod"),
        "module example.com/app\n\ngo 1.22\n\nrequire example.com/lib v1.0.0\n",
    )
    .unwrap();
    std::fs::write(root.join("main.go"), "package main\n").unwrap();
    std::fs::create_dir_all(cache.join("example.com").join("lib@v1.0.0")).unwrap();
    std::fs::create_dir_all(goroot.join("bin")).unwrap();
    std::fs::create_dir_all(goroot.join("src/runtime")).unwrap();
    std::fs::write(goroot.join("bin/go"), "").unwrap();
    std::fs::write(goroot.join("VERSION"), "go1.26.1\n").unwrap();
    let repo = trace_core::paths::RepoPaths::resolve_in(&root, &dir.join("home")).unwrap();
    let registry = Registry::builtin();
    let entry = registry.entry("lsp:gopls").unwrap();
    // The repository is never executed from (as `ToolEnv::discover` sets it up).
    let mut tools = crate::test_support::setup::tool_env(None);
    tools.forbidden_roots = vec![repo.root.clone()];
    let mut settings = trace_core::repo_settings::RepoSettings::default();
    settings.env.insert("go".into(), goroot.clone());
    let files = [("main.go", Language::Go)];
    let facts = |_: &str| -> Option<&FileFacts> { None };
    let platform = Platform {
        os: Os::Linux,
        arch: trace_env::os::Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    };
    let cache_text = repo.root.join(".modcache").display().to_string();
    let vars = EnvVars::from_pairs(&[("GOENV", "off"), ("GOMODCACHE", cache_text.as_str())]);
    let cx = SetupContext {
        repo: &repo,
        entry,
        languages: &[Language::Go],
        files: &files,
        facts: &facts,
        settings: &settings,
        tools: &tools,
        platform: &platform,
        vars: &vars,
        report_only: true,
    };
    // The two contexts: the execute context refuses the repository, the read context not.
    let readable: Vec<PathBuf> = Vec::new();
    let inside = repo.root.join("go.mod");
    assert!(!detect_context(&cx, EcosystemId::Go).allowed(&inside));
    assert!(read_context(&cx, EcosystemId::Go, &readable).allowed(&inside));
    let text = super::super::go::Hooks.preflight(&cx).unwrap_err().lines().join("\n");
    assert!(text.contains("The Go language server is not installed."), "{text}");
    assert!(!text.contains("Dependencies not installed"), "the in-repository module cache is read: {text}");
    // Negative: the module really missing from that cache is the dependency error.
    std::fs::remove_dir_all(cache.join("example.com")).unwrap();
    let text = super::super::go::Hooks.preflight(&cx).unwrap_err().lines().join("\n");
    assert!(text.contains("Dependencies not installed"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rule_default_native_records_install_without_question() {
    let registry = crate::test_support::setup::builtin_strict().unwrap();
    for language in [Language::Rust, Language::Go, Language::C, Language::Cpp] {
        assert!(language.is_default());
        let entry = registry.entry_for(language).unwrap();
        let install = entry.install.as_ref().unwrap();
        assert!(install.licence_gate.is_none(), "{} installs without a question", entry.id);
        assert!(entry.runtime.is_empty(), "{}: native servers need no runtime", entry.id);
        match &install.recipe {
            Recipe::Archive {
                artifacts,
                executables,
            } => {
                assert!(!executables.is_empty());
                for key in ["windows-x86_64", "linux-x86_64", "macos-x86_64", "macos-aarch64"] {
                    assert!(
                        artifacts.iter().any(|a| a.platform == key && a.sha256.len() == 64),
                        "{} has a pinned artifact for {key}",
                        entry.id
                    );
                }
            }
            Recipe::GoInstall { module, versions } => {
                assert_eq!(module, "golang.org/x/tools/gopls");
                assert_eq!(versions.len(), 3);
                assert!(versions
                    .iter()
                    .all(|v| v.h1.starts_with("h1:") && v.version.starts_with('v')));
            }
            other => panic!("{}: unexpected recipe {other:?}", entry.id),
        }
    }
    let ra = registry.entry("lsp:rust-analyzer").unwrap();
    let Recipe::Archive { artifacts, .. } = &ra.install.as_ref().unwrap().recipe else {
        panic!("archive");
    };
    for key in ["windows-aarch64", "linux-aarch64", "linux-x86_64-musl"] {
        assert!(artifacts.iter().any(|a| a.platform == key));
    }
    // Repository-dependent extras never fail when nothing is found: they are skipped.
    let _ = Hooks.install_extras(None);
    let _ = super::super::go::Hooks.install_extras(None);
}
