//! Python adapter: site-packages and the interpreter's `Lib/` (`.pyi` stubs map to their
//! implementation by qualified name, see `Library`). Self slots come from `ImplicitSelf`
//! (`self` / `cls`); `__call__` makes an object a wrapper, `__get__` a property.

use std::path::{Path, PathBuf};

use trace_core::facts::ImportKind;
use trace_core::Language;

use super::{first_file, AdapterSpec, LibrarySpec, BASE};

pub(crate) static SPEC: LibrarySpec = LibrarySpec {
    languages: &[Language::Python],
    table: include_str!("../../../../assets/library/python.json"),
    adapter: Some(AdapterSpec {
        extensions: &["py"],
        keyword_args: true,
        store_methods: &[
            "append",
            "appendleft",
            "add",
            "insert",
            "setdefault",
            "put",
            "put_nowait",
            "__setitem__",
        ],
        read_methods: &[
            "[]",
            "pop",
            "popleft",
            "popitem",
            "get",
            "get_nowait",
            "setdefault",
            "copy",
            "values",
        ],
        identity_functions: &[
            ("enumerate", 0),
            ("reversed", 0),
            ("sorted", 0),
            ("list", 0),
            ("tuple", 0),
            ("iter", 0),
            ("zip", 0),
            // `typing.cast(T, value)` returns `value` (a static-typing marker).
            ("cast", 1),
            ("typing.cast", 1),
        ],
        constructors: &["__init__", "__new__"],
        call_method: Some("__call__"),
        get_method: Some("__get__"),
        member_hook: Some("__getattr__"),
        super_call: Some("super"),
        stdlib_index: true,
        module_name,
        resolve_import,
        ..BASE
    }),
};

/// Directory of the package root: the first ancestor of `path` without `__init__.py`.
fn package_root(path: &Path) -> Option<PathBuf> {
    let mut dir = path.parent()?;
    while dir.join("__init__.py").is_file() || dir.join("__init__.pyi").is_file() {
        dir = dir.parent()?;
    }
    Some(dir.to_path_buf())
}

/// Dotted module name (`asyncio.events`, `functools`; `pkg/__init__.py` -> `pkg`).
fn module_name(path: &Path, _roots: &[PathBuf]) -> Option<String> {
    let root = package_root(path)?;
    let rel = path.strip_prefix(&root).ok()?;
    let mut parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let last = parts.pop()?;
    let stem = last.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or(last);
    if stem != "__init__" {
        parts.push(stem);
    }
    (!parts.is_empty()).then(|| parts.join("."))
}

/// `import a.b` / `from .x import y` / `from a.b import c`: the module file and member.
/// Imports of a stub file (`.pyi`) resolve to stub modules first (a stub tree is typed
/// against itself), then to source modules.
fn resolve_import(
    from: &Path,
    target: &str,
    _kind: ImportKind,
    roots: &[PathBuf],
) -> Option<(PathBuf, Option<String>)> {
    let dots = target.chars().take_while(|c| *c == '.').count();
    let rest = &target[dots..];
    let parts: Vec<&str> = rest.split('.').filter(|p| !p.is_empty()).collect();
    let stub = from.extension().is_some_and(|x| x == "pyi");
    if dots > 0 {
        let mut base = from.parent()?.to_path_buf();
        for _ in 1..dots {
            base = base.parent()?.to_path_buf();
        }
        return locate(&base, &parts, stub);
    }
    let mut bases: Vec<PathBuf> = Vec::new();
    if let Some(root) = package_root(from) {
        bases.push(root);
    }
    bases.extend(roots.iter().cloned());
    bases.into_iter().find_map(|b| locate(&b, &parts, stub))
}

/// Longest prefix of `parts` below `base` that is a module; the next part is the member.
/// `stub`: stub modules (`.pyi`, `__init__.pyi`) come before source modules.
fn locate(base: &Path, parts: &[&str], stub: bool) -> Option<(PathBuf, Option<String>)> {
    let extensions: &[&str] = if stub { &["pyi", "py"] } else { &["py"] };
    if parts.is_empty() {
        return first_file(extensions.iter().map(|x| base.join(format!("__init__.{x}")))).map(|p| (p, None));
    }
    for k in (1..=parts.len()).rev() {
        let mut dir = base.to_path_buf();
        for p in &parts[..k] {
            dir.push(p);
        }
        let candidates = extensions.iter().flat_map(|x| {
            let mut file = dir.clone();
            file.set_extension(x);
            [file, dir.join(format!("__init__.{x}"))]
        });
        if let Some(found) = first_file(candidates) {
            return Some((found, parts.get(k).map(|m| m.to_string())));
        }
    }
    None
}

#[cfg(test)]
#[path = "../../tests/unit/languages/python.rs"]
mod tests;
