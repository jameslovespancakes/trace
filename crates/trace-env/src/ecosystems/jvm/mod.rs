//! JVM toolchains and dependencies: Java and Scala.
//!
//! Found statically, without running anything of the project:
//!
//! * **JDKs**: the `--env` override, `JAVA_HOME`, `java` on PATH (symlinks resolved; launcher
//!   stubs such as Windows `javapath` or macOS `/usr/bin/java` are skipped because they have no
//!   `release` file) and the standard install folders of the OS. The version comes from the
//!   `<home>/release` file. Project pins (`.java-version`, `.sdkmanrc`, `.tool-versions`,
//!   `mise.toml`, `gradle/gradle-daemon-jvm.properties`, the pom `maven.compiler.release`)
//!   raise the required Java release and select among the installed JDKs.
//! * **Maven**: the local repository (`.mvn/maven.config`, `MAVEN_OPTS`, `settings.xml`
//!   `<localRepository>`, `~/.m2/repository`), the wrapper distribution or an installed Maven.
//! * **Gradle**: the user home (`GRADLE_USER_HOME`, `~/.gradle`), the wrapper distribution
//!   (`gradle-wrapper.properties` + the `.ok` marker of the unpacked distribution) or an
//!   installed Gradle.
//! * **sbt / Mill / Scala CLI** launchers, the Coursier cache, the Ivy home and the sbt boot
//!   folder; the Android SDK.
//! * **The build model**: Maven modules (`<modules>`, the dependency closure inside the
//!   repository), Gradle builds (every build directory below a settings file: Gradle build
//!   scripts are programs and are never read), sbt builds (`scalaVersion`, `crossScalaVersions`, `addSbtPlugin` read from the
//!   Scala syntax tree), Android modules, generated sources that only
//!   a build creates, and nested projects that are not part of the root build (sub-projects).
//! * **The static dependency check**: every declared Maven dependency (versions from
//!   properties, parents and imported BOMs) in the local repository, the Gradle wrapper
//!   distribution, the sbt version and sbt plugins in the Coursier/Ivy caches.
//!   What cannot be decided statically is left to the post-import check of the servers.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use trace_core::formats::xml::{self, Element};
use trace_core::Language;

use crate::lookup;
use crate::os::{self, EnvVars, Os, Platform, Version, VersionReq};
use crate::relpath;
use crate::syn::Syn;
use crate::{
    entries, hex, DepsReport, DepsStatus, DetectContext, EcosystemId, LibraryKind, LibraryRoot, Origin,
    SubProject, Toolchain, ToolchainStatus, SKIP_DIRS,
};

mod gradle;
mod jdk;
mod maven;
mod scala;
mod tools;
mod walk;

use gradle::*;
pub use jdk::*;
use maven::*;
pub use scala::*;
use tools::*;
use walk::*;

/// Install advice for a missing JDK (approved catalogue text).
pub const JDK_INSTALL: &str = "Install one from https://adoptium.net";
/// Dependency hints per build system.
pub(crate) const MAVEN_HINT: &str = "mvn dependency:go-offline";
pub(crate) const GRADLE_HINT: &str = "./gradlew build";
pub(crate) const SBT_HINT: &str = "sbt update";
pub(crate) const MILL_HINT: &str = "mill __.compile";
pub(crate) const SCALA_CLI_HINT: &str = "scala-cli compile .";

/// The Java release jdtls accepts as the lowest execution environment.
pub const MIN_JAVA_FEATURE: u32 = 8;

const MAX_WALK_ENTRIES: usize = 200_000;
const MAX_WALK_DEPTH: usize = 12;
const MAX_BUILD_FILES: usize = 5_000;
const MAX_POM_CHAIN: usize = 12;
const MAX_BOM_DEPTH: usize = 4;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_INTERPOLATIONS: usize = 16;

/// Build files the walk records.
const BUILD_FILE_NAMES: &[&str] = &[
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "build.sbt",
    "build.sc",
    "build.mill",
    "project.scala",
];

/// Vendor folders of JDK installers under `Program Files` (Windows).
const WINDOWS_JDK_VENDORS: &[&str] = &[
    "Java",
    "Eclipse Adoptium",
    "Eclipse Foundation",
    "Microsoft",
    "Zulu",
    "Amazon Corretto",
    "BellSoft",
    "Semeru",
    "IBM",
    "OpenJDK",
    "RedHat",
    "GraalVM",
];

/// The project JDK (Java 8 or newer, raised by the project's pins).
pub fn toolchain(cx: &DetectContext<'_>) -> ToolchainStatus {
    setup(cx).select_jdk(MIN_JAVA_FEATURE)
}

/// Static dependency check of every JVM build in the repository.
pub fn deps(cx: &DetectContext<'_>, _toolchain: Option<&Toolchain>) -> DepsReport {
    setup(cx).deps_report(&BuildSystem::ALL)
}

/// `--env <path>` for the JVM: a JDK home, a Maven local repository (a folder named
/// `repository`), a Gradle user home (`caches/modules-2`) or a Coursier cache (`https/`).
pub(crate) fn accepts_env_path(path: &Path) -> bool {
    env_kind(path, &Platform::current()).is_some()
}

/// What an `--env` path is for the JVM ecosystem.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvKind {
    Jdk,
    MavenRepository,
    GradleUserHome,
    CoursierCache,
}

pub(crate) fn env_kind(path: &Path, p: &Platform) -> Option<EnvKind> {
    if !path.is_dir() {
        return None;
    }
    if jdk_home(path, p).is_some() {
        return Some(EnvKind::Jdk);
    }
    if path.join("caches").join("modules-2").is_dir() {
        return Some(EnvKind::GradleUserHome);
    }
    if path.join("https").is_dir() {
        return Some(EnvKind::CoursierCache);
    }
    if path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("repository"))
    {
        return Some(EnvKind::MavenRepository);
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildSystem {
    Maven,
    Gradle,
    Sbt,
    Mill,
    ScalaCli,
}

impl BuildSystem {
    pub const ALL: [BuildSystem; 5] = [
        BuildSystem::Maven,
        BuildSystem::Gradle,
        BuildSystem::Sbt,
        BuildSystem::Mill,
        BuildSystem::ScalaCli,
    ];

    /// Tool name used in the approval error ("Java needs Maven, which runs ...").
    pub const fn tool(self) -> &'static str {
        match self {
            BuildSystem::Maven => "Maven",
            BuildSystem::Gradle => "Gradle",
            BuildSystem::Sbt => "sbt",
            BuildSystem::Mill => "Mill",
            BuildSystem::ScalaCli => "Scala CLI",
        }
    }

    /// What the tool runs of the project (approval error).
    pub const fn runs(self) -> &'static str {
        match self {
            BuildSystem::Maven => "this project's build plugins",
            BuildSystem::Gradle => "this project's build scripts",
            BuildSystem::Sbt | BuildSystem::Mill => "this project's build definition",
            BuildSystem::ScalaCli => "this project's build directives and macros",
        }
    }

    /// Dependency hint.
    pub const fn hint(self) -> &'static str {
        match self {
            BuildSystem::Maven => MAVEN_HINT,
            BuildSystem::Gradle => GRADLE_HINT,
            BuildSystem::Sbt => SBT_HINT,
            BuildSystem::Mill => MILL_HINT,
            BuildSystem::ScalaCli => SCALA_CLI_HINT,
        }
    }
}

/// A JDK found on this machine.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Jdk {
    pub home: PathBuf,
    pub version: Version,
    /// Java feature release: 8 for "1.8.0_504", 21 for "21.0.12".
    pub feature: u32,
    pub origin: Origin,
}

impl Jdk {
    /// The JDK at `dir` (or `dir/Contents/Home` on macOS).
    pub(crate) fn from_home(dir: &Path, origin: Origin, p: &Platform) -> Option<Jdk> {
        let (home, version) = jdk_home(dir, p)?;
        let feature = java_feature(&version)?;
        Some(Jdk {
            home,
            version,
            feature,
            origin,
        })
    }

    /// Eclipse execution environment name: "JavaSE-1.8", "JavaSE-21".
    pub fn environment_name(&self) -> String {
        if self.feature <= 8 {
            format!("JavaSE-1.{}", self.feature)
        } else {
            format!("JavaSE-{}", self.feature)
        }
    }

    pub fn toolchain(&self, p: &Platform) -> Toolchain {
        let mut executables = BTreeMap::new();
        for name in ["java", "javac"] {
            let exe = self.home.join("bin").join(p.exe(name));
            if exe.is_file() {
                executables.insert(name.to_string(), exe);
            }
        }
        let mut facts = BTreeMap::new();
        facts.insert("feature".to_string(), self.feature.to_string());
        Toolchain {
            id: "jdk",
            root: self.home.clone(),
            version: Some(self.version.clone()),
            executables,
            origin: self.origin,
            facts,
        }
    }
}

/// A project pin of the Java release (the highest wins).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct JavaPin {
    pub feature: u32,
    /// Where it was read ("pom.xml", ".java-version", ...).
    pub source: String,
}

/// A Gradle wrapper of one build root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GradleWrapper {
    /// Build root (relative, "" = repository root).
    pub dir: String,
    pub version: String,
    /// "bin" | "all"
    pub kind: String,
    /// The unpacked distribution in the Gradle user home, when installed (`.ok` marker).
    pub installed: Option<PathBuf>,
}

/// The JVM build model of the repository (relative, '/'-separated directories; "" = root).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct JvmProject {
    /// Required Maven build roots (outermost poms).
    pub maven_roots: Vec<String>,
    /// Every Maven module of the required builds (reactor closure).
    pub maven_modules: Vec<String>,
    /// Required Gradle build roots.
    pub gradle_roots: Vec<String>,
    /// Every Gradle project directory of the required builds.
    pub gradle_modules: Vec<String>,
    pub gradle_wrappers: Vec<GradleWrapper>,
    /// Maven wrapper distribution version (`.mvn/wrapper/maven-wrapper.properties`).
    pub maven_wrapper: Option<String>,
    /// The sbt build root.
    pub sbt_root: Option<String>,
    /// `project/build.properties` `sbt.version`.
    pub sbt_version: Option<String>,
    pub mill_root: Option<String>,
    pub scala_cli_roots: Vec<String>,
    /// Scala versions of the build, the default (`scalaVersion`) first.
    pub scala_versions: Vec<String>,
    /// `addSbtPlugin(g % a % v)` of the sbt meta-build (resolved literals).
    pub sbt_plugins: Vec<(String, String, String)>,
    /// Modules that apply an Android Gradle plugin.
    pub android: Vec<String>,
    /// Generated-source folders a build must create first ("core/target/generated-sources").
    pub generated_sources: Vec<String>,
    /// Java release the project compiles for (pom / compiler settings).
    pub java_release: Option<JavaPin>,
    /// Nested projects that are not part of a required build.
    pub subprojects: Vec<SubProject>,
    /// Every build input read (relative), for fingerprints and status.
    pub build_files: Vec<String>,
}

impl JvmProject {
    pub fn systems(&self) -> Vec<BuildSystem> {
        let mut out = Vec::new();
        if !self.maven_roots.is_empty() {
            out.push(BuildSystem::Maven);
        }
        if !self.gradle_roots.is_empty() {
            out.push(BuildSystem::Gradle);
        }
        if self.sbt_root.is_some() {
            out.push(BuildSystem::Sbt);
        }
        if self.mill_root.is_some() {
            out.push(BuildSystem::Mill);
        }
        if !self.scala_cli_roots.is_empty() {
            out.push(BuildSystem::ScalaCli);
        }
        out
    }
}

/// Everything the Java and Scala preflights need, computed once.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct JvmSetup {
    pub platform: Platform,
    pub home: Option<PathBuf>,
    pub project: JvmProject,
    /// Every JDK found (override, JAVA_HOME, PATH, then standard folders newest first).
    pub jdks: Vec<Jdk>,
    /// Where JDKs were searched (for the status line of a missing JDK).
    pub jdk_searched: Vec<String>,
    /// The highest Java release pinned by the project (pin files + pom).
    pub java_pin: Option<JavaPin>,
    pub maven_repo: PathBuf,
    pub gradle_user_home: PathBuf,
    pub coursier_cache: PathBuf,
    pub ivy_home: PathBuf,
    pub sbt_boot: PathBuf,
    pub android_sdk: Option<PathBuf>,
    pub maven: Option<Toolchain>,
    pub gradle: Option<Toolchain>,
    pub sbt: Option<Toolchain>,
    pub mill: Option<Toolchain>,
    pub scala_cli: Option<Toolchain>,
    /// Statically missing dependencies per build system ("g:a:v", "Gradle 9.7.1", ...).
    pub missing: BTreeMap<BuildSystem, Vec<String>>,
    pub fingerprint: String,
}

impl JvmSetup {
    /// The project JDK for a server that needs at least `min_feature`.
    pub fn select_jdk(&self, min_feature: u32) -> ToolchainStatus {
        select_jdk(&self.jdks, min_feature, self.java_pin.as_ref(), &self.jdk_searched, &self.platform)
    }

    /// The newest installed JDK of at least Java `min_feature`: the JDK a build import runs on
    /// when the project JDK is older than the build tool needs (a `JAVA_HOME` of Java 8 never
    /// runs Gradle 9).
    pub fn newest_jdk(&self, min_feature: u32) -> Option<Toolchain> {
        self.jdks
            .iter()
            .filter(|j| j.feature >= min_feature)
            .max_by(|a, b| a.version.cmp(&b.version).then_with(|| b.home.cmp(&a.home)))
            .map(|j| j.toolchain(&self.platform))
    }

    /// The Gradle version the build import runs: the newest wrapper distribution of the
    /// required builds, else the installed Gradle.
    pub fn gradle_version(&self) -> Option<Version> {
        self.project
            .gradle_wrappers
            .iter()
            .filter_map(|w| Version::parse(&w.version))
            .max()
            .or_else(|| self.gradle.as_ref().and_then(|g| g.version.clone()))
    }

    /// One JDK per Java release (first found wins), for jdtls `java.configuration.runtimes`.
    pub fn runtimes(&self) -> Vec<&Jdk> {
        let mut seen = BTreeSet::new();
        let mut out: Vec<&Jdk> = self.jdks.iter().filter(|j| seen.insert(j.feature)).collect();
        out.sort_by_key(|j| j.feature);
        out
    }

    /// Dependency report restricted to `systems` (the build systems a language uses).
    pub fn deps_report(&self, systems: &[BuildSystem]) -> DepsReport {
        let present = self.project.systems();
        let used: Vec<BuildSystem> = present.iter().copied().filter(|s| systems.contains(s)).collect();
        let mut report = DepsReport::none_declared();
        report.fingerprint = self.fingerprint.clone();
        report.subprojects = self.project.subprojects.clone();
        report.roots = self.library_roots();
        if used.is_empty() {
            return report;
        }
        report.status = DepsStatus::Installed;
        let mut hints = Vec::new();
        for s in &used {
            if let Some(missing) = self.missing.get(s).filter(|m| !m.is_empty()) {
                report.status = DepsStatus::Missing;
                report.missing.extend(missing.iter().cloned());
                hints.push(s.hint());
            }
        }
        report.hint = if hints.is_empty() {
            used.first().map(|s| s.hint()).unwrap_or(MAVEN_HINT).to_string()
        } else {
            hints.join("; ")
        };
        report.missing.sort();
        report.missing.dedup();
        if report.missing.len() > 3 {
            report.notes.push(format!(
                "{} JVM dependencies are not installed (first: {})",
                report.missing.len(),
                report.missing[..3].join(", ")
            ));
        }
        report
    }

    /// Library roots the servers read and trace-library derives from.
    pub fn library_roots(&self) -> Vec<LibraryRoot> {
        let mut roots = Vec::new();
        if let ToolchainStatus::Found(jdk) = self.select_jdk(MIN_JAVA_FEATURE) {
            roots.push(LibraryRoot {
                path: jdk.root.clone(),
                kind: LibraryKind::Stdlib,
                ecosystem: EcosystemId::Jvm,
                layout: "toolchain_stdlib",
                version: jdk.version.as_ref().map(|v| v.text.clone()),
            });
        }
        if self.maven_repo.is_dir() {
            roots.push(LibraryRoot {
                path: self.maven_repo.clone(),
                kind: LibraryKind::Dependency,
                ecosystem: EcosystemId::Jvm,
                layout: "maven_repo",
                version: None,
            });
        }
        let gradle_files = gradle_files_dir(&self.gradle_user_home);
        if gradle_files.is_dir() {
            roots.push(LibraryRoot {
                path: gradle_files,
                kind: LibraryKind::Dependency,
                ecosystem: EcosystemId::Jvm,
                layout: "gradle_cache",
                version: None,
            });
        }
        if self.coursier_cache.is_dir() {
            roots.push(LibraryRoot {
                path: self.coursier_cache.clone(),
                kind: LibraryKind::Dependency,
                ecosystem: EcosystemId::Jvm,
                layout: "coursier_cache",
                version: None,
            });
        }
        roots
    }
}

/// The Java release a Gradle version needs to run its daemon: Gradle 9 runs on Java 17 or
/// newer, older releases on Java 8 or newer; an unknown version is treated as current (17).
pub fn gradle_min_jdk(version: Option<&Version>) -> u32 {
    match version.and_then(|v| v.parts.first()) {
        Some(major) if *major < 9 => MIN_JAVA_FEATURE,
        _ => GRADLE_9_MIN_JDK,
    }
}

/// Java release Gradle 9 and newer need to run.
pub(crate) const GRADLE_9_MIN_JDK: u32 = 17;

/// Detect everything (read only).
pub fn setup(cx: &DetectContext<'_>) -> JvmSetup {
    let p = cx.platform;
    let home = os::home_dir(cx.vars, p);
    let override_kind = cx
        .env_override
        .filter(|o| cx.allowed(o))
        .and_then(|o| env_kind(o, p).map(|k| (k, o.to_path_buf())));
    let override_of = |kind: EnvKind| {
        override_kind
            .as_ref()
            .filter(|(k, _)| *k == kind)
            .map(|(_, path)| path.clone())
    };

    let walk = walk(cx);
    let maven_config = maven_config_props(cx.root);
    let maven_repo = override_of(EnvKind::MavenRepository)
        .unwrap_or_else(|| maven_repo(cx, home.as_deref(), &maven_config));
    let gradle_user_home = override_of(EnvKind::GradleUserHome)
        .or_else(|| cx.vars.path("GRADLE_USER_HOME"))
        .or_else(|| home.as_ref().map(|h| h.join(".gradle")))
        .unwrap_or_default();
    let coursier_cache = override_of(EnvKind::CoursierCache)
        .or_else(|| cx.vars.path("COURSIER_CACHE"))
        .or_else(|| default_coursier_cache(cx.vars, p))
        .unwrap_or_default();
    let ivy_home = home.as_ref().map(|h| h.join(".ivy2")).unwrap_or_default();
    let sbt_boot = cx
        .vars
        .path("SBT_BOOT_DIRECTORY")
        .or_else(|| sbt_boot_dir(cx.vars, p, home.as_deref()))
        .unwrap_or_default();
    let android_sdk = android_sdk(cx, home.as_deref());

    let mut missing: BTreeMap<BuildSystem, Vec<String>> = BTreeMap::new();
    let mut project = JvmProject {
        build_files: walk.files.clone(),
        ..JvmProject::default()
    };

    // Maven.
    let maven_model = MavenModel::load(cx.root, &walk, &maven_repo, &maven_config);
    maven_model.fill(cx.root, &walk, &mut project, &mut missing);
    project.maven_wrapper = maven_wrapper_version(cx.root);

    // Gradle.
    let gradle_model = GradleModel::load(cx.root, &walk);
    gradle_model.fill(cx.root, &walk, &gradle_user_home, &mut project, &mut missing);

    // sbt / Mill / Scala CLI.
    scala_builds(cx.root, &walk, &mut project);
    if project.sbt_root.is_some() {
        let lacking = sbt_missing(&project, &coursier_cache, &ivy_home, &sbt_boot);
        if !lacking.is_empty() {
            missing.entry(BuildSystem::Sbt).or_default().extend(lacking);
        }
    }

    // Sub-projects: stable order, no duplicates.
    project
        .subprojects
        .sort_by(|a, b| a.dir.cmp(&b.dir).then_with(|| a.reason.cmp(&b.reason)));
    project.subprojects.dedup_by(|a, b| a.dir == b.dir);
    for list in [
        &mut project.android,
        &mut project.generated_sources,
        &mut project.scala_cli_roots,
    ] {
        list.sort();
        list.dedup();
    }

    // JDKs and pins.
    let (jdks, jdk_searched) = find_jdks(cx, home.as_deref(), override_of(EnvKind::Jdk).as_deref());
    let mut pins = pin_files(cx.root);
    if let Some(release) = &project.java_release {
        pins.push(release.clone());
    }
    let java_pin = pins
        .into_iter()
        .max_by(|a, b| a.feature.cmp(&b.feature).then_with(|| b.source.cmp(&a.source)));

    // Tools (informational for Maven; required by the servers for Gradle without a
    // wrapper, sbt, Mill and Scala CLI).
    let maven = maven_tool(cx, home.as_deref(), project.maven_wrapper.as_deref());
    let gradle = gradle_tool(cx, home.as_deref(), &project.gradle_wrappers);
    let sbt = launcher_tool(cx, "sbt", &sbt_names(p), &sbt_dirs(cx.vars, p, home.as_deref()));
    let mill = launcher_tool(
        cx,
        "mill",
        &["mill", "mill.bat", "millw"],
        &coursier_bin_dirs(cx.vars, p, home.as_deref()),
    );
    let scala_cli = launcher_tool(
        cx,
        "scala-cli",
        &["scala-cli", "scala-cli.bat"],
        &coursier_bin_dirs(cx.vars, p, home.as_deref()),
    );

    let fingerprint = fingerprint(
        cx.root,
        &project,
        &jdks,
        &[maven_repo.as_path(), gradle_user_home.as_path(), coursier_cache.as_path()],
        &missing,
    );
    JvmSetup {
        platform: p.clone(),
        home,
        project,
        jdks,
        jdk_searched,
        java_pin,
        maven_repo,
        gradle_user_home,
        coursier_cache,
        ivy_home,
        sbt_boot,
        android_sdk,
        maven,
        gradle,
        sbt,
        mill,
        scala_cli,
        missing,
        fingerprint,
    }
}

fn fingerprint(
    root: &Path,
    project: &JvmProject,
    jdks: &[Jdk],
    dirs: &[&Path],
    missing: &BTreeMap<BuildSystem, Vec<String>>,
) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"jvm-setup-1\0");
    for rel in &project.build_files {
        h.update(rel.as_bytes());
        h.update(b"\0");
        if let Some(text) = relpath::read_small(&root.join(rel), MAX_FILE_BYTES) {
            h.update(text.as_bytes());
        }
        h.update(b"\0");
    }
    for jdk in jdks {
        h.update(jdk.home.to_string_lossy().as_bytes());
        h.update(jdk.version.text.as_bytes());
        h.update(b"\0");
    }
    for d in dirs {
        h.update(d.to_string_lossy().as_bytes());
        h.update(b"\0");
    }
    for w in &project.gradle_wrappers {
        h.update(w.version.as_bytes());
        h.update(if w.installed.is_some() { b"1" } else { b"0" });
    }
    for (system, names) in missing {
        h.update(system.tool().as_bytes());
        for n in names {
            h.update(n.as_bytes());
            h.update(b"\0");
        }
    }
    hex(h)
}

/// The Jvm ecosystem ([`crate::Ecosystem`]).
pub struct Jvm;

impl crate::Ecosystem for Jvm {
    fn id(&self) -> crate::EcosystemId {
        crate::EcosystemId::Jvm
    }

    fn accepts_env_path(&self, path: &Path) -> bool {
        accepts_env_path(path)
    }

    fn toolchain(&self, cx: &DetectContext<'_>) -> ToolchainStatus {
        toolchain(cx)
    }

    fn deps(&self, read: &DetectContext<'_>, toolchain: Option<&Toolchain>) -> DepsReport {
        deps(read, toolchain)
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/ecosystems/jvm/mod.rs"]
mod tests;
