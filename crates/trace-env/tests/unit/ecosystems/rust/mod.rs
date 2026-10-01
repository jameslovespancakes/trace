use super::*;
use crate::test_support::write;

fn platform() -> Platform {
    Platform {
        os: Os::Linux,
        arch: os::Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    }
}

fn cx<'a>(
    root: &'a Path,
    platform: &'a Platform,
    vars: &'a EnvVars,
    files: &'a [(&'a str, Language)],
) -> DetectContext<'a> {
    DetectContext {
        root,
        platform,
        vars,
        env_override: None,
        forbidden: &[],
        files,
    }
}

/// A fake rustup toolchain `<home>/.rustup/toolchains/<name>` with rustc, cargo, the
/// channel manifest and (optionally) rust-src.
fn fake_toolchain(home: &Path, name: &str, version: &str, src: bool) {
    let dir = home.join(".rustup/toolchains").join(name);
    write(&dir, "bin/rustc", "");
    write(&dir, "bin/cargo", "");
    write(
        &dir,
        "lib/rustlib/multirust-channel-manifest.toml",
        &format!("[pkg.rustc]\nversion = \"{version} (abc 2026-01-01)\"\n"),
    );
    if src {
        write(&dir, "lib/rustlib/src/rust/library/core/src/lib.rs", "");
    }
    #[cfg(unix)]
    for exe in ["bin/rustc", "bin/cargo"] {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(exe);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn rule_env_reads_cargo_lock_packages() {
    let lock = "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"serde\",\n \"rand 0.8.5\",\n]\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.200\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"x\"\n\n[[package]]\nname = \"rand\"\nversion = \"0.8.5\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
    let pkgs = cargo_lock_packages(lock);
    assert_eq!(pkgs.len(), 3);
    assert!(!pkgs[0].registry && pkgs[1].registry);
    assert_eq!(pkgs[0].dependencies, vec!["serde", "rand 0.8.5"]);
    assert_eq!(LockPackage::resolve_dep("rand 0.8.5", &pkgs), Some(2));
    assert_eq!(LockPackage::resolve_dep("serde", &pkgs), Some(1));
}

#[test]
fn rule_rust_toolchain_from_rust_toolchain_toml() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let home = dir.path().join("home");
    write(&root, "src/main.rs", "fn main() {}\n");
    write(&root, "Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n");
    write(&root, "rust-toolchain.toml", "[toolchain]\nchannel = \"1.90.0\"\n");
    fake_toolchain(&home, "stable-x86_64-unknown-linux-gnu", "1.95.0", true);
    fake_toolchain(&home, "1.90.0-x86_64-unknown-linux-gnu", "1.90.0", true);
    write(&home, ".rustup/settings.toml", "default_toolchain = \"stable-x86_64-unknown-linux-gnu\"\n");
    let p = platform();
    let home_text = home.display().to_string();
    let vars = EnvVars::from_pairs(&[("HOME", home_text.as_str())]);
    let files = [("src/main.rs", Language::Rust)];
    let r = resolve(&cx(&root, &p, &vars, &files));
    let ToolchainStatus::Found(t) = r.status else {
        panic!("expected a toolchain: {r:?}");
    };
    assert_eq!(t.version.unwrap().text, "1.90.0");
    assert_eq!(t.origin, Origin::Pin);
    assert_eq!(t.facts["rustup_toolchain"], "1.90.0-x86_64-unknown-linux-gnu");
    assert_eq!(t.facts["pin"], "rust-toolchain.toml");
    assert!(t.facts.contains_key("rust_src"));

    // Without the pin the default toolchain is used.
    std::fs::remove_file(root.join("rust-toolchain.toml")).unwrap();
    let ToolchainStatus::Found(t) = resolve(&cx(&root, &p, &vars, &files)).status else {
        panic!("expected the default toolchain");
    };
    assert_eq!(t.version.unwrap().text, "1.95.0");

    // A pinned toolchain that is not installed is reported as such.
    write(&root, "rust-toolchain", "nightly-2020-01-01\n");
    let r = resolve(&cx(&root, &p, &vars, &files));
    assert!(matches!(r.status, ToolchainStatus::Missing { .. }));
    assert_eq!(r.pinned_missing, Some(("nightly-2020-01-01".to_string(), "rust-toolchain".to_string())));
}

#[test]
fn rule_env_toolchain_in_rustup_dir_keeps_its_rustup_name() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let home = dir.path().join("home");
    write(&root, "src/main.rs", "fn main() {}\n");
    fake_toolchain(&home, "stable-x86_64-unknown-linux-gnu", "1.92.0", true);
    fake_toolchain(&home, "1.98.1-aarch64-unknown-linux-musl", "1.98.1", false);
    write(&home, ".rustup/settings.toml", "default_toolchain = \"stable-x86_64-unknown-linux-gnu\"\n");
    let p = platform();
    let home_text = home.display().to_string();
    let vars = EnvVars::from_pairs(&[("HOME", home_text.as_str())]);
    let files = [("src/main.rs", Language::Rust)];
    let env = home.join(".rustup/toolchains/1.98.1-aarch64-unknown-linux-musl");
    let mut c = cx(&root, &p, &vars, &files);
    c.env_override = Some(&env);
    let ToolchainStatus::Found(t) = resolve(&c).status else {
        panic!("expected the --env toolchain");
    };
    assert_eq!(t.version.unwrap().text, "1.98.1");
    assert_eq!(t.facts["rustup_toolchain"], "1.98.1-aarch64-unknown-linux-musl");
    assert_eq!(t.facts["host"], "aarch64-unknown-linux-musl");

    // A toolchain root outside rustup's layout has no rustup name.
    let loose = dir.path().join("rust-1.98.1");
    write(&loose, "bin/rustc", "");
    write(&loose, "bin/cargo", "");
    write(
        &loose,
        "lib/rustlib/multirust-channel-manifest.toml",
        "[pkg.rustc]\nversion = \"1.98.1 (abc 2026-01-01)\"\n",
    );
    #[cfg(unix)]
    for exe in ["bin/rustc", "bin/cargo"] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(loose.join(exe), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut c = cx(&root, &p, &vars, &files);
    c.env_override = Some(&loose);
    let ToolchainStatus::Found(t) = resolve(&c).status else {
        panic!("expected the loose --env toolchain");
    };
    assert!(!t.facts.contains_key("rustup_toolchain"));
}

#[test]
fn rule_pin_file_is_read_although_the_repository_is_forbidden_for_executables() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let home = dir.path().join("home");
    write(&root, "src/main.rs", "fn main() {}\n");
    write(&root, "rust-toolchain.toml", "[toolchain]\nchannel = \"1.90.0\"\n");
    fake_toolchain(&home, "stable-x86_64-unknown-linux-gnu", "1.95.0", true);
    fake_toolchain(&home, "1.90.0-x86_64-unknown-linux-gnu", "1.90.0", true);
    write(&home, ".rustup/settings.toml", "default_toolchain = \"stable-x86_64-unknown-linux-gnu\"\n");
    let p = platform();
    let home_text = home.display().to_string();
    let vars = EnvVars::from_pairs(&[("HOME", home_text.as_str())]);
    let files = [("src/main.rs", Language::Rust)];
    let forbidden = [root.clone()];
    let mut c = cx(&root, &p, &vars, &files);
    c.forbidden = &forbidden;
    let ToolchainStatus::Found(t) = resolve(&c).status else {
        panic!("expected the pinned toolchain");
    };
    assert_eq!(t.version.unwrap().text, "1.90.0");
}

#[test]
fn rule_rust_msrv_above_toolchain_is_too_old() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let home = dir.path().join("home");
    write(
        &root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/*\"]\n[workspace.package]\nrust-version = \"1.96\"\n",
    );
    write(
        &root,
        "crates/a/Cargo.toml",
        "[package]\nname = \"a\"\nversion = \"0.1.0\"\nrust-version.workspace = true\n",
    );
    write(&root, "crates/a/src/lib.rs", "");
    fake_toolchain(&home, "stable-x86_64-unknown-linux-gnu", "1.92.0", true);
    write(&home, ".rustup/settings.toml", "default_toolchain = \"stable\"\n");
    let p = platform();
    let home_text = home.display().to_string();
    let vars = EnvVars::from_pairs(&[("HOME", home_text.as_str())]);
    let files = [("crates/a/src/lib.rs", Language::Rust)];
    match resolve(&cx(&root, &p, &vars, &files)).status {
        ToolchainStatus::TooOld { needed, source, .. } => {
            assert_eq!(needed.describe("Rust"), "Rust 1.96 or newer");
            assert_eq!(source, "rust-version in Cargo.toml");
        }
        other => panic!("expected TooOld, got {other:?}"),
    }
}

#[test]
fn rule_cargo_workspace_members_and_nested_projects() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"fuzz\"]\n");
    write(root, "Cargo.lock", "version = 4\n");
    write(
        root,
        "crates/a/Cargo.toml",
        "[package]\nname = \"a\"\nversion = \"0.1.0\"\n[lib]\nproc-macro = true\n",
    );
    write(root, "crates/a/src/lib.rs", "");
    write(
        root,
        "crates/b/Cargo.toml",
        "[package]\nname = \"b\"\nversion = \"0.1.0\"\n[dependencies]\na = { path = \"../a\" }\n",
    );
    write(root, "crates/b/build.rs", "fn main() {}");
    write(root, "crates/b/src/lib.rs", "");
    write(root, "fuzz/Cargo.toml", "[package]\nname = \"fuzz\"\nversion = \"0.1.0\"\n");
    write(root, "fuzz/src/main.rs", "");
    let files = [
        ("crates/a/src/lib.rs", Language::Rust),
        ("crates/b/src/lib.rs", Language::Rust),
        ("fuzz/src/main.rs", Language::Rust),
    ];
    let layout = cargo_layout(root, &files);
    assert_eq!(layout.projects.len(), 1);
    let p = &layout.projects[0];
    assert!(p.workspace && p.lock && p.dir.is_empty());
    assert_eq!(p.members, vec!["crates/a", "crates/b"]);
    assert_eq!(layout.subprojects.len(), 1);
    assert_eq!(layout.subprojects[0].dir, "fuzz");
    let a = layout.packages.iter().find(|p| p.name == "a").unwrap();
    let b = layout.packages.iter().find(|p| p.name == "b").unwrap();
    assert!(a.proc_macro && !a.build_script);
    assert!(b.build_script && b.dependencies == vec!["a"]);
}

#[test]
fn rule_cargo_lock_crates_checked_in_the_cargo_home() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let home = dir.path().join("cargo");
    write(&root, "Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nserde = \"1\"\n[target.'cfg(target_os = \"macos\")'.dependencies]\nfsevent-sys = \"4\"\n");
    write(&root, "src/main.rs", "");
    write(
        &root,
        "Cargo.lock",
        "version = 4\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"serde\", \"fsevent-sys\"]\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.200\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"fsevent-sys\"\nversion = \"4.1.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
    );
    let p = platform();
    let home_text = home.display().to_string();
    let vars = EnvVars::from_pairs(&[("CARGO_HOME", home_text.as_str())]);
    let files = [("src/main.rs", Language::Rust)];
    let report = deps(&cx(&root, &p, &vars, &files), None);
    assert_eq!(report.status, DepsStatus::Missing);
    assert_eq!(report.missing, vec!["serde@1.0.200"], "the macOS-only crate is not needed on Linux");
    assert_eq!(report.hint, "cargo fetch");
    write(&home, "registry/cache/index.crates.io-1/serde-1.0.200.crate", "");
    let report = deps(&cx(&root, &p, &vars, &files), None);
    assert_eq!(report.status, DepsStatus::Installed);
    assert!(report.missing.is_empty());
}

#[test]
fn rule_cfg_predicates_follow_the_host_triple() {
    let linux = HostCfg::from_triple("x86_64-unknown-linux-gnu");
    let windows = HostCfg::from_triple("x86_64-pc-windows-msvc");
    assert_eq!(eval_target("cfg(unix)", &linux), Tri::True);
    assert_eq!(eval_target("cfg(windows)", &linux), Tri::False);
    assert_eq!(eval_target("cfg(not(windows))", &windows), Tri::False);
    assert_eq!(eval_target("cfg(all(unix, target_arch = \"x86_64\"))", &linux), Tri::True);
    assert_eq!(eval_target("cfg(any(target_os = \"macos\", target_os = \"ios\"))", &linux), Tri::False);
    assert_eq!(eval_target("cfg(feature = \"x\")", &linux), Tri::Unknown);
    assert_eq!(eval_target("x86_64-pc-windows-msvc", &windows), Tri::True);
    assert_eq!(eval_target("x86_64-pc-windows-msvc", &linux), Tri::False);
    assert_eq!(eval_target("cfg(target_env = \"msvc\")", &windows), Tri::True);
}

#[test]
fn rule_present_crate_not_needed_on_the_host_does_not_make_its_deps_needed() {
    // app -> (unix only) object -> memchr; object is in the Cargo home (fetched for another
    // target), memchr is not. On Windows memchr is not needed; on Linux it is.
    let lock = "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"object\",\n \"serde\",\n]\n\n[[package]]\nname = \"object\"\nversion = \"0.37.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\ndependencies = [\n \"memchr\",\n]\n\n[[package]]\nname = \"memchr\"\nversion = \"2.7.6\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.200\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
    let pkgs = cargo_lock_packages(lock);
    let app: Value = serde_json::json!({
        "package": {"name": "app"},
        "dependencies": {"serde": "1"},
        "target": {"cfg(unix)": {"dependencies": {"object": "0.37"}}}
    });
    let object: Value = serde_json::json!({"package": {"name": "object"}, "dependencies": {"memchr": "2"}});
    let present = |p: &LockPackage| p.name != "memchr";
    let manifest = |p: &LockPackage| match p.name.as_str() {
        "app" => Some(app.clone()),
        "object" => Some(object.clone()),
        _ => None,
    };
    let windows = HostCfg::from_triple("x86_64-pc-windows-msvc");
    let linux = HostCfg::from_triple("x86_64-unknown-linux-gnu");
    assert!(missing_needed(&pkgs, &present, &manifest, &windows).is_empty());
    assert_eq!(missing_needed(&pkgs, &present, &manifest, &linux), vec!["memchr@2.7.6"]);
    // A missing crate nothing declares for the host is not reported; one without a
    // readable parent manifest is (conservative).
    let none = |_: &LockPackage| None;
    assert_eq!(missing_needed(&pkgs, &present, &none, &windows), vec!["memchr@2.7.6"]);
}
