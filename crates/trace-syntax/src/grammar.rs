//! Grammar registry: every compiled-in tree-sitter language plus its compiled query.
//!
//! | Language    | crate (exact version)          | constant                 |
//! |-------------|--------------------------------|--------------------------|
//! | Python      | tree-sitter-python 0.25.0      | `LANGUAGE`               |
//! | JavaScript  | tree-sitter-javascript 0.25.0  | `LANGUAGE` (JSX incl.)   |
//! | TypeScript  | tree-sitter-typescript 0.23.2  | `LANGUAGE_TYPESCRIPT`    |
//! | Tsx         | tree-sitter-typescript 0.23.2  | `LANGUAGE_TSX`           |
//! | Rust        | tree-sitter-rust 0.24.2        | `LANGUAGE`               |
//! | Go          | tree-sitter-go 0.25.0          | `LANGUAGE`               |
//! | Java        | tree-sitter-java 0.23.5        | `LANGUAGE`               |
//! | C           | tree-sitter-c 0.24.2           | `LANGUAGE`               |
//! | Cpp         | tree-sitter-cpp 0.23.4         | `LANGUAGE`               |
//! | CSharp      | tree-sitter-c-sharp 0.23.5     | `LANGUAGE`               |
//! | Php         | tree-sitter-php 0.24.2         | `LANGUAGE_PHP`           |
//! | Bash        | tree-sitter-bash 0.25.1        | `LANGUAGE`               |
//! | Scala       | tree-sitter-scala 0.26.2       | `LANGUAGE`               |
//! | R           | tree-sitter-r 1.3.0            | `LANGUAGE`               |
//! | Haskell     | tree-sitter-haskell 0.23.1     | `LANGUAGE`               |
//!
//! Julia and OCaml are inventoried only (no grammar). Conversion:
//! `tree_sitter::Language::new(CONST)`.

use std::sync::OnceLock;

use trace_core::Language;

use crate::languages::syntax;
use crate::spec::SyntaxSpec;
use crate::SyntaxError;

/// A compiled-in grammar with its query and node-kind table.
pub struct Grammar {
    pub language: Language,
    pub ts: tree_sitter::Language,
    /// Compiled `queries/<lang>.scm` (capture contract: SPEC §6.2).
    pub query: tree_sitter::Query,
    pub spec: &'static SyntaxSpec,
}

impl std::fmt::Debug for Grammar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grammar")
            .field("language", &self.language)
            .field("patterns", &self.query.pattern_count())
            .finish()
    }
}

fn build(spec: &'static SyntaxSpec) -> Result<Grammar, SyntaxError> {
    let (language, ts) = (spec.language, (spec.grammar)());
    // Rejects incompatible ABI versions up front (never at parse time).
    tree_sitter::Parser::new()
        .set_language(&ts)
        .map_err(|e| SyntaxError::Grammar {
            language,
            message: e.to_string(),
        })?;
    let query = tree_sitter::Query::new(&ts, spec.query).map_err(|e| SyntaxError::Query {
        language,
        message: format!("{}:{}: {} ({:?})", e.row + 1, e.column + 1, e.message, e.kind),
    })?;
    Ok(Grammar {
        language,
        ts,
        query,
        spec,
    })
}

/// Per-language grammar slots, each compiled on first use: commands that touch one language
/// never pay for compiling the other queries. Grammars whose query fails to compile are
/// reported by [`grammar_error`] (`trace status`) and treated as not compiled in (never a panic).
fn entry(language: Language) -> Option<&'static Result<Grammar, SyntaxError>> {
    const N: usize = Language::ALL.len();
    static SLOTS: [OnceLock<Option<Result<Grammar, SyntaxError>>>; N] = [const { OnceLock::new() }; N];
    let i = Language::ALL.iter().position(|&l| l == language)?;
    SLOTS[i].get_or_init(|| syntax(language).map(build)).as_ref()
}

/// The grammar for `language`, if compiled in and valid.
pub fn grammar(language: Language) -> Option<&'static Grammar> {
    entry(language)?.as_ref().ok()
}

/// The grammar/query compilation failure for `language`, if any.
pub fn grammar_error(language: Language) -> Option<&'static SyntaxError> {
    entry(language)?.as_ref().err()
}

#[cfg(test)]
#[path = "../tests/unit/grammar.rs"]
mod tests;
