//! trace-analysis: the workspace services behind every CLI command.
//!
//! * [`pipeline`]  — build/update the index: inventory -> syntax -> automatic install ->
//!   setup (preflight of every language server; one combined error, no fallback) -> semantic
//!   -> [`pipeline::update`] (link, families, library knowledge, bridges, inference,
//!   decisions) -> persist.
//! * [`workspace`] — open a root, keep the index fresh, hold the config and the
//!   repository settings (`--allow-build`, `--env`), set up pending languages on first use,
//!   warm caches and (in `trace index --watch`) persistent semantic sessions.
//! * [`queries`]   — `show`, `uses`, `deps`, `path`, `context` and the views they share.
//! * [`completeness`] — "is anything missing?" for `uses` and `deps`.
//! * [`status`]    — index health, setup rows per language, pending languages, resolution
//!   health, library behaviour coverage, bridges, host, non-default settings.
//! * [`install`]   — `status --install <language|all|default>`.
//! * [`cards`]     — symbol cards and row helpers shared by the reports.
//! * [`caches`]    — cache hit-rate counters.
//! * [`search`]    — BM25 symbol search (suggestions for unknown selectors).
//! * [`equivalence`] — index comparison (incremental vs full builds, `index_diff`).
//! * [`report`]    — JSON output contract (`--json`, `schema` 1).

pub mod caches;
pub mod cards;
pub mod completeness;
pub mod equivalence;
pub mod install;
mod languages;
pub mod pipeline;
pub mod queries;
pub mod report;
pub mod search;
pub mod status;
pub mod workspace;

pub use workspace::{OpenOptions, Workspace};

/// Traversal depth of `uses --deep`, `deps`, `path` and `context --deep` (fixed).
pub const DEPTH: u32 = 64;

/// Candidate ids of an ambiguity error, comma separated.
fn candidate_ids(candidates: &[report::CandidateRef]) -> String {
    candidates
        .iter()
        .map(|c| c.id.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// ` Did you mean: a, b?` or nothing.
fn did_you_mean(suggestions: &[String]) -> String {
    if suggestions.is_empty() {
        String::new()
    } else {
        format!(" Did you mean: {}?", suggestions.join(", "))
    }
}

/// Errors surfaced to the CLI (exit code 2 for usage / not found / ambiguous, 3 otherwise;
/// the CLI decides from [`AnalysisError::kind`]).
#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    /// An ambiguous selector with numbered candidates (never guessed).
    #[error("\"{reference}\" matches {} symbols. Use one of: {}", .candidates.len(), candidate_ids(.candidates))]
    Ambiguous {
        reference: String,
        candidates: Vec<report::CandidateRef>,
    },
    /// `No symbol named "parse_config". Did you mean: load_config, parse_args?` (up to 3
    /// suggestions from the search index; without any: `No symbol named "parse_config".`).
    #[error("No symbol named \"{reference}\".{}", did_you_mean(.suggestions))]
    SymbolNotFound {
        reference: String,
        suggestions: Vec<String>,
    },
    #[error(transparent)]
    Core(#[from] trace_core::CoreError),
    #[error(transparent)]
    Syntax(#[from] trace_syntax::SyntaxError),
    #[error(transparent)]
    Semantic(#[from] trace_semantic::SemanticError),
    #[error(transparent)]
    Infer(#[from] trace_infer::InferError),
    #[error("{0}")]
    InvalidArgument(String),
    /// "No index yet. Run: trace index" (the root is kept for JSON / logs).
    #[error("No index yet. Run: trace index")]
    NotIndexed(String),
    /// Setup failures (no fallback): the approved texts of `trace_core::SetupError`.
    #[error(transparent)]
    Setup(#[from] trace_core::SetupError),
    #[error(transparent)]
    Library(#[from] trace_library::LibraryError),
}

impl AnalysisError {
    /// Stable `error_type` for JSON errors.
    pub fn kind(&self) -> &'static str {
        match self {
            AnalysisError::Ambiguous { .. } => "ambiguous_symbol",
            AnalysisError::SymbolNotFound { .. } => "symbol_not_found",
            AnalysisError::Core(e) => e.kind(),
            AnalysisError::Syntax(_) => "syntax",
            AnalysisError::Semantic(_) => "semantic",
            AnalysisError::Infer(_) => "infer",
            AnalysisError::InvalidArgument(_) => "invalid_argument",
            AnalysisError::NotIndexed(_) => "not_indexed",
            AnalysisError::Setup(e) => e.kind(),
            AnalysisError::Library(_) => "library",
        }
    }
}

pub type Result<T, E = AnalysisError> = std::result::Result<T, E>;

/// Test fixtures: small in-memory indexes (`tests/unit/support`).
#[cfg(test)]
#[path = "../tests/unit/support/mod.rs"]
pub(crate) mod test_support;
