//! C library rules: the table only (no derivation adapter).

use trace_core::Language;

use super::LibrarySpec;

pub(crate) static SPEC: LibrarySpec = LibrarySpec {
    languages: &[Language::C],
    table: include_str!("../../../../assets/library/c.json"),
    adapter: None,
};
