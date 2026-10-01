//! Installed Scala parts (Metals, Bloop, SemanticDB plugins, sbt resolver) and their
//! installation through Coursier ([`Coursier`]).

use crate::languages::{InstallExtra, InstallExtraContext};
use crate::registry::{BackendEntry, Recipe, Registry};
use std::path::{Path, PathBuf};
use std::time::Duration;
use trace_core::setup_error::{InstallFailure, SetupError};
use trace_core::Language;
use trace_env::jvm::{self, BuildSystem, JvmSetup};
use trace_env::os::{Os, Platform};

use super::*;

/// Install extras of a repository: the Bloop build server (+ its sbt plugin), the Java
/// SemanticDB plugin, sbt's own Scala version and the parts of each Scala version of the build.
pub(super) fn install_extras_for(repo_root: Option<&Path>) -> Vec<InstallExtra> {
    let Some(root) = repo_root else {
        return Vec::new();
    };
    let (versions, sbt) = jvm::scala_build_versions(root);
    if versions.is_empty() && sbt.is_none() {
        return Vec::new();
    }
    let mut out = vec![
        InstallExtra {
            id: format!("bloop:{BLOOP_VERSION}"),
            coordinates: vec![
                format!("ch.epfl.scala:bloop-frontend_2.12:{BLOOP_VERSION}"),
                format!("ch.epfl.scala:sbt-bloop_2.12_1.0:{BLOOP_VERSION}"),
            ],
            reason: "the Bloop build server".to_string(),
        },
        InstallExtra {
            id: format!("semanticdb-javac:{SEMANTICDB_JAVAC_VERSION}"),
            coordinates: vec![format!("com.sourcegraph:semanticdb-javac:{SEMANTICDB_JAVAC_VERSION}")],
            reason: "Java SemanticDB for the Java sources of the build".to_string(),
        },
    ];
    if let Some(s) = sbt {
        out.push(InstallExtra {
            id: format!("sbt:{s}"),
            coordinates: vec![format!("org.scala-sbt:sbt:{s}")],
            reason: format!("sbt {s}"),
        });
    }
    for v in versions {
        out.push(InstallExtra {
            id: format!("scala:{v}"),
            coordinates: scala_coordinates(&v),
            reason: format!("Scala {v}"),
        });
    }
    out
}

/// Execute one install extra (network allowed: `trace status --install scala` only).
pub(super) fn run_extra(extra: &InstallExtra, cx: &InstallExtraContext<'_>) -> Result<(), SetupError> {
    let fail = || SetupError::Install {
        language: Some(Language::Scala),
        failure: InstallFailure::Failed {
            what: "Scala language server".to_string(),
            log: cx.log.to_path_buf(),
        },
    };
    let version = metals_install_version();
    let metals_dir = installed_dir(cx.tools_dir, METALS_TOOL, &version).ok_or_else(fail)?;
    let cache = metals_dir.join("cache");
    let cs = Coursier::find(&metals_dir, cx.tools_dir, cx.platform).ok_or_else(fail)?;
    let mut parts = Parts::load(&metals_dir);
    let (kind, value) = extra.id.split_once(':').unwrap_or((extra.id.as_str(), ""));
    match kind {
        "bloop" => {
            // The sbt plugin goes into its own Coursier cache: sbt resolves it from there as a
            // file repository, which must hold complete modules only (the main cache also
            // holds pom-only modules, e.g. sbt's own, that would shadow the user's installed
            // sbt).
            let (plugins, server): (Vec<String>, Vec<String>) = extra
                .coordinates
                .iter()
                .cloned()
                .partition(|c| c.contains(":sbt-bloop_"));
            cs.fetch(&cache, &server, false, cx.log).map_err(|_| fail())?;
            cs.fetch(&metals_dir.join(SBT_PLUGINS_CACHE), &plugins, false, cx.log)
                .map_err(|_| fail())?;
            let resolved = cs
                .resolve(&cache, &format!("ch.epfl.scala:bloop-frontend_2.12:{BLOOP_VERSION}"), cx.log)
                .map_err(|_| fail())?;
            parts.zinc = resolved
                .iter()
                .find(|(g, a, _)| g == "org.scala-sbt" && a.starts_with("zinc_"))
                .map(|(_, _, v)| v.clone());
            parts.bloop = Some(BLOOP_VERSION.to_string());
        }
        "semanticdb-javac" => {
            // Read by Metals from its own Coursier cache (the tools cache), offline.
            cs.fetch(&cache, &extra.coordinates, false, cx.log)
                .map_err(|_| fail())?;
            parts.semanticdb_javac = Some(value.to_string());
        }
        "sbt" => {
            let resolved = cs
                .resolve(&cache, &format!("org.scala-sbt:sbt:{value}"), cx.log)
                .map_err(|_| fail())?;
            let scala = resolved
                .iter()
                .find(|(g, a, _)| g == "org.scala-lang" && a == "scala-library")
                .map(|(_, _, v)| v.clone())
                .ok_or_else(fail)?;
            cs.fetch(&cache, &[format!("org.scalameta:mtags_{scala}:{METALS_VERSION}")], false, cx.log)
                .map_err(|_| fail())?;
            parts.sbt.insert(value.to_string(), scala);
        }
        "scala" => {
            let fetched = cs.fetch(&cache, &extra.coordinates, false, cx.log).is_ok()
                || (value.starts_with("3.")
                    && cs
                        .fetch(
                            &cache,
                            &[format!("org.scalameta:mtags_{value}:{METALS_VERSION}")],
                            false,
                            cx.log,
                        )
                        .is_ok());
            if !fetched {
                return Err(fail());
            }
            if let Some(bridge) = bridge_coordinate(value, parts.zinc.as_deref()) {
                cs.fetch(&cache, &[bridge], true, cx.log).map_err(|_| fail())?;
            }
            parts.scala.insert(value.to_string());
        }
        _ => return Ok(()),
    }
    parts.save(&metals_dir).map_err(|_| fail())
}

/// `https://host/path` -> `<cache>/https/host/path` (Coursier cache layout).
pub fn cache_path(cache: &Path, url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("https://")?;
    let mut p = cache.join("https");
    for seg in rest.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return None;
        }
        p.push(seg);
    }
    Some(p)
}

/// The sbt global plugins folder below the trace-owned sbt global base
/// (`-Dsbt.global.base=<short home>/.sbt/1.0`).
pub(super) fn sbt_global_plugins_dir(short_home: &Path) -> PathBuf {
    short_home.join(".sbt").join("1.0").join("plugins")
}

/// The Maven-layout base of Maven Central inside the sbt plugin cache (a file repository
/// for sbt: complete modules only).
pub(super) fn sbt_plugin_base(metals_dir: &Path) -> PathBuf {
    metals_dir
        .join(SBT_PLUGINS_CACHE)
        .join("https")
        .join("repo1.maven.org")
        .join("maven2")
}

/// The Maven-layout base of Maven Central inside the tools cache.
pub(super) fn central_base(metals_dir: &Path) -> PathBuf {
    metals_dir
        .join("cache")
        .join("https")
        .join("repo1.maven.org")
        .join("maven2")
}

/// Every jar of the pinned Metals lock, when all are installed.
pub(super) fn lock_classpath(entry: &BackendEntry, metals_dir: &Path) -> Option<Vec<PathBuf>> {
    let install = entry.install.as_ref()?;
    let Recipe::Coursier { lock, .. } = &install.recipe else {
        return None;
    };
    let cache = metals_dir.join("cache");
    let mut out = Vec::with_capacity(lock.len());
    for file in lock {
        let path = cache_path(&cache, &file.url)?;
        if !path.is_file() {
            return None;
        }
        out.push(path);
    }
    (!out.is_empty()).then_some(out)
}

pub(super) fn maven_dir(base: &Path, group: &str, artifact: &str, version: &str) -> PathBuf {
    let mut p = base.to_path_buf();
    for seg in group.split('.') {
        p.push(seg);
    }
    p.join(artifact).join(version)
}

pub(super) fn has_jar(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .any(|e| e.file_name().to_string_lossy().ends_with(".jar"))
        })
        .unwrap_or(false)
}

/// The presentation compiler (and SemanticDB for Scala 2) of `version` is installed.
pub(super) fn scala_part_installed(base: &Path, version: &str) -> bool {
    if version.starts_with("3.") {
        has_jar(&maven_dir(base, "org.scala-lang", "scala3-presentation-compiler_3", version))
            || has_jar(&maven_dir(base, "org.scalameta", &format!("mtags_{version}"), METALS_VERSION))
    } else {
        has_jar(&maven_dir(base, "org.scalameta", &format!("mtags_{version}"), METALS_VERSION))
            && has_jar(&maven_dir(
                base,
                "org.scalameta",
                &format!("semanticdb-scalac_{version}"),
                SEMANTICDB_VERSION,
            ))
    }
}

/// Labels of the server parts this repository needs that are not installed.
pub fn missing_parts(metals_dir: &Path, setup: &JvmSetup, build: BuildSystem) -> Vec<String> {
    let base = central_base(metals_dir);
    let parts = Parts::load(metals_dir);
    let mut out = Vec::new();
    for v in &setup.project.scala_versions {
        if !scala_part_installed(&base, v) {
            out.push(format!("Scala {v}"));
        }
    }
    if build == BuildSystem::Sbt {
        if let Some(sbt) = &setup.project.sbt_version {
            let ok = parts
                .sbt
                .get(sbt)
                .is_some_and(|scala| scala_part_installed_mtags(&base, scala));
            if !ok {
                out.push(format!("sbt {sbt}"));
            }
        }
    }
    let sbt_plugin_missing = build == BuildSystem::Sbt
        && !has_jar(&maven_dir(
            &sbt_plugin_base(metals_dir),
            "ch.epfl.scala",
            "sbt-bloop_2.12_1.0",
            BLOOP_VERSION,
        ));
    if matches!(build, BuildSystem::Sbt | BuildSystem::Mill)
        && (!has_jar(&maven_dir(&base, "ch.epfl.scala", "bloop-frontend_2.12", BLOOP_VERSION))
            || sbt_plugin_missing)
    {
        out.push("the Bloop build server".to_string());
    }
    out
}

pub(super) fn scala_part_installed_mtags(base: &Path, version: &str) -> bool {
    has_jar(&maven_dir(base, "org.scalameta", &format!("mtags_{version}"), METALS_VERSION))
}

/// "A", "A and B", "A, B and C".
pub(super) fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// "The Scala language server needs its files for Scala 2.13.18." /
/// "Install them: trace status --install scala"
pub fn parts_error(missing: &[String]) -> SetupError {
    SetupError::Unsupported {
        language: Language::Scala,
        first: format!("The Scala language server needs its files for {}.", join_and(missing)),
        second: Some("Install them: trace status --install scala".to_string()),
    }
}

/// Bound of one Coursier run at install time (a fetch of a Scala version's parts).
pub(super) const COURSIER_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The pinned Coursier launcher installed with Metals (a native `cs` or `coursier.jar` run on
/// the trace-managed JDK).
pub(super) struct Coursier {
    pub(super) program: PathBuf,
    pub(super) prefix: Vec<String>,
}

impl Coursier {
    pub(super) fn find(metals_dir: &Path, tools_dir: &Path, p: &Platform) -> Option<Coursier> {
        // `<v>/cs/` is where the Coursier recipe installs the pinned launcher.
        let dirs = [
            metals_dir.join("cs"),
            metals_dir.to_path_buf(),
            metals_dir.join("bin"),
            metals_dir.join("coursier"),
        ];
        for dir in &dirs {
            let Ok(rd) = std::fs::read_dir(dir) else {
                continue;
            };
            let mut names: Vec<(String, PathBuf)> = rd
                .filter_map(Result::ok)
                .filter_map(|e| e.file_name().into_string().ok().map(|n| (n, e.path())))
                .filter(|(_, path)| path.is_file())
                .collect();
            names.sort();
            let native = names.iter().find(|(n, _)| {
                let stem = n.strip_suffix(".exe").unwrap_or(n);
                (stem == "cs" || stem == "coursier" || stem.starts_with("cs-"))
                    && (p.os != Os::Windows || n.ends_with(".exe"))
            });
            if let Some((_, path)) = native {
                return Some(Coursier {
                    program: path.clone(),
                    prefix: Vec::new(),
                });
            }
            if let Some((_, jar)) = names.iter().find(|(n, _)| n == "coursier.jar") {
                let jdk_version = Registry::builtin().runtime("jdk").map(|r| r.version.clone())?;
                let jdk = installed_dir(tools_dir, "jdk", &jdk_version)?;
                let java = jdk.join("bin").join(p.exe("java"));
                if !java.is_file() {
                    return None;
                }
                return Some(Coursier {
                    program: java,
                    prefix: vec!["-jar".to_string(), jar.display().to_string()],
                });
            }
        }
        None
    }

    /// Run the launcher with `args` (its tree stoppable, no stream of trace's own, bounded by
    /// [`COURSIER_TIMEOUT`]): stdout and stderr go to files next to `log`, are appended to
    /// the log and removed; the stdout text on success.
    pub(super) fn run(&self, cache: &Path, args: &[String], log: &Path) -> Result<String, ()> {
        if let Some(parent) = log.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let out_path = log.with_extension("cs-out");
        let err_path = log.with_extension("cs-err");
        let stdout_file = std::fs::File::create(&out_path).map_err(|_| ())?;
        let stderr_file = std::fs::File::create(&err_path).map_err(|_| ())?;
        let mut cmd = crate::procs::command(&self.program);
        cmd.args(&self.prefix)
            .args(args)
            .env("COURSIER_CACHE", cache)
            .env_remove("COURSIER_MODE")
            .current_dir(std::env::temp_dir())
            .stdout(stdout_file)
            .stderr(stderr_file);
        let status = cmd
            .spawn()
            .and_then(|mut child| crate::procs::wait_bounded(&mut child, COURSIER_TIMEOUT));
        let stdout = String::from_utf8_lossy(&std::fs::read(&out_path).unwrap_or_default()).into_owned();
        let stderr = String::from_utf8_lossy(&std::fs::read(&err_path).unwrap_or_default()).into_owned();
        let _ = std::fs::remove_file(&out_path);
        let _ = std::fs::remove_file(&err_path);
        let outcome = match &status {
            Ok(Some(s)) => format!("(exit: {s})"),
            Ok(None) => format!("(timed out after {} s)", COURSIER_TIMEOUT.as_secs()),
            Err(e) => format!("(could not start: {e})"),
        };
        let entry = format!(
            "$ {} {} {}\n{stdout}{stderr}{outcome}\n",
            self.program.display(),
            self.prefix.join(" "),
            args.join(" ")
        );
        append_log(log, &entry);
        if matches!(status, Ok(Some(s)) if s.success()) {
            Ok(stdout)
        } else {
            Err(())
        }
    }

    pub(super) fn fetch(
        &self,
        cache: &Path,
        coordinates: &[String],
        sources: bool,
        log: &Path,
    ) -> Result<(), ()> {
        let mut args = vec!["fetch".to_string(), "--cache".to_string(), cache.display().to_string()];
        if sources {
            args.push("--sources".to_string());
            args.push("--default=true".to_string());
        }
        args.extend(coordinates.iter().cloned());
        self.run(cache, &args, log).map(|_| ())
    }

    /// `group:artifact:version` of every resolved module.
    pub(super) fn resolve(
        &self,
        cache: &Path,
        coordinate: &str,
        log: &Path,
    ) -> Result<Vec<(String, String, String)>, ()> {
        let args = vec![
            "resolve".to_string(),
            "--cache".to_string(),
            cache.display().to_string(),
            coordinate.to_string(),
        ];
        let out = self.run(cache, &args, log)?;
        Ok(out
            .lines()
            .filter_map(|line| {
                let mut parts = line.trim().split(':');
                let g = parts.next()?.to_string();
                let a = parts.next()?.to_string();
                let v = parts.next()?.to_string();
                (!g.is_empty() && !a.is_empty() && !v.is_empty()).then_some((g, a, v))
            })
            .collect())
    }
}

pub(super) fn append_log(log: &Path, text: &str) {
    use std::io::Write;
    if let Some(parent) = log.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        // The log is diagnostic output only; a failed write never changes the result.
        let _ = f.write_all(text.as_bytes());
    }
}
