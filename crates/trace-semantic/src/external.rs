//! Calls answered only by locations outside the index (DESIGN §1.10 item 2; owner derive):
//! map a server location into a library file + declaration position, so trace-library can
//! derive what the callee does. The package and version come from the location's place in a
//! prepared library root (`LibraryRoot::layout`: site-packages, node_modules, the Go module
//! cache, the Cargo registry, ...) and the package metadata next to it (dist-info,
//! `package.json`, `DESCRIPTION`, `installed.json`), read structurally; server-specific virtual
//! documents go through `Server::external_location`. Nothing is executed.
//!
//! Declarations the language server bundles itself (the install directory of the backend's
//! server, `Prepared::vars[SERVER_DIR_VAR]`: Intelephense's PHP stubs, a server's
//! standard-library definition files) are the language's standard library as the server
//! sees it: package `<language>-stdlib`, declarations only (not readable: stubs have no
//! bodies to derive from). Standard-library layouts `locate` recognises and stub
//! distributions naming their package (typeshed) keep their classification.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use trace_core::semantics::LibraryFile;
use trace_core::Language;
use trace_env::{LibraryKind, LibraryRoot};

use crate::languages::{Prepared, Server};
use crate::mapping::{parse_uri, UriTarget};

/// `Prepared::vars` key of the install directory of the backend's language server (set by
/// the preflight; absent when the server is not installed by trace).
pub const SERVER_DIR_VAR: &str = "server_dir";

/// What `classify` may consult.
pub struct ExternalContext<'a> {
    pub prepared: &'a Prepared,
    pub hooks: &'a dyn Server,
}

/// Declaration position of the callee inside the library file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemLibraryCallTarget {
    pub decl_line: u32,
    pub decl_column: u32,
    pub symbol: Option<String>,
}

/// Package identity of a library location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageLocation {
    pub package: String,
    pub version: Option<String>,
    pub stdlib: bool,
}

/// The library file and declaration a server location points to (`None` for locations that
/// are in no library root and no known library layout).
pub fn classify(
    uri: &str,
    line: u32,
    character: u32,
    cx: &ExternalContext<'_>,
) -> Option<(LibraryFile, SemLibraryCallTarget)> {
    if let Some(loc) = cx.hooks.external_location(uri, cx.prepared) {
        let language = trace_core::languages::from_path(Path::new(&loc.path))
            .or_else(|| cx.prepared.languages.first().copied())?;
        return Some((
            LibraryFile {
                path: loc.path,
                package: loc.package,
                version: loc.version,
                stdlib: loc.stdlib,
                readable: loc.readable,
                language,
            },
            SemLibraryCallTarget {
                decl_line: loc.line,
                decl_column: loc.column,
                symbol: loc.symbol,
            },
        ));
    }
    let path = match parse_uri(uri)? {
        UriTarget::File(p) => p,
        UriTarget::Virtual(_) => return None,
    };
    let location = locate(&path, &cx.prepared.library_roots);
    let language =
        trace_core::languages::from_path(&path).or_else(|| cx.prepared.languages.first().copied())?;
    let target = SemLibraryCallTarget {
        decl_line: line,
        decl_column: character,
        symbol: None,
    };
    if server_bundled(&path, cx.prepared) && !location.as_ref().is_some_and(|l| l.stdlib) {
        return Some((
            LibraryFile {
                path: path.to_string_lossy().into_owned(),
                package: format!("{}-stdlib", language.as_str()),
                version: None,
                stdlib: true,
                readable: false,
                language,
            },
            target,
        ));
    }
    let location = location?;
    Some((
        LibraryFile {
            path: path.to_string_lossy().into_owned(),
            package: location.package,
            version: location.version,
            stdlib: location.stdlib,
            readable: readable(&path, language),
            language,
        },
        target,
    ))
}

/// A file inside the install directory of the backend's language server
/// ([`SERVER_DIR_VAR`]) that is not part of a stub distribution naming its package
/// (typeshed `stubs/<distribution>`).
fn server_bundled(path: &Path, prepared: &Prepared) -> bool {
    let Some(dir) = prepared.vars.get(SERVER_DIR_VAR).filter(|d| !d.is_empty()) else {
        return false;
    };
    relative(Path::new(dir), path).is_some() && !components(path).iter().any(|c| c.starts_with("typeshed"))
}

/// Source text trace-library can derive from: a source file of a language with a derivation
/// adapter, or a stub whose implementation is installed (`.pyi` -> `.py` next to it; a
/// `.d.ts` / `.d.mts` / `.d.cts` -> its package's JavaScript found the way trace-library
/// derives it: sibling file, manifest entry mirror, `@types` package) or is a typeshed stub
/// (mapped to the interpreter's module by trace-library).
pub fn readable(path: &Path, language: Language) -> bool {
    let Some(spec) = trace_library::languages::adapter(language) else {
        return false;
    };
    if !path.is_file() {
        return false;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(stem) = name.strip_suffix(".pyi") {
        let typeshed = components(path).iter().any(|c| c.starts_with("typeshed"));
        return typeshed || path.with_file_name(format!("{stem}.py")).is_file();
    }
    if [".d.ts", ".d.mts", ".d.cts"].iter().any(|s| name.ends_with(s)) {
        return trace_library::languages::javascript::implementation_of_declaration(path).is_some();
    }
    trace_library::languages::has_extension(path, spec.extensions)
}

fn components(path: &Path) -> Vec<String> {
    path.components()
        .filter(|c| !matches!(c, Component::RootDir | Component::Prefix(_)))
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect()
}

/// Component-wise prefix test (case-insensitive on Windows).
fn relative(root: &Path, path: &Path) -> Option<Vec<String>> {
    let r = components(root);
    let p = components(path);
    if p.len() <= r.len() {
        return None;
    }
    let same = |a: &str, b: &str| {
        if cfg!(windows) {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };
    r.iter()
        .zip(&p)
        .all(|(a, b)| same(a, b))
        .then(|| p[r.len()..].to_vec())
}

/// `name-1.2.3` -> (`name`, `1.2.3`) at the first `-` followed by a digit.
fn split_version(entry: &str) -> (String, Option<String>) {
    let bytes = entry.as_bytes();
    for (i, w) in bytes.windows(2).enumerate() {
        if w[0] == b'-' && w[1].is_ascii_digit() {
            return (entry[..i].to_string(), Some(entry[i + 1..].to_string()));
        }
    }
    (entry.to_string(), None)
}

fn go_unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut upper = false;
    for c in text.chars() {
        if c == '!' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn json_field(file: &Path, field: &str) -> Option<String> {
    let bytes = std::fs::read(file).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value.get(field)?.as_str().map(str::to_string)
}

/// `Version:` of an R package `DESCRIPTION` (Debian control format: `Key: value` lines).
fn dcf_field(file: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    text.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        (k.trim() == key).then(|| v.trim().to_string())
    })
}

/// Normalised distribution / import name (PEP 503).
fn pep503(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            if !dash {
                out.push('-');
            }
            dash = true;
        } else {
            out.push(c.to_ascii_lowercase());
            dash = false;
        }
    }
    out
}

/// site-packages: top-level import name -> (distribution, version), from `*.dist-info`
/// directory names and their `top_level.txt` (cached per root).
fn python_distributions(root: &Path) -> HashMap<String, (String, String)> {
    type Memo = Mutex<HashMap<PathBuf, HashMap<String, (String, String)>>>;
    static MEMO: OnceLock<Memo> = OnceLock::new();
    let memo = MEMO.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(found) = memo.lock().ok().and_then(|m| m.get(root).cloned()) {
        return found;
    }
    let mut map = HashMap::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        dirs.sort();
        for dir in dirs {
            let Some(name) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            let Some(stem) = name
                .strip_suffix(".dist-info")
                .or_else(|| name.strip_suffix(".egg-info"))
            else {
                continue;
            };
            let (dist, version) = split_version(stem);
            let version = version.unwrap_or_default();
            map.entry(pep503(&dist))
                .or_insert_with(|| (dist.clone(), version.clone()));
            if let Ok(top) = std::fs::read_to_string(dir.join("top_level.txt")) {
                for module in top.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    map.entry(pep503(module))
                        .or_insert_with(|| (dist.clone(), version.clone()));
                }
            }
        }
    }
    if let Ok(mut m) = memo.lock() {
        m.insert(root.to_path_buf(), map.clone());
    }
    map
}

/// vendor/composer/installed.json: package -> version (cached per vendor dir).
fn composer_versions(vendor: &Path) -> HashMap<String, String> {
    static MEMO: OnceLock<Mutex<HashMap<PathBuf, HashMap<String, String>>>> = OnceLock::new();
    let memo = MEMO.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(found) = memo.lock().ok().and_then(|m| m.get(vendor).cloned()) {
        return found;
    }
    let mut map = HashMap::new();
    if let Some(json) = std::fs::read(vendor.join("composer").join("installed.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
    {
        let packages = json
            .get("packages")
            .and_then(|p| p.as_array())
            .cloned()
            .or_else(|| json.as_array().cloned())
            .unwrap_or_default();
        for p in packages {
            if let (Some(n), Some(v)) =
                (p.get("name").and_then(|x| x.as_str()), p.get("version").and_then(|x| x.as_str()))
            {
                map.insert(n.to_string(), v.to_string());
            }
        }
    }
    if let Ok(mut m) = memo.lock() {
        m.insert(vendor.to_path_buf(), map.clone());
    }
    map
}

/// Package identity of `path` inside `root` with layout `layout`.
fn from_layout(
    root: &Path,
    layout: &str,
    rel: &[String],
    stdlib: bool,
    root_version: Option<&str>,
) -> Option<PackageLocation> {
    let dependency = |package: String, version: Option<String>| {
        Some(PackageLocation {
            package,
            version,
            stdlib,
        })
    };
    match layout {
        "site_packages" => {
            let first = rel.first()?;
            let top = first.strip_suffix(".py").unwrap_or(first);
            match python_distributions(root).get(&pep503(top)) {
                Some((dist, version)) => {
                    dependency(dist.clone(), Some(version.clone()).filter(|v| !v.is_empty()))
                }
                None => dependency(top.to_string(), None),
            }
        }
        "node_modules" => {
            let first = rel.first()?;
            let (package, dir) = if first.starts_with('@') {
                let second = rel.get(1)?;
                (format!("{first}/{second}"), root.join(first).join(second))
            } else {
                (first.clone(), root.join(first))
            };
            let version = json_field(&dir.join("package.json"), "version");
            dependency(package, version)
        }
        "go_modcache" => {
            let at = rel.iter().position(|c| c.contains('@'))?;
            let (last, version) = rel[at].split_once('@')?;
            let mut parts: Vec<String> = rel[..at].to_vec();
            parts.push(last.to_string());
            dependency(go_unescape(&parts.join("/")), Some(version.to_string()))
        }
        // `<index>/<crate>-<version>/...`: the crate directory is the one with a Cargo.toml.
        "cargo_registry" => {
            let mut dir = root.to_path_buf();
            rel.iter().take(rel.len().saturating_sub(1)).find_map(|c| {
                dir.push(c);
                if !dir.join("Cargo.toml").is_file() {
                    return None;
                }
                let (name, version) = split_version(c);
                Some(PackageLocation {
                    package: name,
                    version,
                    stdlib,
                })
            })
        }
        "r_library" => {
            let package = rel.first()?.clone();
            let version = dcf_field(&root.join(&package).join("DESCRIPTION"), "Version");
            dependency(package, version)
        }
        "nuget_packages" => dependency(rel.first()?.clone(), rel.get(1).cloned()),
        "maven_repo" | "coursier_cache" => {
            let start = rel.iter().position(|c| c == "maven2").map(|i| i + 1).unwrap_or(0);
            let parts = &rel[start..];
            // <group...>/<artifact>/<version>/<file>
            if parts.len() < 4 {
                return None;
            }
            let n = parts.len();
            let group = parts[..n - 3].join(".");
            dependency(format!("{group}:{}", parts[n - 3]), Some(parts[n - 2].clone()))
        }
        "gradle_cache" => {
            let start = rel.iter().position(|c| c == "files-2.1").map(|i| i + 1).unwrap_or(0);
            let parts = &rel[start..];
            if parts.len() < 3 {
                return None;
            }
            dependency(format!("{}:{}", parts[0], parts[1]), Some(parts[2].clone()))
        }
        "cabal_store" => {
            let (name, rest) = split_version(rel.get(1)?);
            dependency(name, rest.map(|r| r.split('-').next().unwrap_or(&r).to_string()))
        }
        "php_vendor" => {
            let package = format!("{}/{}", rel.first()?, rel.get(1)?);
            let version = composer_versions(root).get(&package).cloned();
            dependency(package, version)
        }
        "rust_src" => Some(PackageLocation {
            package: rel.first()?.clone(),
            version: root_version.map(str::to_string),
            stdlib: true,
        }),
        _ => None,
    }
}

/// Package identity of a library location: the deepest prepared root containing it, else a
/// well-known layout recognised from the path itself (the same layouts, anchored at their
/// marker directory: `site-packages`, `node_modules`, `pkg/mod`, `registry/src`, `vendor`, typeshed and TypeScript standard libraries, `rustlib`).
pub fn locate(path: &Path, roots: &[LibraryRoot]) -> Option<PackageLocation> {
    let best = roots
        .iter()
        .filter_map(|r| relative(&r.path, path).map(|rel| (r, rel)))
        .max_by_key(|(r, _)| components(&r.path).len());
    if let Some((root, rel)) = best {
        let stdlib = root.kind == LibraryKind::Stdlib;
        if root.layout == "toolchain_stdlib" || (stdlib && root.layout != "rust_src") {
            let language = trace_core::languages::from_path(path)
                .map(|l| l.as_str())
                .unwrap_or("toolchain");
            return Some(PackageLocation {
                package: format!("{language}-stdlib"),
                version: root.version.clone(),
                stdlib: true,
            });
        }
        return from_layout(&root.path, root.layout, &rel, stdlib, root.version.as_deref());
    }
    let parts = components(path);
    // The directory made of the first `k` components of `parts` (prefix and root included).
    let anchor = |k: usize| -> PathBuf {
        let mut p = PathBuf::new();
        let mut n = 0;
        for c in path.components() {
            match c {
                Component::RootDir | Component::Prefix(_) => p.push(c.as_os_str()),
                _ => {
                    if n >= k {
                        break;
                    }
                    p.push(c.as_os_str());
                    n += 1;
                }
            }
        }
        p
    };
    if let Some(i) = parts.iter().rposition(|c| c == "node_modules") {
        if parts.get(i + 1).is_some_and(|c| c == "typescript") && parts.get(i + 2).is_some_and(|c| c == "lib")
        {
            return Some(PackageLocation {
                package: "javascript-stdlib".to_string(),
                version: json_field(&anchor(i + 2).join("package.json"), "version"),
                stdlib: true,
            });
        }
        return from_layout(&anchor(i + 1), "node_modules", &parts[i + 1..], false, None);
    }
    if let Some(i) = parts.iter().rposition(|c| c.starts_with("typeshed")) {
        let stdlib = parts.get(i + 1).is_some_and(|c| c == "stdlib");
        return Some(PackageLocation {
            package: if stdlib {
                "python-stdlib".to_string()
            } else {
                parts.get(i + 2).cloned().unwrap_or_else(|| "typeshed".to_string())
            },
            version: None,
            stdlib,
        });
    }
    if let Some(i) = parts
        .iter()
        .rposition(|c| c == "site-packages" || c == "dist-packages")
    {
        return from_layout(&anchor(i + 1), "site_packages", &parts[i + 1..], false, None);
    }
    if let Some(i) = parts.windows(2).rposition(|w| w[0] == "pkg" && w[1] == "mod") {
        return from_layout(&anchor(i + 2), "go_modcache", &parts[i + 2..], false, None);
    }
    if let Some(i) = parts.windows(2).rposition(|w| w[0] == "registry" && w[1] == "src") {
        return from_layout(&anchor(i + 2), "cargo_registry", &parts[i + 2..], false, None);
    }
    if let Some(i) = parts.iter().rposition(|c| c == "rustlib") {
        let library = parts.iter().skip(i).position(|c| c == "library").map(|j| i + j)?;
        return from_layout(&anchor(library + 1), "rust_src", &parts[library + 1..], true, None);
    }
    if let Some(i) = parts.iter().rposition(|c| c == "vendor") {
        let vendor = anchor(i + 1);
        if vendor.join("composer").is_dir() {
            return from_layout(&vendor, "php_vendor", &parts[i + 1..], false, None);
        }
    }
    None
}

#[cfg(test)]
#[path = "../tests/unit/external.rs"]
mod tests;
