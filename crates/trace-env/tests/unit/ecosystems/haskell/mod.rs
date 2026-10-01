use super::*;

const PLAN: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/rule-haskell-cabal-plan/plan.json"
));

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("haskell-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn executable(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn rule_hls_binary_matches_ghc() {
    let dir = temp("hls");
    let platform = Platform::current();
    let ghcup = Ghcup {
        base: dir.join("ghcup"),
        bin: dir.join("ghcup").join("bin"),
    };
    for ghc in ["9.8.4", "9.10.3"] {
        let name = platform.exe(&format!("haskell-language-server-{ghc}"));
        executable(&ghcup.base.join("hls/2.14.0.0/bin").join(&name));
        executable(&ghcup.bin.join(&name));
    }
    let dirs = hls_dirs(Some(&ghcup), "2.14.0.0", &[], &EnvVars::default());
    let exe = find_hls(&dirs, "9.10.3", &platform).unwrap();
    let file = exe.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(file, platform.exe("haskell-language-server-9.10.3"));
    // The pinned release directory comes first.
    assert!(exe.starts_with(ghcup.base.join("hls").join("2.14.0.0")), "{}", exe.display());
    assert_eq!(hls_version_of(&exe).as_deref(), Some("2.14.0.0"));
    // No binary for another GHC is ever taken.
    assert!(find_hls(&dirs, "9.12.2", &platform).is_none());
    let _ = fs::remove_dir_all(&dir);
}

/// cabal on Windows without CABAL_DIR and without a legacy directory: config in
/// `%APPDATA%\cabal`, store and package lists in `%LOCALAPPDATA%\cabal` (XDG layout on
/// Windows' known folders). A legacy `%APPDATA%\cabal` with a store stays the single dir;
/// CABAL_DIR always wins.
#[test]
fn rule_cabal_windows_layout_without_cabal_dir() {
    let root = temp("winlayout");
    let (appdata, local, profile) = (root.join("Roaming"), root.join("Local"), root.join("User"));
    fs::create_dir_all(appdata.join("cabal")).unwrap();
    fs::write(appdata.join("cabal").join("config"), "jobs: $ncpus\n").unwrap();
    fs::create_dir_all(&profile).unwrap();
    let platform = Platform {
        os: Os::Windows,
        arch: crate::os::Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    };
    let mut vars = EnvVars::default();
    vars.set("APPDATA", appdata.as_os_str());
    vars.set("LOCALAPPDATA", local.as_os_str());
    vars.set("USERPROFILE", profile.as_os_str());
    let files = [("Main.hs", Language::Haskell)];
    let detect_with = |vars: &EnvVars| {
        let cx = DetectContext {
            root: &root,
            platform: &platform,
            vars,
            env_override: None,
            forbidden: &[],
            files: &files,
        };
        detect(&cx)
    };
    let s = detect_with(&vars);
    assert!(s.cabal_xdg);
    assert_eq!(s.store_dir, Some(local.join("cabal").join("store")));
    assert_eq!(s.package_cache, Some(local.join("cabal").join("packages")));
    // An older cabal's single directory (it holds the store) stays the cabal directory.
    fs::create_dir_all(appdata.join("cabal").join("store")).unwrap();
    let s = detect_with(&vars);
    assert!(!s.cabal_xdg);
    assert_eq!(s.store_dir, Some(appdata.join("cabal").join("store")));
    // CABAL_DIR wins.
    vars.set("CABAL_DIR", root.join("cabal-dir").as_os_str());
    assert_eq!(detect_with(&vars).store_dir, Some(root.join("cabal-dir").join("store")));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn rule_stack_yaml_without_stack_work_uses_cabal() {
    let root = temp("stack");
    fs::write(root.join("stack.yaml"), "resolver: lts-18.15\npackages:\n- .\n").unwrap();
    fs::write(root.join("app.cabal"), "cabal-version: 2.4\nname: app\nversion: 0.1\n").unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    // A Windows platform without any variables: no Stack can be found on any machine.
    let platform = Platform {
        os: Os::Windows,
        arch: crate::os::Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    };
    let vars = EnvVars::default();
    let files = [("src/Main.hs", Language::Haskell)];
    let cx = DetectContext {
        root: &root,
        platform: &platform,
        vars: &vars,
        env_override: None,
        forbidden: &[],
        files: &files,
    };
    let (project, pending) = project_layout(&cx);
    let project = project.unwrap();
    assert_eq!(project.tool, BuildTool::Cabal);
    assert_eq!(project.packages, [""]);
    assert!(pending.is_empty());
    // With a Stack build directory (and no Stack installed) it is still cabal.
    fs::create_dir_all(root.join(".stack-work")).unwrap();
    assert_eq!(project_layout(&cx).0.unwrap().tool, BuildTool::Cabal);
    // A project hie.yaml with a stack cradle is honoured.
    fs::write(root.join("hie.yaml"), "cradle:\n  stack:\n").unwrap();
    assert_eq!(project_layout(&cx).0.unwrap().tool, BuildTool::Stack);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn rule_plan_json_closure_checked_in_store() {
    let plan: Value = serde_json::from_str(PLAN).unwrap();
    let dir = temp("store");
    let store = dir.join("store");
    let cdir = store.join("ghc-9.10.3-b42a");
    fs::create_dir_all(cdir.join("aeson-2.2.3.0-abc")).unwrap();
    fs::create_dir_all(store.join("ghc-9.8.4")).unwrap();
    let dirs = store_compiler_dirs(&store, "ghc-9.10.3");
    assert_eq!(dirs, std::slice::from_ref(&cdir));
    let (missing, units) = plan_missing(&plan, &dirs);
    // `text` (needed through aeson's library component) is missing; `tasty` is in the plan
    // but outside the closure of the local units, so it is not required.
    assert_eq!(missing, ["text-2.1.2"]);
    assert_eq!(units, 2);
    fs::create_dir_all(cdir.join("text-2.1.2-def")).unwrap();
    assert_eq!(plan_missing(&plan, &dirs).0, Vec::<String>::new());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn rule_template_haskell_pragmas_run_code() {
    assert!(pragma_runs_code("{-# LANGUAGE TemplateHaskell #-}"));
    assert!(pragma_runs_code("{-# LANGUAGE OverloadedStrings, QuasiQuotes #-}"));
    assert!(pragma_runs_code("{-# language TemplateHaskell #-}"));
    assert!(pragma_runs_code("{-# OPTIONS_GHC -Wall -XTemplateHaskell #-}"));
    assert!(pragma_runs_code("{-# OPTIONS_GHC -fplugin=Some.Plugin #-}"));
    assert!(!pragma_runs_code("{-# LANGUAGE TemplateHaskellQuotes #-}"));
    assert!(!pragma_runs_code("{-# LANGUAGE NoTemplateHaskell #-}"));
    assert!(!pragma_runs_code("{-# INLINE templateHaskell #-}"));
    assert!(uses_template_haskell("{-# LANGUAGE TemplateHaskell #-}\nmodule M where\n\nx :: Int\nx = 1\n"));
    assert!(!uses_template_haskell("module M where\n\n-- TemplateHaskell in a comment\nx :: Int\nx = 1\n"));
}

#[test]
fn rule_cabal_project_fields_are_read_structurally() {
    let fields = read_cabal_fields(
        "-- comment\npackages: ./\n          sub/*.cabal\nwith-compiler: ghc-9.6.7\n\npackage app\n  ghc-options: -Wall\n",
    );
    assert_eq!(fields["packages"], "./\nsub/*.cabal");
    assert_eq!(compiler_version(&fields["with-compiler"]).unwrap().text, "9.6.7");
    assert!(!fields.contains_key("ghc-options"));
    assert!(package_entry_matches("./", "", &["app.cabal".into()]));
    assert!(package_entry_matches("sub/*.cabal", "sub", &["sub.cabal".into()]));
    assert!(!package_entry_matches("sub/*.cabal", "examples", &["ex.cabal".into()]));
    assert!(package_entry_matches("libs/*", "libs/core", &[]));
}
