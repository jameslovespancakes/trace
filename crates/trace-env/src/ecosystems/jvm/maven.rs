//! Maven builds: the pom model (parents, properties, imported BOMs, modules) and the static
//! check of every declared dependency against the local repository.

use super::*;

#[derive(Clone, Debug, Default)]
pub(super) struct Pom {
    group: Option<String>,
    artifact: String,
    version: Option<String>,
    parent: Option<ParentRef>,
    properties: BTreeMap<String, String>,
    managed: Vec<Dep>,
    deps: Vec<Dep>,
    modules: Vec<String>,
    /// `<packaging>` (`pom` modules aggregate others and never compile anything).
    packaging: Option<String>,
    /// `<build><sourceDirectory>` (default `src/main/java`).
    source_directory: Option<String>,
    /// Raw compiler release/source/target values.
    release: Vec<String>,
    /// The build creates sources before compiling (generate phases, explicitly named
    /// annotation processors).
    generates_sources: bool,
    /// `annotationProcessorPaths` entries of the compiler plugin. An entry generates sources
    /// only when its jar declares an annotation processor ([`processor_jar_generates`]); a
    /// javac plugin on the same path (`-Xplugin`) generates nothing.
    processor_paths: Vec<Dep>,
}

#[derive(Clone, Debug)]
pub(super) struct ParentRef {
    group: String,
    artifact: String,
    version: String,
    relative_path: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct Dep {
    group: String,
    artifact: String,
    version: Option<String>,
    scope: String,
    kind: String,
    classifier: Option<String>,
}

pub(super) fn text_of(e: &Element, name: &str) -> Option<String> {
    e.child(name)
        .map(|c| c.text.trim().to_string())
        .filter(|t| !t.is_empty())
}

pub(super) fn parse_deps(list: Option<&Element>) -> Vec<Dep> {
    let Some(list) = list else {
        return Vec::new();
    };
    list.children_named("dependency")
        .filter_map(|d| {
            Some(Dep {
                group: text_of(d, "groupId")?,
                artifact: text_of(d, "artifactId")?,
                version: text_of(d, "version"),
                scope: text_of(d, "scope").unwrap_or_else(|| "compile".to_string()),
                kind: text_of(d, "type").unwrap_or_else(|| "jar".to_string()),
                classifier: text_of(d, "classifier"),
            })
        })
        .collect()
}

pub(super) fn parse_pom(text: &str) -> Option<Pom> {
    let root = xml::parse(text)?;
    if root.local_name() != "project" {
        return None;
    }
    let mut pom = Pom {
        artifact: text_of(&root, "artifactId")?,
        group: text_of(&root, "groupId"),
        version: text_of(&root, "version"),
        packaging: text_of(&root, "packaging"),
        ..Pom::default()
    };
    if let Some(p) = root.child("parent") {
        if let (Some(group), Some(artifact), Some(version)) =
            (text_of(p, "groupId"), text_of(p, "artifactId"), text_of(p, "version"))
        {
            pom.parent = Some(ParentRef {
                group,
                artifact,
                version,
                relative_path: p.child("relativePath").map(|r| r.text.trim().to_string()),
            });
        }
    }
    if let Some(props) = root.child("properties") {
        for c in &props.children {
            pom.properties
                .insert(c.local_name().to_string(), c.text.trim().to_string());
        }
    }
    pom.managed = parse_deps(
        root.child("dependencyManagement")
            .and_then(|m| m.child("dependencies")),
    );
    pom.deps = parse_deps(root.child("dependencies"));
    let mut module_lists: Vec<&Element> = root.child("modules").into_iter().collect();
    if let Some(profiles) = root.child("profiles") {
        for profile in profiles.children_named("profile") {
            module_lists.extend(profile.child("modules"));
        }
    }
    for list in module_lists {
        for m in list.children_named("module") {
            let t = m.text.trim();
            if !t.is_empty() {
                pom.modules.push(t.to_string());
            }
        }
    }
    for key in ["maven.compiler.release", "maven.compiler.source", "maven.compiler.target"] {
        if let Some(v) = pom.properties.get(key) {
            pom.release.push(v.clone());
        }
    }
    if let Some(build) = root.child("build") {
        pom.source_directory = text_of(build, "sourceDirectory");
        let mut plugins: Vec<&Element> = Vec::new();
        if let Some(list) = build.child("plugins") {
            plugins.extend(list.children_named("plugin"));
        }
        if let Some(list) = build.child("pluginManagement").and_then(|m| m.child("plugins")) {
            plugins.extend(list.children_named("plugin"));
        }
        for plugin in plugins {
            let artifact = text_of(plugin, "artifactId").unwrap_or_default();
            if let Some(config) = plugin.child("configuration") {
                if artifact == "maven-compiler-plugin" {
                    for key in ["release", "source", "target"] {
                        if let Some(v) = text_of(config, key) {
                            pom.release.push(v);
                        }
                    }
                    if let Some(paths) = config.child("annotationProcessorPaths") {
                        for entry in &paths.children {
                            if let (Some(group), Some(artifact)) =
                                (text_of(entry, "groupId"), text_of(entry, "artifactId"))
                            {
                                pom.processor_paths.push(Dep {
                                    group,
                                    artifact,
                                    version: text_of(entry, "version"),
                                    scope: "compile".to_string(),
                                    kind: text_of(entry, "type").unwrap_or_else(|| "jar".to_string()),
                                    classifier: text_of(entry, "classifier"),
                                });
                            }
                        }
                    }
                    // Processors named explicitly run whether or not their jar lists them.
                    if config
                        .child("annotationProcessors")
                        .is_some_and(|a| !a.children.is_empty())
                    {
                        pom.generates_sources = true;
                    }
                }
            }
            if let Some(executions) = plugin.child("executions") {
                for e in executions.children_named("execution") {
                    if text_of(e, "phase")
                        .is_some_and(|p| p == "generate-sources" || p == "generate-test-sources")
                    {
                        pom.generates_sources = true;
                    }
                }
            }
        }
    }
    Some(pom)
}

/// `${...}` interpolation (properties, `env.*`); None when a reference stays unresolved.
pub(super) fn interpolate(text: &str, props: &BTreeMap<String, String>, vars: &EnvVars) -> Option<String> {
    let mut s = text.to_string();
    for _ in 0..MAX_INTERPOLATIONS {
        let Some(start) = s.find("${") else {
            return Some(s);
        };
        let end = start + s[start..].find('}')?;
        let key = &s[start + 2..end];
        let value = match key.strip_prefix("env.") {
            Some(var) => vars.get(var).and_then(|v| v.to_str()).map(str::to_string),
            None => props.get(key).cloned(),
        }?;
        s.replace_range(start..=end, &value);
    }
    (!s.contains("${")).then_some(s)
}

pub(super) fn release_feature(value: &str) -> Option<u32> {
    let v = Version::parse(value)?;
    java_feature(&v)
}

/// `<repo>/<group path>/<artifact>/<version>/`
pub(super) fn repo_dir(repo: &Path, group: &str, artifact: &str, version: &str) -> PathBuf {
    let mut p = repo.to_path_buf();
    for seg in group.split('.') {
        p.push(seg);
    }
    p.join(artifact).join(version)
}

pub(super) fn is_version_range(v: &str) -> bool {
    v.contains(['[', '(', ',', ')', ']']) || v.ends_with('+') || v == "LATEST" || v == "RELEASE"
}

/// Whether the artifact file of a dependency is in the local repository.
pub(super) fn repo_has(repo: &Path, dep: &Dep, version: &str) -> bool {
    let (classifier, ext) = match dep.kind.as_str() {
        "test-jar" => (Some("tests".to_string()), "jar".to_string()),
        "jar" | "bundle" | "maven-plugin" | "ejb" | "ejb-client" | "java-source" | "javadoc" => {
            (dep.classifier.clone(), "jar".to_string())
        }
        other => (dep.classifier.clone(), other.to_string()),
    };
    let dir = repo_dir(repo, &dep.group, &dep.artifact, version);
    if version.ends_with("-SNAPSHOT") {
        // Remote snapshots are stored with timestamps; any artifact of that type counts.
        return entries(&dir)
            .iter()
            .any(|(n, _)| n.starts_with(&dep.artifact) && n.ends_with(&format!(".{ext}")));
    }
    let file = match &classifier {
        Some(c) => format!("{}-{version}-{c}.{ext}", dep.artifact),
        None => format!("{}-{version}.{ext}", dep.artifact),
    };
    dir.join(file).is_file()
}

/// The jar of `group:artifact:version[:classifier]` in the local repository (SNAPSHOT versions:
/// the newest timestamped jar of the folder), when installed.
pub(super) fn repo_artifact(
    repo: &Path,
    group: &str,
    artifact: &str,
    version: &str,
    classifier: Option<&str>,
) -> Option<PathBuf> {
    let dir = repo_dir(repo, group, artifact, version);
    let suffix = match classifier {
        Some(c) => format!("-{c}.jar"),
        None => ".jar".to_string(),
    };
    if version.ends_with("-SNAPSHOT") {
        // `name-1.0-SNAPSHOT-sources.jar` is not the main jar of a classifier-less entry; the
        // main jar ends in `-SNAPSHOT` (local build) or a build number (remote snapshot).
        let main_jar = |n: &str| {
            classifier.is_some() || {
                let last = n.trim_end_matches(".jar").rsplit('-').next().unwrap_or("");
                last == "SNAPSHOT" || !last.chars().any(|c| c.is_ascii_alphabetic())
            }
        };
        return entries(&dir)
            .into_iter()
            .filter(|(n, _)| n.starts_with(artifact) && n.ends_with(&suffix) && main_jar(n))
            .map(|(_, p)| p)
            .next_back();
    }
    let file = dir.join(format!("{artifact}-{version}{suffix}"));
    file.is_file().then_some(file)
}

/// The service file through which javac discovers annotation processors.
pub(super) const PROCESSOR_SERVICE: &str = "META-INF/services/javax.annotation.processing.Processor";

/// Whether a processor-path jar declares an annotation processor (`None`: not a readable zip).
pub(crate) fn processor_jar_generates(jar: &Path) -> Option<bool> {
    zip_has_entry(jar, PROCESSOR_SERVICE)
}

/// Largest zip central directory read (a jar's file list).
pub(super) const MAX_CENTRAL_DIRECTORY: u64 = 64 * 1024 * 1024;

/// Whether the zip archive at `path` lists `name` in its central directory (`None`: not a
/// readable zip). Reads only the end records and the central directory (zip / zip64 layout,
/// APPNOTE 4.3.12-4.3.16); nothing is decompressed.
pub(crate) fn zip_has_entry(path: &Path, name: &str) -> Option<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let u16_at = |b: &[u8], i: usize| -> Option<u64> {
        Some(u64::from(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?)))
    };
    let u32_at = |b: &[u8], i: usize| -> Option<u64> {
        Some(u64::from(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?)))
    };
    let u64_at =
        |b: &[u8], i: usize| -> Option<u64> { Some(u64::from_le_bytes(b.get(i..i + 8)?.try_into().ok()?)) };
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    // End of central directory: 22 bytes + a comment of at most 65535 bytes, at the end.
    let tail_len = len.min(22 + 65_535);
    let tail_start = len - tail_len;
    file.seek(SeekFrom::Start(tail_start)).ok()?;
    let mut tail = vec![0u8; usize::try_from(tail_len).ok()?];
    file.read_exact(&mut tail).ok()?;
    let eocd = (0..tail.len().checked_sub(21)?)
        .rev()
        .find(|&i| tail[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])?;
    // The 4 signature bytes at `pos` of the file.
    let signature_at = |file: &mut fs::File, pos: u64| -> Option<[u8; 4]> {
        let mut sig = [0u8; 4];
        file.seek(SeekFrom::Start(pos)).ok()?;
        file.read_exact(&mut sig).ok()?;
        Some(sig)
    };
    let eocd_at = tail_start + u64::try_from(eocd).ok()?;
    let mut entries = u16_at(&tail, eocd + 10)?;
    let mut cd_size = u32_at(&tail, eocd + 12)?;
    let mut cd_offset = u32_at(&tail, eocd + 16)?;
    // Where the central directory ends in the file: the end record, or the zip64 record.
    let mut cd_end = eocd_at;
    if entries == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_offset == 0xFFFF_FFFF {
        // Zip64: the locator (20 bytes) right before the end record names the zip64 record
        // (56 bytes without extensible data).
        let locator = eocd.checked_sub(20)?;
        if tail[locator..locator + 4] != [0x50, 0x4b, 0x06, 0x07] {
            return None;
        }
        let recorded = u64_at(&tail, locator + 8)?;
        let behind_locator = eocd_at.checked_sub(20 + 56)?;
        let record_at = [recorded, behind_locator]
            .into_iter()
            .find(|pos| signature_at(&mut file, *pos) == Some([0x50, 0x4b, 0x06, 0x06]))?;
        let mut record = [0u8; 56];
        file.seek(SeekFrom::Start(record_at)).ok()?;
        file.read_exact(&mut record).ok()?;
        entries = u64_at(&record, 32)?;
        cd_size = u64_at(&record, 40)?;
        cd_offset = u64_at(&record, 48)?;
        cd_end = record_at;
    }
    if cd_size > MAX_CENTRAL_DIRECTORY || cd_size > len {
        return None;
    }
    if entries == 0 {
        return Some(false);
    }
    // Archives with data in front (self-extracting) store offsets relative to the zip part:
    // then the directory is found where it ends, right before the end records.
    let cd_start = [Some(cd_offset), cd_end.checked_sub(cd_size)]
        .into_iter()
        .flatten()
        .find(|pos| signature_at(&mut file, *pos) == Some([0x50, 0x4b, 0x01, 0x02]))?;
    file.seek(SeekFrom::Start(cd_start)).ok()?;
    let mut cd = vec![0u8; usize::try_from(cd_size).ok()?];
    file.read_exact(&mut cd).ok()?;
    let wanted = name.as_bytes();
    let mut at = 0usize;
    for _ in 0..entries {
        if cd.get(at..at + 4)? != [0x50, 0x4b, 0x01, 0x02] {
            return None;
        }
        let name_len = usize::try_from(u16_at(&cd, at + 28)?).ok()?;
        let extra_len = usize::try_from(u16_at(&cd, at + 30)?).ok()?;
        let comment_len = usize::try_from(u16_at(&cd, at + 32)?).ok()?;
        let entry = cd.get(at + 46..at + 46 + name_len)?;
        if entry == wanted {
            return Some(true);
        }
        at += 46 + name_len + extra_len + comment_len;
    }
    Some(false)
}

pub(super) struct MavenModel<'a> {
    repo: &'a Path,
    config: &'a BTreeMap<String, String>,
    /// Module dir -> pom.
    poms: BTreeMap<String, Pom>,
    /// (group, artifact) -> module dir.
    by_ga: BTreeMap<(String, String), String>,
}

/// One pom of a parent chain: in the repository (dir) or from the local repository.
pub(super) struct ChainPom {
    pom: Pom,
}

impl<'a> MavenModel<'a> {
    pub(super) fn load(
        root: &Path,
        walk: &Walk,
        repo: &'a Path,
        config: &'a BTreeMap<String, String>,
    ) -> MavenModel<'a> {
        let mut poms = BTreeMap::new();
        for rel in walk.named(&["pom.xml"]) {
            if let Some(pom) =
                relpath::read_small(&root.join(rel), MAX_FILE_BYTES).and_then(|t| parse_pom(&t))
            {
                poms.insert(relpath::parent(rel).to_string(), pom);
            }
        }
        let mut by_ga = BTreeMap::new();
        for (dir, pom) in &poms {
            let group = pom
                .group
                .clone()
                .or_else(|| pom.parent.as_ref().map(|p| p.group.clone()))
                .unwrap_or_default();
            by_ga
                .entry((group, pom.artifact.clone()))
                .or_insert_with(|| dir.clone());
        }
        MavenModel {
            repo,
            config,
            poms,
            by_ga,
        }
    }

    fn repo_pom(&self, group: &str, artifact: &str, version: &str) -> Option<Pom> {
        let file = repo_dir(self.repo, group, artifact, version).join(format!("{artifact}-{version}.pom"));
        relpath::read_small(&file, MAX_FILE_BYTES).and_then(|t| parse_pom(&t))
    }

    /// The pom and its ancestors (nearest first). Missing external parents go to `missing`.
    fn chain(&self, start: Pom, start_dir: Option<&str>, missing: &mut Vec<String>) -> Vec<ChainPom> {
        let mut out = Vec::new();
        let mut current = Some((start, start_dir.map(str::to_string)));
        while let Some((pom, dir)) = current.take() {
            if out.len() >= MAX_POM_CHAIN {
                break;
            }
            let parent = pom.parent.clone();
            out.push(ChainPom { pom });
            let Some(parent) = parent else {
                break;
            };
            // In the repository: relativePath (default ../pom.xml), then by coordinates.
            let in_repo = dir
                .as_deref()
                .and_then(|d| {
                    let rel = parent
                        .relative_path
                        .clone()
                        .unwrap_or_else(|| "../pom.xml".to_string());
                    if rel.is_empty() {
                        return None;
                    }
                    let target = relpath::normalize(d, &rel)?;
                    let target_dir = if target.ends_with(".xml") {
                        relpath::parent(&target).to_string()
                    } else {
                        target
                    };
                    self.poms
                        .get(&target_dir)
                        .filter(|p| p.artifact == parent.artifact)
                        .map(|p| (p.clone(), Some(target_dir)))
                })
                .or_else(|| {
                    self.by_ga
                        .get(&(parent.group.clone(), parent.artifact.clone()))
                        .and_then(|d| self.poms.get(d).map(|p| (p.clone(), Some(d.clone()))))
                });
            current = match in_repo {
                Some(found) => Some(found),
                None => match self.repo_pom(&parent.group, &parent.artifact, &parent.version) {
                    Some(p) => Some((p, None)),
                    None => {
                        missing.push(format!("{}:{}:{}", parent.group, parent.artifact, parent.version));
                        None
                    }
                },
            };
        }
        out
    }

    /// Effective properties of a chain (nearest wins; maven.config defines win over all).
    fn properties(&self, chain: &[ChainPom]) -> BTreeMap<String, String> {
        let mut props = BTreeMap::new();
        for c in chain.iter().rev() {
            for (k, v) in &c.pom.properties {
                props.insert(k.clone(), v.clone());
            }
        }
        if let Some(first) = chain.first() {
            let pom = &first.pom;
            let parent_group = pom.parent.as_ref().map(|p| p.group.clone());
            let parent_version = pom.parent.as_ref().map(|p| p.version.clone());
            let group = pom.group.clone().or_else(|| parent_group.clone());
            let version = pom.version.clone().or_else(|| parent_version.clone());
            for (k, v) in [
                ("project.groupId", group.clone()),
                ("pom.groupId", group.clone()),
                ("groupId", group),
                ("project.version", version.clone()),
                ("pom.version", version.clone()),
                ("version", version),
                ("project.artifactId", Some(pom.artifact.clone())),
                ("artifactId", Some(pom.artifact.clone())),
                ("project.parent.groupId", parent_group),
                ("project.parent.version", parent_version),
            ] {
                if let Some(v) = v {
                    props.entry(k.to_string()).or_insert(v);
                }
            }
        }
        for (k, v) in self.config {
            props.insert(k.clone(), v.clone());
        }
        props
    }

    /// Managed versions of a chain (nearest wins), imported BOMs included.
    fn managed(
        &self,
        chain: &[ChainPom],
        props: &BTreeMap<String, String>,
        vars: &EnvVars,
        depth: usize,
        missing: &mut Vec<String>,
    ) -> BTreeMap<(String, String), String> {
        let mut out = BTreeMap::new();
        for c in chain.iter().rev() {
            for d in &c.pom.managed {
                let (Some(group), Some(artifact)) =
                    (interpolate(&d.group, props, vars), interpolate(&d.artifact, props, vars))
                else {
                    continue;
                };
                let Some(version) = d.version.as_deref().and_then(|v| interpolate(v, props, vars)) else {
                    continue;
                };
                if d.scope == "import" && d.kind == "pom" {
                    if depth >= MAX_BOM_DEPTH || is_version_range(&version) {
                        continue;
                    }
                    if let Some(dir) = self.by_ga.get(&(group.clone(), artifact.clone())) {
                        if let Some(bom) = self.poms.get(dir).cloned() {
                            let bom_chain = self.chain(bom, Some(dir.as_str()), missing);
                            let bom_props = self.properties(&bom_chain);
                            out.extend(self.managed(&bom_chain, &bom_props, vars, depth + 1, missing));
                        }
                        continue;
                    }
                    match self.repo_pom(&group, &artifact, &version) {
                        Some(bom) => {
                            let bom_chain = self.chain(bom, None, missing);
                            let bom_props = self.properties(&bom_chain);
                            out.extend(self.managed(&bom_chain, &bom_props, vars, depth + 1, missing));
                        }
                        None => missing.push(format!("{group}:{artifact}:{version}")),
                    }
                } else {
                    out.insert((group, artifact), version);
                }
            }
        }
        out
    }

    /// Whether an `annotationProcessorPaths` entry generates sources: its jar in the local
    /// repository declares `META-INF/services/javax.annotation.processing.Processor` (javac's
    /// processor discovery). A jar that only declares a javac plugin
    /// (`com.sun.source.util.Plugin`, run with `-Xplugin`) or nothing generates nothing.
    /// Undecidable entries (coordinates that do not resolve, a jar that is not installed or not
    /// readable) count as generators: only the build can tell, so it must run first.
    fn processor_path_generates(
        &self,
        dep: &Dep,
        props: &BTreeMap<String, String>,
        managed: &BTreeMap<(String, String), String>,
        vars: &EnvVars,
    ) -> bool {
        let (Some(group), Some(artifact)) =
            (interpolate(&dep.group, props, vars), interpolate(&dep.artifact, props, vars))
        else {
            return true;
        };
        let version = match &dep.version {
            Some(v) => interpolate(v, props, vars),
            None => managed.get(&(group.clone(), artifact.clone())).cloned(),
        };
        let Some(version) = version.filter(|v| !is_version_range(v)) else {
            return true;
        };
        let Some(jar) = repo_artifact(self.repo, &group, &artifact, &version, dep.classifier.as_deref())
        else {
            return true;
        };
        processor_jar_generates(&jar).unwrap_or(true)
    }

    /// Reactor closure of each required root.
    fn closure(&self, roots: &[String]) -> BTreeSet<String> {
        let mut members = BTreeSet::new();
        let mut queue: Vec<String> = roots.to_vec();
        while let Some(dir) = queue.pop() {
            if !members.insert(dir.clone()) {
                continue;
            }
            let Some(pom) = self.poms.get(&dir) else {
                continue;
            };
            for m in &pom.modules {
                let Some(target) = relpath::normalize(&dir, m) else {
                    continue;
                };
                let target_dir = if target.ends_with(".xml") {
                    relpath::parent(&target).to_string()
                } else {
                    target
                };
                if self.poms.contains_key(&target_dir) {
                    queue.push(target_dir);
                }
            }
        }
        members
    }

    pub(super) fn fill(
        &self,
        root: &Path,
        walk: &Walk,
        project: &mut JvmProject,
        missing: &mut BTreeMap<BuildSystem, Vec<String>>,
    ) {
        if self.poms.is_empty() {
            return;
        }
        let dirs: Vec<String> = self.poms.keys().cloned().collect();
        let roots = outermost(&dirs);
        let members = self.closure(&roots);
        for dir in &dirs {
            if !members.contains(dir) {
                let owner = roots
                    .iter()
                    .find(|r| relpath::within(dir, r))
                    .map(String::as_str)
                    .unwrap_or("");
                project.subprojects.push(SubProject {
                    dir: dir.clone(),
                    reason: format!(
                        "Maven project not listed in the modules of {}",
                        relpath::join(owner, "pom.xml")
                    ),
                });
            }
        }
        project.maven_roots = roots;
        project.maven_modules = members.iter().cloned().collect();

        let vars = EnvVars::default();
        let mut lacking: Vec<String> = Vec::new();
        let mut release: Option<JavaPin> = None;
        for dir in &members {
            let Some(pom) = self.poms.get(dir).cloned() else {
                continue;
            };
            let chain = self.chain(pom, Some(dir.as_str()), &mut lacking);
            let props = self.properties(&chain);
            let managed = self.managed(&chain, &props, &vars, 0, &mut lacking);
            // Release: own compiler settings, then inherited ones.
            for c in &chain {
                let mut best: Option<u32> = None;
                for raw in &c.pom.release {
                    if let Some(f) = interpolate(raw, &props, &vars).as_deref().and_then(release_feature) {
                        best = Some(best.map_or(f, |b| b.max(f)));
                    }
                }
                if let Some(f) = best {
                    if release.as_ref().is_none_or(|r| f > r.feature) {
                        release = Some(JavaPin {
                            feature: f,
                            source: relpath::join(dir, "pom.xml"),
                        });
                    }
                    break;
                }
            }
            // Generated sources that only a build creates.
            // A `pom` module never compiles: its (inherited) compiler settings and generate
            // phases apply to the modules that inherit them, never to itself.
            // The compiler skips a module without main sources ("No sources to compile") and
            // then creates no generated-sources folder either.
            let own = chain.first().map(|c| &c.pom);
            let compiles = own.is_none_or(|p| p.packaging.as_deref() != Some("pom"));
            let has_main = own
                .and_then(|p| p.source_directory.as_deref())
                .map_or_else(|| root.join(dir).join("src/main/java").is_dir(), |_| true);
            let generates = chain.iter().any(|c| {
                c.pom.generates_sources
                    || c.pom
                        .processor_paths
                        .iter()
                        .any(|d| self.processor_path_generates(d, &props, &managed, &vars))
            }) || walk.proto.contains(dir);
            if compiles && (has_main || walk.proto.contains(dir)) && generates {
                let generated = relpath::join(dir, "target/generated-sources");
                if !root.join(&generated).is_dir() {
                    project.generated_sources.push(generated);
                }
            }
            if compiles && walk.proto_test.contains(dir) {
                let generated = relpath::join(dir, "target/generated-test-sources");
                if !root.join(&generated).is_dir() {
                    project.generated_sources.push(generated);
                }
            }
            // Declared dependencies of the module and its ancestors.
            for c in &chain {
                for d in &c.pom.deps {
                    if matches!(d.scope.as_str(), "system" | "import") {
                        continue;
                    }
                    let (Some(group), Some(artifact)) =
                        (interpolate(&d.group, &props, &vars), interpolate(&d.artifact, &props, &vars))
                    else {
                        continue;
                    };
                    if self.by_ga.contains_key(&(group.clone(), artifact.clone())) {
                        continue;
                    }
                    let version = match &d.version {
                        Some(v) => interpolate(v, &props, &vars),
                        None => managed.get(&(group.clone(), artifact.clone())).cloned(),
                    };
                    let Some(version) = version.filter(|v| !is_version_range(v)) else {
                        continue;
                    };
                    let dep = Dep {
                        group: group.clone(),
                        artifact: artifact.clone(),
                        ..d.clone()
                    };
                    if !repo_has(self.repo, &dep, &version) {
                        lacking.push(format!("{group}:{artifact}:{version}"));
                    }
                }
            }
        }
        project.java_release = release;
        lacking.sort();
        lacking.dedup();
        if !lacking.is_empty() {
            missing.entry(BuildSystem::Maven).or_default().extend(lacking);
        }
    }
}
