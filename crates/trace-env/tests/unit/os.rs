use super::*;

fn platform(os: Os, arch: Arch, name: &str) -> Platform {
    Platform {
        os,
        arch,
        arch_name: name.into(),
        musl: false,
    }
}

#[test]
fn rule_platform_keys_and_names_are_per_os() {
    let w = platform(Os::Windows, Arch::X86_64, "x86_64");
    assert_eq!(w.key(), "windows-x86_64");
    assert_eq!(w.os_name(), "Windows");
    assert_eq!(w.exe("go"), "go.exe");
    assert_eq!(w.exe("npm.cmd"), "npm.cmd");
    assert_eq!(w.path_list_sep(), ';');
    let mut l = platform(Os::Linux, Arch::Aarch64, "aarch64");
    assert_eq!(l.display(), "Linux on aarch64");
    assert_eq!(l.exe("go"), "go");
    l.musl = true;
    assert_eq!(l.key(), "linux-aarch64-musl");
    let m = platform(Os::MacOs, Arch::Other, "powerpc");
    assert_eq!(m.key(), "macos-powerpc");
    assert_eq!(m.os_name(), "macOS");
    let current = Platform::current();
    assert_eq!(current.arch_name, std::env::consts::ARCH);
}

/// The install key comes from `std::env::consts` (OS + ARCH) plus the musl probe.
#[test]
fn rule_platform_key_from_consts() {
    let p = Platform::current();
    let os = match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "macos",
        _ => "linux",
    };
    let expected = format!("{os}-{}{}", std::env::consts::ARCH, if p.musl { "-musl" } else { "" });
    assert_eq!(p.key(), expected);
    assert!(!p.musl || p.os == Os::Linux, "musl only on Linux");
    if p.os != Os::Linux {
        assert_eq!(p.glibc(), None);
        assert_eq!(p.linux_distro(), None);
    }
}

#[test]
fn rule_glibc_version_is_the_highest_version_definition() {
    let mut lib = b"\x7fELF....GLIBC_2.2.5\0GLIBC_2.17\0GLIBC_PRIVATE\0GLIBC_2.34\0".to_vec();
    lib.extend_from_slice(b"xxGLIBC_2.28\0GLIBC_2.9\0");
    let v = glibc_from_bytes(&lib).unwrap();
    assert_eq!(v.parts, vec![2, 34]);
    assert!(VersionReq::at_least(Version::parse("2.28").unwrap()).matches(&v));
    assert_eq!(glibc_from_bytes(b"GLIBC_2.1x not terminated"), None);
    assert_eq!(glibc_from_bytes(b"no version here"), None);
    let old = glibc_from_bytes(b"GLIBC_2.17\0GLIBC_2.27\0").unwrap();
    assert!(!VersionReq::at_least(Version::parse("2.28").unwrap()).matches(&old));
}

#[test]
fn rule_linux_distro_key_from_os_release() {
    let kv = |text: &str| trace_core::formats::ini::key_values(text);
    assert_eq!(
        distro_from_os_release(&kv("ID=ubuntu\nVERSION_ID=\"24.04\"\nVERSION_CODENAME=noble\n")),
        Some("noble".into())
    );
    assert_eq!(
        distro_from_os_release(&kv("ID=debian\nVERSION_CODENAME=bookworm\n")),
        Some("bookworm".into())
    );
    assert_eq!(
        distro_from_os_release(&kv("ID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"\nVERSION_ID=\"9.4\"\n")),
        Some("rhel9".into())
    );
    assert_eq!(
        distro_from_os_release(&kv("ID=\"opensuse-leap\"\nVERSION_ID=\"15.6\"\n")),
        Some("opensuse156".into())
    );
    assert_eq!(distro_from_os_release(&kv("ID=fedora\nVERSION_ID=40\n")), None);
}

#[test]
fn rule_versions_parse_and_order_numerically() {
    let v = |s: &str| Version::parse(s).unwrap();
    assert_eq!(v("go1.27.0").parts, vec![1, 27, 0]);
    assert_eq!(v("3.4.10p0").parts, vec![3, 4, 10]);
    let dev = v("0.17.0-dev.1936+5a6");
    assert_eq!(dev.parts, vec![0, 17, 0]);
    assert_eq!(dev.pre.as_deref(), Some("dev.1936"));
    assert_eq!(v("21.0.12.1+1").parts, vec![21, 0, 12, 1]);
    assert!(v("1.10") > v("1.9.9"));
    assert!(v("0.17.0-dev.1") < v("0.17.0"));
    assert!(v("1.2") < v("1.2.1"));
    assert!(Version::parse("none").is_none());
}

#[test]
fn rule_version_requirements_match_and_describe() {
    let v = |s: &str| Version::parse(s).unwrap();
    let req = VersionReq::at_least(v("1.26"));
    assert!(req.matches(&v("1.26.0")));
    assert!(req.matches(&v("go1.27.1")));
    assert!(!req.matches(&v("1.25.3")));
    assert_eq!(req.describe("Go"), "Go 1.26 or newer");
    let range = VersionReq {
        min: Some(v("17")),
        below: Some(v("26")),
        text: String::new(),
    };
    assert!(range.matches(&v("21.0.12.1+1")));
    assert!(!range.matches(&v("26.0.1")));
    assert_eq!(range.describe("Java"), "Java 17 or newer, older than 26");
}

#[test]
fn rule_missing_key_value_file_is_empty() {
    assert!(read_key_values(Path::new("does/not/exist")).is_empty());
}

#[test]
fn rule_versioned_children_are_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["R-4.3.1", "R-4.5.0", "R-4.10.2", "Rtools", "other"] {
        fs::create_dir_all(dir.path().join(name)).unwrap();
    }
    let found: Vec<String> = versioned_children(dir.path(), "R-")
        .into_iter()
        .map(|(v, _)| v.text)
        .collect();
    assert_eq!(found, vec!["4.10.2", "4.5.0", "4.3.1"]);
}

#[test]
fn rule_executables_are_found_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    let p = Platform::current();
    let exe = b.join(p.exe("tool"));
    fs::write(&exe, b"").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_eq!(find_executable(&["missing", "tool"], &[a.clone(), b.clone()], &p), Some(exe));
    assert_eq!(find_executable(&["missing"], &[a, b], &p), None);
}

/// Rule: a command word is looked up in the PATH directories like a shell does (Windows:
/// `PATHEXT` extensions; elsewhere: the execute bit), first directory first, and the
/// program found is never run.
#[test]
fn rule_program_lookup_reads_path_directories_without_executing() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    let p = Platform::current();
    let marker = dir.path().join("ran");
    let name = if p.os == Os::Windows { "tool.cmd" } else { "tool" };
    let script = if p.os == Os::Windows {
        format!("@echo x > \"{}\"\r\n", marker.display())
    } else {
        format!("#!/bin/sh\ntouch '{}'\n", marker.display())
    };
    fs::write(b.join(name), script).unwrap();
    fs::write(a.join("plain.txt"), b"").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(b.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let vars = EnvVars::from_pairs(&[("PATHEXT", ".COM;.EXE;.BAT;.CMD")]);
    let programs = ProgramDirs::read(&[a.clone(), b.clone(), a.clone()], &vars, &p);
    assert_eq!(programs.dirs().count(), 2, "each directory is listed once");
    assert_eq!(programs.find("tool"), Some(b.join(name)));
    assert_eq!(programs.find("missing"), None);
    assert_eq!(programs.find("plain"), None);
    for word in ["", ".", "..", "-x", "b/tool", "$TOOL", "'tool'", "A=1", "to ol"] {
        assert!(!is_program_name(word), "{word:?}");
        assert_eq!(programs.find(word), None, "{word:?}");
    }
    assert!(
        is_program_name("g++") && is_program_name("python3.12") && is_program_name("x86_64-linux-gnu-gcc")
    );
    assert!(!marker.exists(), "the lookup never runs the program");
    let win = platform(Os::Windows, Arch::X86_64, "x86_64");
    assert_eq!(path_extensions(&EnvVars::default(), &win), vec![".com", ".exe", ".bat", ".cmd"]);
    assert!(path_extensions(&vars, &platform(Os::Linux, Arch::X86_64, "x86_64")).is_empty());
}

#[test]
fn rule_env_vars_and_standard_dirs_follow_the_os() {
    let vars =
        EnvVars::from_pairs(&[("HOME", "/home/u"), ("XDG_CACHE_HOME", "relative/not/used"), ("EMPTY", "")]);
    assert_eq!(vars.path("EMPTY"), None);
    assert_eq!(vars.path("XDG_CACHE_HOME"), None, "relative paths are ignored");
    let linux = platform(Os::Linux, Arch::X86_64, "x86_64");
    if cfg!(unix) {
        assert_eq!(data_local_dir(&vars, &linux), Some(PathBuf::from("/home/u/.local/share")));
        assert_eq!(cache_dir(&vars, &linux), Some(PathBuf::from("/home/u/.cache")));
        let mac = platform(Os::MacOs, Arch::Aarch64, "aarch64");
        assert_eq!(cache_dir(&vars, &mac), Some(PathBuf::from("/home/u/Library/Caches")));
    }
    let win = platform(Os::Windows, Arch::X86_64, "x86_64");
    let wvars = EnvVars::from_pairs(&[]);
    assert_eq!(data_local_dir(&wvars, &win), None);
    assert!(path_dirs(&EnvVars::default()).is_empty());
}
