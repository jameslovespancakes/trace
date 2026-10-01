//! C# library rules: the table only (no derivation adapter; installed assemblies are read for
//! reflection metadata by `crate::reflect`).

use trace_core::Language;

use super::LibrarySpec;

pub(crate) static SPEC: LibrarySpec = LibrarySpec {
    languages: &[Language::CSharp],
    table: include_str!("../../../../assets/library/csharp.json"),
    adapter: None,
};
