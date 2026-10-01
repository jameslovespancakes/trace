//! Language rules of the engine's answer mapping: C / C++ call narrowing ([`cpp_calls`]),
//! template-dependent calls ([`cpp_templates`]), inactive preprocessor regions
//! ([`preprocessor`]), Scala applications ([`scala_apply`]) and nested-function scoping ([`scoping`]).

pub(crate) mod cpp_calls;
pub(crate) mod cpp_templates;
pub(crate) mod preprocessor;
pub(crate) mod scala_apply;
pub(crate) mod scoping;
