//! Incremental per-file invalidation (SPEC §5.3; interface rule and stale dependents: PLAN
//! decision 13).
//!
//! Syntax facts are reused per file when the content hash and extractor version match.
//! Semantic results are cached per file with uid targets; a file is re-queried when:
//!
//! 1. it is new or its content changed, or it has no cached semantics, or the backend tool
//!    fingerprint changed;
//! 2. any configuration file changed (all files of the backend's partition);
//! 3. its cached edges / candidates / value refs target a symbol in a removed file or in a
//!    changed file whose INTERFACE changed (`FileFacts::interface`, the interface rule: a
//!    body edit that keeps what other files can see never re-queries dependents);
//! 4. its syntax mentions a declaration name that appeared or disappeared in this update
//!    ([`changed_declaration_names`]: a call member / callee or a value reference with that
//!    name): a new declaration may now be the target, or one more candidate, of a call that
//!    resolved before; a removed one may leave it ambiguous or unresolved;
//! 5. it is stale (a dependent left by an earlier interface change) and the update resolves it.
//!
//! Rules 3 and 4 find DEPENDENTS. With [`StalePolicy::ResolveAll`] (in-process commands,
//! `trace index`) they are re-queried in the same update; with [`StalePolicy::Defer`]
//! (`trace index --watch`) they become stale ([`Requery::stale`], persisted in `Index::stale`)
//! and are resolved in the watcher's following batches. A command that reads an index with
//! stale files waits for the watcher to resolve them, or resolves them itself
//! ([`StalePolicy::ResolveAll`]) when no watcher runs.
//!
//! [`index_delta`] turns one update into the [`IndexDelta`] every incremental phase reads.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::assemble::symbol_uid;
use crate::delta::IndexDelta;
use crate::facts::FileFacts;
use crate::fingerprint::Hash32;
use crate::inventory::HashedEntry;
use crate::languages::Language;
use crate::model::{FileRecord, Index};

/// Classification of the current inventory against the previous index.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdatePlan {
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: Vec<String>,
    pub configs_changed: bool,
}

impl UpdatePlan {
    pub fn is_noop(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty() && !self.configs_changed
    }
    /// Added or changed paths.
    pub fn dirty(&self) -> impl Iterator<Item = &str> {
        self.added.iter().chain(self.changed.iter()).map(String::as_str)
    }
}

/// Compare hashed inventory entries with the previous index.
pub fn plan(prev: Option<&Index>, sources: &[HashedEntry], configs: &[HashedEntry]) -> UpdatePlan {
    let mut plan = UpdatePlan::default();
    let Some(prev) = prev else {
        plan.added = sources.iter().map(|e| e.entry.path.clone()).collect();
        plan.configs_changed = true;
        return plan;
    };
    let old: HashMap<&str, &Hash32> = prev.files.iter().map(|f| (f.path.as_str(), &f.hash)).collect();
    let mut present = HashSet::with_capacity(sources.len());
    for e in sources {
        present.insert(e.entry.path.as_str());
        match old.get(e.entry.path.as_str()) {
            None => plan.added.push(e.entry.path.clone()),
            Some(h) if **h != e.hash => plan.changed.push(e.entry.path.clone()),
            Some(_) => plan.unchanged.push(e.entry.path.clone()),
        }
    }
    plan.removed = prev
        .files
        .iter()
        .filter(|f| !present.contains(f.path.as_str()))
        .map(|f| f.path.clone())
        .collect();
    let mut new_configs: Vec<(&str, &Hash32)> =
        configs.iter().map(|e| (e.entry.path.as_str(), &e.hash)).collect();
    new_configs.sort_unstable();
    let mut old_configs: Vec<(&str, &Hash32)> = prev.configs.iter().map(|(p, h)| (p.as_str(), h)).collect();
    old_configs.sort_unstable();
    plan.configs_changed = new_configs != old_configs;
    plan
}

/// Previous record whose syntax facts can be reused for `path` at `hash`.
pub fn reusable_record<'i>(
    prev: Option<&'i Index>,
    path: &str,
    hash: &Hash32,
    syntax_version: u32,
) -> Option<&'i FileRecord> {
    let prev = prev?;
    if prev.header.syntax_version != syntax_version {
        return None;
    }
    let id = prev.file_by_path(path)?;
    let rec = prev.file(id);
    (rec.hash == *hash).then_some(rec)
}

/// Callable and type names declared by `facts` (input for rule 4 of [`semantic_requery`]).
pub fn declared_names<'f>(facts: impl IntoIterator<Item = &'f FileFacts>) -> HashSet<String> {
    facts
        .into_iter()
        .flat_map(|f| f.declarations.iter().map(|d| d.name.clone()))
        .collect()
}

/// Declaration names that appeared or disappeared in this update (input for rule 4 of
/// [`semantic_requery`]): for every added / changed file the names its new facts declare
/// but its previous facts did not, and the reverse; every name a removed file declared.
/// `dirty` = (path, new facts) of every added / changed file. An interface-stable file
/// declares the same names, so it contributes nothing.
pub fn changed_declaration_names<'f>(
    prev: Option<&Index>,
    plan: &UpdatePlan,
    dirty: impl IntoIterator<Item = (&'f str, Option<&'f FileFacts>)>,
) -> HashSet<String> {
    let old_facts = |path: &str| {
        prev.and_then(|p| p.file_by_path(path).map(|id| p.file(id)))
            .and_then(|rec| rec.facts.as_ref())
    };
    let mut out = HashSet::new();
    for (path, facts) in dirty {
        let new = declared_names(facts);
        let old = declared_names(old_facts(path));
        out.extend(new.symmetric_difference(&old).cloned());
    }
    for path in &plan.removed {
        out.extend(declared_names(old_facts(path)));
    }
    out
}

/// Files whose interface changed in this update: added files, removed files, and changed
/// files whose new `FileFacts::interface` differs from the previous one (or whose facts are
/// missing on either side). `dirty` = (path, new facts) of every added / changed file.
pub fn interface_changed<'f>(
    prev: Option<&Index>,
    plan: &UpdatePlan,
    dirty: impl IntoIterator<Item = (&'f str, Option<&'f FileFacts>)>,
) -> HashSet<String> {
    let mut out: HashSet<String> = plan.removed.iter().cloned().collect();
    for (path, facts) in dirty {
        let old = prev
            .and_then(|p| p.file_by_path(path).map(|id| p.file(id)))
            .and_then(|rec| rec.facts.as_ref());
        let same = matches!((old, facts), (Some(o), Some(n)) if o.interface == n.interface);
        if !same {
            out.insert(path.to_string());
        }
    }
    out
}

/// What happens to stale dependents in one update.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum StalePolicy {
    /// Re-query every dependent now (in-process commands and `trace index`: the answer that
    /// follows is always from a fully fresh graph).
    #[default]
    ResolveAll,
    /// `trace index --watch`: dependents of an interface change become stale; only the stale
    /// files in `resolve` (the next batch) are re-queried now.
    Defer { resolve: BTreeSet<String> },
}

impl StalePolicy {
    /// Whether a stale / dependent file is re-queried in this update.
    pub fn resolves(&self, path: &str) -> bool {
        match self {
            StalePolicy::ResolveAll => true,
            StalePolicy::Defer { resolve } => resolve.contains(path),
        }
    }
}

/// Result of [`semantic_requery`] for one backend partition.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Requery {
    /// Files re-queried in this update.
    pub now: HashSet<String>,
    /// Dependents left (or kept) stale: their cached semantics may be outdated and must not
    /// be served until they are resolved.
    pub stale: HashSet<String>,
}

/// Inputs of [`semantic_requery`] for one backend partition.
pub struct RequeryInput<'a> {
    pub prev: Option<&'a Index>,
    pub plan: &'a UpdatePlan,
    /// Languages served by the backend.
    pub partition: &'a [Language],
    /// Every current source `(path, language)`.
    pub current_files: &'a [(String, Language)],
    /// The backend's current fingerprint.
    pub tool_fingerprint: &'a str,
    /// Declaration names that appeared or disappeared ([`changed_declaration_names`]).
    pub declared_names: &'a HashSet<String>,
    /// [`interface_changed`] of this update.
    pub interface_changed: &'a HashSet<String>,
    pub policy: &'a StalePolicy,
}

/// Files of one backend partition that must be (re)queried now, and the dependents left
/// stale (module docs, rules 1-5).
pub fn semantic_requery(input: RequeryInput<'_>) -> Requery {
    let RequeryInput {
        prev,
        plan,
        partition,
        current_files,
        tool_fingerprint,
        declared_names,
        interface_changed,
        policy,
    } = input;
    let all = current_files
        .iter()
        .filter(|(_, l)| partition.contains(l))
        .map(|(p, _)| p.as_str());
    let Some(prev) = prev else {
        return Requery {
            now: all.map(str::to_owned).collect(),
            stale: HashSet::new(),
        };
    };
    if plan.configs_changed {
        return Requery {
            now: all.map(str::to_owned).collect(),
            stale: HashSet::new(),
        };
    }
    let dirty: HashSet<&str> = plan.dirty().collect();
    // Only removed files and dirty files whose interface changed invalidate dependents.
    let changed_interfaces: HashSet<&str> = plan
        .dirty()
        .filter(|p| interface_changed.contains(*p))
        .chain(plan.removed.iter().map(String::as_str))
        .collect();
    // A uid is `{path}:{qualified}`; paths never contain ':' (see inventory), so the file
    // part is everything before the first ':'.
    let targets_changed = |uid: &str| -> bool {
        !changed_interfaces.is_empty()
            && uid
                .split_once(':')
                .is_some_and(|(p, _)| changed_interfaces.contains(p))
    };
    let mut out = Requery::default();
    for path in all {
        if dirty.contains(path) || needs_requery_now(prev, path, tool_fingerprint) {
            out.now.insert(path.to_owned());
            continue;
        }
        let dependent =
            prev.stale.contains(path) || is_dependent(prev, path, &targets_changed, declared_names);
        if !dependent {
            continue;
        }
        if policy.resolves(path) {
            out.now.insert(path.to_owned());
        } else {
            out.stale.insert(path.to_owned());
        }
    }
    out
}

/// Rule 1 for a file that is not dirty: no cached record, no cached semantics, or another
/// tool fingerprint.
fn needs_requery_now(prev: &Index, path: &str, tool_fingerprint: &str) -> bool {
    let Some(id) = prev.file_by_path(path) else {
        return true;
    };
    match &prev.file(id).semantic {
        None => true,
        Some(sem) => sem.tool_fingerprint != tool_fingerprint,
    }
}

/// Rules 3 and 4: the file's cached semantics depend on a changed interface.
fn is_dependent(
    prev: &Index,
    path: &str,
    targets_changed: &impl Fn(&str) -> bool,
    declared_names: &HashSet<String>,
) -> bool {
    let Some(id) = prev.file_by_path(path) else {
        return false;
    };
    let rec = prev.file(id);
    let Some(sem) = &rec.semantic else {
        return false;
    };
    let targets_dirty = targets_changed;
    let stale_target = sem.edges.iter().any(|e| targets_dirty(e.target.as_str()))
        || sem.value_refs.iter().any(|r| targets_dirty(r.target.as_str()))
        || sem
            .implementations
            .iter()
            .any(|i| targets_dirty(i.implementor.as_str()))
        || sem
            .unresolved
            .iter()
            .any(|u| u.candidates.iter().any(|c| targets_dirty(c.as_str())));
    if stale_target {
        return true;
    }
    if declared_names.is_empty() {
        return false;
    }
    let Some(facts) = &rec.facts else {
        return true;
    };
    mentions_any(facts, declared_names)
}

/// Rule 4: a call member / callee (last name segment) or a value reference of `facts` is
/// one of `names`.
fn mentions_any(facts: &FileFacts, names: &HashSet<String>) -> bool {
    let last = |callee: &str| -> String {
        callee
            .rsplit(['.', ':', '>', ' '])
            .find(|s| !s.is_empty())
            .unwrap_or(callee)
            .to_string()
    };
    facts.calls.iter().any(|c| match &c.member {
        Some(m) => names.contains(m),
        None => names.contains(&last(&c.callee)),
    }) || facts.references.iter().any(|r| names.contains(&r.name))
        || facts.callbacks.iter().any(|cb| names.contains(&cb.name))
}

/// Everything [`index_delta`] reads about one update.
pub struct DeltaInput<'a> {
    pub prev: Option<&'a Index>,
    pub plan: &'a UpdatePlan,
    /// Full rebuild: no previous index, `IndexMode::Rebuild`, or an extractor / inference /
    /// bridge / schema version change.
    pub full: bool,
    /// The file records of the new index (all of them, or at least every added, changed and
    /// re-queried one).
    pub records: &'a [FileRecord],
    /// Files re-queried in this update (union of [`Requery::now`] over the partitions).
    pub requeried: &'a HashSet<String>,
    /// [`interface_changed`] of this update.
    pub interface_changed: &'a HashSet<String>,
    /// Files stale after this update (union of [`Requery::stale`] over the partitions).
    pub stale: &'a HashSet<String>,
}

/// The [`IndexDelta`] of one update: file sets from the plan, symbol uids
/// that appeared / disappeared / may have a changed header (every surviving uid of an
/// interface-changed file), and the complete stale set after the update.
pub fn index_delta(input: DeltaInput<'_>) -> IndexDelta {
    let DeltaInput {
        prev,
        plan,
        full,
        records,
        requeried,
        interface_changed,
        stale,
    } = input;
    let stale: BTreeSet<String> = stale.iter().cloned().collect();
    let Some(prev) = prev.filter(|_| !full) else {
        return IndexDelta {
            stale,
            ..IndexDelta::full()
        };
    };
    let mut delta = IndexDelta {
        added: plan.added.iter().cloned().collect(),
        modified: plan.changed.iter().cloned().collect(),
        removed: plan.removed.iter().cloned().collect(),
        requeried: requeried.iter().cloned().collect(),
        stale,
        ..IndexDelta::default()
    };
    let new_records: HashMap<&str, &FileRecord> = records.iter().map(|r| (r.path.as_str(), r)).collect();
    for path in interface_changed {
        let touched =
            delta.added.contains(path) || delta.modified.contains(path) || delta.removed.contains(path);
        if !touched {
            continue;
        }
        delta.interface_changed.insert(path.clone());
        let old: BTreeSet<&str> = prev
            .file_by_path(path)
            .map(|id| prev.symbols_of(id).iter().map(|s| s.uid.as_str()).collect())
            .unwrap_or_default();
        let new: BTreeSet<String> = match (delta.removed.contains(path), new_records.get(path.as_str())) {
            (false, Some(rec)) => rec
                .facts
                .as_ref()
                .map(|f| uids_of(path, f).into_iter().collect())
                .unwrap_or_default(),
            _ => BTreeSet::new(),
        };
        for uid in &new {
            if old.contains(uid.as_str()) {
                delta.symbols_changed.insert(uid.clone());
            } else {
                delta.symbols_added.insert(uid.clone());
            }
        }
        for uid in old {
            if !new.contains(uid) {
                delta.symbols_removed.insert(uid.to_string());
            }
        }
    }
    delta
}

/// Symbol uids assembly gives the declarations of `facts` in `path` (declaration order).
pub fn uids_of(path: &str, facts: &FileFacts) -> Vec<String> {
    let mut seen: HashMap<&str, u32> = HashMap::new();
    facts
        .declarations
        .iter()
        .map(|d| {
            let occurrence = seen.entry(d.qualified_name.as_str()).or_insert(0);
            *occurrence += 1;
            symbol_uid(path, &d.qualified_name, *occurrence)
        })
        .collect()
}

#[cfg(test)]
#[path = "../tests/unit/incremental.rs"]
mod tests;
