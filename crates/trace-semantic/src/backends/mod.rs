//! Backends: Pyright ([`pyright`]), the TypeScript compiler worker ([`typescript`]), the
//! generic LSP backend for every other registry entry ([`generic`]) with rust-analyzer's
//! protocol helpers ([`rust_analyzer`]), and the function-type rule ([`fntype`]).

pub mod fntype;
pub mod generic;
pub mod pyright;
pub mod rust_analyzer;
pub mod typescript;
