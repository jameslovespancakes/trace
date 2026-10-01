//! Rust adapter: `rust-src` (`library/<crate>/src`) and the Cargo registry. `self` slots come
//! from `ImplicitSelf`; closures called with `f()` / `f.call(..)`; iterator adapters call
//! their closure with every element; `for x in xs` iterates. Cross-module paths stay within
//! the file (derivation never guesses a module by name).

use std::path::{Path, PathBuf};

use trace_core::Language;

use super::{components, AdapterSpec, LibrarySpec, BASE};

pub(crate) static SPEC: LibrarySpec = LibrarySpec {
    languages: &[Language::Rust],
    table: include_str!("../../../../assets/library/rust.json"),
    adapter: Some(AdapterSpec {
        extensions: &["rs"],
        store_methods: &["push", "push_back", "push_front", "insert"],
        read_methods: &[
            "[]",
            "get",
            "get_mut",
            "pop",
            "pop_front",
            "pop_back",
            "remove",
            "iter",
            "iter_mut",
            "values",
            "take",
        ],
        invoke_methods: &["call", "call_mut", "call_once"],
        element_methods: &[
            "for_each",
            "map",
            "filter",
            "filter_map",
            "flat_map",
            "any",
            "all",
            "find",
            "fold",
            "retain",
        ],
        constructors: &["new"],
        symbol_separator: "::",
        module_name,
        ..BASE
    }),
};

/// `<crate>::<module path>` from the file's position below the crate's `src/` directory
/// (`lib.rs`, `main.rs` and `mod.rs` name their directory's module). The crate name is the
/// directory above `src` without a registry version suffix.
fn module_name(path: &Path, _roots: &[PathBuf]) -> Option<String> {
    let parts = components(path);
    let src = parts.iter().rposition(|c| c == "src")?;
    let crate_dir = parts.get(src.checked_sub(1)?)?;
    let crate_name = strip_version(crate_dir).replace('-', "_");
    let mut out = vec![crate_name];
    let inner = &parts[src + 1..];
    for (i, p) in inner.iter().enumerate() {
        if i + 1 == inner.len() {
            let stem = p.strip_suffix(".rs").unwrap_or(p);
            if !matches!(stem, "lib" | "main" | "mod") {
                out.push(stem.to_string());
            }
        } else {
            out.push(p.clone());
        }
    }
    Some(out.join("::"))
}

/// `serde-1.0.200` -> `serde` (a `-` followed by a digit starts the version).
pub(crate) fn strip_version(dir: &str) -> &str {
    let bytes = dir.as_bytes();
    for (i, w) in bytes.windows(2).enumerate() {
        if w[0] == b'-' && w[1].is_ascii_digit() {
            return &dir[..i];
        }
    }
    dir
}
