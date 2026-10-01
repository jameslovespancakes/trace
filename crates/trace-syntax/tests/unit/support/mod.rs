//! Helpers shared by the unit tests of this crate.

use trace_core::facts::FileFacts;
use trace_core::model::SymbolKind;
use trace_core::Language;

/// Check the synthetic `<module>` declaration invariants (SPEC "module scope") and remove it, so
/// tests written before extractor 5 compare the remaining facts unchanged.
pub(crate) fn strip_module(mut f: FileFacts, source_len: usize) -> FileFacts {
    let n = f.declarations.len();
    let module = f
        .module_decl
        .expect("every extracted file has a <module> declaration");
    assert_eq!(module as usize, n - 1, "<module> is appended last");
    let d = &f.declarations[n - 1];
    assert_eq!(d.name, "<module>");
    assert_eq!(d.qualified_name, "<module>");
    assert_eq!(d.kind, SymbolKind::Module);
    assert_eq!(d.parent, None);
    assert_eq!((d.span.bytes.start, d.span.bytes.end), (0, source_len as u32));
    assert_eq!((d.name_span.start, d.name_span.end, d.body_start), (0, 0, 0));
    assert!(f.declarations.iter().all(|x| x.parent != Some(module)));
    assert!(f
        .calls
        .iter()
        .all(|c| c.owner != Some(module) && c.lexical_owner != Some(module)));
    assert!(f.references.iter().all(|r| r.owner != Some(module)));
    assert!(!f.anonymous.iter().any(|a| a.decl == module));
    f.declarations.pop();
    f.module_decl = None;
    f
}

/// Languages with a usable grammar in this build (compiles every grammar).
pub(crate) fn compiled_languages() -> Vec<Language> {
    Language::ALL
        .into_iter()
        .filter(|&l| crate::grammar(l).is_some())
        .collect()
}
