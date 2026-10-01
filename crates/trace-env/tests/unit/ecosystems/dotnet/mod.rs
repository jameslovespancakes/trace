use super::*;
use crate::test_support::write;

fn v(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn global(version: &str, roll: RollForward) -> GlobalJson {
    GlobalJson {
        file: PathBuf::from("global.json"),
        rel: "global.json".into(),
        version: Version::parse(version),
        roll_forward: roll,
        allow_prerelease: false,
        paths: Vec::new(),
    }
}

fn linux() -> Platform {
    Platform {
        os: Os::Linux,
        arch: Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    }
}

fn cx<'a>(
    root: &'a Path,
    platform: &'a Platform,
    vars: &'a EnvVars,
    env: Option<&'a Path>,
) -> DetectContext<'a> {
    DetectContext {
        root,
        platform,
        vars,
        env_override: env,
        forbidden: &[],
        files: &[],
    }
}

#[test]
fn rule_global_json_roll_forward_selects_sdk() {
    let installed = vec![
        v("8.0.404"),
        v("9.0.100"),
        v("9.0.318"),
        v("10.0.100"),
        v("10.0.302"),
        v("10.0.305"),
        v("10.0.401"),
        v("11.0.100-preview.1"),
    ];
    let pick = |version: &str, roll: RollForward| {
        select_sdk(&installed, Some(&global(version, roll))).map(|v| v.text)
    };
    assert_eq!(pick("10.0.300", RollForward::LatestPatch).as_deref(), Some("10.0.305"));
    assert_eq!(pick("10.0.302", RollForward::Patch).as_deref(), Some("10.0.302"));
    assert_eq!(pick("10.0.303", RollForward::Patch).as_deref(), Some("10.0.305"));
    assert_eq!(pick("10.0.303", RollForward::Disable), None);
    assert_eq!(pick("10.0.200", RollForward::LatestPatch), None);
    assert_eq!(pick("10.0.200", RollForward::Feature).as_deref(), Some("10.0.305"));
    assert_eq!(pick("10.0.300", RollForward::LatestFeature).as_deref(), Some("10.0.401"));
    assert_eq!(pick("9.0.200", RollForward::Minor).as_deref(), Some("9.0.318"));
    assert_eq!(pick("9.1.100", RollForward::Minor), None);
    assert_eq!(pick("9.1.100", RollForward::Major).as_deref(), Some("10.0.100"));
    assert_eq!(pick("9.0.100", RollForward::LatestMinor).as_deref(), Some("9.0.318"));
    assert_eq!(
        pick("8.0.100", RollForward::LatestMajor).as_deref(),
        Some("10.0.401"),
        "prerelease SDKs only with allowPrerelease"
    );
    let mut pre = global("8.0.100", RollForward::LatestMajor);
    pre.allow_prerelease = true;
    assert_eq!(select_sdk(&installed, Some(&pre)).map(|v| v.text).as_deref(), Some("11.0.100-preview.1"));
    assert_eq!(select_sdk(&installed[..3], None).map(|v| v.text).as_deref(), Some("9.0.318"));
}

#[test]
fn rule_global_json_is_read_with_defaults() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Src/global.json",
        "\u{feff}{ // pinned\n \"sdk\": { \"version\": \"10.0.300\", \"rollForward\": \"latestFeature\" } }",
    );
    let g = find_global_json(dir.path(), "Src/App").unwrap();
    assert_eq!(g.rel, "Src/global.json");
    assert_eq!(g.version.as_ref().map(|v| v.text.as_str()), Some("10.0.300"));
    assert_eq!(g.roll_forward, RollForward::LatestFeature);
    assert!(g.allow_prerelease);
    write(dir.path(), "global.json", "{\"sdk\": {\"version\": \"9.0.100\"}}");
    assert_eq!(find_global_json(dir.path(), "").unwrap().roll_forward, RollForward::LatestPatch);
    assert!(find_global_json(dir.path(), "Other").is_some(), "the root file applies below it");
}

fn fake_sdk(root: &Path, version: &str) {
    write(root, &format!("sdk/{version}/dotnet.dll"), "");
}

#[test]
fn rule_sdk_below_global_json_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let dotnet = dir.path().join("dotnet");
    fake_sdk(&dotnet, "9.0.318");
    write(
        &repo,
        "Src/global.json",
        "{\"sdk\": {\"version\": \"10.0.300\", \"rollForward\": \"latestFeature\"}}",
    );
    write(&repo, "Src/App.sln", "Project(\"{FAE04EC0}\") = \"App\", \"App\\App.csproj\", \"{1}\"\n");
    write(&repo, "Src/App/App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net8.0</TargetFramework></PropertyGroup></Project>");
    let platform = linux();
    let vars = EnvVars::default();
    let c = cx(&repo, &platform, &vars, Some(&dotnet));
    match toolchain(&c) {
        ToolchainStatus::TooOld {
            found,
            needed,
            source,
        } => {
            assert_eq!(found.version.unwrap().text, "9.0.318");
            assert_eq!(needed.describe(".NET SDK"), ".NET SDK 10.0.300 or newer");
            assert_eq!(source, "Src/global.json");
        }
        other => panic!("expected TooOld, got {other:?}"),
    }
    fake_sdk(&dotnet, "10.0.401");
    match toolchain(&c) {
        ToolchainStatus::Found(t) => {
            assert_eq!(t.version.unwrap().text, "10.0.401");
            assert_eq!(t.origin, Origin::Override);
        }
        other => panic!("expected the SDK, got {other:?}"),
    }
    // A target framework newer than every accepted SDK is the same error.
    write(&repo, "Src/App/App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net11.0</TargetFramework></PropertyGroup></Project>");
    assert!(
        matches!(toolchain(&c), ToolchainStatus::TooOld { source, .. } if source == "Src/App/App.csproj")
    );
    let empty = dir.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let c = cx(&repo, &platform, &vars, Some(&empty));
    assert!(matches!(toolchain(&c), ToolchainStatus::Missing { .. }));
    assert!(accepts_env_path(&dotnet));
    assert!(!accepts_env_path(&empty));
}

#[test]
fn rule_solution_covering_most_projects_is_opened() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let proj = "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net8.0</TargetFramework></PropertyGroup></Project>";
    write(root, "src/A/A.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"><ItemGroup><ProjectReference Include=\"..\\C\\C.csproj\" /></ItemGroup></Project>");
    write(root, "src/B/B.csproj", proj);
    write(root, "src/C/C.csproj", proj);
    write(root, "samples/S/S.csproj", proj);
    write(root, "samples/Samples.sln", "Project(\"{9A19103F}\") = \"S\", \"S\\S.csproj\", \"{1}\"\n");
    write(
        root,
        "All.slnx",
        "<Solution><Folder Name=\"/src/\"><Project Path=\"src/A/A.csproj\" /><Project Path=\"src/B/B.csproj\" /></Folder><Project Path=\"tools/x.fsproj\" /></Solution>",
    );
    let platform = linux();
    let vars = EnvVars::default();
    let p = project(&cx(root, &platform, &vars, None));
    assert_eq!(p.chosen_solution().map(|s| s.rel.as_str()), Some("All.slnx"));
    let required: Vec<&str> = p.required_projects().map(|p| p.rel.as_str()).collect();
    assert_eq!(
        required,
        ["src/A/A.csproj", "src/B/B.csproj", "src/C/C.csproj"],
        "project references are required too"
    );
    let subs: Vec<&str> = p.subprojects.iter().map(|i| p.projects[*i].rel.as_str()).collect();
    assert_eq!(subs, ["samples/S/S.csproj"]);
    // Without solutions every project is opened.
    assert_eq!(choose_solution(&[], &p.projects), None);
    let tie = vec![
        Solution {
            rel: "deep/x/One.sln".into(),
            projects: vec!["src/B/B.csproj".into()],
        },
        Solution {
            rel: "Two.sln".into(),
            projects: vec!["src/C/C.csproj".into()],
        },
        Solution {
            rel: "Empty.sln".into(),
            projects: vec!["gone/G.csproj".into()],
        },
    ];
    assert_eq!(choose_solution(&tie, &p.projects), Some(1), "equal coverage: the shallower solution");
}

fn assets(frameworks: &[&str], folder: &Path, libs: &[(&str, &str)]) -> String {
    let fw: serde_json::Map<String, Value> = frameworks
        .iter()
        .map(|f| ((*f).to_string(), serde_json::json!({})))
        .collect();
    let lib: serde_json::Map<String, Value> = libs
        .iter()
        .map(|(id, ver)| {
            (
                format!("{id}/{ver}"),
                serde_json::json!({"type": "package", "path": format!("{}/{}", id.to_ascii_lowercase(), ver)}),
            )
        })
        .collect();
    let mut folders = serde_json::Map::new();
    folders.insert(folder.display().to_string(), serde_json::json!({}));
    format!(
        "\u{feff}{}",
        serde_json::json!({"version": 3, "libraries": lib, "packageFolders": folders, "project": {"frameworks": fw}})
    )
}

#[test]
fn rule_project_assets_json_required_per_project() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let nuget = dir.path().join("nuget");
    // NuGet's install marker: `<id>/<version>/.nupkg.metadata` (+ `<id>.<version>.nupkg.sha512`).
    write(&nuget, "newtonsoft.json/13.0.3/.nupkg.metadata", "{}");
    let multi = "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFrameworks>net8.0;net9.0</TargetFrameworks></PropertyGroup></Project>";
    write(&root, "A/A.csproj", multi);
    write(&root, "B/B.csproj", multi);
    write(
        &root,
        "A/obj/project.assets.json",
        &assets(&["net8.0", "net9.0"], &nuget, &[("Newtonsoft.Json", "13.0.3")]),
    );
    write(&root, "A/obj/A.csproj.nuget.g.props", "<Project />");
    let platform = linux();
    let vars = EnvVars::default();
    let c = cx(&root, &platform, &vars, None);
    let report = deps(&c, None);
    assert_eq!(report.status, DepsStatus::Missing);
    assert_eq!(report.missing, vec!["B/B.csproj (not restored)".to_string()]);
    assert_eq!(report.hint, "dotnet restore");
    assert_eq!(report.roots.len(), 1);
    assert_eq!(report.roots[0].layout, "nuget_packages");
    // Restored, but one target framework and one package are missing.
    write(&root, "B/obj/project.assets.json", &assets(&["net8.0"], &nuget, &[("Serilog", "4.0.0")]));
    write(&root, "B/obj/B.csproj.nuget.g.props", "<Project />");
    let report = deps(&c, None);
    assert_eq!(report.missing, vec!["B/B.csproj (net9.0 not restored, Serilog 4.0.0)".to_string()]);
    write(
        &root,
        "B/obj/project.assets.json",
        &assets(&["net8.0", "net9.0"], &nuget, &[("Newtonsoft.Json", "13.0.3")]),
    );
    let before = deps(&c, None);
    assert_eq!(before.status, DepsStatus::Installed);
    write(&root, "B/obj/project.assets.json", &assets(&["net8.0", "net9.0"], &nuget, &[]));
    assert_ne!(deps(&c, None).fingerprint, before.fingerprint, "a new restore changes the fingerprint");
}

#[test]
fn rule_legacy_projects_and_workloads_are_detected() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "Old/Old.csproj",
        "<?xml version=\"1.0\"?><Project ToolsVersion=\"15.0\" xmlns=\"http://schemas.microsoft.com/developer/msbuild/2003\"><PropertyGroup><TargetFrameworkVersion>v4.5</TargetFrameworkVersion></PropertyGroup></Project>",
    );
    write(root, "Old/packages.config", "<packages><package id=\"NUnit\" version=\"3.13.0\" /></packages>");
    write(
        root,
        "App/App.csproj",
        "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFrameworks>net8.0-ios;net8.0-android34.0</TargetFrameworks><UseMaui>true</UseMaui></PropertyGroup></Project>",
    );
    let platform = linux();
    let vars = EnvVars::default();
    let p = project(&cx(root, &platform, &vars, None));
    assert_eq!(p.legacy_projects().len(), 1);
    assert_eq!(p.workloads(), vec!["android", "ios", "maui"]);
    let sdk_root = dir.path().join("dotnet");
    write(&sdk_root, "metadata/workloads/x64/8.0.400/InstalledWorkloads/maui-android", "");
    write(&sdk_root, "metadata/workloads/8.0.400/InstalledWorkloads/ios", "");
    let sdk = v("8.0.404");
    assert!(workload_installed(&sdk_root, &sdk, "ios"));
    assert!(workload_installed(&sdk_root, &sdk, "maui"));
    assert!(!workload_installed(&sdk_root, &sdk, "android"));
    assert!(!workload_installed(&sdk_root, &v("8.0.300"), "ios"), "workloads belong to a feature band");
    let restore =
        project_restore(root, &p.projects[p.projects.iter().position(|x| !x.sdk_style).unwrap()], None);
    assert_eq!(restore.missing, vec!["NUnit 3.13.0".to_string()]);
}

/// The shared fixture: a solution whose only project was never restored.
#[test]
fn rule_unrestored_fixture_needs_dotnet_restore() {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/rule-dotnet-csharp-unrestored");
    let platform = linux();
    let vars = EnvVars::default();
    let c = cx(&root, &platform, &vars, None);
    let p = project(&c);
    assert_eq!(p.chosen_solution().map(|s| s.rel.as_str()), Some("App.sln"));
    let report = deps_for(&c, &p);
    assert_eq!(report.status, DepsStatus::Missing);
    assert_eq!(report.missing, vec!["src/App/App.csproj (not restored)".to_string()]);
}

#[test]
fn rule_windows_only_projects_need_windows_targeting_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Wpf/Wpf.csproj",
        "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net8.0-windows</TargetFramework><UseWPF>true</UseWPF></PropertyGroup></Project>",
    );
    let platform = linux();
    let vars = EnvVars::default();
    let c = cx(dir.path(), &platform, &vars, None);
    let report = deps(&c, None);
    assert_eq!(report.hint, "dotnet restore -p:EnableWindowsTargeting=true");
    assert!(workload_ids(&["net8.0-windows".to_string()], false).is_empty());
    assert_eq!(join_rel("src/A", "..\\B\\B.csproj").as_deref(), Some("src/B/B.csproj"));
    assert_eq!(join_rel("", "../x.csproj"), None);
}
