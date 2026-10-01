//! sbt, Mill and Scala CLI builds: versions and plugins read from the Scala syntax tree of the
//! build definitions, and their static dependency check against the Coursier / Ivy caches.

use super::*;

/// A plain Scala string literal (not interpolated).
pub(super) fn scala_string(t: &Syn<'_>, i: usize) -> Option<String> {
    if t.kind(i) != "string" {
        return None;
    }
    let t = t.text(i);
    let inner = t
        .strip_prefix("\"\"\"")
        .and_then(|r| r.strip_suffix("\"\"\""))
        .or_else(|| t.strip_prefix('"').and_then(|r| r.strip_suffix('"')))?;
    Some(inner.to_string())
}

/// Facts of sbt / Mill build definitions.
#[derive(Default)]
pub(super) struct ScalaBuildFacts {
    vals: BTreeMap<String, String>,
    scala_version: Option<ScalaRef>,
    cross: Vec<ScalaRef>,
    plugins: Vec<Vec<ScalaRef>>,
}

#[derive(Clone, Debug)]
pub(super) enum ScalaRef {
    Lit(String),
    Name(String),
}

impl ScalaRef {
    fn resolve(&self, vals: &BTreeMap<String, String>) -> Option<String> {
        match self {
            ScalaRef::Lit(s) => Some(s.clone()),
            ScalaRef::Name(n) => vals.get(n).cloned(),
        }
    }
}

pub(super) fn scala_ref(t: &Syn<'_>, i: usize) -> Option<ScalaRef> {
    match t.kind(i) {
        "string" => scala_string(t, i).map(ScalaRef::Lit),
        "identifier" => Some(ScalaRef::Name(t.text(i).to_string())),
        _ => None,
    }
}

pub(super) fn scala_facts(src: &str, facts: &mut ScalaBuildFacts) {
    let Some(t) = Syn::parse(Language::Scala, src.as_bytes()) else {
        return;
    };
    for i in 0..t.len() {
        match t.kind(i) {
            "val_definition" | "function_definition" => {
                let name = t
                    .field(i, "pattern")
                    .or_else(|| t.field(i, "name"))
                    .map(|n| t.text(n).to_string());
                let value = t.field(i, "value").or_else(|| t.field(i, "body"));
                let (Some(name), Some(value)) = (name, value) else {
                    continue;
                };
                if let Some(s) = scala_string(&t, value) {
                    if name == "scalaVersion" {
                        facts.scala_version = Some(ScalaRef::Lit(s.clone()));
                    }
                    facts.vals.entry(name).or_insert(s);
                } else if name == "scalaVersion" && t.kind(value) == "identifier" {
                    facts.scala_version = Some(ScalaRef::Name(t.text(value).to_string()));
                }
            }
            "infix_expression" => {
                let Some(op) = t.field(i, "operator") else {
                    continue;
                };
                if t.text(op) != ":=" {
                    continue;
                }
                let (Some(left), Some(right)) = (t.field(i, "left"), t.field(i, "right")) else {
                    continue;
                };
                let keys: Vec<&str> = t
                    .subtree(left)
                    .into_iter()
                    .filter(|n| t.kind(*n) == "identifier")
                    .map(|n| t.text(n))
                    .collect();
                if keys.contains(&"crossScalaVersions") {
                    if t.kind(right) == "call_expression" {
                        if let Some(args) = t.field(right, "arguments") {
                            for a in t.named_children(args) {
                                facts.cross.extend(scala_ref(&t, a));
                            }
                        }
                    } else {
                        facts.cross.extend(scala_ref(&t, right));
                    }
                } else if keys.contains(&"scalaVersion") {
                    if let Some(r) = scala_ref(&t, right) {
                        facts.scala_version = Some(r);
                    }
                }
            }
            "call_expression" => {
                let Some(function) = t.field(i, "function") else {
                    continue;
                };
                if t.text(function) != "addSbtPlugin" {
                    continue;
                }
                let Some(args) = t.field(i, "arguments") else {
                    continue;
                };
                let leaves: Vec<ScalaRef> = t
                    .subtree(args)
                    .into_iter()
                    .filter(|n| matches!(t.kind(*n), "string" | "identifier"))
                    .filter(|n| {
                        // Only the operands of `%` chains, never the operators themselves.
                        t.parent(*n).is_none_or(|p| t.field(p, "operator") != Some(*n))
                    })
                    .filter_map(|n| scala_ref(&t, n))
                    .collect();
                if leaves.len() == 3 {
                    facts.plugins.push(leaves);
                }
            }
            _ => {}
        }
    }
}

/// Scala versions (default first) and the sbt version of the build at `root` (used by the
/// Scala server's install extras, which get only the repository root).
pub fn scala_build_versions(root: &Path) -> (Vec<String>, Option<String>) {
    let mut project = JvmProject::default();
    let cx_walk = walk_plain(root);
    scala_builds(root, &cx_walk, &mut project);
    (project.scala_versions, project.sbt_version)
}

/// A walk without forbidden roots (install time: the repository root only).
pub(super) fn walk_plain(root: &Path) -> Walk {
    let platform = Platform::current();
    let vars = EnvVars::default();
    let cx = DetectContext {
        root,
        platform: &platform,
        vars: &vars,
        env_override: None,
        forbidden: &[],
        files: &[],
    };
    walk(&cx)
}

pub(super) fn scala_builds(root: &Path, walk: &Walk, project: &mut JvmProject) {
    let sbt_dirs: Vec<String> = walk
        .named(&["build.sbt"])
        .iter()
        .map(|f| relpath::parent(f).to_string())
        .collect();
    let sbt_roots = outermost(&sbt_dirs);
    let mut facts = ScalaBuildFacts::default();
    if let Some(sbt_root) = sbt_roots.first() {
        project.sbt_root = Some(sbt_root.clone());
        let base = root.join(sbt_root);
        project.sbt_version = os::read_key_values(&base.join("project").join("build.properties"))
            .get("sbt.version")
            .cloned();
        // Build definition: build.sbt + project/*.scala (vals), project/*.sbt (plugins).
        for (name, path) in entries(&base.join("project")) {
            if name.ends_with(".scala") || name.ends_with(".sbt") {
                if let Some(text) = relpath::read_small(&path, MAX_FILE_BYTES) {
                    scala_facts(&text, &mut facts);
                }
                project
                    .build_files
                    .push(relpath::join(sbt_root, &format!("project/{name}")));
            }
        }
        if let Some(text) = relpath::read_small(&base.join("build.sbt"), MAX_FILE_BYTES) {
            scala_facts(&text, &mut facts);
        }
        project
            .build_files
            .push(relpath::join(sbt_root, "project/build.properties"));
        // Nested sbt builds with their own project/build.properties are separate builds.
        for d in &sbt_dirs {
            if d != sbt_root && root.join(d).join("project").join("build.properties").is_file() {
                project.subprojects.push(SubProject {
                    dir: d.clone(),
                    reason: "separate sbt build inside the repository".to_string(),
                });
            }
        }
    }
    let mill_dirs: Vec<String> = walk
        .named(&["build.sc", "build.mill"])
        .iter()
        .map(|f| relpath::parent(f).to_string())
        .collect();
    if let Some(mill_root) = outermost(&mill_dirs).first() {
        project.mill_root = Some(mill_root.clone());
        for name in ["build.mill", "build.sc"] {
            if let Some(text) =
                relpath::read_small(&root.join(relpath::join(mill_root, name)), MAX_FILE_BYTES)
            {
                scala_facts(&text, &mut facts);
            }
        }
    }
    project.scala_cli_roots = walk
        .named(&["project.scala"])
        .iter()
        .map(|f| relpath::parent(f).to_string())
        .collect();

    let mut versions: Vec<String> = Vec::new();
    if let Some(v) = facts.scala_version.as_ref().and_then(|r| r.resolve(&facts.vals)) {
        versions.push(v);
    }
    for r in &facts.cross {
        if let Some(v) = r.resolve(&facts.vals) {
            if !versions.contains(&v) {
                versions.push(v);
            }
        }
    }
    versions.retain(|v| Version::parse(v).is_some());
    project.scala_versions = versions;
    project.sbt_plugins = facts
        .plugins
        .iter()
        .filter_map(|p| {
            Some((p[0].resolve(&facts.vals)?, p[1].resolve(&facts.vals)?, p[2].resolve(&facts.vals)?))
        })
        .collect();
    project.build_files.sort();
    project.build_files.dedup();
}

/// Maven-layout bases inside a Coursier cache (`https/<host>/...`).
pub(super) fn coursier_bases(coursier_cache: &Path) -> Vec<PathBuf> {
    let mut bases = Vec::new();
    if coursier_cache.as_os_str().is_empty() {
        return bases;
    }
    for (_, host) in entries(&coursier_cache.join("https")) {
        for sub in [
            "",
            "maven2",
            "maven",
            "content/repositories/releases",
            "artifactory/maven-central",
        ] {
            let base = if sub.is_empty() {
                host.clone()
            } else {
                host.join(sub)
            };
            if base.is_dir() {
                bases.push(base);
            }
        }
    }
    bases
}

pub(super) fn sbt_missing(
    project: &JvmProject,
    coursier_cache: &Path,
    ivy_home: &Path,
    sbt_boot: &Path,
) -> Vec<String> {
    let mut lacking = Vec::new();
    let bases = coursier_bases(coursier_cache);
    let maven_has = |group: &str, artifact: &str, version: &str| {
        bases.iter().any(|b| repo_dir(b, group, artifact, version).is_dir())
    };
    if let Some(v) = &project.sbt_version {
        let in_boot = entries(sbt_boot).iter().any(|(name, path)| {
            name.starts_with("scala-") && path.join("org.scala-sbt").join("sbt").join(v).is_dir()
        });
        if !in_boot && !maven_has("org.scala-sbt", "sbt", v) {
            lacking.push(format!("sbt {v}"));
        }
    }
    let sbt1 = project.sbt_version.as_deref().is_none_or(|v| v.starts_with("1."));
    if sbt1 {
        for (g, a, v) in &project.sbt_plugins {
            let cross = format!("{a}_2.12_1.0");
            let ivy_layouts = if ivy_home.as_os_str().is_empty() {
                Vec::new()
            } else {
                vec![
                    ivy_home
                        .join("local")
                        .join(g)
                        .join(a)
                        .join("scala_2.12")
                        .join("sbt_1.0")
                        .join(v),
                    ivy_home
                        .join("cache")
                        .join("scala_2.12")
                        .join("sbt_1.0")
                        .join(g)
                        .join(a),
                ]
            };
            let sbt_plugin_repo = !coursier_cache.as_os_str().is_empty()
                && entries(&coursier_cache.join("https")).into_iter().any(|(_, host)| {
                    host.join("scalasbt")
                        .join("sbt-plugin-releases")
                        .join(g)
                        .join(a)
                        .join("scala_2.12")
                        .join("sbt_1.0")
                        .join(v)
                        .is_dir()
                });
            if !maven_has(g, &cross, v) && !sbt_plugin_repo && !ivy_layouts.iter().any(|p| p.is_dir()) {
                lacking.push(format!("{g}:{cross}:{v}"));
            }
        }
    }
    lacking
}
