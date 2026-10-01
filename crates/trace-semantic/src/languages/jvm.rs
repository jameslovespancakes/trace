//! Helpers shared by the JVM servers (jdtls, Metals): JDK status and errors, build approval,
//! state directories, jar and archive locations, URIs, and the classification of load
//! messages.

use crate::languages::{ExternalLocation, SetupContext, WorkspaceContext};
use crate::registry::{BuildSpec, BuildWhen};
use crate::setup::require_approval;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use trace_core::setup_error::SetupError;
use trace_core::Language;
use trace_env::jvm::{BuildSystem, JvmSetup, JDK_INSTALL, MIN_JAVA_FEATURE};
use trace_env::{Toolchain, ToolchainStatus};

use crate::languages::first_line;

/// "{L} needs a JDK 17 or newer, which is not installed. Install one from
/// https://adoptium.net and run trace again." (also when every installed JDK is too old for
/// the project's release).
pub(crate) fn jdk_error(
    language: Language,
    setup: &JvmSetup,
    min_feature: u32,
    status: &ToolchainStatus,
) -> Option<SetupError> {
    let required = required_feature(setup, min_feature);
    let needs = |feature: u64| {
        if feature <= u64::from(MIN_JAVA_FEATURE) {
            "a JDK".to_string()
        } else {
            format!("a JDK {feature} or newer")
        }
    };
    match status {
        ToolchainStatus::Missing { .. } => Some(SetupError::ToolchainMissing {
            language,
            needs: needs(u64::from(required)),
            install: JDK_INSTALL.to_string(),
        }),
        ToolchainStatus::TooOld { needed, .. } => {
            let feature = needed
                .min
                .as_ref()
                .and_then(|v| v.parts.first().copied())
                .unwrap_or(u64::from(required));
            Some(SetupError::ToolchainMissing {
                language,
                needs: format!("a JDK {feature} or newer"),
                install: JDK_INSTALL.to_string(),
            })
        }
        _ => None,
    }
}

/// `BuildNotAllowed` for `system` unless the repository is approved.
pub(crate) fn approval(cx: &SetupContext<'_>, system: BuildSystem) -> Result<(), SetupError> {
    require_approval(
        cx,
        &BuildSpec {
            tool: system.tool().to_string(),
            runs: system.runs().to_string(),
            when: BuildWhen::DecidedByHooks,
        },
    )
}

/// Stable text of a path for server settings.
pub(crate) fn path_text(path: &Path) -> String {
    path.display().to_string()
}

/// `file:///C:/x/y` / `file:///x/y` with unsafe bytes percent-encoded.
pub(crate) fn file_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.trim_start_matches("//?/");
    let mut out = String::from("file://");
    if !text.starts_with('/') {
        out.push('/');
    }
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => {
                out.push(char::from(b))
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Percent-decoding of URI text (invalid escapes are kept).
pub(crate) fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| char::from(b).to_digit(16);
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                // Two hex digits always fit in a byte.
                out.push(u8::try_from(h * 16 + l).unwrap_or(b'?'));
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Package ("group:artifact" or a jar name) and version of a library jar path: Gradle cache
/// (`files-2.1/<group>/<name>/<version>/<hash>/<file>`), Maven layouts
/// (`.../repository|maven2/<group path>/<artifact>/<version>/<artifact>-<version>*.jar`),
/// else the jar file name (`name-1.2.3.jar`).
pub(crate) fn jar_library(path: &str) -> (String, Option<String>) {
    let norm = path.replace('\\', "/");
    let segs: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
    let n = segs.len();
    if let Some(i) = segs.iter().position(|s| *s == "files-2.1") {
        if n > i + 3 {
            return (format!("{}:{}", segs[i + 1], segs[i + 2]), Some(segs[i + 3].to_string()));
        }
    }
    if n >= 4 {
        let (file, version, artifact) = (segs[n - 1], segs[n - 2], segs[n - 3]);
        if file.starts_with(&format!("{artifact}-{version}")) {
            let start = segs[..n - 3]
                .iter()
                .rposition(|s| *s == "repository" || *s == "maven2")
                .map(|i| i + 1);
            return match start {
                Some(start) if start < n - 3 => {
                    (format!("{}:{artifact}", segs[start..n - 3].join(".")), Some(version.to_string()))
                }
                _ => (artifact.to_string(), Some(version.to_string())),
            };
        }
    }
    let file = segs.last().copied().unwrap_or("");
    let stem = file
        .strip_suffix(".jar")
        .or_else(|| file.strip_suffix(".zip"))
        .unwrap_or(file);
    let split = stem
        .char_indices()
        .find(|(i, c)| *c == '-' && stem[i + 1..].chars().next().is_some_and(|d| d.is_ascii_digit()));
    match split {
        Some((i, _)) => (stem[..i].to_string(), Some(stem[i + 1..].to_string())),
        None => (stem.to_string(), None),
    }
}

/// The language's own standard library (JDK, Scala library): a language rule.
pub(crate) fn is_language_stdlib(package: &str) -> bool {
    package == "jdk"
        || package == "org.scala-lang:scala-library"
        || package.starts_with("org.scala-lang:scala3-library")
}

/// `jar:file:///C:/x/lib.jar!/pkg/Cls.class` / `jar:///c%3A/x/lib.jar!/pkg/File.class` and
/// `jrt:/java.base/java/util/List.class` -> an external, unreadable library location.
pub(crate) fn jar_location(uri: &str) -> Option<ExternalLocation> {
    if let Some(rest) = uri.strip_prefix("jrt:") {
        let decoded = percent_decode(rest);
        let parts: Vec<&str> = decoded.trim_start_matches('/').splitn(2, '/').collect();
        let entry = parts.get(1).copied().unwrap_or("");
        return Some(ExternalLocation {
            path: uri.to_string(),
            line: 0,
            column: 0,
            package: "jdk".to_string(),
            version: None,
            stdlib: true,
            readable: false,
            symbol: class_symbol(entry),
        });
    }
    let rest = uri.strip_prefix("jar:")?;
    let rest = rest.strip_prefix("file:").unwrap_or(rest);
    let decoded = percent_decode(rest);
    let (jar, entry) = decoded.split_once("!/")?;
    let jar = jar.trim_start_matches('/');
    let (package, version) = jar_library(jar);
    Some(ExternalLocation {
        path: uri.to_string(),
        line: 0,
        column: 0,
        stdlib: is_language_stdlib(&package),
        package,
        version,
        readable: false,
        symbol: class_symbol(entry),
    })
}

/// State folders of the server (`{outside}/tmp`, `{outside}/home`) exist before launch.
pub(crate) fn ensure_state_dirs(cx: &WorkspaceContext<'_>, language: Language) -> Result<(), SetupError> {
    for dir in ["tmp", "home"] {
        std::fs::create_dir_all(cx.outside.join(dir)).map_err(|e| SetupError::BuildFailed {
            language,
            what: format!("creating {dir} failed: {e}"),
            log: cx.log.to_path_buf(),
        })?;
    }
    Ok(())
}

/// Write a file into the backend's state folder (`{outside}`), never into the workspace.
pub(crate) fn write_state_file(
    cx: &WorkspaceContext<'_>,
    language: Language,
    name: &str,
    bytes: &[u8],
) -> Result<(), SetupError> {
    let path = cx.outside.join(name);
    std::fs::write(&path, bytes).map_err(|e| SetupError::BuildFailed {
        language,
        what: format!("writing {name} failed: {e}"),
        log: cx.log.to_path_buf(),
    })
}

/// The fingerprint of a JVM preflight.
pub(crate) fn prepared_fingerprint(parts: &[&str]) -> String {
    let mut h = blake3::Hasher::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update(b"\0");
    }
    h.finalize().to_hex()[..32].to_string()
}

/// Pending sub-projects (their own builds) as `Prepared.pending_dirs`.
pub(crate) fn pending_dirs(setup: &JvmSetup) -> BTreeMap<String, String> {
    setup
        .project
        .subprojects
        .iter()
        .map(|s| (s.dir.clone(), s.reason.clone()))
        .collect()
}

/// `trace status` line of the chosen JDK.
pub(crate) fn jdk_status(jdk: Option<&Toolchain>) -> Option<String> {
    let jdk = jdk?;
    let version = jdk.version.as_ref().map(|v| v.text.as_str()).unwrap_or("");
    Some(format!("JDK {version} ({})", jdk.root.display()))
}

/// Texts of a loaded server that may report build-import problems: log messages, server
/// notifications (language/status, intellij/importLog, ...) and error diagnostics of build
/// files.
pub(crate) fn load_texts(
    log_messages: &[(u8, String)],
    notifications: &[(String, Value)],
    diagnostics: &[(String, Value)],
) -> Vec<String> {
    let mut out: Vec<String> = log_messages.iter().map(|(_, t)| t.clone()).collect();
    for (method, params) in notifications {
        if let Some(m) = params.get("message").and_then(Value::as_str) {
            out.push(format!("{method}: {m}"));
        }
        if let Some(folders) = params.get("folders").and_then(Value::as_array) {
            for f in folders {
                let status = f.get("status").and_then(Value::as_str).unwrap_or("");
                let message = f.get("message").and_then(Value::as_str).unwrap_or("");
                out.push(format!("{method}: {status} {message}"));
            }
        }
    }
    for (uri, params) in diagnostics {
        let build_file = [
            "pom.xml",
            "build.gradle",
            "build.gradle.kts",
            "settings.gradle",
            "settings.gradle.kts",
            "build.sbt",
        ]
        .iter()
        .any(|n| uri.ends_with(n));
        if !build_file {
            continue;
        }
        for d in params
            .get("diagnostics")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if d.get("severity").and_then(Value::as_u64) == Some(1) {
                if let Some(m) = d.get("message").and_then(Value::as_str) {
                    out.push(m.to_string());
                }
            }
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LoadOutcome {
    DepsMissing,
    BuildFailed(String),
    Ok(Vec<String>),
}

/// Messages of build tools about artifacts they could not find offline.
const DEPS_SIGNALS: &[&str] = &[
    "missing artifact",
    "could not resolve dependencies",
    "unable to collect dependencies",
    "could not transfer artifact",
    "has not been downloaded from it before",
    "no cached version",
    "available for offline mode",
    "could not resolve:",
    "could not resolve all files",
    "could not resolve all dependencies",
    "could not find artifact",
    "failed to read artifact descriptor",
    "non-resolvable parent pom",
    "non-resolvable import pom",
    "unresolved dependency",
    "error downloading",
    "not found: https://",
];

/// Messages of build tools about a failed import.
const BUILD_SIGNALS: &[&str] = &[
    "failed to configure some maven project",
    "could not run build action",
    "could not run phased build action",
    "build failed",
    "unsupported class file major version",
    "workspaceimportstate: failed",
    "project/status: error",
    "language/status: error",
    "bloopinstall failed",
    "failed to import build",
    "sdk location not found",
];

/// Classify the load texts: dependency signals win over build failures.
pub(crate) fn classify_load(texts: &[String]) -> LoadOutcome {
    let lower: Vec<String> = texts.iter().map(|t| t.to_ascii_lowercase()).collect();
    if lower.iter().any(|t| DEPS_SIGNALS.iter().any(|s| t.contains(*s))) {
        return LoadOutcome::DepsMissing;
    }
    if let Some((i, _)) = lower
        .iter()
        .enumerate()
        .find(|(_, t)| BUILD_SIGNALS.iter().any(|s| t.contains(*s)))
    {
        return LoadOutcome::BuildFailed(first_line(&texts[i]));
    }
    let notes = lower
        .iter()
        .enumerate()
        .filter(|(_, t)| t.starts_with("language/status: warning"))
        .map(|(i, _)| first_line(&texts[i]))
        .collect();
    LoadOutcome::Ok(notes)
}

/// The Java release a server needs: its own minimum, raised by the project's pins.
pub(crate) fn required_feature(setup: &JvmSetup, min_feature: u32) -> u32 {
    setup
        .java_pin
        .as_ref()
        .map_or(min_feature, |p| p.feature.max(min_feature))
}

/// "pkg/sub/Cls.class" -> "pkg.sub.Cls".
pub(crate) fn class_symbol(entry: &str) -> Option<String> {
    let stem = entry
        .rsplit_once('.')
        .map(|(s, _)| s)
        .unwrap_or(entry)
        .trim_matches('/');
    (!stem.is_empty()).then(|| stem.replace('/', "."))
}
