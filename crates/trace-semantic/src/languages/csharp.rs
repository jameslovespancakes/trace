//! C# setup hooks (owner dotnet; DESIGN §4.8).
//!
//! Server: Roslyn LS (`Microsoft.CodeAnalysis.LanguageServer`, the server behind VS Code's C#
//! extension) ALWAYS on the trace-managed .NET 10 runtime (`_runtimes.json` `dotnet`, PLAN
//! decision 11); the user's .NET SDK is only the project's MSBuild toolchain (Roslyn's
//! BuildHost starts from the `dotnet` on PATH, which the preflight points at the selected SDK).
//!
//! **Preflight** (collects every independent failure):
//! 1. project shape / platform: no `*.csproj` at all -> `Unsupported`; the old .NET Framework
//!    project format needs Visual Studio's MSBuild: off Windows -> `Unsupported` (stops), on
//!    Windows without Visual Studio (Build Tools) -> `ToolchainMissing`;
//! 2. toolchain: the SDK global.json accepts (roll-forward rules) and that supports the
//!    projects' `netX.Y` target frameworks, else `ToolchainMissing` / `ToolchainVersion`;
//!    workloads of `-ios`/`-android`/... target frameworks and MAUI;
//! 3. server + runtime installed in the tools folder;
//! 4. dependencies: every required project restored (`obj/project.assets.json` covering all
//!    target frameworks, `*.nuget.g.props`, every package in the NuGet folder) - measured: an
//!    unrestored project loads and answers WRONG;
//! 5. build approval, always (MSBuild evaluation, design-time build, source generators).
//!
//! **Opening**: the solution covering most of the repository's C# projects (`solution/open`),
//! else every project (`project/open`); never `--autoLoadProjects` (measured wrong
//! cross-project definitions). Projects outside the opened solution are sub-projects.
//!
//! **check_loaded**: Roslyn signals readiness even when every project failed to load, so the
//! load messages are mapped: SDK not found -> `ToolchainMissing`, unresolved dependencies ->
//! `DepsMissing`, other load errors -> `BuildFailed`.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use trace_core::fingerprint::PartsHasher;
use trace_core::model::Diagnostic;
use trace_core::setup_error::SetupError;
use trace_core::Language;
use trace_env::dotnet::{self, DotnetProject};
use trace_env::os::Os;
use trace_env::{EcosystemId, ToolchainStatus};

use super::{
    default_prepared, detect_context, read_context, toolchain_spec, ExternalLocation, LoadedContext,
    Prepared, Server, SetupContext, WorkspaceMode,
};
use crate::backends::fntype::FnTypeRoute;
use crate::registry::{BuildSpec, BuildWhen};
use crate::setup::{
    deps_error, require_approval, require_runtimes, require_server, Collect, STATUS_DEPENDENCIES,
    STATUS_TOOLCHAIN,
};

pub struct Hooks;

/// Where to get the .NET SDK (error texts).
pub const SDK_INSTALL: &str = "Install it from https://dotnet.microsoft.com/download";

/// Backend-private data for `check_loaded` / `external_location`.
#[derive(Clone, Debug, Default)]
pub struct CsharpData {
    pub restore_hint: String,
}

fn csharp_data(prepared: &Prepared) -> Option<&CsharpData> {
    prepared.data.as_ref()?.downcast_ref::<CsharpData>()
}

/// "This C# code has no project file (.csproj), so it cannot be built."
pub fn no_project_error() -> SetupError {
    SetupError::Unsupported {
        language: Language::CSharp,
        first: "This C# code has no project file (.csproj), so it cannot be built.".to_string(),
        second: Some(
            "Create one (dotnet new classlib) or exclude these files in your trace settings, and run trace again."
                .to_string(),
        ),
    }
}

/// The old .NET Framework project format off Windows.
pub fn legacy_project_error(os_name: &str) -> SetupError {
    SetupError::Unsupported {
        language: Language::CSharp,
        first: format!(
            "This C# project could not be built on {os_name} (it uses the old .NET Framework project format, which needs Visual Studio)."
        ),
        second: Some("Run trace on Windows to analyze it.".to_string()),
    }
}

/// A missing .NET workload.
pub fn workload_error(id: &str) -> SetupError {
    SetupError::ToolchainMissing {
        language: Language::CSharp,
        needs: format!("the .NET {id} workload"),
        install: format!("Install it with dotnet workload install {id}"),
    }
}

/// `{snapshot_uri}/<rel>` (the client expands `{snapshot_uri}` to the workspace root URI).
fn workspace_uri(rel: &str) -> String {
    let encoded: Vec<String> = rel.split('/').map(percent_encode).collect();
    format!("{{snapshot_uri}}/{}", encoded.join("/"))
}

/// Percent-encode one URI path segment (RFC 3986 unreserved characters kept).
fn percent_encode(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b'+'
                    | b'('
                    | b')'
                    | b'@'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b','
                    | b';'
                    | b'='
            )
        {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `after_initialized` parameters: (`solution/open` params, `project/open` params); exactly one
/// is non-null (the registry entry sends both notifications; null params are not sent).
pub fn open_params(project: &DotnetProject) -> (Value, Value) {
    match project.chosen_solution() {
        Some(s) => (json!({ "solution": workspace_uri(&s.rel) }), Value::Null),
        None => {
            let projects: Vec<String> = project.required_projects().map(|p| workspace_uri(&p.rel)).collect();
            (Value::Null, json!({ "projects": projects }))
        }
    }
}

/// Relative directories that become pending (sub-projects): never the root, never a
/// directory holding a required directory.
pub(crate) fn pending_dirs(subs: &[(String, String)], required: &[String]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (dir, reason) in subs {
        if dir.is_empty() {
            continue;
        }
        let holds_required = required.iter().any(|r| r == dir || r.starts_with(&format!("{dir}/")));
        if !holds_required {
            out.insert(dir.clone(), reason.clone());
        }
    }
    // Drop directories below another pending directory.
    let keys: Vec<String> = out.keys().cloned().collect();
    out.retain(|d, _| !keys.iter().any(|k| k != d && d.starts_with(&format!("{k}/"))));
    out
}

impl Server for Hooks {
    fn preflight(&self, cx: &SetupContext<'_>) -> Result<Prepared, SetupError> {
        let language = Language::CSharp;
        let dcx = detect_context(cx, EcosystemId::Dotnet);
        let readable = trace_core::paths::forbidden_roots();
        let rcx = read_context(cx, EcosystemId::Dotnet, &readable);
        let project = dotnet::project(&rcx);
        if project.projects.is_empty() {
            return Err(no_project_error());
        }
        let mut collect = Collect::default();
        if !project.legacy_projects().is_empty() {
            if cx.platform.os != Os::Windows {
                return Err(legacy_project_error(cx.platform.os_name()));
            }
            if dotnet::visual_studio_msbuild(cx.vars, cx.platform).is_none() {
                collect.push(SetupError::ToolchainMissing {
                    language,
                    needs: "Visual Studio or its Build Tools (the old .NET Framework project format needs their MSBuild)"
                        .to_string(),
                    install: "Install them from https://visualstudio.microsoft.com/downloads".to_string(),
                });
            }
        }

        let spec = toolchain_spec(cx, "dotnet", "the .NET SDK", SDK_INSTALL);
        let toolchain = match dotnet::toolchain_for(&dcx, &project) {
            ToolchainStatus::Found(t) => Some(t),
            ToolchainStatus::TooOld {
                found,
                needed,
                source,
            } => {
                collect.push(SetupError::ToolchainVersion {
                    language,
                    needs: needed.describe("the .NET SDK"),
                    source,
                    tool: ".NET SDK".to_string(),
                    found: found.version.map(|v| v.text).unwrap_or_else(|| "unknown".to_string()),
                    install: spec.install.clone(),
                });
                None
            }
            ToolchainStatus::Missing { .. } => {
                collect.push(SetupError::ToolchainMissing {
                    language,
                    needs: spec.needs.clone(),
                    install: spec.install.clone(),
                });
                None
            }
            ToolchainStatus::NotNeeded => None,
        };
        if let Some(tc) = &toolchain {
            for id in dotnet::missing_workloads(&project, tc) {
                collect.push(workload_error(&id));
            }
        }

        collect.check(require_server(cx));
        collect.check(require_runtimes(cx));

        let deps = dotnet::deps_for(&rcx, &project);
        if let Some(e) = deps_error(language, &deps) {
            collect.push(e);
        }

        let build = cx.entry.requires_build.clone().unwrap_or(BuildSpec {
            tool: "MSBuild".to_string(),
            runs: "this project's build scripts".to_string(),
            when: BuildWhen::Always,
        });
        collect.check(require_approval(cx, &build));

        let Some(toolchain) = toolchain.filter(|_| collect.is_empty()) else {
            return collect.finish(default_prepared(cx));
        };

        let mut prepared = default_prepared(cx);
        prepared.workspace = WorkspaceMode::Mirror;
        prepared.runs_project_code = true;
        let sdk_root = toolchain.root.display().to_string();
        let sdk_version = toolchain
            .version
            .as_ref()
            .map(|v| v.text.clone())
            .unwrap_or_else(|| "unknown".to_string());
        prepared.vars.insert("toolchain".to_string(), sdk_root.clone());
        // Roslyn's BuildHost starts from the `dotnet` on PATH: the selected SDK's root first.
        prepared.env.insert(
            "PATH".to_string(),
            trace_env::lookup::compose_path(std::slice::from_ref(&toolchain.root), cx.vars, cx.platform),
        );
        if let Some(folder) = deps
            .roots
            .iter()
            .find(|r| r.layout == "nuget_packages")
            .map(|r| r.path.display().to_string())
        {
            prepared.env.insert("NUGET_PACKAGES".to_string(), folder);
        }
        if project.windows_targeting() && cx.platform.os != Os::Windows {
            // MSBuild reads environment variables as properties.
            prepared
                .env
                .insert("EnableWindowsTargeting".to_string(), "true".to_string());
        }
        let (solution_open, project_open) = open_params(&project);
        prepared
            .json_vars
            .insert("roslyn_solution_open".to_string(), solution_open);
        prepared
            .json_vars
            .insert("roslyn_project_open".to_string(), project_open);
        prepared.library_roots = deps.roots.clone();

        let required_dirs: Vec<String> = project.required_projects().map(|p| p.dir.clone()).collect();
        let subs: Vec<(String, String)> = deps
            .subprojects
            .iter()
            .map(|s| (s.dir.clone(), s.reason.clone()))
            .collect();
        prepared.pending_dirs = pending_dirs(&subs, &required_dirs);

        let mut fp = PartsHasher::new();
        fp.text(&sdk_root)
            .text(&sdk_version)
            .text(&deps.fingerprint)
            .text(project.chosen_solution().map_or("", |s| s.rel.as_str()))
            .text(if prepared.env.contains_key("EnableWindowsTargeting") {
                "win-targeting"
            } else {
                ""
            });
        for p in project.required_projects() {
            fp.text(&p.rel);
        }
        prepared.fingerprint = fp.finish().hex_prefix(32);

        prepared
            .status
            .push(format!("{STATUS_TOOLCHAIN}.NET SDK {sdk_version} ({sdk_root})"));
        let required = project.required.len();
        prepared.status.push(format!(
            "{STATUS_DEPENDENCIES}restored ({required} project{})",
            if required == 1 { "" } else { "s" }
        ));
        match project.chosen_solution() {
            Some(s) => prepared.status.push(format!(
                "opens {} ({required} of {} C# projects)",
                s.rel,
                project.projects.len()
            )),
            None => prepared
                .status
                .push(format!("opens {required} C# project(s) (no solution file)")),
        }
        if let Some(runtime) = cx.tools.manifest.tools.get("dotnet") {
            prepared
                .status
                .push(format!("server runs on the trace-managed .NET runtime {}", runtime.version));
        }
        prepared.status.extend(deps.notes.iter().cloned());
        prepared.data = Some(Arc::new(CsharpData {
            restore_hint: deps.hint.clone(),
        }));
        prepared.toolchain = Some(toolchain);
        Ok(prepared)
    }

    fn check_loaded(&self, cx: &LoadedContext<'_>) -> Result<Vec<Diagnostic>, SetupError> {
        let hint = csharp_data(cx.prepared)
            .map(|d| d.restore_hint.clone())
            .unwrap_or_else(|| "dotnet restore".to_string());
        match classify_load(&load_texts(cx)) {
            RoslynLoad::Sdk(requested) => Err(SetupError::ToolchainMissing {
                language: Language::CSharp,
                needs: match requested {
                    Some(v) => format!("the .NET SDK {v}"),
                    None => "the .NET SDK this project requests".to_string(),
                },
                install: SDK_INSTALL.to_string(),
            }),
            RoslynLoad::Unrestored => Err(SetupError::DepsMissing {
                language: Language::CSharp,
                hint,
            }),
            RoslynLoad::Failed(what) => Err(SetupError::BuildFailed {
                language: Language::CSharp,
                what,
                log: cx.log.to_path_buf(),
            }),
            RoslynLoad::Ok(warnings) => Ok(warnings
                .into_iter()
                .map(|w| Diagnostic::new("build_warning", None, w))
                .collect()),
        }
    }

    fn external_location(&self, uri: &str, prepared: &Prepared) -> Option<ExternalLocation> {
        external(uri, prepared)
    }

    fn answer_policy(&self) -> super::AnswerPolicy {
        // Multi-targeted projects are loaded once per target framework: identical locations
        // can come back once per framework, and a file is answered only in the project
        // context (target framework) the request names, so every context of the file is
        // asked (files compiled only for one framework, e.g. under `#if`, get answers).
        super::AnswerPolicy {
            dedupe_locations: true,
            project_contexts: true,
            ..super::AnswerPolicy::default()
        }
    }

    fn fn_type_route(&self, _language: Language) -> FnTypeRoute {
        FnTypeRoute::Label
    }
}

/// What Roslyn's load messages say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoslynLoad {
    /// "A compatible .NET SDK was not found" (requested version when the message names it).
    Sdk(Option<String>),
    /// "Project ... has unresolved dependencies" / restore required.
    Unrestored,
    /// Another load error (short description).
    Failed(String),
    /// Loaded; warnings kept as notes.
    Ok(Vec<String>),
}

/// Classify Roslyn project-load messages (severity 1 = error, 2 = warning). Priority: SDK,
/// restore, other errors.
pub fn classify_load(texts: &[(u8, String)]) -> RoslynLoad {
    let mut unrestored = false;
    let mut failed: Option<String> = None;
    let mut warnings = Vec::new();
    for (severity, text) in texts {
        let lower = text.to_ascii_lowercase();
        if lower.contains("compatible .net sdk was not found")
            || lower.contains("does not support targeting")
            || lower.contains("the .net sdk could not be found")
        {
            let requested = text
                .find("Requested SDK version:")
                .map(|i| &text[i + "Requested SDK version:".len()..])
                .and_then(|rest| rest.split_whitespace().next())
                .map(|v| v.trim_end_matches(['.', ',']).to_string());
            return RoslynLoad::Sdk(requested);
        }
        if lower.contains("unresolved dependencies")
            || lower.contains("run a nuget package restore")
            || (lower.contains("project.assets.json") && lower.contains("not found"))
        {
            unrestored = true;
            continue;
        }
        let load_error = lower.contains("error while loading")
            || lower.contains("failed to load project")
            || lower.contains("failed to initialize buildhost")
            || (lower.contains("reference assemblies") && lower.contains("were not found"));
        if load_error && (*severity == 1 || lower.contains("reference assemblies")) {
            failed.get_or_insert_with(|| short(text));
        } else if *severity == 2 && lower.contains("while loading") {
            warnings.push(short(text));
        }
    }
    if unrestored {
        return RoslynLoad::Unrestored;
    }
    if let Some(what) = failed {
        return RoslynLoad::Failed(format!("MSBuild could not load it: {what}"));
    }
    RoslynLoad::Ok(warnings)
}

/// First line of a message, at most 160 characters.
fn short(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    let mut out: String = line.chars().take(160).collect();
    if line.chars().count() > 160 {
        out.push_str("...");
    }
    out
}

/// A NuGet package file (`<packages>/<id>/<version>/...`) or Roslyn's metadata-as-source
/// document (declarations only).
fn external(uri: &str, prepared: &Prepared) -> Option<ExternalLocation> {
    if let Ok(path) = crate::lsp::uri_to_path(uri) {
        for root in prepared.library_roots.iter().filter(|r| r.layout == "nuget_packages") {
            let Ok(rest) = path.strip_prefix(&root.path) else {
                continue;
            };
            let mut parts = rest.components().map(|c| c.as_os_str().to_string_lossy().to_string());
            let (Some(id), Some(version)) = (parts.next(), parts.next()) else {
                continue;
            };
            return Some(ExternalLocation {
                path: path.display().to_string(),
                line: 0,
                column: 0,
                package: id,
                version: Some(version),
                stdlib: false,
                readable: path.extension().is_some_and(|e| e == "cs") && path.is_file(),
                symbol: None,
            });
        }
        let metadata = path
            .components()
            .any(|c| c.as_os_str().to_string_lossy().contains("MetadataAsSource"));
        if metadata {
            return Some(ExternalLocation {
                path: path.display().to_string(),
                line: 0,
                column: 0,
                package: "dotnet-metadata".to_string(),
                version: None,
                stdlib: false,
                readable: false,
                symbol: path.file_stem().map(|s| s.to_string_lossy().to_string()),
            });
        }
        return None;
    }
    uri.starts_with("csharp:").then(|| ExternalLocation {
        path: uri.to_string(),
        line: 0,
        column: 0,
        package: "dotnet-metadata".to_string(),
        version: None,
        stdlib: false,
        readable: false,
        symbol: uri
            .rsplit('/')
            .next()
            .map(|s| s.trim_end_matches(".cs").to_string())
            .filter(|s| !s.is_empty()),
    })
}

// ---------------------------------------------------------------------------------------------
// Helpers of the C# hooks
// ---------------------------------------------------------------------------------------------

/// Load messages of a server: `window/logMessage` / `showMessage` of severity error or
/// warning, plus toast / message notifications (`window/_roslyn_showToast`,
/// `window/showMessage`) with their `messageType` / `type`.
pub(crate) fn load_texts(cx: &LoadedContext<'_>) -> Vec<(u8, String)> {
    let mut out: Vec<(u8, String)> = cx
        .log_messages
        .iter()
        .filter(|(t, _)| *t == 1 || *t == 2)
        .cloned()
        .collect();
    for (method, params) in cx.notifications {
        if !(method.ends_with("showToast") || method == "window/showMessage" || method == "window/logMessage")
        {
            continue;
        }
        let severity = params
            .get("messageType")
            .or_else(|| params.get("type"))
            .and_then(Value::as_u64)
            .and_then(|t| u8::try_from(t).ok())
            .unwrap_or(3);
        if let Some(message) = params.get("message").and_then(Value::as_str) {
            if severity <= 2 && !out.iter().any(|(_, m)| m == message) {
                out.push((severity, message.to_string()));
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "../../tests/unit/languages/csharp.rs"]
mod tests;
