//! trace-infer: edge-case inference.
//!
//! Data flow: `Index` (proven edges, unresolved, value refs, syntax facts)
//! -> [`hierarchy`] + [`flow`] (allocation- and receiver-sensitive value flow, anonymous
//! scopes, library behaviour from trace-library knowledge ([`behaviour`], every language),
//! by-name injection rules from the irreducible table, test provenance) ->
//! [`sites::generate_report`] (candidates, never edges; composition of dispatch after every
//! site kind; `Site::library` on callbacks into library calls)
//! -> [`decide`] (the decision rules incl. the library-behaviour gate) -> `Index::decisions`.
//!
//! [`family`] adds proven `overrides` / `implements` edges by language rule (pipeline phase
//! 4b); [`narrow`] filters name-based candidate pools (name matching, receiver shape,
//! lexical scope, visibility). [`flow`] is solved once with test-origin bits, re-evaluating
//! only constraints whose inputs changed, with bounded slots (`flow_bound`).
//!
//! Incremental (PLAN decision 13): [`hierarchy::Hierarchy::update`],
//! [`family::family_edges_delta`], [`imports::import_path_edges_delta`],
//! and [`sites::generate_delta`] (with [`sites::SitesState`]) produce exactly what their full
//! functions produce on the same inputs.

pub mod behaviour;
pub mod decide;
pub mod family;
pub mod flow;
pub mod hierarchy;
pub mod imports;
pub mod narrow;
pub mod sites;
pub mod types;

#[cfg(test)]
#[path = "../tests/unit/support/mod.rs"]
pub(crate) mod test_support;

#[cfg(test)]
#[path = "../tests/unit/scale.rs"]
mod scale_tests;

pub use hierarchy::Hierarchy;

/// Version of site generation rules; cached sites with another version are regenerated.
/// 2: allocation/receiver-sensitive flow, anonymous scopes, summaries, composition, test
/// provenance (`Site::via`, `Site::test_only`). 4: `<module>`-owned flow sites, syntax-only
/// narrowing and name matching (`narrow`), bounded flow slots, family edges (`family`).
/// 5: `field_write` sites, C/C++ prototype edges and prototype-collapsed name pools,
/// receiver-allocation and implicit-receiver narrowing for more languages.
/// 6: name pools stay within one language namespace (`narrow::name_interop`).
/// 7: a receiver that is itself an allocation (`new K().m()`, Go `K{..}.m()`) narrows
/// method candidates of every blind call (`narrow::Narrower::literal_receiver`).
/// 8: Java/Scala explicit single-name imports bind the simple name
/// (`narrow::Narrower::explicit_import_verdict`); Bash scripts see only their own and
/// sourced files' functions (`narrow::Narrower::sourced_files`, `ModuleMap::resolve` Bash).
/// 9: family members across files/crates (impl/extension conformance, server base types),
/// receiver-typed narrowing from `FileFacts::types` (declared, constructed, comment
/// annotations), import-path rule (`imports::import_path_edges`), deref/wrapper/super
/// receivers dispatching to the family.
/// 10 (trace launch): library knowledge for every language instead of the Python summaries
/// (`Site::library`, library-behaviour decision gate), by-name injection from
/// `runtime_dispatch` rows, syntax-only narrowing deleted, candidates not visible at the
/// site dropped (non-exported bindings of other files, lexical shadowing), incremental
/// sites / decisions.
/// 11 (language fixes): library-declared dispatch sites, member copy / delegation effects,
/// library receivers, arity filtering, unresolved kinds `template_dependent` / `inactive_code`.
/// 13: dispatch sites at the call the edge spans; inherited field types; generic-looking type
/// names resolve only when bound; bodiless members with a foreign body (Java `native`) run.
/// 14: members of library bases of repository classes are library receivers
/// (`LibraryKnowledge::classes`); parameters served only by an installed plugin's provider
/// hold its value class (`LibraryKnowledge::providers`); Python proven-local reads follow
/// lexical scoping (nearest binding function scope, never the module variable). JavaScript
/// CommonJS module values in the flow (relative `require` loads the file's `module.exports`
/// value or implicit `exports` object; `Object.create` / `Object.setPrototypeOf` link lookups
/// as language rules: `flow::js_objects`).
pub const INFER_VERSION: u32 = 14;

/// What inference reads about libraries (trace-library): the knowledge of every library
/// call, the tables (irreducible `runtime_dispatch` rows) and the installed dependency
/// packages (`activated_by` of those rows).
#[derive(Clone, Copy)]
pub struct LibraryInputs<'a> {
    pub knowledge: &'a trace_library::LibraryKnowledge,
    pub tables: &'a trace_library::table::Tables,
    pub installed: &'a trace_library::installed::InstalledPackages,
}

impl LibraryInputs<'static> {
    /// No library knowledge, no table rows, no installed packages (tests, tools that read
    /// an index without its library phase).
    pub fn none() -> LibraryInputs<'static> {
        type Empty = (
            trace_library::LibraryKnowledge,
            trace_library::table::Tables,
            trace_library::installed::InstalledPackages,
        );
        static EMPTY: std::sync::OnceLock<Empty> = std::sync::OnceLock::new();
        let (knowledge, tables, installed) = EMPTY.get_or_init(Default::default);
        LibraryInputs {
            knowledge,
            tables,
            installed,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InferError {
    #[error(transparent)]
    Core(#[from] trace_core::CoreError),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
