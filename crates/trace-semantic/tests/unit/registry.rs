use super::*;

const ALL_PLATFORMS: [&str; 6] = [
    "windows-x86_64",
    "windows-aarch64",
    "linux-x86_64",
    "linux-aarch64",
    "macos-x86_64",
    "macos-aarch64",
];

/// A minimal valid lsp entry for rule tests (independent of the language files).
fn test_entry(id: &str, language: Language) -> BackendEntry {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "kind": "lsp",
        "languages": [language],
        "language_ids": ["x"],
        "server": {"name": "x", "version": "1", "license": "MIT"},
        "executable": {"from": "tool", "tool": "x", "path": "bin/x"},
        "install": {
            "id": "x", "version": "1", "license": "MIT", "display": "X language server",
            "product": "X", "recipe": "archive",
            "artifacts": [{"platform": "any", "url": "https://example.com/x.zip",
                           "sha256": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"}],
            "executables": ["bin/x"]
        }
    }))
    .unwrap()
}

/// Every embedded file parses as schema 2 and the whole registry validates (unique ids,
/// one entry per language, known placeholders, pinned https artifacts, known runtimes).
#[test]
fn rule_registry_files_are_valid() {
    for (stem, text) in BUILTIN_FILES {
        let file: RegistryFile = serde_json::from_str(text).unwrap_or_else(|e| panic!("{stem}: {e}"));
        assert_eq!(file.schema, REGISTRY_VERSION, "{stem}");
    }
    let strict = crate::test_support::setup::builtin_strict().unwrap();
    assert_eq!(strict, Registry::builtin());
    for e in &strict.backends {
        assert!(!e.safety.is_empty(), "{}: safety settings documented", e.id);
    }
    for id in ["node", "jdk", "dotnet"] {
        assert!(strict.runtime(id).is_some(), "runtime {id}");
    }
}

/// PLAN decision 10: the default languages install without a question on every OS: each
/// has an entry with an install record and no licence gate; archive records and the
/// runtimes they run on cover all six platforms (or "any"), except where upstream ships
/// no build (the server's preflight then reports it as unavailable with advice).
#[test]
fn rule_default_records_cover_all_platforms() {
    // (install id, platform) without an upstream build (research platform-speed §2.4).
    const NO_UPSTREAM_BUILD: &[(&str, &str)] = &[("clangd", "linux-aarch64"), ("clangd", "windows-aarch64")];
    let r = Registry::builtin();
    let covers = |id: &str, artifacts: &[Artifact]| {
        for p in ALL_PLATFORMS {
            let ok = artifacts.iter().any(|a| a.platform == p || a.platform == "any")
                || NO_UPSTREAM_BUILD.contains(&(id, p));
            assert!(ok, "{id} has no artifact for {p}");
        }
    };
    for runtime in &r.runtimes {
        let Recipe::Archive { artifacts, .. } = &runtime.recipe else {
            panic!("runtime {} must be an archive", runtime.id)
        };
        covers(&runtime.id, artifacts);
        assert!(runtime.licence_gate.is_none(), "{}", runtime.id);
    }
    for language in trace_core::DEFAULT_LANGUAGES {
        let entry = r
            .entry_for(language)
            .unwrap_or_else(|| panic!("{language}: registry entry"));
        let install = entry
            .install
            .as_ref()
            .unwrap_or_else(|| panic!("{language}: install record"));
        assert!(install.licence_gate.is_none(), "{language}: default servers install without a question");
        match &install.recipe {
            Recipe::Archive { artifacts, .. } => covers(&install.id, artifacts),
            Recipe::Npm { .. } | Recipe::GoInstall { .. } => {}
            other => panic!("{language}: unexpected default recipe {other:?}"),
        }
    }
}

/// PLAN decision 11: servers run on trace-managed runtimes only: Node scripts declare the
/// `node` runtime, runtime programs name a runtime of `_runtimes.json` they declare.
#[test]
fn rule_server_runtime_is_always_trace_managed() {
    let r = Registry::builtin();
    for e in &r.backends {
        match &e.executable {
            ExecutableSpec::NodeScript { .. } => {
                assert!(e.runtime.iter().any(|x| x == "node"), "{}", e.id)
            }
            ExecutableSpec::Runtime { runtime, .. } => {
                assert!(e.runtime.contains(runtime), "{}", e.id);
                assert!(r.runtime(runtime).is_some(), "{}", e.id);
            }
            _ => {}
        }
    }
}

#[test]
fn validate_rejects_unsafe_entries() {
    let base = || Registry {
        backends: vec![test_entry("lsp:a", Language::Go), test_entry("lsp:b", Language::Haskell)],
        runtimes: Vec::new(),
    };
    assert_eq!(base().validate(), Ok(()));
    let mut r = base();
    r.backends.push(r.backends[0].clone());
    assert!(r.validate().unwrap_err().contains("duplicate"));
    let mut r = base();
    r.backends[1].languages = vec![Language::Go];
    assert!(r.validate().unwrap_err().contains("served by both"));
    let mut r = base();
    r.backends[0].args.push("{nope}".into());
    assert!(r.validate().unwrap_err().contains("unknown placeholder"));
    let mut r = base();
    if let Some(InstallSpec {
        recipe: Recipe::Archive { artifacts, .. },
        ..
    }) = r.backends[0].install.as_mut()
    {
        artifacts[0].sha256 = String::new();
    }
    assert!(r.validate().unwrap_err().contains("sha256"));
    let mut r = base();
    if let Some(InstallSpec {
        recipe: Recipe::Archive { artifacts, .. },
        ..
    }) = r.backends[0].install.as_mut()
    {
        artifacts[0].platform = "solaris-sparc".into();
    }
    assert!(r.validate().unwrap_err().contains("platform"));
    let mut r = base();
    r.backends[0].workspace.mode = WorkspaceMode::Mirror;
    assert!(r.validate().unwrap_err().contains("requires_build"));
    let mut r = base();
    r.backends[0].runtime = vec!["node".into()];
    assert!(r.validate().unwrap_err().contains("unknown runtime"));
    let mut r = base();
    if let Some(i) = r.backends[0].install.as_mut() {
        i.licence_gate = Some(LicenceGate {
            url: "http://example.com".into(),
            summary: "s".into(),
        });
    }
    assert!(r.validate().unwrap_err().contains("licence gate"));
}

#[test]
fn load_dir_reads_an_override_directory() {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join("trace-fixtures-registry")
        .join(format!("load-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = RegistryFile {
        schema: REGISTRY_VERSION,
        backends: vec![test_entry("lsp:a", Language::Go)],
        runtimes: Vec::new(),
    };
    std::fs::write(dir.join("all.json"), serde_json::to_vec_pretty(&file).unwrap()).unwrap();
    let loaded = Registry::load_dir(&dir).unwrap();
    assert!(loaded.entry("lsp:a").is_some());
    assert_eq!(loaded.entry_for(Language::Go).map(|e| e.id.as_str()), Some("lsp:a"));
    std::fs::write(dir.join("bad.json"), b"{\"schema\": 1, \"backends\": []}").unwrap();
    assert!(Registry::load_dir(&dir).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn relative_paths_stay_inside_the_tools_dir() {
    assert!(safe_relative("gopls/bin").is_some());
    assert!(safe_relative("npm-lsp/node_modules/x/cli.js").is_some());
    for bad in ["", "/abs", "C:/x", "a/../b", "a//b", "./a", "\\\\server\\x"] {
        assert!(safe_relative(bad).is_none(), "{bad}");
    }
    assert!(safe_prefix("jdk-21.0.12.1+1/Contents/Home/"));
    assert!(!safe_prefix("../x/"));
    assert_eq!(placeholders("{tool:jdtls}/x-{snapshot}"), vec!["tool:jdtls", "snapshot"]);
    for key in ["any", "windows-x86_64", "linux-aarch64-musl", "macos-aarch64", "linux-riscv64"] {
        assert!(valid_platform_key(key), "{key}");
    }
    for key in ["windows", "macos-aarch64-musl", "linux-x86_64-gnu", "Linux-x86_64"] {
        assert!(!valid_platform_key(key), "{key}");
    }
}

/// Rule: the backend files hold launch and protocol only; the resource numbers of every
/// built-in backend are its `semantic.per_backend` row, which has the heap a `{heap_mb}`
/// launch names and the grace / settle of a `progress` readiness.
#[test]
fn rule_backend_resources_are_settings() {
    for (stem, text) in BUILTIN_FILES {
        for key in [
            "\"processes\"",
            "\"max_in_flight\"",
            "\"ready_timeout_secs\"",
            "\"grace_ms\"",
            "\"settle_ms\"",
            "-Xmx",
        ] {
            assert!(!text.replace("-Xmx{heap_mb}m", "").contains(key), "{stem}: {key}");
        }
    }
    let settings = trace_core::config::defaults();
    let registry = Registry::builtin();
    for e in &registry.backends {
        let r = settings
            .semantic
            .per_backend
            .get(&e.id)
            .unwrap_or_else(|| panic!("{}", e.id));
        let heap = e.args.iter().any(|a| a.contains("{heap_mb}"));
        assert_eq!(r.heap_mb.is_some(), heap, "{} heap_mb", e.id);
        let progress = e.ready == ReadySpec::Progress;
        assert_eq!(r.ready_grace_ms.is_some() && r.ready_settle_ms.is_some(), progress, "{} ready", e.id);
    }
    assert_eq!(settings.semantic.per_backend.len(), registry.backends.len());
}
