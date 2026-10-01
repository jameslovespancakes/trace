//! Gradle builds: the wrapper distribution and the build directories below settings files
//! (build scripts are programs and are never read).

use super::*;

/// `<gradle user home>/caches/modules-2/files-2.1`
pub(super) fn gradle_files_dir(gradle_user_home: &Path) -> PathBuf {
    gradle_user_home.join("caches").join("modules-2").join("files-2.1")
}

pub(super) fn gradle_wrapper(root: &Path, dir: &str, gradle_user_home: &Path) -> Option<GradleWrapper> {
    let props =
        os::read_key_values(&root.join(relpath::join(dir, "gradle/wrapper/gradle-wrapper.properties")));
    let url = unescape_properties(props.get("distributionUrl")?);
    let file = url.rsplit('/').next()?;
    let rest = file.strip_prefix("gradle-")?.strip_suffix(".zip")?;
    let (version, kind) = rest.rsplit_once('-')?;
    let dists = gradle_user_home
        .join("wrapper")
        .join("dists")
        .join(format!("gradle-{version}-{kind}"));
    let installed = entries(&dists).into_iter().find_map(|(_, hash_dir)| {
        let unpacked = hash_dir.join(format!("gradle-{version}"));
        let ok = entries(&hash_dir)
            .iter()
            .any(|(n, p)| n.ends_with(".ok") && p.is_file());
        (ok && unpacked.is_dir()).then_some(unpacked)
    });
    Some(GradleWrapper {
        dir: dir.to_string(),
        version: version.to_string(),
        kind: kind.to_string(),
        installed,
    })
}

pub(super) struct GradleModel {
    roots: Vec<String>,
    members: BTreeSet<String>,
    subprojects: Vec<SubProject>,
}

impl GradleModel {
    pub(super) fn load(root: &Path, walk: &Walk) -> GradleModel {
        let settings: Vec<&str> = walk.named(&["settings.gradle", "settings.gradle.kts"]);
        let builds: Vec<&str> = walk.named(&["build.gradle", "build.gradle.kts"]);
        let settings_dirs: Vec<String> = settings.iter().map(|s| relpath::parent(s).to_string()).collect();
        let build_dirs: Vec<String> = builds.iter().map(|s| relpath::parent(s).to_string()).collect();
        let mut model = GradleModel {
            roots: Vec::new(),
            members: BTreeSet::new(),
            subprojects: Vec::new(),
        };
        if settings_dirs.is_empty() && build_dirs.is_empty() {
            return model;
        }
        // Orphan build dirs (no settings file above them) are single-project builds.
        let orphans: Vec<String> = build_dirs
            .iter()
            .filter(|d| !settings_dirs.iter().any(|s| relpath::within(d, s)))
            .cloned()
            .collect();
        let mut candidates = settings_dirs.clone();
        candidates.extend(outermost(&orphans));
        let roots = outermost(&candidates);
        // Members of each build (included builds join their parent build).
        let mut queue: Vec<String> = roots.clone();
        let mut visited_builds: BTreeSet<String> = BTreeSet::new();
        while let Some(build_root) = queue.pop() {
            if !visited_builds.insert(build_root.clone()) {
                continue;
            }
            model.members.insert(build_root.clone());
            model.members.insert(relpath::join(&build_root, "buildSrc"));
            let nested_settings: Vec<&String> = settings_dirs
                .iter()
                .filter(|s| **s != build_root && relpath::within(s, &build_root))
                .collect();
            let inside_nested = |d: &str| nested_settings.iter().any(|s| relpath::within(d, s));
            let has_settings = root.join(relpath::join(&build_root, "settings.gradle.kts")).is_file()
                || root.join(relpath::join(&build_root, "settings.gradle")).is_file();
            if has_settings {
                // Settings scripts are programs (never read): every build directory below
                // counts as part of the build, nested builds too.
                for d in build_dirs.iter().chain(settings_dirs.iter()) {
                    if relpath::within(d, &build_root) {
                        model.members.insert(d.clone());
                    }
                }
                continue;
            }
            // A single-project build without a settings file: build dirs below this build that are neither included nor inside a nested build.
            for d in &build_dirs {
                if relpath::within(d, &build_root) && !inside_nested(d) && !model.members.contains(d) {
                    model.subprojects.push(SubProject {
                        dir: d.clone(),
                        reason: format!(
                            "Gradle project not included in {}",
                            relpath::join(&build_root, "settings.gradle.kts")
                        ),
                    });
                }
            }
        }
        // Nested settings roots that no build includes are separate builds.
        for s in &settings_dirs {
            if !model.members.contains(s) && !roots.contains(s) {
                model.subprojects.push(SubProject {
                    dir: s.clone(),
                    reason: "separate Gradle build inside the repository".to_string(),
                });
            }
        }
        model.subprojects.retain(|sp| !model.members.contains(&sp.dir));
        model.roots = roots;
        model
    }

    pub(super) fn fill(
        &self,
        root: &Path,
        walk: &Walk,
        gradle_user_home: &Path,
        project: &mut JvmProject,
        missing: &mut BTreeMap<BuildSystem, Vec<String>>,
    ) {
        if self.roots.is_empty() {
            return;
        }
        project.gradle_roots = self.roots.clone();
        project.gradle_modules = self
            .members
            .iter()
            .filter(|d| {
                let base = root.join(d.as_str());
                base.join("build.gradle").is_file()
                    || base.join("build.gradle.kts").is_file()
                    || base.join("settings.gradle").is_file()
                    || base.join("settings.gradle.kts").is_file()
            })
            .cloned()
            .collect();
        project.subprojects.extend(self.subprojects.iter().cloned());
        let mut lacking: Vec<String> = Vec::new();
        for r in &self.roots {
            if let Some(w) = gradle_wrapper(root, r, gradle_user_home) {
                if w.installed.is_none() {
                    lacking.push(format!("Gradle {} (wrapper distribution)", w.version));
                }
                project.gradle_wrappers.push(w);
            }
        }
        // Source-folder conventions that need no build script grammar.
        for m in &walk.android_manifest {
            if self.members.contains(m) {
                project.android.push(m.clone());
            }
        }
        for m in walk.proto.iter().chain(&walk.proto_test) {
            if self.members.contains(m)
                && !project
                    .generated_sources
                    .contains(&relpath::join(m, "build/generated"))
            {
                let generated = relpath::join(m, "build/generated");
                if !root.join(&generated).is_dir() {
                    project.generated_sources.push(generated);
                }
            }
        }
        lacking.sort();
        lacking.dedup();
        if !lacking.is_empty() {
            missing.entry(BuildSystem::Gradle).or_default().extend(lacking);
        }
    }
}
