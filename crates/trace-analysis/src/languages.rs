//! Language rules of the analysis layer: one row per language, the only per-language data of
//! this crate (every other module asks [`rules`]). Languages without a row follow
//! [`DEFAULT`]: no binding rule applies, so completeness never claims an occurrence
//! "elsewhere" for them by a language rule.

use std::collections::BTreeMap;

use trace_core::Language;

/// Where a bare name can still denote a member of its own file although the language has no
/// implicit receiver (completeness `member_binding`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BareMember {
    /// Inside the member's own declaration (a named function expression's own name).
    OwnDeclaration,
    /// Code at class-body level of the declaring class (not inside a method).
    ClassBody,
    /// Inside the declaring type / module body.
    TypeBody,
}

/// How names bind in one language and which of its files are not product code.
pub(crate) struct AnalysisRules {
    /// Free functions are never reached through a member access on a value (no extension
    /// functions, no implicit `self` calls through an explicit receiver).
    pub free_functions_not_members: bool,
    /// Top-level functions are also reached through a package-path qualifier
    /// (`app.text.formatName()` for `.../app/text/Format.scala`).
    pub package_qualified_calls: bool,
    /// No implicit receiver: a bare name never denotes a member, except [`Self::bare_member`].
    pub no_implicit_receiver: bool,
    pub bare_member: BareMember,
    /// Bare names resolve to the nearest binding of their file (module or file scope; rule
    /// `other_scope`).
    pub file_scope: bool,
    /// Files are modules that import each other (a wildcard import makes every name
    /// possible).
    pub file_modules: bool,
    /// A module file without imports and exports is a script sharing one global scope.
    pub scripts_share_scope: bool,
    /// Packages / namespaces map to directories: the files of one directory form one module.
    pub directory_is_module: bool,
    /// Files in a root `ci/` folder are CI scripts, not product code.
    pub ci_scripts: bool,
    /// File names of build configuration written in the language (the language is not needed
    /// for them alone).
    pub build_scripts: &'static [&'static str],
}

/// Languages without a row.
const DEFAULT: AnalysisRules = AnalysisRules {
    free_functions_not_members: false,
    package_qualified_calls: false,
    no_implicit_receiver: false,
    bare_member: BareMember::TypeBody,
    file_scope: false,
    file_modules: false,
    scripts_share_scope: false,
    directory_is_module: false,
    ci_scripts: false,
    build_scripts: &[],
};

/// Free functions are plain functions, no implicit receiver (C, Rust).
const PLAIN: AnalysisRules = AnalysisRules {
    free_functions_not_members: true,
    no_implicit_receiver: true,
    ..DEFAULT
};

const PYTHON: AnalysisRules = AnalysisRules {
    bare_member: BareMember::ClassBody,
    file_scope: true,
    file_modules: true,
    ci_scripts: true,
    ..PLAIN
};
const JAVASCRIPT: AnalysisRules = AnalysisRules {
    ci_scripts: true,
    ..TYPESCRIPT
};
const TYPESCRIPT: AnalysisRules = AnalysisRules {
    bare_member: BareMember::OwnDeclaration,
    file_scope: true,
    file_modules: true,
    scripts_share_scope: true,
    ..PLAIN
};
const GO: AnalysisRules = AnalysisRules {
    file_scope: true,
    directory_is_module: true,
    ..PLAIN
};
/// Implicit `this` in member functions.
const CPP: AnalysisRules = AnalysisRules {
    free_functions_not_members: true,
    ..DEFAULT
};
/// No free functions (Java), extension methods (C#).
const JVM_CLR: AnalysisRules = AnalysisRules {
    directory_is_module: true,
    ..DEFAULT
};
const PHP: AnalysisRules = AnalysisRules {
    directory_is_module: true,
    ..PLAIN
};
const SCRIPT: AnalysisRules = AnalysisRules {
    ci_scripts: true,
    ..PLAIN
};
/// Top-level functions that are not extensions are never members; they are reached through a
/// package-path qualifier. Implicit `this`. Mill builds.
const SCALA: AnalysisRules = AnalysisRules {
    free_functions_not_members: true,
    package_qualified_calls: true,
    directory_is_module: true,
    build_scripts: &["build.sc", "build.mill"],
    ..DEFAULT
};
/// Type-class methods are called by bare name (Haskell); no methods on values (Julia, OCaml).
const FUNCTIONAL: AnalysisRules = AnalysisRules {
    free_functions_not_members: true,
    ..DEFAULT
};

/// The language with the most files among `files` (one entry per file); ties go to the first
/// language in language order.
pub(crate) fn most_files(files: impl IntoIterator<Item = Language>) -> Option<Language> {
    let mut counts: BTreeMap<Language, usize> = BTreeMap::new();
    for language in files {
        *counts.entry(language).or_insert(0) += 1;
    }
    let mut best: Option<(Language, usize)> = None;
    for (language, n) in counts {
        if best.is_none_or(|(_, m)| n > m) {
            best = Some((language, n));
        }
    }
    best.map(|(l, _)| l)
}

/// The rules of `language`.
pub(crate) fn rules(language: Language) -> &'static AnalysisRules {
    match language {
        Language::Python => &PYTHON,
        Language::JavaScript => &JAVASCRIPT,
        Language::TypeScript | Language::Tsx => &TYPESCRIPT,
        Language::Go => &GO,
        Language::Rust | Language::C => &PLAIN,
        Language::Cpp => &CPP,
        Language::Java | Language::CSharp => &JVM_CLR,
        Language::Php => &PHP,
        Language::Bash | Language::R => &SCRIPT,
        Language::Scala => &SCALA,
        Language::Haskell | Language::Julia | Language::OCaml => &FUNCTIONAL,
        _ => &DEFAULT,
    }
}
