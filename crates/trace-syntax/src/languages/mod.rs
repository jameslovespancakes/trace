//! The syntax languages: one file per language holding its [`SyntaxSpec`] (grammar, query,
//! node-kind tables and language rules). This registry is the only place that lists them.

use trace_core::Language;

use crate::spec::SyntaxSpec;

mod bash;
mod c;
mod cpp;
mod csharp;
mod go;
mod haskell;
mod java;
mod javascript;
mod php;
mod python;
mod r;
mod rust;
mod scala;
mod typescript;

/// The syntax spec of `language`, if it has a compiled-in grammar (other languages are
/// inventoried only).
pub fn syntax(language: Language) -> Option<&'static SyntaxSpec> {
    Some(match language {
        Language::Python => &python::SYNTAX,
        Language::JavaScript => &javascript::SYNTAX,
        Language::TypeScript => &typescript::SYNTAX,
        Language::Tsx => &typescript::TSX_SYNTAX,
        Language::Rust => &rust::SYNTAX,
        Language::Go => &go::SYNTAX,
        Language::Java => &java::SYNTAX,
        Language::C => &c::SYNTAX,
        Language::Cpp => &cpp::SYNTAX,
        Language::CSharp => &csharp::SYNTAX,
        Language::Php => &php::SYNTAX,
        Language::Bash => &bash::SYNTAX,
        Language::Scala => &scala::SYNTAX,
        Language::R => &r::SYNTAX,
        Language::Haskell => &haskell::SYNTAX,
        _ => return None,
    })
}
