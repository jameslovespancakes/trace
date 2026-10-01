use super::*;
use crate::registry::Registry;

#[test]
fn rule_unsupported_ghc_is_an_error() {
    let supported: Vec<String> = SUPPORTED_GHC.iter().map(|s| s.to_string()).collect();
    let e = server_error("8.10.7", &supported);
    assert_eq!(e.kind(), "unsupported");
    assert_eq!(
            e.lines().join("\n"),
            "The Haskell language server does not support GHC 8.10.7.\n       Use GHC 9.6.7, 9.8.4, 9.10.3, 9.12.2, 9.12.4 or 9.14.1 (ghcup install ghc 9.10.3) and run trace again."
        );
    // A supported GHC without its HLS binary: install it with GHCup.
    let missing = server_error("9.10.3", &supported);
    assert_eq!(missing.kind(), "toolchain_missing");
    assert!(missing.to_string().contains(
        "the Haskell language server for GHC 9.10.3, which is not installed. Install it: ghcup install hls"
    ));
    // The registry recipe lists the same GHC versions.
    let registry = Registry::builtin();
    let entry = registry.entry("lsp:haskell-language-server").unwrap();
    match &entry.install.as_ref().unwrap().recipe {
        Recipe::Ghcup {
            supported_ghc,
            hls_version,
        } => {
            assert_eq!(supported_ghc, &supported);
            assert_eq!(hls_version, PINNED_HLS);
        }
        other => panic!("unexpected recipe {other:?}"),
    }
}

#[test]
fn rule_hie_yaml_uses_cabal_cradle() {
    let files = generated_files(
        "",
        BuildTool::Cabal,
        false,
        Some("package app\n  ghc-options: -O0\noffline: False\n"),
    );
    let by_name: std::collections::BTreeMap<&str, String> = files
        .iter()
        .map(|(n, b)| (n.as_str(), String::from_utf8(b.clone()).unwrap()))
        .collect();
    assert_eq!(by_name["hie.yaml"], "cradle:\n  cabal:\n");
    let local = &by_name["cabal.project.local"];
    assert!(local.contains("package app\n  ghc-options: -O0\n"), "{local}");
    assert!(local.ends_with("offline: True\n"), "{local}");
    assert!(!local.contains("offline: False"), "{local}");
    // A project's own cabal / stack cradle is kept; Stack projects get no cabal file.
    let stack = generated_files("pkg", BuildTool::Stack, true, None);
    assert!(stack.is_empty());
    let stack = generated_files("pkg", BuildTool::Stack, false, None);
    assert_eq!(stack, vec![("pkg/hie.yaml".to_string(), b"cradle:\n  stack:\n".to_vec())]);
}

/// Rule (I-64): Template Haskell / quasi-quotes run project code while HLS compiles the
/// module, so a project using them needs the build approval (the catalogue text); a
/// module that only quotes (`TemplateHaskellQuotes`) runs nothing and needs none.
#[test]
fn rule_template_haskell_needs_build_approval() {
    let preflight_text = |source: &str| -> String {
        let dir = std::env::temp_dir()
            .join("trace-tests")
            .join(format!("trace-fixtures-haskell-th-{}", uuid::Uuid::new_v4().simple()));
        let root = dir.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Main.hs"), source).unwrap();
        let repo = trace_core::paths::RepoPaths::resolve_in(&root, &dir.join("home")).unwrap();
        let registry = Registry::builtin();
        let entry = registry.entry("lsp:haskell-language-server").unwrap();
        let tools = crate::test_support::setup::tool_env(None);
        let settings = trace_core::repo_settings::RepoSettings::default();
        let files = [("Main.hs", Language::Haskell)];
        let facts = |_: &str| -> Option<&trace_core::facts::FileFacts> { None };
        let platform = Platform {
            os: Os::Linux,
            arch: trace_env::os::Arch::X86_64,
            arch_name: "x86_64".into(),
            musl: false,
        };
        let vars = trace_env::os::EnvVars::default();
        let cx = SetupContext {
            repo: &repo,
            entry,
            languages: &[Language::Haskell],
            files: &files,
            facts: &facts,
            settings: &settings,
            tools: &tools,
            platform: &platform,
            vars: &vars,
            report_only: true,
        };
        let text = match Hooks.preflight(&cx) {
            Ok(_) => String::new(),
            Err(e) => e.lines().join("\n"),
        };
        let _ = std::fs::remove_dir_all(&dir);
        text
    };
    let approval = "Only allow this for projects you trust: trace index --allow-build";
    let th = preflight_text(
        "{-# LANGUAGE TemplateHaskell #-}\nmodule Main where\n\nmain :: IO ()\nmain = pure ()\n",
    );
    assert!(th.contains("Haskell needs GHC, which runs this project's Template Haskell code."), "{th}");
    assert!(th.contains(approval), "{th}");
    let qq = preflight_text(
        "{-# OPTIONS_GHC -XQuasiQuotes #-}\nmodule Main where\n\nmain :: IO ()\nmain = pure ()\n",
    );
    assert!(qq.contains(approval), "{qq}");
    let quotes = preflight_text(
        "{-# LANGUAGE TemplateHaskellQuotes #-}\nmodule Main where\n\nmain :: IO ()\nmain = pure ()\n",
    );
    assert!(!quotes.contains(approval), "quotes alone run nothing: {quotes}");
    let plain = preflight_text("module Main where\n\nmain :: IO ()\nmain = pure ()\n");
    assert!(!plain.contains(approval), "{plain}");
}

#[test]
fn rule_cabal_offline_error_maps_to_deps() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("hls.log");
    let texts = vec![
            "Error: [Cabal-7125] --offline was specified, hence refusing to download the package: split version 0.2.5.1."
                .to_string(),
        ];
    let e = failure_from_texts(&texts, true, "cabal", Os::Linux, &log).unwrap();
    assert_eq!(e.kind(), "deps_missing");
    assert!(e
        .to_string()
        .contains("(cabal build --only-dependencies --enable-tests all)"));
    // Through check_loaded (log message of the server).
    let prepared = Prepared::default();
    let messages = vec![(1u8, texts[0].clone())];
    let cx = LoadedContext {
        prepared: &prepared,
        log_messages: &messages,
        notifications: &[],
        diagnostics: &[],
        log: &log,
    };
    assert_eq!(Hooks.check_loaded(&cx).unwrap_err().kind(), "deps_missing");
    // A missing package list, a cradle failure, the unix package on Windows.
    let list = vec![
        "The package list for 'hackage.haskell.org' does not exist. Run 'cabal update' to download it."
            .to_string(),
    ];
    assert_eq!(failure_from_texts(&list, false, "cabal", Os::Linux, &log).unwrap(), package_list_error());
    let unix = vec!["Failed to load cradle: rejecting: unix-2.8.5.1 (conflict: os=windows)".to_string()];
    let e = failure_from_texts(&unix, true, "cabal", Os::Windows, &log).unwrap();
    assert!(e
        .to_string()
        .starts_with("This Haskell project could not be built on Windows (it uses the unix package)."));
    assert_eq!(
        failure_from_texts(&unix, true, "cabal", Os::Linux, &log)
            .unwrap()
            .kind(),
        "build_failed"
    );
    assert!(failure_from_texts(&["Processing 3/34".to_string()], false, "cabal", Os::Linux, &log).is_none());
    // `Cabal-7125` is also any failed build step: a cradle failure, never "not installed".
    let build =
        "Error: [Cabal-7125]\nFailed to build ShellCheck-0.11.0 (which is required by exe:shellcheck).";
    assert!(is_cradle_failure(build));
    let e = failure_from_texts(&[build.to_string()], true, "cabal", Os::Linux, &log).unwrap();
    assert_eq!(e.kind(), "build_failed");
}
