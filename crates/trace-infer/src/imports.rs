//! Import-path rule (general fixes plan, rule 4; SPEC §7.12): an import / use / re-export
//! statement whose qualified path names exactly one declaration of the index is a proven
//! use of that declaration.
//!
//! For every `Import` fact (kinds `Member` and `Module` whose last segment names a
//! declaration; never `Wildcard`) and every `Export` fact (never `*`) of a file:
//! 1. resolve the path's module part with the module path rules of
//!    [`crate::narrow::ModuleMap`] (Python relative / absolute modules, JS/TS specifiers,
//!    Rust `crate::` / `super::` / workspace crate names, Java/Scala/C# packages and
//!    namespaces, Go import paths, PHP namespaces, Haskell module names), plus one re-export
//!    hop;
//! 2. among the declarations of the resolved files, keep those whose qualified name equals
//!    the path's member part (`TestUtil.randomBytes` for Java
//!    `import static pkg.TestUtil.randomBytes`, `redirect` for Python
//!    `from .helpers import redirect`);
//! 3. exactly one survivor -> a proven edge `imports` (or `reexports` for export facts),
//!    `from` = the executing owner of the statement (`<module>` for module-level imports),
//!    `to` = the survivor, `at` = the imported name's identifier span (the `Reference` of
//!    kind `import` / `export` at that statement; the statement span when there is none),
//!    provider `rule:import-path`, resolution [`trace_core::Resolution::ImportPath`].
//!
//! Zero or several survivors produce nothing (the site stays unresolved and is listed under
//! `check:`). Sites that already carry a proven `imports` / `reexports` edge from a server
//! at the same span are skipped (evidence is never duplicated). Syntax facts only; no
//! source text is read. Called by the pipeline right after the family phase (4b).
//!
//! Details:
//! * `Module` imports name modules, never declarations: they are skipped.
//! * A binding that is both an import and a re-export (Rust `pub use`) is one
//!   site: the `reexports` edge.
//! * The re-export hop follows `Export` facts of the resolved files whose exported name is
//!   the member's first segment (and JS/TS `export *`), and, for Python packages, the
//!   module-level imports of the package file binding that name
//!   (`from .helpers import redirect` in `__init__.py`).

use std::collections::{BTreeSet, HashMap, HashSet};

use trace_core::facts::{FileFacts, ImportKind, RefKind, Reference};
use trace_core::{
    ByteSpan, Edge, EdgeKind, FileId, Index, Language, Location, Provider, Resolution, SymbolId, SymbolKind,
    Tier,
};
use trace_syntax::language_rules::{rules, ModulePaths};

use crate::narrow::{name_interop, ModuleMap};

/// Provider rule name (`rule:import-path`).
pub const RULE: &str = "import-path";

/// Incremental import-path edges (PLAN decision 13, DESIGN §1.14.5): equal to
/// [`import_path_edges`] on the same index. `prev` = the previous import-path edges,
/// remapped to the new ids.
///
/// The edges of a file are a function of its import / export statements, the module map
/// (the file list), the declarations and re-export
/// statements of the files its statements resolve to (one re-export hop) and the proven
/// server import edges of the file. So a file's edges are computed again when the file
/// changed, when one of its server import edges names a touched symbol, or when a file its
/// statements read (resolved files and re-export hop targets) changed; every other file
/// keeps its previous edges. Added / removed files change the module map: then every file
/// is computed again.
pub fn import_path_edges_delta(
    index: &Index,
    delta: &trace_core::delta::IndexDelta,
    prev: &[Edge],
) -> Vec<Edge> {
    if delta.full || !delta.added.is_empty() || !delta.removed.is_empty() {
        return import_path_edges(index);
    }
    let changed: HashSet<FileId> = index
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| delta.file_changed(&f.path))
        .map(|(i, _)| FileId(i as u32))
        .collect();
    let modules = ModuleMap::new(index);
    let proven = proven_spans(index);
    let mut kept: HashMap<FileId, Vec<&Edge>> = HashMap::new();
    for e in prev {
        kept.entry(e.at.file).or_default().push(e);
    }
    let mut out: Vec<Edge> = Vec::new();
    for fi in 0..index.files.len() {
        let file = FileId(fi as u32);
        let dirty = changed.contains(&file)
            || server_imports_touched(index, file, delta)
            || reads_any(index, &modules, file, &changed);
        if dirty {
            out.extend(file_edges(index, &modules, &proven, file));
        } else if let Some(edges) = kept.remove(&file) {
            out.extend(edges.into_iter().cloned());
        }
    }
    out
}

/// Proven import / re-export edges by the import-path rule (module docs).
pub fn import_path_edges(index: &Index) -> Vec<Edge> {
    let modules = ModuleMap::new(index);
    let proven = proven_spans(index);
    let mut out: Vec<Edge> = Vec::new();
    for fi in 0..index.files.len() {
        out.extend(file_edges(index, &modules, &proven, FileId(fi as u32)));
    }
    out
}

/// (file, start, end) of every proven server `imports` / `reexports` edge.
fn proven_spans(index: &Index) -> HashSet<(FileId, u32, u32)> {
    index
        .edges
        .iter()
        .filter(|e| matches!(e.kind, EdgeKind::Imports | EdgeKind::Reexports) && e.tier == Tier::Proven)
        .map(|e| (e.at.file, e.at.bytes.start, e.at.bytes.end))
        .collect()
}

/// Whether a server import / re-export edge of `file` names a symbol the delta touched (the
/// link may have dropped or re-targeted it, which changes the skipped spans).
fn server_imports_touched(index: &Index, file: FileId, delta: &trace_core::delta::IndexDelta) -> bool {
    index.file(file).semantic.as_ref().is_some_and(|s| {
        s.edges.iter().any(|e| {
            matches!(e.kind, EdgeKind::Imports | EdgeKind::Reexports) && delta.symbol_touched(&e.target)
        })
    })
}

/// Targets of the import / export statements of a file the rule reads (the statement
/// filter of [`file_edges`]).
fn statements(facts: &FileFacts) -> Vec<&str> {
    let mut out: Vec<&str> = facts
        .exports
        .iter()
        .filter(|e| e.exported != "*")
        .map(|e| e.target.as_str())
        .collect();
    for i in &facts.imports {
        match i.kind {
            ImportKind::Wildcard | ImportKind::Module => continue,
            _ => out.push(i.target.as_str()),
        }
    }
    out
}

/// Whether a statement of `file` resolves to (or hops through) a file in `changed`.
fn reads_any(index: &Index, modules: &ModuleMap, file: FileId, changed: &HashSet<FileId>) -> bool {
    if changed.is_empty() {
        return false;
    }
    let record = index.file(file);
    let Some(facts) = &record.facts else {
        return false;
    };
    let path = record.path.as_str();
    let language = record.language;
    for target in statements(facts) {
        for (files, member) in modules.member_paths(path, language, target) {
            if files.iter().any(|f| changed.contains(f)) {
                return true;
            }
            let head = member.split_once('.').map_or(member.as_str(), |(h, _)| h);
            for &f in &files {
                let Some(ff) = index.file(f).facts.as_ref() else {
                    continue;
                };
                let fpath = index.file_path(f);
                let flang = index.file(f).language;
                for hop in reexport_targets(ff, flang, head) {
                    for (more, _) in modules.member_paths(fpath, flang, &hop) {
                        if more.iter().any(|m| changed.contains(m)) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// The import-path edges of one file (module docs).
fn file_edges(
    index: &Index,
    modules: &ModuleMap,
    proven: &HashSet<(FileId, u32, u32)>,
    file: FileId,
) -> Vec<Edge> {
    let record = index.file(file);
    let Some(facts) = &record.facts else {
        return Vec::new();
    };
    let path = record.path.as_str();
    let language = record.language;
    let names: Vec<&Reference> = facts
        .references
        .iter()
        .filter(|r| matches!(r.kind, RefKind::Import | RefKind::Export))
        .collect();
    let mut out: Vec<Edge> = Vec::new();
    let mut done: HashSet<(u32, u32)> = HashSet::new();
    let mut emit =
        |kind: EdgeKind, span: ByteSpan, line: u32, reference: Option<&Reference>, target: &str| {
            let at = reference.map_or(span, |r| r.span);
            if proven.contains(&(file, at.start, at.end)) || done.contains(&(at.start, at.end)) {
                return;
            }
            let owner = reference
                .and_then(|r| facts.executing_owner(r.owner))
                .or(facts.module_decl)
                .and_then(|d| record.symbol_of_decl(d));
            let Some(from) = owner else { return };
            let named = declarations_at_path(index, modules, path, language, target);
            let [to] = named.as_slice() else { return };
            if *to == from {
                return;
            }
            done.insert((at.start, at.end));
            out.push(Edge {
                from,
                to: *to,
                kind,
                tier: Tier::Proven,
                provider: Provider::Rule(RULE.into()),
                resolution: Resolution::ImportPath,
                at: Location {
                    file,
                    bytes: at,
                    line,
                },
                site: None,
                bridge: None,
            });
        };
    // Re-exports first: a `pub use` / `export` binding is one site, a re-export.
    for e in &facts.exports {
        if e.exported == "*" {
            continue;
        }
        let reference = name_reference(&names, e.span, &e.target, RefKind::Export);
        emit(EdgeKind::Reexports, e.span, e.line, reference, &e.target);
    }
    for i in &facts.imports {
        match i.kind {
            ImportKind::Wildcard | ImportKind::Module => continue,
            _ => {}
        }
        let reference = name_reference(&names, i.span, &i.target, RefKind::Import)
            .or_else(|| name_reference(&names, i.span, &i.target, RefKind::Export));
        let kind = match reference.map(|r| r.kind) {
            Some(RefKind::Export) => EdgeKind::Reexports,
            _ => EdgeKind::Imports,
        };
        emit(kind, i.span, i.line, reference, &i.target);
    }
    out
}

/// Last segment of an import target (`a.b.c`, `a::b::c`, `A\B\C`, `./m.f`).
fn last_name(target: &str) -> &str {
    let t = target.trim();
    let cut = [
        t.rfind("::").map(|i| i + 2),
        t.rfind('.').map(|i| i + 1),
        t.rfind('\\').map(|i| i + 1),
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(0);
    &t[cut..]
}

/// The `import` / `export` reference of a statement: inside its span, preferring the one
/// spelling the imported name.
fn name_reference<'r>(
    names: &[&'r Reference],
    span: ByteSpan,
    target: &str,
    kind: RefKind,
) -> Option<&'r Reference> {
    let inside: Vec<&'r Reference> = names
        .iter()
        .copied()
        .filter(|r| r.kind == kind && span.encloses(r.span))
        .collect();
    let wanted = last_name(target);
    inside
        .iter()
        .copied()
        .find(|r| r.name == wanted || last_name(&r.name) == wanted)
        .or_else(|| inside.first().copied())
}

/// Declarations an import / re-export `target` of a file of `language` at `from_path`
/// names exactly (module path rules of [`ModuleMap::member_paths`], plus one re-export
/// hop), sorted. The import-path rule proves a use only when there is exactly one; syntax
/// narrowing keeps exactly these candidates for an explicit single-name import.
pub(crate) fn declarations_at_path(
    index: &Index,
    modules: &ModuleMap,
    from_path: &str,
    language: Language,
    target: &str,
) -> Vec<SymbolId> {
    let mut found: BTreeSet<SymbolId> = BTreeSet::new();
    for (files, member) in modules.member_paths(from_path, language, target) {
        let before = found.len();
        collect(index, &files, &member, language, &mut found);
        if found.len() != before {
            continue;
        }
        // One re-export hop through the resolved module files.
        let (head, rest) = member.split_once('.').unwrap_or((member.as_str(), ""));
        for &f in &files {
            let Some(ff) = index.file(f).facts.as_ref() else {
                continue;
            };
            let fpath = index.file_path(f);
            let flang = index.file(f).language;
            for hop in reexport_targets(ff, flang, head) {
                for (more, m) in modules.member_paths(fpath, flang, &hop) {
                    let m = if rest.is_empty() { m } else { format!("{m}.{rest}") };
                    collect(index, &more, &m, flang, &mut found);
                }
            }
        }
    }
    found.into_iter().collect()
}

/// Targets a module file re-exports under `name`: `Export` facts, `export *` of relative
/// specifier modules (JS/TS), and module-level member imports of dotted modules (Python
/// packages).
fn reexport_targets(facts: &FileFacts, language: Language, name: &str) -> Vec<String> {
    let modules = rules(language).modules;
    let mut out = Vec::new();
    for e in &facts.exports {
        if e.exported == name {
            out.push(e.target.clone());
        } else if e.exported == "*" && modules == ModulePaths::RelativeSpecifiers {
            out.push(format!("{}.{name}", e.target));
        }
    }
    if modules == ModulePaths::DottedModules {
        for i in &facts.imports {
            if i.kind == ImportKind::Member && i.local == name && i.scope == trace_core::facts::Scope::Module
            {
                out.push(i.target.clone());
            }
        }
    }
    out
}

/// Declarations of `files` whose qualified name is `member` (never synthetic, never the
/// `<module>` scope), in `language`'s name namespace.
fn collect(
    index: &Index,
    files: &[FileId],
    member: &str,
    language: Language,
    found: &mut BTreeSet<SymbolId>,
) {
    for &f in files {
        for s in index.symbols_of(f) {
            if s.qualified_name == member
                && !s.is_synthetic()
                && s.kind != SymbolKind::Module
                && name_interop(language, s.language)
            {
                found.insert(s.id);
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/imports.rs"]
mod tests;
