//! R adapter: installed package code as the language server deparses it (one file per
//! function) or package sources (`R/` directories: one namespace). `do.call(f, args)` calls
//! `f`; `match.fun(f)` is `f`; `c(..)` / `list(..)` hold their arguments. Primitives and
//! `.Internal` leaves compose through the native table.

use std::path::{Path, PathBuf};

use trace_core::Language;

use super::{components, AdapterSpec, LibrarySpec, BASE};

pub(crate) static SPEC: LibrarySpec = LibrarySpec {
    languages: &[Language::R],
    table: include_str!("../../../../assets/library/r.json"),
    adapter: Some(AdapterSpec {
        extensions: &["R", "r"],
        keyword_args: true,
        read_methods: &["[]"],
        invoke_functions: &[("do.call", 0)],
        identity_functions: &[("match.fun", 0)],
        container_functions: &["c", "list"],
        symbol_separator: "::",
        namespace_group: true,
        module_name,
        ..BASE
    }),
};

/// The package name: the directory above `R/` (package sources) or above the deparsed
/// file's directory; `None` for loose files.
fn module_name(path: &Path, _roots: &[PathBuf]) -> Option<String> {
    let parts = components(path);
    let n = parts.len();
    if n >= 3 && parts[n - 2] == "R" {
        return Some(parts[n - 3].clone());
    }
    None
}
