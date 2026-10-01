//! The completeness summary (status and its sentence) and the ranking of unresolved sites.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use trace_core::model::{EdgeKind, FileId, SymbolId};
use trace_core::source::SourceStore;
use trace_core::{Index, Language, SupportLevel};

use super::{unresolved_match, Occurrence, State, IMPORTS_TARGET, MAX_UNRESOLVED, NAME_ONLY, SAME_MODULE};
use crate::languages::rules;
use crate::report::{Completeness, UnresolvedMatch};

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `8 calls, 3 reads, 1 import` over the open occurrences (kinds in a fixed order).
fn kind_counts(open: &[&Occurrence]) -> String {
    const KINDS: [(&str, &str, &str); 7] = [
        ("call", "call", "calls"),
        ("read", "read", "reads"),
        ("write", "write", "writes"),
        ("callback", "callback", "callbacks"),
        ("import", "import", "imports"),
        ("reexport", "re-export", "re-exports"),
        ("declaration", "declaration", "declarations"),
    ];
    KINDS
        .iter()
        .filter_map(|&(kind, one, many)| {
            let n = open.iter().filter(|o| o.kind == kind).count();
            (n > 0).then(|| plural(n, one, many))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Status + summary over classified occurrences (`calls`: wording of call completeness).
pub(super) fn summarize(
    index: &Index,
    sources: &SourceStore<'_>,
    occ: &[Occurrence],
    bounded: Option<&str>,
    unknown_files: usize,
    calls: bool,
) -> Completeness {
    let to = occ.iter().filter(|o| o.state == State::Target).count();
    let mut elsewhere_reasons: BTreeMap<&'static str, usize> = BTreeMap::new();
    for o in occ {
        if let State::Elsewhere(reason) = o.state {
            *elsewhere_reasons.entry(reason).or_insert(0) += 1;
        }
    }
    let elsewhere: usize = elsewhere_reasons.values().sum();
    let mut open: Vec<&Occurrence> = occ
        .iter()
        .filter(|o| matches!(o.state, State::Unresolved(_)))
        .collect();
    // Occurrences in files not analysed yet (pending languages / sub-projects) and in files
    // outside the build on this machine.
    let mut pending_languages: BTreeSet<Language> = BTreeSet::new();
    let mut pending_files: BTreeSet<FileId> = BTreeSet::new();
    let mut in_pending = 0;
    let mut outside_files: BTreeSet<FileId> = BTreeSet::new();
    let mut in_outside = 0;
    for o in &open {
        let rec = index.file(o.file);
        if rec.language.is_contract() {
            continue;
        }
        if rec.support == SupportLevel::Pending {
            pending_languages.insert(rec.language);
            pending_files.insert(o.file);
            in_pending += 1;
        } else if rec.semantic.as_ref().is_some_and(|s| s.outside_build.is_some()) {
            outside_files.insert(o.file);
            in_outside += 1;
        }
    }
    let cap = if calls { MAX_UNRESOLVED } else { usize::MAX };
    let mut unresolved: Vec<UnresolvedMatch> = open
        .iter()
        .take(cap)
        .map(|o| {
            let reason = match o.state {
                State::Unresolved(r) => r,
                _ => "",
            };
            unresolved_match(index, sources, o, reason)
        })
        .collect();
    // Position in (file, line) order; ties keep occurrence order.
    unresolved.sort_by(|a, b| (&a.at.file, a.at.line).cmp(&(&b.at.file, b.at.line)));
    for (i, u) in unresolved.iter_mut().enumerate() {
        u.rank = i as u32 + 1;
    }
    let n = occ.len();
    let k = open.len();
    let (status, summary) = if let Some(bound) = bounded {
        ("unknown", format!("unknown: {bound}; {} unresolved", plural(k, "site", "sites")))
    } else if unknown_files > 0 {
        (
            "unknown",
            format!(
                "unknown: the name occurs in {} without syntax facts; {} unresolved",
                plural(unknown_files, "file", "files"),
                plural(k, "site", "sites")
            ),
        )
    } else if k > 0 && calls {
        (
            "partial",
            format!("partial: {} unresolved (listed as possible)", plural(k, "call inside", "calls inside")),
        )
    } else if k > 0 {
        open.sort_by_key(|o| (o.file, o.span.start));
        let mut detail = kind_counts(&open);
        if in_pending > 0 {
            let languages: Vec<&str> = pending_languages.iter().map(|l| l.display_name()).collect();
            detail.push_str(&format!(
                "; {in_pending} in {} not analyzed yet ({}); run a query on one of them to set it up",
                plural(pending_files.len(), "file", "files"),
                languages.join(", ")
            ));
        }
        if in_outside > 0 {
            detail.push_str(&format!(
                "; {in_outside} in {} outside the build on this machine",
                plural(outside_files.len(), "file", "files")
            ));
        }
        (
            "partial",
            format!("partial: {} unresolved ({detail})", plural(k, "same-name site", "same-name sites")),
        )
    } else if calls {
        (
            "complete",
            format!(
                "complete: all {} resolved ({to} in the index, {elsewhere} external)",
                plural(n, "call inside", "calls inside")
            ),
        )
    } else {
        (
            "complete",
            format!(
                "complete: all {} resolved ({to} to the target, {elsewhere} elsewhere)",
                plural(n, "name match", "name matches")
            ),
        )
    };
    Completeness {
        status,
        summary,
        name_matches: n,
        resolved_to_target: to,
        resolved_elsewhere: elsewhere,
        unresolved,
        pending_languages: pending_languages.into_iter().collect(),
        pending_files: pending_files.len(),
        outside_build_files: outside_files.len(),
        elsewhere_reasons,
    }
}

// ------------------------------------------------------------------ ranking

/// Rank group of `file` for a target declared in `target_file` (SPEC §10.1): 0
/// `same_module`, 1 `imports_target`, 2 `name_only`.
fn rank_group(
    index: &Index,
    modules: &OnceCell<trace_infer::narrow::ModuleMap>,
    importing: &HashSet<FileId>,
    target_file: FileId,
    file: FileId,
) -> u8 {
    if file == target_file {
        return 0;
    }
    let (t, f) = (index.file(target_file), index.file(file));
    if rules(t.language).directory_is_module
        && trace_core::languages::same_module_namespace(t.language, f.language)
        && trace_core::relpath::parent(&t.path) == trace_core::relpath::parent(&f.path)
    {
        return 0;
    }
    if importing.contains(&file) {
        return 1;
    }
    let Some(facts) = &f.facts else { return 2 };
    let map = modules.get_or_init(|| trace_infer::narrow::ModuleMap::new(index));
    let resolves = |target: &str, member: bool| {
        map.resolve(&f.path, f.language, target, member)
            .is_some_and(|(files, _)| files.contains(&target_file))
    };
    let imports = facts
        .imports
        .iter()
        .any(|i| resolves(&i.target, i.kind == trace_core::facts::ImportKind::Member));
    let reexports = facts.exports.iter().any(|e| resolves(&e.target, true));
    if imports || reexports {
        1
    } else {
        2
    }
}

/// Rank the unresolved sites of a `uses` answer (SPEC §10.1): `same_module` (the target's
/// file; for Go / Java / Scala / C# / PHP the target's directory in the
/// same language namespace), then `imports_target` (files with a proven `imports` /
/// `reexports` edge into a symbol of the target's file, or an import / re-export fact that
/// `trace_infer::narrow::ModuleMap` resolves to that file), then `name_only`; within a group
/// by (file, line, start byte). `rank` is the 1-based position, `scope` the group.
pub(crate) fn rank_unresolved(index: &Index, target: SymbolId, c: &mut Completeness) {
    let target_file = index.symbol(target).file;
    let mut importing: HashSet<FileId> = HashSet::new();
    for e in &index.edges {
        if matches!(e.kind, EdgeKind::Imports | EdgeKind::Reexports) && index.symbol(e.to).file == target_file
        {
            importing.insert(e.at.file);
        }
    }
    let modules = OnceCell::new();
    let mut groups: HashMap<String, u8> = HashMap::new();
    for u in &c.unresolved {
        if groups.contains_key(&u.at.file) {
            continue;
        }
        let group = match index.file_by_path(&u.at.file) {
            Some(f) => rank_group(index, &modules, &importing, target_file, f),
            None => 2,
        };
        groups.insert(u.at.file.clone(), group);
    }
    let group = |u: &UnresolvedMatch| groups.get(&u.at.file).copied().unwrap_or(2);
    c.unresolved.sort_by(|a, b| {
        (group(a), &a.at.file, a.at.line, a.at.start_byte).cmp(&(
            group(b),
            &b.at.file,
            b.at.line,
            b.at.start_byte,
        ))
    });
    for (i, u) in c.unresolved.iter_mut().enumerate() {
        let g = groups.get(&u.at.file).copied().unwrap_or(2);
        u.rank = i as u32 + 1;
        u.scope = [SAME_MODULE, IMPORTS_TARGET, NAME_ONLY][g as usize];
    }
}

// ------------------------------------------------------------------ deps
