//! Declarations of reflection types in installed packages (DESIGN-bridges §2 rule 5; owner
//! derive): the declaration of an annotation type a repository file uses, located by the
//! language's own naming rules from its qualified name, so the bridge stage can follow its
//! meta-annotation chain up to a `reflection_roots` row.
//!
//! * Java: a public top-level type `a.b.C` is declared in `a/b/C.java` (JLS 7.6); installed
//!   dependency sources are the `-sources.jar` archives a Maven / Gradle cache keeps next to
//!   the dependency jars, below the group folder of the artifact. The group of an artifact is
//!   a prefix of the packages it declares (checked: the entry must exist in the archive), so
//!   only the archives below the folders of the package's prefixes are opened.
//!
//! * C#: installed packages are compiled assemblies ([`super::clr_meta`]); a type
//!   `N.T` is defined in an assembly whose name is a prefix of its namespace or extends it
//!   (checked: the assembly must define the type); base types in other assemblies are
//!   found by the assembly name their reference carries. The lineage (type, then its bases)
//!   lists each type's declared interfaces and, from every installed assembly defining it
//!   (reference assemblies carry no method bodies, implementation assemblies do), the
//!   string literals its own code loads.
//!
//! Bounded: a few hundred archives per name at most; answers are cached per process.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use trace_core::Language;
use trace_env::LibraryRoot;

use super::clr_meta::{self, TypeDef};
use crate::archive;
use crate::once::OnceMap;

/// A compiled type of a lineage: its definition and the string literals its own code loads
/// (merged over every installed assembly defining it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompiledType {
    pub def: TypeDef,
    pub literals: Vec<String>,
}

/// Source archives examined per qualified name at most.
const MAX_ARCHIVES: usize = 400;
/// Largest declaration source read.
const MAX_SOURCE_BYTES: u64 = 4 << 20;

type Found = Vec<(PathBuf, Vec<u8>)>;
/// Cache key: the library root paths and the qualified type name.
type FoundKey = (Vec<PathBuf>, String);

/// Sources of the declaration of the type `qualified` (dotted) in the installed packages of
/// `roots`: every source entry found (one per artifact version), sorted by path. Empty when
/// the language has no source layout for installed types or nothing declares it.
pub fn type_sources(roots: &[LibraryRoot], language: Language, qualified: &str) -> Found {
    if language != Language::Java || qualified.split('.').count() < 2 {
        return Vec::new();
    }
    static CACHE: LazyLock<OnceMap<FoundKey, Found>> = LazyLock::new(OnceMap::default);
    let key = (roots.iter().map(|r| r.path.clone()).collect::<Vec<_>>(), qualified.to_string());
    CACHE.get_or_init(key, || {
        let mut found = java_sources(roots, qualified);
        found.sort_by(|a, b| a.0.cmp(&b.0));
        found
    })
}

fn dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    out.sort();
    out
}

fn source_archives(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .is_some_and(|n| n.to_string_lossy().ends_with("-sources.jar"))
        })
        .collect();
    out.sort();
    out
}

/// `a.b.C` -> `a/b/C.java` in the source archives below the group folders of the package's
/// prefixes (deepest first; Maven: `<a>/<b>/<artifact>/<version>/`, Gradle:
/// `<a.b>/<artifact>/<version>/<hash>/`).
fn java_sources(roots: &[LibraryRoot], qualified: &str) -> Found {
    let parts: Vec<&str> = qualified.split('.').collect();
    let package = &parts[..parts.len() - 1];
    let entry = format!("{}.java", parts.join("/"));
    let mut out = Vec::new();
    let mut examined = 0usize;
    for root in roots {
        let gradle = root.layout == "gradle_cache";
        if !matches!(root.layout, "maven_repo" | "gradle_cache" | "coursier_cache") {
            continue;
        }
        let base = if gradle && root.path.join("files-2.1").is_dir() {
            root.path.join("files-2.1")
        } else {
            root.path.clone()
        };
        for k in (1..=package.len()).rev() {
            let group = if gradle {
                base.join(package[..k].join("."))
            } else {
                package[..k].iter().fold(base.clone(), |p, s| p.join(s))
            };
            if !group.is_dir() {
                continue;
            }
            for artifact in dirs(&group) {
                for version in dirs(&artifact) {
                    let holders = if gradle {
                        dirs(&version)
                    } else {
                        vec![version.clone()]
                    };
                    for holder in holders {
                        for jar in source_archives(&holder) {
                            examined += 1;
                            if examined > MAX_ARCHIVES {
                                return out;
                            }
                            if !archive::has_entry(&jar, &entry) {
                                continue;
                            }
                            let path = archive::entry_path(&jar, &entry);
                            if let Some(bytes) = archive::read(&path, MAX_SOURCE_BYTES) {
                                out.push((path, bytes));
                            }
                        }
                    }
                }
            }
            if !out.is_empty() {
                return out;
            }
        }
    }
    out
}

/// Assemblies read per process at most.
const MAX_ASSEMBLIES: usize = 256;
/// Assembly files listed per root at most.
const MAX_LISTED: usize = 20_000;
/// Base types followed at most.
const MAX_LINEAGE: usize = 16;

/// The .NET library roots among `roots` (the key of the per-process caches).
fn dotnet_roots(roots: &[LibraryRoot]) -> Vec<PathBuf> {
    roots
        .iter()
        .filter(|r| r.ecosystem == trace_env::EcosystemId::Dotnet)
        .map(|r| r.path.clone())
        .collect()
}

/// Assembly files (`.dll`) below the .NET roots (package folders and toolchain frameworks),
/// sorted; cached per process.
fn assembly_files(roots: &[LibraryRoot]) -> Vec<PathBuf> {
    static CACHE: LazyLock<OnceMap<Vec<PathBuf>, Vec<PathBuf>>> = LazyLock::new(OnceMap::default);
    let key = dotnet_roots(roots);
    CACHE.get_or_init(key.clone(), || list_assemblies(&key))
}

fn list_assemblies(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        let mut stack = vec![(root.clone(), 0usize)];
        let mut listed = 0usize;
        while let Some((dir, depth)) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                listed += 1;
                if listed > MAX_LISTED {
                    break;
                }
                let path = e.path();
                if path.is_dir() {
                    if depth < 6 {
                        stack.push((path, depth + 1));
                    }
                } else if path.extension().is_some_and(|x| x.eq_ignore_ascii_case("dll")) {
                    out.push(path);
                }
            }
        }
    }
    out.sort();
    out
}

/// Parsed assembly of a file (cached per process, bounded).
fn assembly(path: &Path) -> Option<Arc<clr_meta::Assembly>> {
    static CACHE: LazyLock<OnceMap<PathBuf, Option<Arc<clr_meta::Assembly>>>> =
        LazyLock::new(OnceMap::default);
    CACHE.get_or_init_bounded(path.to_path_buf(), MAX_ASSEMBLIES, || {
        std::fs::read(path)
            .ok()
            .and_then(|b| clr_meta::read(&b))
            .map(Arc::new)
    })
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Assemblies examined per type lookup at most (beyond: no definition, never a guess).
const MAX_EXAMINED: usize = 64;

/// How closely an assembly named `stem` relates to `namespace`: 0 same name, 1 the name is a
/// prefix of the namespace, 2 the name extends the namespace; `None` unrelated.
fn relation(stem: &str, namespace: &str) -> Option<u8> {
    let (s, n) = (stem.to_ascii_lowercase(), namespace.to_ascii_lowercase());
    if s == n {
        Some(0)
    } else if n.starts_with(&format!("{s}.")) {
        Some(1)
    } else if s.starts_with(&format!("{n}.")) {
        Some(2)
    } else {
        None
    }
}

/// Installed definitions of `full` (`Namespace.Name`) with their assembly files. The first
/// definition is found in `same` (the derived type's assemblies, for a base defined next to
/// it), in the assemblies named `assembly_name` (a reference's), else in the assemblies whose
/// name relates to the namespace (closest first), following type forwards; every other
/// installed assembly of the defining assembly's name then adds its definition (reference
/// and implementation assemblies of one name).
fn definitions(
    files: &[PathBuf],
    full: &str,
    assembly_name: Option<&str>,
    same: &[PathBuf],
) -> Vec<(PathBuf, TypeDef)> {
    let namespace = full.rsplit_once('.').map(|(n, _)| n).unwrap_or("");
    let mut order: Vec<(u8, &PathBuf)> = files
        .iter()
        .filter_map(|f| {
            let s = stem(f);
            let rank = match assembly_name {
                Some(a) => s.eq_ignore_ascii_case(a).then_some(0),
                None if same.contains(f) => Some(0),
                None => relation(&s, namespace).map(|r| r + 1),
            };
            rank.map(|r| (r, f))
        })
        .collect();
    order.sort();
    let mut defining: Option<String> = None;
    let mut forwarded_to: Option<String> = None;
    for (_, f) in order.iter().take(MAX_EXAMINED) {
        let Some(a) = assembly(f) else { continue };
        if a.find(full).is_some() {
            defining = Some(stem(f));
            break;
        }
        if forwarded_to.is_none() {
            forwarded_to = a.forwarded(full).map(str::to_string);
        }
    }
    let defining = match (defining, forwarded_to) {
        (Some(d), _) => d,
        (None, Some(target)) if assembly_name != Some(target.as_str()) => {
            return definitions(files, full, Some(&target), &[]);
        }
        _ => return Vec::new(),
    };
    files
        .iter()
        .filter(|f| stem(f).eq_ignore_ascii_case(&defining))
        .filter_map(|f| {
            assembly(f)
                .and_then(|a| a.find(full).cloned())
                .map(|t| (f.clone(), t))
        })
        .collect()
}

/// The lineage of the compiled type `full` in the installed .NET packages: the type first,
/// then its base types (each with its declared interfaces; literals merged over every
/// installed definition). Empty when no installed assembly defines it.
pub fn clr_lineage(roots: &[LibraryRoot], full: &str) -> Vec<CompiledType> {
    /// (.NET roots, type) -> lineage.
    type Lineages = OnceMap<(Vec<PathBuf>, String), Vec<CompiledType>>;
    static CACHE: LazyLock<Lineages> = LazyLock::new(OnceMap::default);
    CACHE.get_or_init((dotnet_roots(roots), full.to_string()), || read_lineage(roots, full))
}

/// [`clr_lineage`] read from the installed assemblies.
fn read_lineage(roots: &[LibraryRoot], full: &str) -> Vec<CompiledType> {
    let files = assembly_files(roots);
    if files.is_empty() {
        return Vec::new();
    }
    let mut lineage: Vec<CompiledType> = Vec::new();
    let mut next: Option<(String, Option<String>)> = Some((full.to_string(), None));
    let mut same: Vec<PathBuf> = Vec::new();
    while let Some((name, assembly_name)) = next.take() {
        if lineage.len() >= MAX_LINEAGE || lineage.iter().any(|t| t.def.full() == name) {
            break;
        }
        let found = definitions(&files, &name, assembly_name.as_deref(), &same);
        let Some((_, first)) = found.first() else { break };
        let mut merged = CompiledType {
            def: first.clone(),
            literals: Vec::new(),
        };
        for (path, d) in &found {
            for i in &d.interfaces {
                if !merged.def.interfaces.iter().any(|x| x.full() == i.full()) {
                    merged.def.interfaces.push(i.clone());
                }
            }
            if d.bodies.is_empty() {
                continue;
            }
            let (Some(a), Ok(bytes)) = (assembly(path), std::fs::read(path)) else { continue };
            for l in a.literals(&bytes, d) {
                if !merged.literals.contains(&l) {
                    merged.literals.push(l);
                }
            }
        }
        same = found.iter().map(|(p, _)| p.clone()).collect();
        next = merged.def.extends.as_ref().map(|e| (e.full(), e.assembly.clone()));
        lineage.push(merged);
    }
    lineage
}

#[cfg(test)]
#[path = "../../tests/unit/reflect/types.rs"]
mod tests;
