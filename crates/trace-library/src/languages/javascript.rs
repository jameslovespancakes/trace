//! JavaScript / TypeScript adapter: `.js` / `.mjs` / `.cjs` in `node_modules` (a declaration
//! file maps to its implementation, [`implementation_of_declaration`]). `this` slots come
//! from `ImplicitSelf`; `f.call(..)` / `f.apply(..)` call `f`; array iteration methods call
//! their callback with every element; the engine's member-name functions (`Object.keys`,
//! `Object.getOwnPropertyNames`, `Reflect.ownKeys`) and member definitions
//! (`Object.defineProperty`, `Reflect.defineProperty`) are language built-ins the member-copy
//! rule reads.

use std::path::{Path, PathBuf};

use trace_core::facts::ImportKind;
use trace_core::Language;

use super::objects::ObjectModel;
use super::{components, first_file, AdapterSpec, LibrarySpec, BASE};

pub(crate) static SPEC: LibrarySpec = LibrarySpec {
    languages: &[Language::JavaScript, Language::TypeScript, Language::Tsx],
    table: include_str!("../../../../assets/library/javascript.json"),
    adapter: Some(AdapterSpec {
        extensions: &["js", "mjs", "cjs"],
        store_methods: &["push", "unshift", "set", "add"],
        read_methods: &["[]", "get", "pop", "shift", "at", "find", "values"],
        invoke_methods: &["call", "apply"],
        identity_functions: &[("Array.from", 0), ("Object.values", 0)],
        element_methods: &[
            "forEach",
            "map",
            "filter",
            "some",
            "every",
            "reduce",
            "find",
            "findIndex",
            "flatMap",
        ],
        constructors: &["constructor"],
        key_functions: &[("Object.keys", 0), ("Object.getOwnPropertyNames", 0), ("Reflect.ownKeys", 0)],
        define_functions: &[("Object.defineProperty", 0, 1), ("Reflect.defineProperty", 0, 1)],
        objects: Some(ObjectModel {
            receiver: "this",
            arguments: "arguments",
            prototype: "prototype",
            prototype_link: "__proto__",
            call_with_receiver: "call",
            apply_with_receiver: "apply",
            bind_receiver: "bind",
            slice_methods: &["slice"],
            mapping_methods: &["map", "flatMap"],
            lower_case_methods: &["toLowerCase", "toLocaleLowerCase"],
            upper_case_methods: &["toUpperCase", "toLocaleUpperCase"],
            prototype_setters: &[("Object.setPrototypeOf", 0, 1), ("Reflect.setPrototypeOf", 0, 1)],
            prototype_creators: &[("Object.create", 0)],
            exports: "exports",
            module: "module",
            require: "require",
        }),
        module_name,
        resolve_import,
        source_of_location,
        package_entry,
        ..BASE
    }),
};

/// Declaration-file suffixes (`.d.ts`, `.d.mts`, `.d.cts`).
const DECLARATION_SUFFIXES: [&str; 3] = [".d.ts", ".d.mts", ".d.cts"];
/// Implementation extensions tried for a declaration file.
const IMPLEMENTATION_EXTENSIONS: [&str; 3] = [".js", ".mjs", ".cjs"];
/// Condition keys of a package manifest whose values are implementation files.
const IMPLEMENTATION_CONDITIONS: [&str; 6] = ["import", "require", "default", "node", "module", "main"];

/// `x.d.ts` -> `x` (None for other files).
fn declaration_stem(name: &str) -> Option<&str> {
    DECLARATION_SUFFIXES.iter().find_map(|s| name.strip_suffix(s))
}

/// The installed package directory holding `path` (`node_modules/<name>` or
/// `node_modules/@scope/<name>`) and the package name.
fn package_dir(path: &Path) -> Option<(PathBuf, String)> {
    let mut dir = path.parent();
    while let Some(d) = dir {
        let parent = d.parent()?;
        let parent_name = parent
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = d.file_name()?.to_string_lossy().into_owned();
        if parent_name == "node_modules" && !name.starts_with('@') {
            return Some((d.to_path_buf(), name));
        }
        if parent_name.starts_with('@')
            && parent
                .parent()
                .and_then(|p| p.file_name())
                .is_some_and(|n| n == "node_modules")
        {
            return Some((d.to_path_buf(), format!("{parent_name}/{name}")));
        }
        dir = d.parent();
    }
    None
}

/// `a/b` relative path text with `/` separators, without a leading `./`.
fn clean_rel(text: &str) -> String {
    text.trim_start_matches("./").replace('\\', "/")
}

/// (declaration entry, implementation entry) pairs of a package manifest: top-level
/// `types` / `typings` with `module` / `main`, and every `exports` condition object that
/// names `types` next to implementation conditions (nested conditions and subpaths too).
fn manifest_pairs(manifest: &serde_json::Value) -> Vec<(String, String)> {
    fn walk(value: &serde_json::Value, depth: usize, out: &mut Vec<(String, String)>) {
        let Some(obj) = value.as_object() else { return };
        if depth > 6 {
            return;
        }
        if let Some(types) = obj
            .get("types")
            .or_else(|| obj.get("typings"))
            .and_then(|t| t.as_str())
        {
            for key in IMPLEMENTATION_CONDITIONS {
                if let Some(implementation) = obj.get(key).and_then(|v| v.as_str()) {
                    out.push((clean_rel(types), clean_rel(implementation)));
                }
            }
        }
        for (key, child) in obj {
            if key.starts_with('.') || IMPLEMENTATION_CONDITIONS.contains(&key.as_str()) || key == "exports" {
                walk(child, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(manifest, 0, &mut out);
    out.sort();
    out.dedup();
    out
}

fn read_manifest(dir: &Path) -> Option<serde_json::Value> {
    let bytes = std::fs::read(dir.join("package.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// `<base><ext>` for the implementation extensions.
fn with_implementation_extension(base: &Path) -> Option<PathBuf> {
    first_file(IMPLEMENTATION_EXTENSIONS.iter().map(|ext| {
        let mut s = base.as_os_str().to_os_string();
        s.push(ext);
        PathBuf::from(s)
    }))
}

/// The implementation of a declaration file of an installed package:
/// 1. the sibling `.js` / `.mjs` / `.cjs`;
/// 2. the package manifest's declaration entry mirrored onto its implementation entry
///    (`types: dist/types/index.d.ts` + `main: dist/index.js`: `dist/types/x/y.d.ts` is
///    `dist/x/y.js`);
/// 3. a DefinitelyTyped package `@types/<name>` (`@types/<scope>__<name>`) describes the
///    package `<name>` installed next to it: the same relative path there, `index` = its
///    manifest entry.
///
/// `None` for other files or when no implementation is installed.
pub fn implementation_of_declaration(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let stem = declaration_stem(&name)?.to_string();
    if let Some(sibling) = with_implementation_extension(&path.with_file_name(&stem)) {
        return Some(sibling);
    }
    let (package, package_name) = package_dir(path)?;
    let rel_dir = path.parent()?.strip_prefix(&package).ok()?.to_path_buf();
    let rel = clean_rel(&rel_dir.join(&stem).to_string_lossy());
    if let Some(described) = package_name.strip_prefix("@types/") {
        let target_name = match described.split_once("__") {
            Some((scope, inner)) => format!("@{scope}/{inner}"),
            None => described.to_string(),
        };
        // `node_modules/@types/<x>` -> `node_modules/<target>`.
        let modules = package.parent()?.parent()?;
        let target = modules.join(&target_name);
        if !target.is_dir() {
            return None;
        }
        if let Some(found) = with_implementation_extension(&target.join(&rel)) {
            return Some(found);
        }
        if rel == "index" {
            let manifest = read_manifest(&target);
            let main = manifest
                .as_ref()
                .and_then(|m| m.get("main").or_else(|| m.get("module")))
                .and_then(|m| m.as_str())
                .map(clean_rel)
                .unwrap_or_else(|| "index.js".to_string());
            return file_candidates(&target.join(main));
        }
        return None;
    }
    let manifest = read_manifest(&package)?;
    for (types, implementation) in manifest_pairs(&manifest) {
        let types_dir = types.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let impl_dir = implementation.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let inner = if types_dir.is_empty() {
            Some(rel.as_str())
        } else {
            rel.strip_prefix(types_dir).and_then(|r| r.strip_prefix('/'))
        };
        let Some(inner) = inner else { continue };
        let base = if impl_dir.is_empty() {
            package.join(inner)
        } else {
            package.join(impl_dir).join(inner)
        };
        if let Some(found) = with_implementation_extension(&base) {
            return Some(found);
        }
    }
    None
}

/// A declaration file's implementation ([`implementation_of_declaration`]).
fn source_of_location(location: &str, _roots: &[PathBuf]) -> Option<PathBuf> {
    implementation_of_declaration(Path::new(location))
}

/// `<package>/<path without extension>` for files below `node_modules`, else the file stem.
fn module_name(path: &Path, _roots: &[PathBuf]) -> Option<String> {
    let parts = components(path);
    let idx = parts.iter().rposition(|c| c == "node_modules");
    let rel: Vec<String> = match idx {
        Some(i) => parts[i + 1..].to_vec(),
        None => vec![parts.last()?.clone()],
    };
    let mut rel = rel;
    let last = rel.pop()?;
    let stem = last.split('.').next().unwrap_or(&last).to_string();
    rel.push(stem);
    Some(rel.join("/"))
}

/// The entry module of the installed package holding `path` (its manifest's `main`, else
/// `index.js`): the module the repository's bare import of the package loads.
fn package_entry(path: &Path) -> Option<PathBuf> {
    let (package, _) = package_dir(path)?;
    let main = read_manifest(&package)
        .and_then(|m| m.get("main").and_then(|v| v.as_str()).map(clean_rel))
        .unwrap_or_else(|| "index.js".to_string());
    file_candidates(&package.join(main))
}

/// ES / CommonJS imports: `<specifier>.<export>` (member) or `<specifier>` (module).
fn resolve_import(
    from: &Path,
    target: &str,
    kind: ImportKind,
    _roots: &[PathBuf],
) -> Option<(PathBuf, Option<String>)> {
    let (specifier, member) = match kind {
        ImportKind::Member => {
            let (s, m) = target.rsplit_once('.')?;
            (s, (m != "default").then(|| m.to_string()))
        }
        ImportKind::Module | ImportKind::Wildcard => (target, None),
    };
    let file = if specifier.starts_with("./") || specifier.starts_with("../") {
        file_candidates(&from.parent()?.join(specifier))
    } else {
        package_file(from, specifier)
    }?;
    Some((file, member))
}

/// `x`, `x.js`, `x.mjs`, `x.cjs`, `x/index.js`.
fn file_candidates(base: &Path) -> Option<PathBuf> {
    let with = |ext: &str| {
        let mut s = base.as_os_str().to_os_string();
        s.push(ext);
        PathBuf::from(s)
    };
    first_file([
        base.to_path_buf(),
        with(".js"),
        with(".mjs"),
        with(".cjs"),
        base.join("index.js"),
    ])
}

/// A bare specifier: the nearest `node_modules/<package>` above `from`; the package's
/// `main` entry (`package.json`) or a subpath.
fn package_file(from: &Path, specifier: &str) -> Option<PathBuf> {
    let mut segments = specifier.split('/');
    let first = segments.next()?;
    let package = if first.starts_with('@') {
        format!("{first}/{}", segments.next()?)
    } else {
        first.to_string()
    };
    let subpath: Vec<&str> = segments.collect();
    let mut dir = from.parent();
    while let Some(d) = dir {
        let candidate = d.join("node_modules").join(&package);
        if candidate.is_dir() {
            if !subpath.is_empty() {
                return file_candidates(&candidate.join(subpath.join("/")));
            }
            let main = std::fs::read(candidate.join("package.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .and_then(|v| v.get("main").and_then(|m| m.as_str()).map(str::to_string))
                .unwrap_or_else(|| "index.js".to_string());
            return file_candidates(&candidate.join(main.trim_start_matches("./")));
        }
        dir = d.parent();
    }
    None
}

#[cfg(test)]
#[path = "../../tests/unit/languages/javascript.rs"]
mod tests;
