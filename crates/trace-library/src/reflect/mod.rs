//! Reflection metadata read from installed packages (DESIGN-bridges §2 rule 5): the
//! declarations a repository's reflection roots need when the chain of an annotation,
//! attribute or decorator leaves the repository.
//!
//! * [`types`]: declarations of annotation / attribute types located by the language's own
//!   naming rules (Java sources in `-sources.jar` archives, .NET assemblies);
//! * [`clr_meta`]: type metadata of compiled .NET assemblies (ECMA-335);
//! * [`decorator_meta`]: what a JavaScript decorator factory stores for a reflection scanner.

pub(crate) mod clr_meta;
pub mod decorator_meta;
pub mod types;
