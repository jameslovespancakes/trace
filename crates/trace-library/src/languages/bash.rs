//! Bash library rules: the table only (no derivation adapter).

use trace_core::Language;

use super::LibrarySpec;

pub(crate) static SPEC: LibrarySpec = LibrarySpec {
    languages: &[Language::Bash],
    table: include_str!("../../../../assets/library/bash.json"),
    adapter: None,
};
