use super::*;

fn artifact(p: &str) -> Artifact {
    Artifact {
        platform: p.into(),
        url: format!("https://x/{p}"),
        sha256: String::new(),
        strip: 0,
        strip_prefix: None,
        subdir: None,
        min_glibc: None,
    }
}

fn platform(os: Os, arch: Arch, musl: bool) -> Platform {
    let arch_name = match arch {
        Arch::X86_64 => "x86_64",
        Arch::Aarch64 => "aarch64",
        Arch::Other => "riscv64",
    };
    Platform {
        os,
        arch,
        arch_name: arch_name.into(),
        musl,
    }
}

#[test]
fn rule_installer_picks_artifact_for_current_platform() {
    let list = [artifact("windows-x86_64"), artifact("linux-x86_64"), artifact("any")];
    let win = platform(Os::Windows, Arch::X86_64, false);
    let mac = platform(Os::MacOs, Arch::Aarch64, false);
    assert_eq!(artifact_for(&list, &win).unwrap().platform, "windows-x86_64");
    assert_eq!(artifact_for(&list, &mac).unwrap().platform, "any");
    assert!(artifact_for(&list[..1], &mac).is_none());
    // musl never takes the glibc build; the advice says why.
    let musl = platform(Os::Linux, Arch::X86_64, true);
    assert!(artifact_for(&list[..2], &musl).is_none());
    assert!(unavailable_advice(&list[..2], &musl).unwrap().contains("musl"));
    assert_eq!(unavailable_advice(&list[..1], &mac), None);
    // The current platform picks its own key.
    let current = Platform::current();
    let own = [artifact(&current.key())];
    assert!(artifact_for(&own, &current).is_some());
    // Old glibc.
    let mut node = artifact("linux-x86_64");
    node.min_glibc = Some("2.28".into());
    let v = |s: &str| Version::parse(s).unwrap();
    assert!(glibc_too_old(&node, Some(&v("2.17"))).unwrap().contains("glibc 2.28"));
    assert_eq!(glibc_too_old(&node, Some(&v("2.35"))), None);
    assert_eq!(glibc_too_old(&artifact("linux-x86_64"), Some(&v("2.17"))), None);
}

#[test]
fn rule_installer_skips_foreign_platform_npm_packages() {
    let pkg = |os: &[&str], cpu: &[&str]| NpmPackage {
        path: "node_modules/x".into(),
        version: "1".into(),
        url: "https://x".into(),
        integrity: "sha512-x".into(),
        os: os.iter().map(|s| s.to_string()).collect(),
        cpu: cpu.iter().map(|s| s.to_string()).collect(),
        optional: true,
    };
    let win = platform(Os::Windows, Arch::X86_64, false);
    let linux_arm = platform(Os::Linux, Arch::Aarch64, false);
    assert!(npm_package_applies(&pkg(&[], &[]), &win));
    assert!(npm_package_applies(&pkg(&["win32"], &["x64"]), &win));
    assert!(!npm_package_applies(&pkg(&["win32"], &["arm64"]), &win));
    assert!(!npm_package_applies(&pkg(&["darwin"], &[]), &win), "fsevents");
    assert!(npm_package_applies(&pkg(&["linux"], &["arm64"]), &linux_arm));
    assert!(!npm_package_applies(&pkg(&["!linux"], &[]), &linux_arm));
    assert!(npm_package_applies(&pkg(&["!win32"], &[]), &linux_arm));
}

#[test]
fn rule_r_binary_is_selected_by_platform_distro_and_r_minor() {
    let bin = |platform: &str, distro: Option<&str>, minor: &str, pkg: &str| RBinary {
        platform: platform.into(),
        distro: distro.map(Into::into),
        r_minor: minor.into(),
        package: pkg.into(),
        version: "1".into(),
        url: format!("https://p/{platform}/{minor}/{pkg}"),
        sha256: String::new(),
    };
    let files = vec![
        bin("windows-x86_64", None, "4.5", "R6"),
        bin("windows-x86_64", None, "4.6", "R6"),
        bin("windows-x86_64", None, "4.6", "languageserver"),
        bin("linux-x86_64", Some("noble"), "4.6", "R6"),
        bin("linux-x86_64", Some("jammy"), "4.6", "R6"),
    ];
    let win = platform(Os::Windows, Arch::X86_64, false);
    let picked: Vec<&str> = r_binaries_for(&files, &win, None, "4.6")
        .iter()
        .map(|f| f.package.as_str())
        .collect();
    assert_eq!(picked, vec!["R6", "languageserver"], "recorded order kept");
    assert!(r_binaries_for(&files, &win, None, "4.4").is_empty());
    assert_eq!(r_minors_for(&files, &win, None), vec!["4.5", "4.6"]);
    let linux = platform(Os::Linux, Arch::X86_64, false);
    let noble = r_binaries_for(&files, &linux, Some("noble"), "4.6");
    assert_eq!(noble.len(), 1);
    assert!(noble[0].url.contains("linux-x86_64"));
    assert!(r_binaries_for(&files, &linux, Some("bookworm"), "4.6").is_empty());
    assert!(r_binaries_for(&files, &linux, None, "4.6").is_empty());
    assert_eq!(r_minor(&Version::parse("4.6.1").unwrap()), "4.6");
}
