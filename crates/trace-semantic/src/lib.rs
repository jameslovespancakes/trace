//! trace-semantic: guarded semantic backends mapped onto trace-syntax symbols.
//!
//! Backends: Pyright (Python, LSP), TypeScript 7 compiler worker (JS/TS/TSX) and a
//! generic LSP backend for every other registry entry (rust-analyzer included)
//! (`assets/backends/*.json`). Every language is served by exactly one registry entry;
//! before a server starts, its preflight ([`setup`], `languages`) checks platform,
//! toolchain, server (+ runtime), dependencies and build approval. There is no syntax-only
//! mode: a server, toolchain, dependency or build that is missing or failing is a
//! [`trace_core::SetupError`] and indexing stops with it (syntax trees are still used next
//! to the servers for exact positions and symbols, never instead of them). Every backend
//! analyzes a workspace copy of the sources outside the target root, launched with a
//! trusted absolute executable, a controlled environment and a controlled configuration.
//!
//! Output is per-file [`trace_core::semantics::FileSemantics`]; symbols always come from
//! syntax. Results that cannot be mapped to exactly one syntax declaration are dropped with
//! a diagnostic; call sites without targets become explicit unresolved entries.
//!
//! Speed: LSP work is sharded (by directory) over a pool of analyzer processes (Pyright:
//! `min(cores/2, 4)`, fewer when the estimated pool memory exceeds
//! `memory.budget_mb`: `pool::pool_size`, [`estimate_mb`]) with pipelined requests;
//! definitions syntax answers identically are not requested (local bindings, the only
//! same-file function of a bare Bash / PHP call, names no file declares);
//! [`session::SemanticSessions`] adds a per-file cache keyed by
//! content and dependency hashes (`cache`) and persistent sessions for the background
//! host (processes stay alive; only changed files are re-sent and only requested files
//! re-queried).
//!
//! Module map: `tools` (trusted executables, clean environments), `assets` (embedded
//! worker and guard), `snapshot` (verified workspace copies), `lsp` (stdio JSON-RPC
//! client), `mapping` (positions -> declarations), `engine` (the shared LSP algorithm and
//! its language rules), `pool` (process pools and persistent LSP sessions), [`session`]
//! (cached / persistent runs), `cache` (per-file semantic cache), `stubs` (Python stub
//! rule), `references` (live find-references), [`mod@registry`] (backend registry), `backends`
//! (Pyright, the TypeScript worker, the generic LSP backend, rust-analyzer protocol helpers,
//! the function-type rule) and `languages` (the `Server` of each language).
//!
//! Completeness (engine 4, SPEC section 8.5): every result is attributed to the syntax
//! executing owner (`FileFacts::executing_owner`: module-level and class-body code belongs
//! to the synthetic `<module>` declaration, callbacks to their `<lambda>`), calls no
//! prepared item covers are resolved with `definition`, outgoing-call ranges are matched to
//! calls by member identifier, and non-call uses become `references` / `writes` /
//! `imports` / `reexports` / `passes_callback` edges.
//!
//! Engine 6 (SPEC sections 8.5a, 8.8): server implementations and type-hierarchy subtypes
//! (`FileSemantics::implementations`), non-call uses resolved outside the index
//! (`FileSemantics::resolved_elsewhere`), readiness waits (`BackendRun::ready`).

//! Setup (trace launch, DESIGN §1.7-§1.9): [`setup`] (preflight of every backend, all
//! failures combined), `languages` (per-language hooks), [`install`] (installer + tools
//! directory), `external` (library call locations), `mirror` (persistent mirror
//! workspaces), `procs` (process trees of every program trace starts: stopped as a whole,
//! never given trace's standard streams).
#![forbid(unsafe_code)]

mod assets;
pub mod backend;
mod backends;
mod bases;
mod cache;
mod engine;
mod external;
pub mod install;
mod languages;
mod lsp;
mod mapping;
mod mirror;
mod pool;
mod procs;
mod references;
pub mod registry;
pub mod session;
pub mod setup;
mod snapshot;
mod stubs;
mod tools;

#[cfg(test)]
#[path = "../tests/unit/support/mod.rs"]
pub(crate) mod test_support;

pub use backend::{registry, Backend, SemanticFile, SemanticRequest};
pub use languages::{server_for, Prepared};
pub use pool::estimate_mb;
pub use references::{LiveReferences, ReferenceQuery};
pub use session::{BackendSession, RunPolicy, SemanticSessions};
pub use tools::ToolEnv;

use std::path::PathBuf;

/// Errors from semantic backends. There is no fallback: the pipeline stops indexing with
/// every one of them. `Setup` carries its own approved text; any other error of a backend run
/// is reported as `SetupError::ServerCrashed` with the backend log (DESIGN §2).
#[derive(Debug, thiserror::Error)]
pub enum SemanticError {
    #[error(transparent)]
    Core(#[from] trace_core::CoreError),
    /// Setup failures (no fallback): missing server / toolchain / dependencies, build
    /// approval, crashes, timeouts, load failures.
    #[error(transparent)]
    Setup(#[from] trace_core::SetupError),
    #[error("A language server program could not be found: {0}")]
    ExecutableUnavailable(String),
    #[error("trace does not run programs from inside the analyzed folder: {}", .0.display())]
    UntrustedExecutable(PathBuf),
    #[error("Could not start {program}: {source}")]
    Launch {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("The language server stopped unexpectedly ({0}).")]
    ServerExited(String),
    #[error("The language server did not answer in time ({method}).")]
    Timeout { method: String },
    #[error("The language server did not finish in time.")]
    Deadline,
    #[error("The language server reported an error for {method} ({code}): {message}")]
    Rpc {
        method: String,
        code: i64,
        message: String,
    },
    #[error("The language server sent a message trace cannot read: {0}")]
    Protocol(String),
    #[error("The language server does not support a feature trace needs: {0}")]
    Capability(String),
    #[error("The analysis worker failed: {0}")]
    Worker(String),
    #[error("A file changed while it was being analyzed: {0}")]
    SourceChanged(String),
    #[error("Could not read or write a file: {0}")]
    Io(#[from] std::io::Error),
    #[error("Could not read a JSON answer: {0}")]
    Json(#[from] serde_json::Error),
}
