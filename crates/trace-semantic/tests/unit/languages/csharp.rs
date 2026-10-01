use super::*;
use crate::registry::Registry;
use std::path::Path;
use std::path::PathBuf;
use trace_core::facts::FileFacts;
use trace_env::os::{Arch, EnvVars, Platform};

struct Repo {
    dir: PathBuf,
}

impl Repo {
    fn new(name: &str) -> Repo {
        let dir = std::env::temp_dir()
            .join("trace-tests")
            .join(format!("trace-fixtures-csharp-{name}-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("repo")).unwrap();
        Repo { dir }
    }
    fn root(&self) -> PathBuf {
        self.dir.join("repo")
    }
    fn write(&self, rel: &str, text: &str) {
        let p = rel
            .split('/')
            .filter(|s| !s.is_empty())
            .fold(self.dir.clone(), |p, s| p.join(s));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
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

fn run(repo: &Repo, platform: &Platform, allow: bool, sdk: Option<&Path>) -> Result<Prepared, SetupError> {
    let paths = trace_core::paths::RepoPaths::resolve_in(&repo.root(), &repo.dir.join("home")).unwrap();
    let registry = Registry::builtin();
    let entry = registry.entry("lsp:roslyn").unwrap();
    let mut tools = crate::test_support::setup::tool_env(None);
    // As in production: the repository is a root nothing may be EXECUTED from; project
    // files below it must still be read (rule_project_files_below_the_root_are_read).
    tools.forbidden_roots = vec![repo.root()];
    let mut settings = trace_core::repo_settings::RepoSettings {
        allow_build: allow,
        ..Default::default()
    };
    if let Some(sdk) = sdk {
        settings.env.insert("dotnet".into(), sdk.to_path_buf());
    }
    let files = [("src/App/Program.cs", Language::CSharp)];
    let facts = |_: &str| -> Option<&FileFacts> { None };
    let vars = EnvVars::default();
    let cx = SetupContext {
        repo: &paths,
        entry,
        languages: &[Language::CSharp],
        files: &files,
        facts: &facts,
        settings: &settings,
        tools: &tools,
        platform,
        vars: &vars,
        report_only: true,
    };
    Hooks.preflight(&cx)
}

const SDK_PROJECT: &str = "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net8.0</TargetFramework></PropertyGroup></Project>";

#[test]
fn rule_csharp_needs_build_approval() {
    let repo = Repo::new("approval");
    repo.write("repo/src/App/App.csproj", SDK_PROJECT);
    repo.write("dotnet/sdk/9.0.318/dotnet.dll", "");
    let sdk = repo.dir.join("dotnet");
    let text = run(&repo, &linux(), false, Some(&sdk))
        .unwrap_err()
        .lines()
        .join("\n");
    assert!(text.contains("C# needs MSBuild, which runs this project's build scripts."), "{text}");
    assert!(text.contains("Only allow this for projects you trust: trace index --allow-build"), "{text}");
    assert!(text.contains("dotnet restore"), "the unrestored project is reported too: {text}");
    assert!(text.contains("The C# language server is not installed."), "{text}");
    let allowed = run(&repo, &linux(), true, Some(&sdk)).unwrap_err().lines().join("\n");
    assert!(!allowed.contains("--allow-build"), "{allowed}");
}

#[test]
fn rule_project_files_below_the_root_are_read() {
    // The repository root is in `tools.forbidden_roots` (never executed from); the project
    // scan must still see `src/App/App.csproj` below it.
    let repo = Repo::new("below-root");
    repo.write("repo/src/App/App.csproj", SDK_PROJECT);
    let err = run(&repo, &linux(), true, None).unwrap_err();
    assert!(!err.to_string().contains("no project file"), "the project below the root was not found: {err}");
}

#[test]
fn rule_csharp_project_shape_errors() {
    let repo = Repo::new("shape");
    repo.write("repo/src/App/Program.cs", "class P {}");
    let err = run(&repo, &linux(), true, None).unwrap_err();
    assert_eq!(err.kind(), "unsupported");
    assert!(err
        .to_string()
        .starts_with("This C# code has no project file (.csproj)"));
    repo.write(
            "repo/src/App/App.csproj",
            "<Project ToolsVersion=\"15.0\"><PropertyGroup><TargetFrameworkVersion>v4.8</TargetFrameworkVersion></PropertyGroup></Project>",
        );
    let err = run(&repo, &linux(), true, None).unwrap_err();
    let lines = err.lines();
    assert_eq!(
            lines[0],
            "This C# project could not be built on Linux (it uses the old .NET Framework project format, which needs Visual Studio)."
        );
    assert_eq!(lines[1].trim(), "Run trace on Windows to analyze it.");
}

#[test]
fn rule_sdk_version_error_names_global_json() {
    let repo = Repo::new("sdk");
    repo.write("repo/src/App/App.csproj", SDK_PROJECT);
    repo.write("repo/global.json", "{\"sdk\": {\"version\": \"10.0.300\"}}");
    repo.write("dotnet/sdk/9.0.318/dotnet.dll", "");
    let sdk = repo.dir.join("dotnet");
    let err = run(&repo, &linux(), true, Some(&sdk)).unwrap_err();
    let text = err.lines().join("\n");
    assert!(
            text.contains("This C# project needs the .NET SDK 10.0.300 or newer (global.json); the installed .NET SDK is 9.0.318."),
            "{text}"
        );
}

#[test]
fn rule_roslyn_load_errors_map_to_setup_errors() {
    let sdk = vec![(
            1u8,
            "Failed to load project: A compatible .NET SDK was not found. Requested SDK version: 10.0.300 global.json file: C:\\x".to_string(),
        )];
    assert_eq!(classify_load(&sdk), RoslynLoad::Sdk(Some("10.0.300".into())));
    let restore = vec![
        (2u8, "Project App (net8.0) has unresolved dependencies".to_string()),
        (1u8, "Error while loading C:\\ws\\App.csproj: something".to_string()),
    ];
    assert_eq!(classify_load(&restore), RoslynLoad::Unrestored, "restore wins over the follow-up error");
    let failed =
        vec![(1u8, "Error while loading C:\\ws\\App.csproj: The imported project was not found".to_string())];
    match classify_load(&failed) {
        RoslynLoad::Failed(what) => {
            assert!(what.starts_with("MSBuild could not load it: Error while loading"), "{what}")
        }
        other => panic!("{other:?}"),
    }
    let refs = vec![(
        2u8,
        "Warning while loading App: The reference assemblies for .NETFramework,Version=v4.5 were not found."
            .to_string(),
    )];
    assert!(matches!(classify_load(&refs), RoslynLoad::Failed(_)));
    let ok = vec![(3u8, "Completed (re)load of all projects in 00:00:04".to_string())];
    assert_eq!(classify_load(&ok), RoslynLoad::Ok(Vec::new()));
    // Through the hook: the texts become the catalogue errors.
    let prepared = Prepared {
        data: Some(Arc::new(CsharpData {
            restore_hint: "dotnet restore".into(),
        })),
        ..Prepared::default()
    };
    let log_messages = restore.clone();
    let cx = LoadedContext {
        prepared: &prepared,
        log_messages: &log_messages,
        notifications: &[],
        diagnostics: &[],
        log: Path::new("roslyn.log"),
    };
    assert_eq!(Hooks.check_loaded(&cx).unwrap_err().kind(), "deps_missing");
    let toast = vec![(
        "window/_roslyn_showToast".to_string(),
        json!({"messageType": 1, "message": "A compatible .NET SDK was not found."}),
    )];
    let cx = LoadedContext {
        prepared: &prepared,
        log_messages: &[],
        notifications: &toast,
        diagnostics: &[],
        log: Path::new("roslyn.log"),
    };
    assert_eq!(
            Hooks.check_loaded(&cx).unwrap_err().to_string(),
            "C# needs the .NET SDK this project requests, which is not installed. Install it from https://dotnet.microsoft.com/download and run trace again."
        );
}

#[test]
fn rule_solution_or_projects_are_opened_explicitly() {
    let mut project = DotnetProject::default();
    project.projects.push(dotnet::CsProject {
        rel: "src/My App/App.csproj".into(),
        dir: "src/My App".into(),
        sdk_style: true,
        ..Default::default()
    });
    project.required = vec![0];
    let (sln, projects) = open_params(&project);
    assert_eq!(sln, Value::Null);
    assert_eq!(projects, json!({"projects": ["{snapshot_uri}/src/My%20App/App.csproj"]}));
    project.solutions.push(dotnet::Solution {
        rel: "All.sln".into(),
        projects: vec!["src/My App/App.csproj".into()],
    });
    project.chosen = Some(0);
    let (sln, projects) = open_params(&project);
    assert_eq!(sln, json!({"solution": "{snapshot_uri}/All.sln"}));
    assert_eq!(projects, Value::Null);
    let pending = pending_dirs(
        &[
            ("samples".into(), "not part of All.sln".into()),
            ("samples/A".into(), "not part of All.sln".into()),
            ("".into(), "not part of All.sln".into()),
            ("src".into(), "not part of All.sln".into()),
        ],
        &["src/My App".into()],
    );
    assert_eq!(pending.keys().collect::<Vec<_>>(), vec!["samples"]);
}

#[test]
fn rule_csharp_answers_are_asked_per_project_context() {
    let policy = Hooks.answer_policy();
    assert!(policy.project_contexts);
    assert!(policy.dedupe_locations);
    assert!(!policy.inactive_regions);
}

#[test]
fn rule_nuget_package_locations_are_library_files() {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("nuget-{}", uuid::Uuid::new_v4().simple()));
    let file = dir
        .join("newtonsoft.json")
        .join("13.0.3")
        .join("src")
        .join("JsonConvert.cs");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "class JsonConvert {}").unwrap();
    let prepared = Prepared {
        library_roots: vec![trace_env::LibraryRoot {
            path: dir.clone(),
            kind: trace_env::LibraryKind::Dependency,
            ecosystem: EcosystemId::Dotnet,
            layout: "nuget_packages",
            version: None,
        }],
        ..Prepared::default()
    };
    let uri = url::Url::from_file_path(&file).unwrap().to_string();
    let loc = Hooks.external_location(&uri, &prepared).unwrap();
    assert_eq!(loc.package, "newtonsoft.json");
    assert_eq!(loc.version.as_deref(), Some("13.0.3"));
    assert!(loc.readable);
    let meta = Hooks
        .external_location(
            "csharp:/metadata/projects/App/assemblies/System.Runtime/symbols/System.String.cs",
            &prepared,
        )
        .unwrap();
    assert!(!meta.readable);
    assert_eq!(meta.symbol.as_deref(), Some("System.String"));
    let _ = std::fs::remove_dir_all(&dir);
}
