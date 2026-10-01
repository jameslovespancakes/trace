//! The Python stub rule (port of codepath_v3 `collapse_stub_pairs` and
//! `link_stub_implementations`). A proven language rule, not inference:
//!
//! 1. For every declaration in `pkg/mod.pyi` with a declaration of the same qualified name in
//!    the sibling `pkg/mod.py`: edge `stub_implementation` (owner = stub, target = impl,
//!    resolution `stub_packaging_rule`). The implementation that runs is the *last*
//!    same-named definition of the module. Evidence location: the stub's name (edge
//!    locations are always in the owner's file).
//! 2. `external_or_ambiguous` call sites whose candidate locations ALL map to `.pyi`
//!    declarations having such an implementation, and whose implementations are one single
//!    declaration: replace by an edge to that implementation, kind by its execution model
//!    (`generator`/`async_generator` -> `creates_generator`, `coroutine` ->
//!    `creates_coroutine`, else `calls`), resolution `overloaded_stub_implementation`.
//!    Sites that also had targets outside the index are never rewritten.
//! 3. Definition lookups returning both a stub and its sibling implementation collapse to the
//!    implementation(s) when every stub has a same-qualified-name `.py` sibling in the result.
//!    Other ambiguity remains ambiguous.

use std::collections::{BTreeSet, HashMap, HashSet};

use trace_core::model::{EdgeKind, ExecutionModel, Resolution, UnresolvedKind};
use trace_core::semantics::{FileSemantics, SemEdge, SemUnresolved};

use crate::mapping::{DeclRef, DeclTable};

/// Qualified identity of a declaration: (relative path, qualified name).
pub type QualKey<'a> = (&'a str, &'a str);

/// Rule 3: collapse `{stub, impl}` definition results. Input and output are uids.
pub fn collapse_stub_pairs(targets: BTreeSet<String>) -> BTreeSet<String> {
    let collapsed: Option<BTreeSet<String>> = {
        let (implementations, stubs): (Vec<&str>, Vec<&str>) = targets
            .iter()
            .map(String::as_str)
            .partition(|uid| split_uid(uid).is_some_and(|(path, _)| is_impl_path(path)));
        let impl_keys: HashSet<QualKey<'_>> =
            implementations.iter().filter_map(|uid| split_uid(uid)).collect();
        let all_paired = !implementations.is_empty()
            && !stubs.is_empty()
            && stubs.iter().all(|uid| match split_uid(uid) {
                Some((path, qualified)) if is_stub_path(path) => {
                    impl_keys.contains(&(&path[..path.len() - 1], qualified))
                }
                _ => false,
            });
        all_paired.then(|| implementations.iter().map(|uid| uid.to_string()).collect())
    };
    collapsed.unwrap_or(targets)
}

/// Rules 1 and 2 over all files of the Python partition (mutates results in place).
/// `candidates` of rule 2 are the uids stored on `SemUnresolved::candidates` by pyright.
/// The set of `(path, callee start)` sites whose analyzer results included
/// targets outside the index (their candidate lists are incomplete; rule 2 skips them).
pub(crate) fn apply_rules(
    files: &mut HashMap<String, FileSemantics>,
    decls: &DeclTable<'_>,
    incomplete: &HashSet<(String, u32)>,
) {
    let mut paths: Vec<String> = files.keys().cloned().collect();
    paths.sort();
    for path in &paths {
        let Some(sem) = files.get_mut(path) else {
            continue;
        };
        // Rule 1: stub -> sibling implementation.
        if is_stub_path(path) {
            for stub in decls.decls_of(path) {
                // Synthetic scopes (`<module>`, `<lambda>`) are not stub declarations.
                if decls.is_synthetic(stub) {
                    continue;
                }
                if let Some(implementation) = implementation_of(stub, decls) {
                    sem.edges.push(SemEdge {
                        owner: stub.decl,
                        target: decls.uid(implementation),
                        kind: EdgeKind::StubImplementation,
                        at: decls.decl(stub).name_span,
                        line: decls.name_line(stub).unwrap_or(0),
                        resolution: Resolution::StubPackagingRule,
                    });
                }
            }
        }
        // Rule 2: calls whose only targets were overloads in a sibling stub.
        let mut kept = Vec::with_capacity(sem.unresolved.len());
        for u in std::mem::take(&mut sem.unresolved) {
            match overloaded_target(&u, path, decls, incomplete) {
                Some((owner, target)) => sem.edges.push(SemEdge {
                    owner,
                    target: decls.uid(target),
                    kind: stub_call_kind(decls.decl(target).execution),
                    at: u.at,
                    line: u.line,
                    resolution: Resolution::OverloadedStubImplementation,
                }),
                None => kept.push(u),
            }
        }
        sem.unresolved = kept;
        sem.edges.sort_by(|a, b| {
            (a.at.start, a.at.end, a.owner, &a.target, a.kind)
                .cmp(&(b.at.start, b.at.end, b.owner, &b.target, b.kind))
        });
        sem.edges
            .dedup_by(|a, b| a.at == b.at && a.owner == b.owner && a.target == b.target && a.kind == b.kind);
    }
}

/// The single implementation behind an ambiguous site whose candidates are all stubs.
fn overloaded_target<'a>(
    u: &SemUnresolved,
    path: &str,
    decls: &DeclTable<'a>,
    incomplete: &HashSet<(String, u32)>,
) -> Option<(u32, DeclRef<'a>)> {
    if u.kind != UnresolvedKind::ExternalOrAmbiguous || u.candidates.is_empty() {
        return None;
    }
    let owner = u.owner?;
    if incomplete.contains(&(path.to_string(), u.at.start)) {
        return None;
    }
    let mut target: Option<DeclRef<'a>> = None;
    for candidate in &u.candidates {
        let stub = decls.by_uid(candidate)?;
        let implementation = implementation_of(stub, decls)?;
        match target {
            None => target = Some(implementation),
            Some(t) if t == implementation => {}
            Some(_) => return None,
        }
    }
    target.map(|t| (owner, t))
}

/// Sibling `.py` declaration with the same qualified name (the last definition wins, as
/// Python rebinds names in order).
fn implementation_of<'a>(stub: DeclRef<'a>, decls: &DeclTable<'a>) -> Option<DeclRef<'a>> {
    if !is_stub_path(stub.path) {
        return None;
    }
    let sibling = decls.path_key(&stub.path[..stub.path.len() - 1])?;
    let qualified = &decls.decl(stub).qualified_name;
    let facts = decls.facts(sibling)?;
    let index = facts
        .declarations
        .iter()
        .rposition(|d| &d.qualified_name == qualified)?;
    Some(DeclRef {
        path: sibling,
        decl: index as u32,
    })
}

fn stub_call_kind(execution: ExecutionModel) -> EdgeKind {
    match execution {
        ExecutionModel::Generator | ExecutionModel::AsyncGenerator => EdgeKind::CreatesGenerator,
        ExecutionModel::Coroutine => EdgeKind::CreatesCoroutine,
        ExecutionModel::Ordinary => EdgeKind::Calls,
    }
}

/// `path:Qualified.name[#k]` -> (path, qualified name without the occurrence suffix).
fn split_uid(uid: &str) -> Option<QualKey<'_>> {
    let (path, rest) = uid.rsplit_once(':')?;
    let qualified = rest.split_once('#').map_or(rest, |(q, _)| q);
    Some((path, qualified))
}

/// `.pyi` (any case), checked on bytes so odd paths never split a character.
fn is_stub_path(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() > 4 && b[b.len() - 4..].eq_ignore_ascii_case(b".pyi")
}

/// `.py` (any case).
fn is_impl_path(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() > 3 && b[b.len() - 3..].eq_ignore_ascii_case(b".py")
}

#[cfg(test)]
#[path = "../tests/unit/stubs.rs"]
mod tests;
