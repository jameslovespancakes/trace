//! Error type shared by trace-core operations.
//!
//! Texts follow the approved style: capitalized, one or two lines, plain words,
//! no jargon; a second line carries its own 7-space indentation (the CLI prints `Error: `
//! before the first line). `error_type` codes ([`CoreError::kind`]) never change.

use std::io;
use std::path::PathBuf;

/// Errors raised by trace-core. Higher crates wrap this in their own error enums.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("Could not read or write {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// "Folder not found: C:\proj"
    #[error("Folder not found: {}", .0.display())]
    InvalidRoot(PathBuf),
    /// A root that is, is inside, or contains a protected location (config `forbidden_roots`,
    /// `TRACE_FORBIDDEN_ROOTS`). `error_type` `invalid_root`.
    #[error("This folder is excluded in your trace settings.")]
    Excluded(PathBuf),
    #[error("This path is outside the repository: {}", .0.display())]
    OutsideRoot(PathBuf),
    #[error("Use a path relative to the repository: {0}")]
    InvalidRelativePath(String),
    #[error("trace does not read this file (it may hold secrets or is excluded): {0}")]
    Sensitive(String),
    #[error("trace does not follow symbolic links: {0}")]
    Symlink(String),
    #[error("trace keeps its caches outside the analyzed folder; this location is inside it: {}", .0.display())]
    InsideInspectedRoot(PathBuf),
    #[error("This repository is too large for trace: {0}")]
    Limit(String),
    #[error("A file changed since the last index: {0}. Run: trace index")]
    SourceChanged(String),
    #[error("This file is not in the index: {0}")]
    UnknownFile(String),
    #[error("The saved index is damaged ({0}). Run: trace index")]
    CacheCorrupt(String),
    #[error("The saved index is from another trace version (format {found}, expected {expected}). Run: trace index")]
    CacheVersion { found: u32, expected: u32 },
    #[error("The saved index belongs to another folder ({0}). Run: trace index")]
    CacheRoot(String),
    /// "Another trace process is updating this index. Try again in a moment." (the lock file
    /// is kept for logs and JSON).
    #[error("Another trace process is updating this index. Try again in a moment.")]
    Locked(PathBuf),
    /// `No symbol named "parse_config".` (the workspace adds "Did you mean: ..." from its
    /// search index, `AnalysisError::SymbolNotFound`).
    #[error("No symbol named \"{0}\".")]
    SymbolNotFound(String),
    /// A `file:line` selector whose line is covered only by module-level or anonymous code
    /// (SPEC 9.4): never a silent `<module>`; `nearest` lists up to 5 named symbols of
    /// the file (`Qualified.name (line N)`), nearest first. `error_type` is
    /// `symbol_not_found`.
    /// "Line 185 of www/index.js is not inside a named function."
    /// "       Nearest: drawCells (line 128), getIndex (line 124)"
    #[error("{}", line_selector_text(.reference, .nearest))]
    NoNamedSymbolAt { reference: String, nearest: Vec<String> },
    /// `"save" matches 3 symbols. Use one of: a, b, c` (the CLI numbers the candidates on
    /// their own lines).
    #[error("\"{reference}\" matches {} symbols. Use one of: {}", .candidates.len(), .candidates.join(", "))]
    AmbiguousSymbol {
        reference: String,
        candidates: Vec<String>,
    },
    #[error("These query limits are not valid: {0}")]
    InvalidBounds(String),
    #[error("This position is not valid: {0}")]
    InvalidPosition(String),
    #[error("Your trace settings are not valid: {0}")]
    Config(String),
    #[error("Could not save trace data: {0}")]
    Serialize(String),
}

impl CoreError {
    /// Wrap an I/O error with the path it concerns.
    pub fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        CoreError::Io {
            path: path.into(),
            source,
        }
    }

    /// Stable machine-readable name used in JSON error output (`error_type`).
    pub fn kind(&self) -> &'static str {
        match self {
            CoreError::Io { .. } => "io",
            CoreError::InvalidRoot(_) | CoreError::Excluded(_) => "invalid_root",
            CoreError::OutsideRoot(_) => "outside_root",
            CoreError::InvalidRelativePath(_) => "invalid_relative_path",
            CoreError::Sensitive(_) => "sensitive_path",
            CoreError::Symlink(_) => "symlink",
            CoreError::InsideInspectedRoot(_) => "inside_inspected_root",
            CoreError::Limit(_) => "limit",
            CoreError::SourceChanged(_) => "source_changed",
            CoreError::UnknownFile(_) => "unknown_file",
            CoreError::CacheCorrupt(_) => "cache_corrupt",
            CoreError::CacheVersion { .. } => "cache_version",
            CoreError::CacheRoot(_) => "cache_root",
            CoreError::Locked(_) => "locked",
            CoreError::SymbolNotFound(_) | CoreError::NoNamedSymbolAt { .. } => "symbol_not_found",
            CoreError::AmbiguousSymbol { .. } => "ambiguous_symbol",
            CoreError::InvalidBounds(_) => "invalid_bounds",
            CoreError::InvalidPosition(_) => "invalid_position",
            CoreError::Config(_) => "config",
            CoreError::Serialize(_) => "serialize",
        }
    }
}

/// "Line 185 of www/index.js is not inside a named function." + "       Nearest: a (line 3),
/// b (line 10)" (or "       This file declares no named functions.").
fn line_selector_text(reference: &str, nearest: &[String]) -> String {
    let first = match reference.rsplit_once(':') {
        Some((file, line)) if line.chars().all(|c| c.is_ascii_digit()) && !line.is_empty() => {
            format!("Line {line} of {file} is not inside a named function.")
        }
        _ => format!("{reference} is not inside a named function."),
    };
    let second = if nearest.is_empty() {
        "This file declares no named functions.".to_string()
    } else {
        format!("Nearest: {}", nearest.join(", "))
    };
    format!("{first}\n       {second}")
}

/// Result alias for trace-core.
pub type Result<T, E = CoreError> = std::result::Result<T, E>;

#[cfg(test)]
#[path = "../tests/unit/error.rs"]
mod tests;
