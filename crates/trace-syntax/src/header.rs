//! Language of `.h` headers (shared by C and C++). Owner: native.
//!
//! Two syntax rules, nothing is executed or built:
//! * per file ([`cpp_alternative`], applied by the extractor to its own tree): a `.h` header is
//!   C unless the C grammar reports errors and the C++ grammar parses it with fewer errors
//!   (trailing return types, references, namespaces, templates). A header that tests whether
//!   `__cplusplus` is defined (`#ifdef __cplusplus`, `#if defined(__cplusplus)`) is written
//!   for both languages: it is C, its C++-only sections do not count (the repository rule
//!   below still makes it C++ when only C++ files include it);
//! * repository-wide ([`repo_header_languages`]): a header is C++ when at least one C++ file
//!   includes it and no C file does (headers that include other headers pass their decided
//!   language on, to a fixed point). A C++ project's plain-looking API headers (`fmt/core.h`)
//!   are therefore C++, and the language server receives `languageId: cpp` for them.

use std::collections::{BTreeMap, BTreeSet};

use trace_core::Language;
use tree_sitter::Tree;

/// Whether `path` is a header whose dialect is decided per file ([`cpp_alternative`]).
pub(crate) fn is_shared_header(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".h")
}

/// The C++ tree of a C-classified header when the C++ grammar parses it with fewer errors
/// than `c_tree`; `None` = the file stays C.
pub(crate) fn cpp_alternative(path: &str, source: &[u8], c_tree: &Tree) -> Option<Tree> {
    if !is_shared_header(path) || !c_tree.root_node().has_error() || tests_cplusplus(c_tree, source) {
        return None;
    }
    let cpp = crate::grammar::grammar(Language::Cpp)?;
    let alternative = crate::parse::parse(cpp, path, source).ok()?;
    (crate::extract::error_nodes(alternative.root_node()) < crate::extract::error_nodes(c_tree.root_node()))
        .then_some(alternative)
}

/// Whether a preprocessor conditional of the file tests whether the C++ macro `__cplusplus`
/// is defined (the idiom of headers written for C and C++). Version comparisons
/// (`#if __cplusplus >= 201703L`) are C++-only headers and do not count.
fn tests_cplusplus(tree: &Tree, source: &[u8]) -> bool {
    const MACRO: &[u8] = b"__cplusplus";
    let names =
        |n: tree_sitter::Node<'_>| n.kind() == "identifier" && source.get(n.byte_range()) == Some(MACRO);
    let mut stack = vec![tree.root_node()];
    let mut visited = 0usize;
    while let Some(n) = stack.pop() {
        visited += 1;
        if visited > 200_000 {
            break;
        }
        let tested = match n.kind() {
            "preproc_ifdef" => n.child_by_field_name("name").is_some_and(names),
            "preproc_if" | "preproc_elif" => n
                .child_by_field_name("condition")
                .filter(|c| c.kind() == "preproc_defined")
                .is_some_and(|c| crate::node::named_children(c).into_iter().any(names)),
            _ => false,
        };
        if tested {
            return true;
        }
        if matches!(
            n.kind(),
            "translation_unit"
                | "linkage_specification"
                | "declaration_list"
                | "preproc_if"
                | "preproc_ifdef"
                | "preproc_else"
                | "preproc_elif"
        ) {
            stack.extend(crate::node::named_children(n));
        }
    }
    false
}

/// The headers of `known` an include target of `includer` may name: the path relative to the
/// includer's directory when it is a known header, else every header whose path ends with the
/// target (include directories are not known without a build).
fn resolve_include<'h>(includer: &str, target: &str, known: &BTreeSet<&'h str>) -> Vec<&'h str> {
    let target = target
        .trim()
        .trim_matches(|c| c == '"' || c == '<' || c == '>')
        .replace('\\', "/");
    if target.is_empty() || target.starts_with('/') || target.contains(':') {
        return Vec::new();
    }
    let dir = includer.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let relative = if dir.is_empty() {
        trace_core::relpath::lexical(&target)
    } else {
        trace_core::relpath::lexical(&format!("{dir}/{target}"))
    };
    if let Some(hit) = relative.as_deref().and_then(|r| known.get(r)) {
        return vec![*hit];
    }
    if target.split('/').any(|s| s == "..") {
        return Vec::new();
    }
    let suffix = format!("/{target}");
    known
        .iter()
        .filter(|h| **h == target || h.ends_with(&suffix))
        .copied()
        .collect()
}

/// Repository-wide header languages (module docs). `headers`: every `.h` file with its
/// per-file language; `includers`: every C / C++ file (headers included) with its language
/// and its include targets as written. Returns the decided language of every header.
pub fn repo_header_languages(
    headers: &[(&str, Language)],
    includers: &[(&str, Language, Vec<String>)],
) -> BTreeMap<String, Language> {
    let mut decided: BTreeMap<String, Language> = headers
        .iter()
        .map(|(path, language)| (path.to_string(), *language))
        .collect();
    let known: BTreeSet<&str> = headers.iter().map(|(p, _)| *p).collect();
    // header -> its includers (path, own language)
    let mut included_by: BTreeMap<&str, Vec<(&str, Language)>> = BTreeMap::new();
    for (path, language, targets) in includers {
        for target in targets {
            for header in resolve_include(path, target, &known) {
                if header != *path {
                    included_by.entry(header).or_default().push((*path, *language));
                }
            }
        }
    }
    // Fixed point: languages only change from C to C++, so at most one pass per header.
    for _ in 0..=headers.len() {
        let mut changed = false;
        for (header, users) in &included_by {
            if decided.get(*header) != Some(&Language::C) {
                continue;
            }
            let language_of =
                |(path, own): &(&str, Language)| -> Language { decided.get(*path).copied().unwrap_or(*own) };
            let cpp = users.iter().any(|u| language_of(u) == Language::Cpp);
            let c = users.iter().any(|u| language_of(u) == Language::C);
            if cpp && !c {
                decided.insert((*header).to_string(), Language::Cpp);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    decided
}

#[cfg(test)]
#[path = "../tests/unit/header.rs"]
mod tests;
