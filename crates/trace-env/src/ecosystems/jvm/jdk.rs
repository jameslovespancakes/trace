//! JDKs: installed homes (the `--env` override, `JAVA_HOME`, PATH, the standard install
//! folders), their versions (`release` file) and the selection by the project's Java pins.

use super::*;

/// Java feature release of a JDK version ("1.8.0_504" -> 8, "21.0.12" -> 21).
pub(crate) fn java_feature(v: &Version) -> Option<u32> {
    let first = *v.parts.first()?;
    let feature = if first == 1 { *v.parts.get(1)? } else { first };
    u32::try_from(feature).ok().filter(|f| *f > 0)
}

/// A JDK home: `<dir>/bin/java` and a `release` file with `JAVA_VERSION` (macOS bundles:
/// `<dir>/Contents/Home`).
pub fn jdk_home(dir: &Path, p: &Platform) -> Option<(PathBuf, Version)> {
    let mut candidates = vec![dir.to_path_buf()];
    let bundle = dir.join("Contents").join("Home");
    if bundle.is_dir() {
        candidates.push(bundle);
    }
    candidates.into_iter().find_map(|home| {
        if !home.join("bin").join(p.exe("java")).is_file() {
            return None;
        }
        let version = jdk_version(&home)?;
        Some((home, version))
    })
}

/// `JAVA_VERSION` of `<home>/release`.
pub fn jdk_version(home: &Path) -> Option<Version> {
    let kv = os::read_key_values(&home.join("release"));
    kv.get("JAVA_VERSION").and_then(|v| Version::parse(v))
}

pub(super) fn path_key(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let s = s.trim_start_matches("//?/").trim_end_matches('/').to_string();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

pub(super) fn add_jdk(
    cx: &DetectContext<'_>,
    dir: &Path,
    origin: Origin,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<Jdk>,
) {
    if !cx.allowed(dir) {
        return;
    }
    let Some(jdk) = Jdk::from_home(dir, origin, cx.platform) else {
        return;
    };
    let canonical = fs::canonicalize(&jdk.home).unwrap_or_else(|_| jdk.home.clone());
    if cx.allowed(&canonical) && seen.insert(path_key(&canonical)) {
        out.push(jdk);
    }
}

/// Every JDK: the override, `JAVA_HOME`, `java` on PATH (symlinks resolved), then the
/// standard folders of the OS (newest first).
pub(crate) fn find_jdks(
    cx: &DetectContext<'_>,
    home: Option<&Path>,
    env_jdk: Option<&Path>,
) -> (Vec<Jdk>, Vec<String>) {
    let p = cx.platform;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut searched = Vec::new();
    if let Some(dir) = env_jdk {
        searched.push(format!("--env {}", dir.display()));
        add_jdk(cx, dir, Origin::Override, &mut seen, &mut out);
    }
    searched.push("JAVA_HOME".to_string());
    if let Some(java_home) = cx.vars.path("JAVA_HOME") {
        add_jdk(cx, &java_home, Origin::Path, &mut seen, &mut out);
    }
    searched.push("PATH".to_string());
    if let Some(java) = lookup::on_path(&["java"], cx.vars, p) {
        let real = fs::canonicalize(&java).unwrap_or(java);
        if let Some(home_dir) = real.parent().and_then(Path::parent) {
            add_jdk(cx, home_dir, Origin::Path, &mut seen, &mut out);
        }
    }
    let mut standard = Vec::new();
    for dir in jdk_standard_dirs(cx.vars, p, home) {
        let mut local = Vec::new();
        add_jdk(cx, &dir, Origin::StandardLocation, &mut seen, &mut local);
        standard.extend(local);
    }
    standard.sort_by(|a: &Jdk, b: &Jdk| b.version.cmp(&a.version).then_with(|| a.home.cmp(&b.home)));
    out.extend(standard);
    searched.push(match p.os {
        Os::Windows => "Program Files, ~/.jdks, ~/.gradle/jdks, scoop".to_string(),
        Os::Linux => "/usr/lib/jvm, /usr/java, /opt, sdkman, ~/.jdks, ~/.gradle/jdks, asdf, mise".to_string(),
        Os::MacOs => {
            "/Library/Java/JavaVirtualMachines, Homebrew, sdkman, ~/.jdks, ~/.gradle/jdks".to_string()
        }
    });
    (out, searched)
}

pub(super) fn jdk_standard_dirs(vars: &EnvVars, p: &Platform, home: Option<&Path>) -> Vec<PathBuf> {
    let mut parents: Vec<PathBuf> = Vec::new();
    let mut direct: Vec<PathBuf> = Vec::new();
    let user_parents = |h: &Path| -> Vec<PathBuf> {
        vec![
            h.join(".jdks"),
            h.join(".gradle").join("jdks"),
            h.join(".sdkman").join("candidates").join("java"),
        ]
    };
    match p.os {
        Os::Windows => {
            let mut bases: Vec<PathBuf> = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
                .iter()
                .filter_map(|k| vars.path(k))
                .collect();
            if bases.is_empty() {
                bases.push(PathBuf::from(r"C:\Program Files"));
            }
            bases.sort();
            bases.dedup();
            for base in bases {
                for vendor in WINDOWS_JDK_VENDORS {
                    parents.push(base.join(vendor));
                }
            }
            if let Some(h) = home {
                parents.extend(user_parents(h));
                for (name, path) in entries(&h.join("scoop").join("apps")) {
                    let lower = name.to_ascii_lowercase();
                    if lower.contains("jdk") || lower.contains("java") || lower.contains("temurin") {
                        direct.push(path.join("current"));
                    }
                }
            }
        }
        Os::Linux => {
            for d in ["/usr/lib/jvm", "/usr/java", "/opt/java", "/opt/jdk", "/usr/local/java"] {
                parents.push(PathBuf::from(d));
            }
            if let Some(h) = home {
                parents.extend(user_parents(h));
            }
        }
        Os::MacOs => {
            parents.push(PathBuf::from("/Library/Java/JavaVirtualMachines"));
            if let Some(h) = home {
                parents.push(h.join("Library").join("Java").join("JavaVirtualMachines"));
                parents.extend(user_parents(h));
            }
            for base in ["/opt/homebrew/opt", "/usr/local/opt"] {
                for (name, path) in entries(Path::new(base)) {
                    if name.starts_with("openjdk") {
                        direct.push(path.join("libexec").join("openjdk.jdk"));
                    }
                }
            }
        }
    }
    // Version managers (last of the user folders, as before).
    parents.extend(lookup::asdf_tool_dir(vars, p, "java"));
    parents.extend(lookup::mise_tool_dir(vars, p, "java"));
    let mut out = direct;
    for parent in parents {
        for (_, path) in entries(&parent) {
            out.push(path);
        }
    }
    out
}

/// Choose the project JDK: the override when given (too old -> `TooOld`), else the pinned
/// release when installed, else the first JDK (JAVA_HOME, PATH, newest standard) that meets
/// `max(min_feature, pin)`.
pub fn select_jdk(
    jdks: &[Jdk],
    min_feature: u32,
    pin: Option<&JavaPin>,
    searched: &[String],
    p: &Platform,
) -> ToolchainStatus {
    let required = pin.map_or(min_feature, |pin| pin.feature.max(min_feature));
    let source = pin.map(|pin| pin.source.clone()).unwrap_or_default();
    let needed = VersionReq::at_least(Version {
        parts: vec![u64::from(required)],
        pre: None,
        text: required.to_string(),
    });
    if jdks.is_empty() {
        return ToolchainStatus::Missing {
            searched: searched.to_vec(),
        };
    }
    if let Some(chosen) = jdks.iter().find(|j| j.origin == Origin::Override) {
        return if chosen.feature >= required {
            ToolchainStatus::Found(chosen.toolchain(p))
        } else {
            ToolchainStatus::TooOld {
                found: chosen.toolchain(p),
                needed,
                source,
            }
        };
    }
    let fitting: Vec<&Jdk> = jdks.iter().filter(|j| j.feature >= required).collect();
    if let Some(pin) = pin {
        if let Some(exact) = fitting.iter().find(|j| j.feature == pin.feature) {
            let mut t = exact.toolchain(p);
            t.origin = Origin::Pin;
            t.facts.insert("pin".to_string(), pin.source.clone());
            return ToolchainStatus::Found(t);
        }
    }
    match fitting.first() {
        Some(j) => ToolchainStatus::Found(j.toolchain(p)),
        None => {
            let newest = jdks
                .iter()
                .max_by(|a, b| a.version.cmp(&b.version))
                .unwrap_or(&jdks[0]);
            ToolchainStatus::TooOld {
                found: newest.toolchain(p),
                needed,
                source,
            }
        }
    }
}

/// Java release in a pin text: "21", "21.0.2", "temurin-21.0.2", "openjdk64-17.0.2",
/// "21.0.2-tem", "1.8".
pub(super) fn pin_feature(text: &str) -> Option<u32> {
    let text = text.trim();
    let from_segment = text
        .rsplit('-')
        .find(|seg| seg.starts_with(|c: char| c.is_ascii_digit()))
        .and_then(Version::parse)
        .and_then(|v| java_feature(&v));
    from_segment.or_else(|| Version::parse(text).and_then(|v| java_feature(&v)))
}

/// Pins of the Java release in files at the repository root.
pub(super) fn pin_files(root: &Path) -> Vec<JavaPin> {
    let mut out = Vec::new();
    let mut push = |feature: Option<u32>, source: &str| {
        if let Some(feature) = feature {
            out.push(JavaPin {
                feature,
                source: source.to_string(),
            });
        }
    };
    if let Some(text) = relpath::read_small(&root.join(".java-version"), MAX_FILE_BYTES) {
        push(
            text.lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with('#'))
                .and_then(pin_feature),
            ".java-version",
        );
    }
    push(
        os::read_key_values(&root.join(".sdkmanrc"))
            .get("java")
            .and_then(|v| pin_feature(v)),
        ".sdkmanrc",
    );
    if let Some(text) = relpath::read_small(&root.join(".tool-versions"), MAX_FILE_BYTES) {
        let value = text.lines().find_map(|line| {
            let mut words = line.split_whitespace();
            (words.next() == Some("java")).then(|| words.next()).flatten()
        });
        push(value.and_then(pin_feature), ".tool-versions");
    }
    for name in ["mise.toml", ".mise.toml"] {
        if let Some(value) = relpath::read_small(&root.join(name), MAX_FILE_BYTES)
            .and_then(|t| trace_core::formats::toml_value(&t))
        {
            let java = &value["tools"]["java"];
            let text = java
                .as_str()
                .map(str::to_string)
                .or_else(|| {
                    java.as_array()
                        .and_then(|a| a.first())
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .or_else(|| java["version"].as_str().map(str::to_string));
            push(text.as_deref().and_then(pin_feature), name);
        }
    }
    push(
        os::read_key_values(&root.join("gradle").join("gradle-daemon-jvm.properties"))
            .get("toolchainVersion")
            .and_then(|v| pin_feature(v)),
        "gradle/gradle-daemon-jvm.properties",
    );
    out
}
