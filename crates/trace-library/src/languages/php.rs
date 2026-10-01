//! PHP adapter: `vendor/` packages. `$this->x` slots come from `ImplicitSelf`;
//! `call_user_func($f)` / `call_user_func_array($f, ..)` call `$f`; `$f->__invoke()` calls
//! it; `array_push($a, $v)` stores `$v`. Namespaced classes resolve through the PSR-4 map
//! of `vendor/composer/installed.json` (JSON, read structurally).

use std::path::{Path, PathBuf};

use trace_core::facts::ImportKind;
use trace_core::Language;

use super::{first_file, AdapterSpec, LibrarySpec, BASE};

pub(crate) static SPEC: LibrarySpec = LibrarySpec {
    languages: &[Language::Php],
    table: include_str!("../../../../assets/library/php.json"),
    adapter: Some(AdapterSpec {
        extensions: &["php"],
        keyword_args: true,
        store_methods: &["push", "add", "set", "offsetSet", "attach"],
        read_methods: &["[]", "get", "offsetGet", "pop", "shift"],
        invoke_methods: &["__invoke", "call"],
        invoke_functions: &[("call_user_func", 0), ("call_user_func_array", 0)],
        identity_functions: &[("array_values", 0)],
        store_functions: &[("array_push", 0), ("array_unshift", 0)],
        constructors: &["__construct"],
        call_method: Some("__invoke"),
        symbol_separator: "\\",
        resolve_import,
        ..BASE
    }),
};

/// `use Vendor\Pkg\Class;`: the class file through the PSR-4 prefixes of the installed
/// packages; the member is the class name.
fn resolve_import(
    from: &Path,
    target: &str,
    _kind: ImportKind,
    _roots: &[PathBuf],
) -> Option<(PathBuf, Option<String>)> {
    let target = target.trim_start_matches('\\');
    let vendor = from
        .ancestors()
        .find(|a| a.file_name().is_some_and(|n| n == "vendor"))?;
    let installed = std::fs::read(vendor.join("composer").join("installed.json")).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&installed).ok()?;
    let packages = json
        .get("packages")
        .and_then(|p| p.as_array())
        .cloned()
        .or_else(|| json.as_array().cloned())?;
    let class = target.rsplit('\\').next()?.to_string();
    for package in packages {
        let Some(name) = package.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let install = package
            .get("install-path")
            .and_then(|p| p.as_str())
            .map(|p| vendor.join("composer").join(p))
            .unwrap_or_else(|| vendor.join(name));
        let Some(psr4) = package.pointer("/autoload/psr-4").and_then(|m| m.as_object()) else {
            continue;
        };
        for (prefix, dirs) in psr4 {
            let Some(rest) = target.strip_prefix(prefix.as_str()) else {
                continue;
            };
            let rel: PathBuf = rest.split('\\').collect::<PathBuf>().with_extension("php");
            let dirs: Vec<String> = match dirs {
                serde_json::Value::String(s) => vec![s.clone()],
                serde_json::Value::Array(a) => {
                    a.iter().filter_map(|d| d.as_str().map(str::to_string)).collect()
                }
                _ => Vec::new(),
            };
            if let Some(file) = first_file(dirs.iter().map(|d| install.join(d).join(&rel))) {
                return Some((file, Some(class)));
            }
        }
    }
    None
}
