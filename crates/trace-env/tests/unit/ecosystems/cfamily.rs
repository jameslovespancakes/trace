use super::*;
use crate::test_support::write;

#[test]
fn rule_msvc_environment_comes_from_the_installation_layout() {
    let dir = tempfile::tempdir().unwrap();
    let pf = dir.path().join("pf86");
    let vs = pf.join("Microsoft Visual Studio/2022/BuildTools");
    let tools = vs.join("VC/Tools/MSVC/14.44.35207");
    let host_bin = tools.join("bin/Hostx64/x64");
    for d in [host_bin.clone(), tools.join("include"), tools.join("lib/x64")] {
        std::fs::create_dir_all(d).unwrap();
    }
    write(&host_bin, "cl.exe", "");
    let kits = pf.join("Windows Kits/10");
    for d in [
        "Include/10.0.26100.0/ucrt",
        "Include/10.0.26100.0/shared",
        "Lib/10.0.26100.0/um/x64",
        "bin/10.0.26100.0/x64",
    ] {
        std::fs::create_dir_all(kits.join(d)).unwrap();
    }
    write(&kits.join("Include/10.0.26100.0/um"), "windows.h", "");
    // An SDK version without headers is skipped.
    std::fs::create_dir_all(kits.join("Include/10.0.99999.0/ucrt")).unwrap();
    let mut vars = EnvVars::default();
    vars.set("ProgramFiles(x86)", pf.as_os_str());
    let windows = Platform {
        os: Os::Windows,
        arch: crate::os::Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    };
    let compiler = Compiler {
        kind: CompilerKind::Msvc,
        cc: host_bin.join("cl.exe"),
        cxx: host_bin.join("cl.exe"),
        version: Version::parse("14.44.35207"),
        vs_install: Some(vs.clone()),
    };
    let env: BTreeMap<String, String> = msvc_env(&compiler, &vars, &windows, r"C:\base")
        .unwrap()
        .into_iter()
        .collect();
    let include: Vec<&str> = env["INCLUDE"].split(';').collect();
    assert_eq!(include.len(), 4, "only existing directories: {include:?}");
    assert!(include[0].ends_with("include") && include[0].contains("14.44.35207"));
    assert!(include.iter().any(|d| d.ends_with("um")));
    assert!(env["LIB"].contains("10.0.26100.0") && env["LIB"].contains("x64"));
    let path: Vec<&str> = env["PATH"].split(';').collect();
    assert_eq!(path[0], host_bin.display().to_string(), "the compiler first");
    assert_eq!(*path.last().unwrap(), r"C:\base");
    assert_eq!(env["WindowsSDKVersion"], "10.0.26100.0\\");
    assert_eq!(env["VCToolsVersion"], "14.44.35207");
    // Other compilers have no MSVC environment.
    let gcc = Compiler {
        kind: CompilerKind::Gcc,
        vs_install: None,
        ..compiler
    };
    assert_eq!(msvc_env(&gcc, &vars, &windows, ""), None);
}

#[test]
fn rule_msvc_system_headers_include_the_windows_sdk() {
    let dir = tempfile::tempdir().unwrap();
    let pf = dir.path().join("pf86");
    let tools = pf.join("Microsoft Visual Studio/2022/BuildTools/VC/Tools/MSVC/14.44.35207");
    let host_bin = tools.join("bin/Hostx64/x64");
    std::fs::create_dir_all(tools.join("include")).unwrap();
    write(&host_bin, "cl.exe", "");
    let kits = pf.join("Windows Kits/10");
    std::fs::create_dir_all(kits.join("Include/10.0.26100.0/ucrt")).unwrap();
    write(&kits.join("Include/10.0.26100.0/um"), "windows.h", "");
    let mut vars = EnvVars::default();
    vars.set("ProgramFiles(x86)", pf.as_os_str());
    let windows = Platform {
        os: Os::Windows,
        arch: crate::os::Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    };
    let mut facts = BTreeMap::new();
    facts.insert("kind".to_string(), "msvc".to_string());
    let mut executables = BTreeMap::new();
    executables.insert("cc".to_string(), host_bin.join("cl.exe"));
    let t = Toolchain {
        id: "cc",
        root: host_bin.clone(),
        version: None,
        executables,
        origin: Origin::StandardLocation,
        facts,
    };
    // stdio.h / stdlib.h live in the SDK's Universal CRT, not in the MSVC tree.
    let dirs = system_header_dirs(&t, &vars, &windows);
    assert_eq!(dirs, vec![tools.join("include"), kits.join("Include").join("10.0.26100.0")]);
    // MinGW-style compilers: the prefix of their bin directory.
    let prefix = dir.path().join("mingw64");
    write(&prefix.join("bin"), "gcc.exe", "");
    let mut t = t;
    t.facts.insert("kind".to_string(), "gcc".to_string());
    t.executables
        .insert("cc".to_string(), prefix.join("bin").join("gcc.exe"));
    assert_eq!(system_header_dirs(&t, &vars, &windows), vec![prefix]);
}

#[test]
fn rule_msvc_tools_dir_is_the_newest_version_with_headers() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir
        .path()
        .join("Microsoft Visual Studio/2022/BuildTools/VC/Tools/MSVC");
    for v in ["14.29.30133", "14.44.35207", "14.50.1"] {
        std::fs::create_dir_all(base.join(v)).unwrap();
    }
    std::fs::create_dir_all(base.join("14.29.30133/include")).unwrap();
    std::fs::create_dir_all(base.join("14.44.35207/include")).unwrap();
    let best = newest_msvc(&[dir.path().to_path_buf()]).unwrap();
    assert!(best.ends_with("14.44.35207"), "newest with an include dir");
    let linux = Platform {
        os: Os::Linux,
        arch: crate::os::Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    };
    assert_eq!(msvc_tools_dir(&EnvVars::default(), &linux), None);
}

#[test]
fn rule_build_system_order_is_database_cmake_meson_visual_studio() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "src/a.c", "int main(void){return 0;}\n");
    let files = [("src/a.c", Language::C)];
    assert_eq!(build_system(root, &files, None), BuildSystem::None);
    write(root, "proj/app.vcxproj", "<Project/>");
    assert_eq!(build_system(root, &files, None), BuildSystem::VisualStudioOnly);
    write(root, "meson.build", "project('x', 'c')\n");
    assert_eq!(build_system(root, &files, None), BuildSystem::Meson(String::new()));
    write(root, "CMakeLists.txt", "project(x C)\n");
    assert_eq!(build_system(root, &files, None), BuildSystem::CMake(String::new()));
    write(root, "build/compile_commands.json", "[]");
    assert_eq!(build_system(root, &files, None), BuildSystem::CompileCommands(root.join("build")));
    let other = dir.path().join("elsewhere");
    write(&other, "compile_commands.json", "[]");
    assert_eq!(build_system(root, &files, Some(&other)), BuildSystem::CompileCommands(other.clone()));
}

#[test]
fn rule_vcpkg_manifest_needs_installed_packages() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "vcpkg.json", "{\"dependencies\": [\"fmt\"]}");
    write(root, "a.c", "");
    let p = Platform::current();
    let vars = EnvVars::default();
    let files = [("a.c", Language::C)];
    let cx = DetectContext {
        root,
        platform: &p,
        vars: &vars,
        env_override: None,
        forbidden: &[],
        files: &files,
    };
    let report = deps(&cx, None);
    assert_eq!(report.status, DepsStatus::Missing);
    assert_eq!(report.hint, "vcpkg install");
    write(root, "build/vcpkg_installed/x64-windows/include/fmt/core.h", "");
    assert_eq!(deps(&cx, None).status, DepsStatus::Installed);
}
