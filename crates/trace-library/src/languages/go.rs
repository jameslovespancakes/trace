//! Go adapter: `GOROOT/src` and the module cache. One package = all non-test files of a
//! directory (one namespace); methods attach to their receiver type; `append(s, x)` holds
//! its arguments; `for _, x := range s` iterates.

use std::path::{Path, PathBuf};

use trace_core::facts::ImportKind;
use trace_core::Language;

use crate::derive::typed::{AmbiguousIndexCall, TypeSyntax};

use super::{components, relative_to_roots, AdapterSpec, LibrarySpec, BASE};

pub(crate) static SPEC: LibrarySpec = LibrarySpec {
    languages: &[Language::Go],
    table: include_str!("../../../../assets/library/go.json"),
    adapter: Some(AdapterSpec {
        extensions: &["go"],
        read_methods: &["[]"],
        container_functions: &["append"],
        namespace_group: true,
        module_name,
        resolve_import,
        namespace_member,
        typed: Some(&TYPED),
        ..BASE
    }),
};

/// Type declaration syntax of typed dispatch.
const TYPED: TypeSyntax = TypeSyntax {
    definitions: &["type_spec", "type_alias"],
    function_types: &["function_type"],
    parameters: &["parameter_declaration", "variadic_parameter_declaration"],
    sequences: &[("slice_type", "element"), ("array_type", "element")],
    maps: &[("map_type", "value")],
    methods: &["method_declaration"],
    functions: &["function_declaration"],
    interface_methods: &["method_elem"],
    imports: &["import_spec"],
    string_types: &["string"],
    ambiguous_index_calls: &[AmbiguousIndexCall {
        kind: "type_conversion_expression",
        type_field: "type",
        generic: "generic_type",
        base: "type",
        qualified: "qualified_type",
        qualifier: "package",
        name: "name",
    }],
    exported_capitalized: true,
};

/// `GOOS` values (Go specification of file name build constraints, `go tool dist list`).
const KNOWN_OS: [&str; 18] = [
    "aix",
    "android",
    "darwin",
    "dragonfly",
    "freebsd",
    "hurd",
    "illumos",
    "ios",
    "js",
    "linux",
    "nacl",
    "netbsd",
    "openbsd",
    "plan9",
    "solaris",
    "wasip1",
    "windows",
    "zos",
];

/// `GOARCH` values (same source).
const KNOWN_ARCH: [&str; 24] = [
    "386",
    "amd64",
    "amd64p32",
    "arm",
    "armbe",
    "arm64",
    "arm64be",
    "loong64",
    "mips",
    "mipsle",
    "mips64",
    "mips64le",
    "mips64p32",
    "mips64p32le",
    "ppc",
    "ppc64",
    "ppc64le",
    "riscv",
    "riscv64",
    "s390",
    "s390x",
    "sparc",
    "sparc64",
    "wasm",
];

/// The host's `GOOS` / `GOARCH`.
fn host() -> (&'static str, &'static str) {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "x86" => "386",
        "aarch64" => "arm64",
        "powerpc64" => "ppc64",
        other => other,
    };
    (os, arch)
}

fn os_matches(want: &str, host_os: &str) -> bool {
    want == host_os
        || (want == "linux" && host_os == "android")
        || (want == "solaris" && host_os == "illumos")
        || (want == "darwin" && host_os == "ios")
}

/// Whether the toolchain builds the file named `name` on this host: a `_GOOS`, `_GOARCH` or
/// `_GOOS_GOARCH` suffix (after removing `.go` and `_test`) restricts the file to that
/// platform (Go file name build constraints). Only the host's platform files belong to the
/// package a call resolves into.
fn built_on_host(name: &str) -> bool {
    let stem = name.strip_suffix(".go").unwrap_or(name);
    let stem = stem.strip_suffix("_test").unwrap_or(stem);
    let parts: Vec<&str> = stem.split('_').collect();
    let (host_os, host_arch) = host();
    let n = parts.len();
    if n >= 3 && KNOWN_OS.contains(&parts[n - 2]) && KNOWN_ARCH.contains(&parts[n - 1]) {
        return os_matches(parts[n - 2], host_os) && parts[n - 1] == host_arch;
    }
    if n >= 2 {
        let last = parts[n - 1];
        if KNOWN_OS.contains(&last) {
            return os_matches(last, host_os);
        }
        if KNOWN_ARCH.contains(&last) {
            return last == host_arch;
        }
    }
    true
}

/// Test files and files built only for another platform (file name constraints) are not
/// part of the package a library call resolves into.
fn namespace_member(name: &str) -> bool {
    !name.ends_with("_test.go") && built_on_host(name)
}

/// Import path of the file's package: relative to `GOROOT/src` or the module cache (the
/// `@version` suffix removed), else the directory name.
fn module_name(path: &Path, roots: &[PathBuf]) -> Option<String> {
    let dir = path.parent()?;
    let parts: Vec<String> = match relative_to_roots(dir, roots) {
        Some((rel, _)) => components(rel),
        None => {
            let all = components(dir);
            match all.iter().rposition(|c| c == "src") {
                Some(i) => all[i + 1..].to_vec(),
                None => vec![all.last()?.clone()],
            }
        }
    };
    let cleaned: Vec<String> = parts
        .into_iter()
        .map(|p| p.split('@').next().unwrap_or(&p).to_string())
        .filter(|p| !p.is_empty())
        .collect();
    (!cleaned.is_empty()).then(|| cleaned.join("/"))
}

/// `import "net/http"`: the package directory below a root (`GOROOT/src/net/http`), first
/// Go file (the loader adds the package's other files).
fn resolve_import(
    _from: &Path,
    target: &str,
    _kind: ImportKind,
    roots: &[PathBuf],
) -> Option<(PathBuf, Option<String>)> {
    for root in roots {
        let dir = root.join(target);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension().is_some_and(|e| e == "go")
                    && !p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with("_test.go"))
            })
            .collect();
        files.sort();
        if let Some(first) = files.into_iter().next() {
            return Some((first, None));
        }
    }
    None
}

#[cfg(test)]
#[path = "../../tests/unit/languages/go.rs"]
mod tests;
