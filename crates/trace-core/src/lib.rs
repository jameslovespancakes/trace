//! trace-core: the shared contract of the `trace` workspace.
//!
//! Every other crate exchanges data exclusively through the types defined here:
//!
//! * [`facts`]     — syntax facts produced by `trace-syntax` (per file, declaration-local indices).
//! * [`semantics`] — semantic results produced by `trace-semantic` (per file, targets by stable uid).
//! * [`model`]     — the assembled, persisted [`Index`] (dense ids, proven edges, sites, decisions).
//! * [`graph`]     — the in-memory [`Graph`] with tiered views and CSR adjacency.
//! * [`query`]     — bounded traversals (reach, shortest path, bounded simple paths).
//!
//! Plus the infrastructure every layer needs: the only list of [`languages`], the only reader
//! of process environment variables ([`env`]), the central settings ([`config`]: [`Settings`],
//! `assets/config/defaults.jsonc`), per-repository settings
//! ([`repo_settings`]), file [`inventory`], blake3 [`fingerprint`]s, the versioned [`cache`],
//! cache location resolution ([`paths`]), verified [`source`] access and UTF-8/UTF-16 [`text`]
//! position mapping.
//!
//! Safety invariants enforced here: sources are read-only and re-verified by hash before use;
//! every cache path is outside the inspected root; evidence tiers are never merged silently.
#![forbid(unsafe_code)]

pub mod assemble;
pub mod cache;
pub mod config;
pub mod delta;
pub mod env;
pub mod error;
pub mod facts;
pub mod fingerprint;
pub mod formats;
pub mod graph;
pub mod incremental;
pub mod inventory;
pub mod languages;
pub mod model;
pub mod paths;
pub mod query;
pub mod relpath;
pub mod repo_settings;
pub mod resolve;
pub mod semantics;
pub mod setup_error;
pub mod source;
pub mod text;
pub mod tiers;

pub use config::Settings;
pub use error::{CoreError, Result};
pub use fingerprint::Hash32;
pub use graph::{Graph, TierCounts};
pub use languages::{Language, LanguageSupport, SupportLevel, DEFAULT_LANGUAGES};
pub use model::*;
pub use query::{Bounds, BoundsHit, Direction, PathResult, Reach};
pub use repo_settings::RepoSettings;
pub use setup_error::{InstallFailure, SetupError};
pub use tiers::KindSet;

/// Version of the persisted index layout. Bump on any change to persisted types.
/// 5: `Index::bridges`, `Edge::bridge`, new edge kinds / providers / resolutions,
/// `SymbolKind::Module`, `FileFacts::{module_decl, exports, boundaries}`, `Reference::kind`.
/// 6: `FileFacts::{types, local_spans, member_accesses}`,
/// `FileSemantics::{implementations, resolved_elsewhere}`,
/// `BackendRun::ready`, resolutions `import_path` / `family_overloads` / `syntax_definition`.
/// 7 (trace launch): `Site::library`, `FileRecord::pending`, `Index::{phase_state, stale}`,
/// `CallbackArg::{index, keyword}`, `FileFacts::interface`, `FileSemantics::{callback_params,
/// library_files, library_calls, outside_build, expanded}`.
/// 9 (language fixes): `FileSemantics::library_dispatch`, `Site::declared_library`,
/// `Index::library_receivers`, unresolved kinds `template_dependent` / `inactive_code`.
/// 10: `Language` and `TypeSubject` variants removed.
/// 11: `FileSemantics::library_bases`.
/// 12: JEV removed: `Decision` is `{site, status, targets, reason}`; edge kind
/// `possible_dispatch`, provider `jev` and the JEV resolutions removed.
/// 14: consolidated source-retrieval facts (`data_definitions`, `body_identifiers`)
/// and the JEV-free model. Experimental schemas 12/13 are not compatible.
pub const SCHEMA_VERSION: u32 = 14;

/// Crate version string shared by every `trace` crate (workspace version).
pub const TRACE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Test fixtures (`tests/unit/support`).
#[cfg(test)]
#[path = "../tests/unit/support/mod.rs"]
pub(crate) mod test_support;
