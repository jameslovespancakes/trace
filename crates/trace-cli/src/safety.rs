//! Root admission (SPEC §2.4) and CLI-level errors.
//!
//! Protected directories are never inspected: a root equal to, inside, or containing one of
//! `trace_core::paths::forbidden_roots()` is refused before any cache, inventory or analyzer
//! is touched. The check itself lives in `trace_analysis::workspace::check_forbidden` (one
//! implementation for the CLI and the library).

use std::path::{Path, PathBuf};

/// Errors raised by the CLI itself (everything else comes from the library crates).
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// A root that is, is inside, or contains a protected location.
    #[error("This folder is excluded in your trace settings.")]
    ProtectedRoot(PathBuf),
    #[error("{0}")]
    InvalidArgument(String),
}

impl CliError {
    /// `error_type` for JSON errors.
    pub fn kind(&self) -> &'static str {
        match self {
            CliError::ProtectedRoot(_) => "invalid_root",
            CliError::InvalidArgument(_) => "invalid_argument",
        }
    }
}

/// Refuse a canonical root that is, is inside, or contains a protected directory.
pub fn ensure_allowed(canonical_root: &Path) -> Result<(), CliError> {
    trace_analysis::workspace::check_forbidden(canonical_root)
        .map_err(|_| CliError::ProtectedRoot(canonical_root.to_path_buf()))
}

#[cfg(test)]
#[path = "../tests/unit/safety.rs"]
mod tests;
