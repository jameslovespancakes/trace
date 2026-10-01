//! Java setup hooks (owner jvm): Eclipse jdtls on the trace-managed JDK 21.
//!
//! * **Toolchain**: the user's JDK (`trace_env::jvm`), Java 8 or newer and at least the
//!   release the project compiles for; every detected JDK is passed to jdtls as an explicit
//!   `java.configuration.runtimes` entry so the choice is deterministic.
//! * **Build import**: Maven (m2e) and Gradle (Buildship) run the project's build plugins /
//!   build scripts, so they need `trace index --allow-build`. The import runs offline: a
//!   generated `settings.xml` in the state folder (`<offline>true</offline>`, the detected local
//!   repository), `--offline` for Gradle, and a dead proxy for the whole JVM so jdtls' own
//!   network lookups (Maven Central identification, source downloads) fail locally. The Gradle
//!   daemon runs on a JDK the Gradle version accepts (`gradle_import_jdk`: never an older
//!   `JAVA_HOME`), keeps its registry in the state folder and stops after a short idle time
//!   (15 s: `-Dorg.gradle.daemon.idletimeout` in the registry entry).
//! * **Annotation processors**: generated sources are required only for processor-path jars
//!   that declare an annotation processor (`trace_env::jvm`); javac plugins generate nothing.
//! * **Build-less Java** (no pom.xml / Gradle build): no approval; `prepare_workspace` writes
//!   Eclipse `.project` + `.classpath` whose `src` entries are the source roots implied by
//!   the Java rule "package `a.b` lives in `<root>/a/b/`": for every import of a fully
//!   qualified name `a.b.C` that names a partition file `<root>/a/b/C.java`, `<root>` is a
//!   source root; every other Java file whose directory ends with a known package path gets
//!   the prefix as its root (test sources). jdtls reads these static files with its Eclipse
//!   importer; it is the project model of plain source folders, never a fallback.
//! * **After loading**: m2e / Buildship markers of missing artifacts are the dependency error,
//!   other import failures the build error.
//! * **Call hierarchy**: implementor search is off in jdtls' instance preferences (it builds a
//!   type hierarchy per abstract / interface method of the queried file on every
//!   `outgoingCalls`); trace asks implementations itself. The registry entry also caps the
//!   requests in flight (`max_in_flight`): jdtls serializes the work of concurrent requests,
//!   so a deep pipeline only turns waiting time into request timeouts.
//! * **Answers**: `jdt://` class-file locations are external libraries (package from the jar
//!   path); `-32603` on a definition into a jar without sources is "unresolved", not a failure.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use trace_core::model::Diagnostic;
use trace_core::setup_error::SetupError;
use trace_core::Language;
use trace_env::jvm::{self, BuildSystem, JvmSetup, MIN_JAVA_FEATURE};
use trace_env::os::{Arch, Os, Platform};
use trace_env::{EcosystemId, Toolchain, ToolchainStatus};

use super::jvm::{
    approval, classify_load, ensure_state_dirs, is_language_stdlib, jar_library, jar_location, jdk_error,
    jdk_status, load_texts, path_text, pending_dirs, percent_decode, prepared_fingerprint, write_state_file,
    LoadOutcome,
};
use super::read_context;
use super::{
    write_generated, AnswerPolicy, ExternalLocation, LoadedContext, Prepared, Server, SetupContext,
    WorkspaceContext, WorkspaceMode,
};
use crate::backend::SemanticFile;
use crate::backends::fntype::FnTypeRoute;
use crate::setup::{deps_error, require_runtimes, require_server, Collect};

pub struct Hooks;

/// Backend-private data of a Java preflight (read by `prepare_workspace` / `check_loaded`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JavaData {
    /// The imported build system (None: build-less sources).
    pub build: Option<BuildSystem>,
    pub maven_repo: PathBuf,
}

impl Server for Hooks {
    fn preflight(&self, cx: &SetupContext<'_>) -> Result<Prepared, SetupError> {
        let language = Language::Java;
        let readable = trace_core::paths::forbidden_roots();
        let dcx = read_context(cx, EcosystemId::Jvm, &readable);
        let setup = jvm::setup(&dcx);
        let mut c = Collect::default();
        let systems = java_systems(&setup);

        // 1. Toolchain: the project JDK, and Gradle itself when the build has no wrapper.
        let status = setup.select_jdk(MIN_JAVA_FEATURE);
        if let Some(e) = jdk_error(language, &setup, MIN_JAVA_FEATURE, &status) {
            c.push(e);
        }
        if let Some(e) = gradle_error(language, &setup, &systems) {
            c.push(e);
        }
        // 2. Server + the trace-managed JDK 21 it runs on.
        c.check(require_server(cx));
        c.check(require_runtimes(cx));
        // 3. Dependencies and generated sources (static).
        let report = setup.deps_report(&systems);
        if let Some(e) = deps_error(language, &report) {
            c.push(e);
        }
        if let Some(e) = generated_sources_error(language, &setup, &systems) {
            c.push(e);
        }
        // 4. Approval: Maven / Gradle run project code during the import.
        if let Some(first) = systems.first() {
            c.check(approval(cx, *first));
        }
        let jdk = match status {
            ToolchainStatus::Found(t) => Some(t),
            _ => None,
        };
        let prepared = java_prepared(cx, &setup, &systems, jdk, report.roots);
        c.finish(prepared)
    }

    fn prepare_workspace(&self, cx: &WorkspaceContext<'_>) -> Result<(), SetupError> {
        let language = Language::Java;
        ensure_state_dirs(cx, language)?;
        let data = java_data(cx.prepared);
        let repo = data.map(|d| d.maven_repo.clone()).unwrap_or_default();
        write_state_file(cx, language, "maven-settings.xml", maven_settings_xml(&repo).as_bytes())?;
        write_call_hierarchy_preference(cx, language)?;
        if data.is_none_or(|d| d.build.is_none()) {
            for (name, bytes) in eclipse_project(cx.files) {
                write_generated(cx, language, &name, &bytes)?;
            }
        }
        Ok(())
    }

    fn check_loaded(&self, cx: &LoadedContext<'_>) -> Result<Vec<Diagnostic>, SetupError> {
        let Some(build) = java_data(cx.prepared).and_then(|d| d.build) else {
            return Ok(Vec::new());
        };
        let texts = load_texts(cx.log_messages, cx.notifications, cx.diagnostics);
        match classify_load(&texts) {
            LoadOutcome::DepsMissing => Err(SetupError::DepsMissing {
                language: Language::Java,
                hint: build.hint().to_string(),
            }),
            LoadOutcome::BuildFailed(message) => Err(SetupError::BuildFailed {
                language: Language::Java,
                what: format!("{} import failed: {message}", build.tool()),
                log: cx.log.to_path_buf(),
            }),
            LoadOutcome::Ok(notes) => Ok(notes
                .into_iter()
                .map(|n| Diagnostic::new("build_warning", None, n))
                .collect()),
        }
    }

    fn external_location(&self, uri: &str, prepared: &Prepared) -> Option<ExternalLocation> {
        let repo = java_data(prepared).map(|d| d.maven_repo.clone());
        jdt_location(uri, repo.as_deref()).or_else(|| jar_location(uri))
    }

    fn answer_policy(&self) -> AnswerPolicy {
        AnswerPolicy {
            internal_error_is_unresolved: true,
            ..AnswerPolicy::default()
        }
    }

    fn fn_type_route(&self, _language: Language) -> FnTypeRoute {
        FnTypeRoute::LanguageRule
    }
}

// ---------------------------------------------------------------------------------------------
// JDK requirements, Gradle, jar and jdt locations, state files
// ---------------------------------------------------------------------------------------------

/// A Gradle build without a wrapper needs an installed Gradle.
pub(crate) fn gradle_error(
    language: Language,
    setup: &JvmSetup,
    systems: &[BuildSystem],
) -> Option<SetupError> {
    (systems.contains(&BuildSystem::Gradle)
        && setup.project.gradle_wrappers.is_empty()
        && setup.gradle.is_none())
    .then(|| SetupError::ToolchainMissing {
        language,
        needs: "Gradle".to_string(),
        install: "Install it from https://gradle.org/install".to_string(),
    })
}

/// Generated sources that only a build creates (annotation processors, protobuf):
/// "Java needs this project's generated sources (core/target/generated-sources)." /
/// "Build the project once (mvn compile) and run trace again."
pub(crate) fn generated_sources_error(
    language: Language,
    setup: &JvmSetup,
    systems: &[BuildSystem],
) -> Option<SetupError> {
    let dirs = &setup.project.generated_sources;
    let first = dirs.first()?;
    let command = if first.ends_with("build/generated") {
        if !systems.contains(&BuildSystem::Gradle) {
            return None;
        }
        "./gradlew build"
    } else {
        if !systems.contains(&BuildSystem::Maven) {
            return None;
        }
        // Test sources are generated by the test-compile part of the lifecycle.
        if dirs.iter().any(|d| d.ends_with("target/generated-test-sources")) {
            "mvn test-compile"
        } else {
            "mvn compile"
        }
    };
    let list = if dirs.len() > 1 {
        format!("{first} and {} more", dirs.len() - 1)
    } else {
        first.clone()
    };
    Some(SetupError::Unsupported {
        language,
        first: format!("{} needs this project's generated sources ({list}).", language.display_name()),
        second: Some(format!("Build the project once ({command}) and run trace again.")),
    })
}

/// The JDK the Gradle import runs its daemon on (`java.import.gradle.java.home`): build imports
/// never run with a JDK older than the Gradle version needs (`jvm::gradle_min_jdk`; a user's
/// `JAVA_HOME` of Java 8 cannot run Gradle 9). The project JDK when it is new enough, else the
/// newest installed JDK that is, else trace's JDK runtime (`{runtime:jdk}`, JDK 21). Returns the
/// setting value and, when the project JDK was replaced, the `trace status` line saying so.
pub(crate) fn gradle_import_jdk(
    setup: &JvmSetup,
    project_jdk: Option<&Toolchain>,
) -> (Value, Option<String>) {
    let gradle = setup.gradle_version();
    let min = jvm::gradle_min_jdk(gradle.as_ref());
    let feature = |t: &Toolchain| {
        t.facts
            .get("feature")
            .and_then(|f| f.parse::<u32>().ok())
            .unwrap_or(0)
    };
    if let Some(t) = project_jdk.filter(|t| feature(t) >= min) {
        return (json!(path_text(&t.root)), None);
    }
    let tool = gradle.map_or_else(|| "Gradle".to_string(), |v| format!("Gradle {}", v.text));
    let old = project_jdk.map_or_else(
        || "no project JDK".to_string(),
        |t| {
            let version = t.version.as_ref().map(|v| v.text.as_str()).unwrap_or("");
            format!("the project JDK {version}")
        },
    );
    match setup.newest_jdk(min) {
        Some(t) => {
            let version = t.version.as_ref().map(|v| v.text.clone()).unwrap_or_default();
            let line = format!(
                "{tool} import runs on JDK {version} ({}): {old} is older than Java {min}",
                t.root.display()
            );
            (json!(path_text(&t.root)), Some(line))
        }
        None => (
            json!("{runtime:jdk}"),
            Some(format!("{tool} import runs on trace's JDK runtime: {old} is older than Java {min}")),
        ),
    }
}

/// Maven and Gradle builds of the repository (the build systems jdtls imports).
fn java_systems(setup: &JvmSetup) -> Vec<BuildSystem> {
    setup
        .project
        .systems()
        .into_iter()
        .filter(|s| matches!(s, BuildSystem::Maven | BuildSystem::Gradle))
        .collect()
}

/// `jdt://contents/<jar>/<package>/<Class>.class?=<project>/<escaped jar path><<package>(<Class>.class`
fn jdt_location(uri: &str, maven_repo: Option<&Path>) -> Option<ExternalLocation> {
    let rest = uri.strip_prefix("jdt://contents/")?;
    let (path_part, query) = rest.split_once("?=").unwrap_or((rest, ""));
    let path_part = percent_decode(path_part);
    let segs: Vec<&str> = path_part.split('/').collect();
    let jar_name = segs.first().copied().unwrap_or("");
    let (package_name, class_file) = match segs.len() {
        0 | 1 => ("", ""),
        2 => ("", segs[1]),
        _ => (segs[1], segs[segs.len() - 1]),
    };
    let class = class_file.strip_suffix(".class").unwrap_or(class_file);
    let symbol = if class.is_empty() {
        None
    } else if package_name.is_empty() {
        Some(class.to_string())
    } else {
        Some(format!("{package_name}.{class}"))
    };
    let decoded = percent_decode(query).replace("\\/", "/").replace('\\', "/");
    let jar_path = decoded
        .split_once('/')
        .map(|(_, p)| p)
        .unwrap_or(&decoded)
        .split('<')
        .next()
        .unwrap_or("")
        .to_string();
    let lower = jar_path.to_ascii_lowercase();
    let jdk = !jar_name.ends_with(".jar")
        || lower.contains("/jmods/")
        || lower.ends_with("/rt.jar")
        || lower.contains("jrt-fs.jar")
        || lower.ends_with("/lib/modules");
    let (package, version) = if jdk {
        ("jdk".to_string(), None)
    } else {
        let source = if jar_path.is_empty() {
            jar_name.to_string()
        } else {
            jar_path.clone()
        };
        let (mut package, version) = jar_library(&source);
        // A jar under the detected local repository names its group exactly.
        if let Some(repo) = maven_repo {
            let repo_text = repo.to_string_lossy().replace('\\', "/");
            if let Some(inner) = jar_path.strip_prefix(repo_text.trim_end_matches('/')) {
                let parts: Vec<&str> = inner.split('/').filter(|s| !s.is_empty()).collect();
                if parts.len() >= 4 {
                    package = format!("{}:{}", parts[..parts.len() - 3].join("."), parts[parts.len() - 3]);
                }
            }
        }
        (package, version)
    };
    Some(ExternalLocation {
        path: uri.to_string(),
        line: 0,
        column: 0,
        stdlib: is_language_stdlib(&package),
        package,
        version,
        readable: false,
        symbol,
    })
}

/// jdtls shared configuration folder of the platform (`-Dosgi.sharedConfiguration.area`).
pub fn jdtls_config_dir(p: &Platform) -> &'static str {
    match (p.os, p.arch) {
        (Os::Windows, _) => "config_win",
        (Os::Linux, Arch::Aarch64) => "config_linux_arm",
        (Os::Linux, _) => "config_linux",
        (Os::MacOs, Arch::Aarch64) => "config_mac_arm",
        (Os::MacOs, _) => "config_mac",
    }
}

/// jdtls' own instance preferences, in the `-data` workspace of the state folder (the
/// registry's `-data {outside}/jdtls-data`).
const SERVER_PREFERENCES: &str =
    "jdtls-data/.metadata/.plugins/org.eclipse.core.runtime/.settings/org.eclipse.jdt.ls.core.prefs";

/// The call hierarchy preference trace sets: no implementor search. With it, every
/// `callHierarchy/outgoingCalls` builds a type hierarchy for each abstract / interface method
/// declared anywhere in the queried method's file (the visitor walks the whole file) and
/// swaps an interface call for its single implementation; trace resolves implementations
/// itself (`textDocument/implementation`), so the answer it needs is the called declaration.
const IMPLEMENTOR_SEARCH_OFF: (&str, &str) = ("PREF_USE_IMPLEMENTORS", "false");

/// Set [`IMPLEMENTOR_SEARCH_OFF`] in jdtls' instance preferences, keeping what the server
/// stored there itself (workspace state).
fn write_call_hierarchy_preference(cx: &WorkspaceContext<'_>, language: Language) -> Result<(), SetupError> {
    let path = cx.outside.join(SERVER_PREFERENCES);
    let failed = |e: std::io::Error| SetupError::BuildFailed {
        language,
        what: format!("writing {} failed: {e}", path.display()),
        log: cx.log.to_path_buf(),
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(failed)?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let (key, value) = IMPLEMENTOR_SEARCH_OFF;
    std::fs::write(&path, with_preference(&existing, key, value)).map_err(failed)
}

/// An Eclipse `.prefs` text with `key=value` set: other entries are kept, an earlier value of
/// `key` is replaced.
fn with_preference(existing: &str, key: &str, value: &str) -> String {
    let mut out = String::new();
    let mut versioned = false;
    for line in existing.lines() {
        if line.split_once('=').is_some_and(|(k, _)| k.trim() == key) {
            continue;
        }
        versioned |= line.starts_with("eclipse.preferences.version=");
        out.push_str(line);
        out.push('\n');
    }
    if !versioned {
        out.push_str("eclipse.preferences.version=1\n");
    }
    out.push_str(&format!("{key}={value}\n"));
    out
}

/// The offline Maven settings jdtls (m2e) reads: the detected local repository, offline.
pub fn maven_settings_xml(repo: &Path) -> String {
    let esc = repo
        .display()
        .to_string()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<settings xmlns=\"http://maven.apache.org/SETTINGS/1.0.0\">\n  <localRepository>{esc}</localRepository>\n  <offline>true</offline>\n  <interactiveMode>false</interactiveMode>\n</settings>\n"
    )
}

fn java_data(prepared: &Prepared) -> Option<&JavaData> {
    prepared.data.as_ref()?.downcast_ref::<JavaData>()
}

fn java_prepared(
    cx: &SetupContext<'_>,
    setup: &JvmSetup,
    systems: &[BuildSystem],
    jdk: Option<Toolchain>,
    library_roots: Vec<trace_env::LibraryRoot>,
) -> Prepared {
    let build = systems.first().copied();
    let project = &setup.project;
    let jdk_home = jdk.as_ref().map(|t| path_text(&t.root)).unwrap_or_default();
    let mut vars = BTreeMap::new();
    vars.insert("jdk".to_string(), jdk_home.clone());
    // `-Dosgi.sharedConfiguration.area`: the platform's read-only configuration folder of the
    // installed jdtls (absolute; not a `{tool:..}` reference, which cannot hold a preflight
    // variable).
    let config = jdtls_config_dir(cx.platform);
    vars.insert(
        "jdtls_config".to_string(),
        cx.tools
            .tool_dir("jdtls")
            .map_or_else(|| config.to_string(), |dir| path_text(&dir.join(config))),
    );

    // Explicit runtimes (jdtls would otherwise pick installed JVMs on its own).
    let mut runtimes: Vec<Value> = Vec::new();
    for j in setup.runtimes() {
        let chosen = jdk
            .as_ref()
            .filter(|t| t.facts.get("feature").is_some_and(|f| *f == j.feature.to_string()));
        let path = chosen
            .map(|t| path_text(&t.root))
            .unwrap_or_else(|| path_text(&j.home));
        runtimes.push(json!({
            "name": j.environment_name(),
            "path": path,
            "default": chosen.is_some(),
        }));
    }
    let wrapper = !project.gradle_wrappers.is_empty();
    let gradle_home = if wrapper {
        Value::Null
    } else {
        setup
            .gradle
            .as_ref()
            .map_or(Value::Null, |g| Value::String(path_text(&g.root)))
    };
    let mut exclusions = vec![
        json!("**/node_modules/**"),
        json!("**/.git/**"),
        json!("**/.metals/**"),
        json!("**/.bloop/**"),
    ];
    for sp in &project.subprojects {
        exclusions.push(json!(format!("**/{}/**", sp.dir)));
    }
    let mut json_vars = BTreeMap::new();
    json_vars.insert("maven_import".to_string(), json!(systems.contains(&BuildSystem::Maven)));
    json_vars.insert("gradle_import".to_string(), json!(systems.contains(&BuildSystem::Gradle)));
    json_vars.insert("gradle_wrapper".to_string(), json!(wrapper));
    json_vars.insert("gradle_user_home".to_string(), json!(path_text(&setup.gradle_user_home)));
    json_vars.insert("gradle_home".to_string(), gradle_home);
    let (gradle_java_home, gradle_jdk_note) = gradle_import_jdk(setup, jdk.as_ref());
    json_vars.insert("gradle_java_home".to_string(), gradle_java_home);
    json_vars.insert("java_runtimes".to_string(), Value::Array(runtimes));
    json_vars.insert("import_exclusions".to_string(), Value::Array(exclusions));

    let mut env = BTreeMap::new();
    env.insert("GRADLE_USER_HOME".to_string(), path_text(&setup.gradle_user_home));
    if let Some(sdk) = &setup.android_sdk {
        env.insert("ANDROID_HOME".to_string(), path_text(sdk));
    }

    let mut status = Vec::new();
    status.extend(jdk_status(jdk.as_ref()));
    match build {
        Some(b) => status.push(format!(
            "{} import ({} module(s), offline)",
            b.tool(),
            match b {
                BuildSystem::Maven => project.maven_modules.len(),
                _ => project.gradle_modules.len(),
            }
        )),
        None => status.push("plain source folders (Eclipse .classpath from package directories)".to_string()),
    }
    if systems.contains(&BuildSystem::Gradle) {
        status.extend(gradle_jdk_note);
    }
    let approved = if cx.settings.allow_build {
        "allowed"
    } else {
        "not allowed"
    };
    let fingerprint = prepared_fingerprint(&[
        "java-1",
        setup.fingerprint.as_str(),
        jdk_home.as_str(),
        build.map(BuildSystem::tool).unwrap_or("none"),
        approved,
        jdtls_config_dir(cx.platform),
    ]);
    Prepared {
        backend: cx.entry.id.clone(),
        languages: cx.languages.to_vec(),
        vars,
        json_vars,
        env,
        workspace: if build.is_some() {
            WorkspaceMode::Mirror
        } else {
            WorkspaceMode::Snapshot
        },
        generated: Vec::new(),
        library_roots,
        toolchain: jdk,
        runs_project_code: build.is_some(),
        pending_dirs: pending_dirs(setup),
        fingerprint,
        status,
        data: Some(Arc::new(JavaData {
            build,
            maven_repo: setup.maven_repo.clone(),
        })),
    }
}

// ---------------------------------------------------------------------------------------------
// Build-less Java: Eclipse .project / .classpath
// ---------------------------------------------------------------------------------------------

/// Eclipse `.project` + `.classpath` for jdtls (see the module docs).
pub fn eclipse_project(files: &[&SemanticFile<'_>]) -> Vec<(String, Vec<u8>)> {
    let java: Vec<&str> = files
        .iter()
        .filter(|f| f.language == Language::Java)
        .map(|f| f.path)
        .collect();
    if java.is_empty() {
        return Vec::new();
    }
    let mut roots: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut packages: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for f in files.iter().filter(|f| f.language == Language::Java) {
        for imp in &f.facts.imports {
            let fqn = imp.target.trim_end_matches(".*");
            let parts: Vec<&str> = fqn.split('.').filter(|s| !s.is_empty()).collect();
            // `a.b.C` (class) and `a.b.C.member` (static import): try both class prefixes.
            for n in (2..=parts.len()).rev() {
                let rel = format!("{}.java", parts[..n].join("/"));
                if let Some(p) = java.iter().find(|p| **p == rel || p.ends_with(&format!("/{rel}"))) {
                    let root = p[..p.len() - rel.len()].trim_end_matches('/');
                    roots.insert(if root.is_empty() {
                        ".".to_string()
                    } else {
                        root.to_string()
                    });
                    packages.insert(parts[..n - 1].join("/"));
                    break;
                }
            }
        }
    }
    for p in &java {
        let dir = p.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        if let Some(pkg) = packages
            .iter()
            .filter(|pkg| !pkg.is_empty() && (dir == pkg.as_str() || dir.ends_with(&format!("/{pkg}"))))
            .max_by_key(|pkg| pkg.len())
        {
            let root = dir[..dir.len() - pkg.len()].trim_end_matches('/');
            roots.insert(if root.is_empty() {
                ".".to_string()
            } else {
                root.to_string()
            });
        }
    }
    if roots.is_empty() {
        return Vec::new();
    }
    // Nested roots (a root inside another root) would make Eclipse reject the classpath.
    let list: Vec<String> = roots.iter().cloned().collect();
    let kept: Vec<&String> = list
        .iter()
        .filter(|r| {
            !list
                .iter()
                .any(|o| o != *r && (o == "." || r.starts_with(&format!("{o}/"))))
        })
        .collect();
    let project = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<projectDescription><name>trace-snapshot</name><comment/><projects/>\
<buildSpec><buildCommand><name>org.eclipse.jdt.core.javabuilder</name><arguments/></buildCommand></buildSpec>\
<natures><nature>org.eclipse.jdt.core.javanature</nature></natures></projectDescription>\n";
    let mut classpath = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<classpath>\n");
    for r in kept {
        let esc = r.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;");
        classpath.push_str(&format!("<classpathentry kind=\"src\" path=\"{esc}\"/>\n"));
    }
    classpath.push_str("<classpathentry kind=\"con\" path=\"org.eclipse.jdt.launching.JRE_CONTAINER\"/>\n<classpathentry kind=\"output\" path=\".trace-bin\"/>\n</classpath>\n");
    vec![
        (".project".to_string(), project.as_bytes().to_vec()),
        (".classpath".to_string(), classpath.into_bytes()),
    ]
}

#[cfg(test)]
#[path = "../../tests/unit/languages/java.rs"]
pub(crate) mod tests;
