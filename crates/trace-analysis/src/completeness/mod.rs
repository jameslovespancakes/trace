//! "Is anything missing?" (NEXT.md item 5, SPEC §10.1 `completeness`).
//!
//! **Name completeness** (`uses`, `uses --deep`): every syntax occurrence of the target
//! family's names in indexed files is an occurrence — calls by `CallSite::member`
//! (identifier span = the callee's last segment), `Reference`s by name (local bindings
//! included: they are counted, never listed), import bindings, re-exports and declarations.
//! Each occurrence is classified ([`decide`]); target evidence wins, except at local-binding
//! occurrences, where only proven evidence (or a row of the answer) names the target:
//! * `resolved_to_target`: a span the caller counts as the target
//!   (`NameQuery::target_spans`: every `uses` row, so no site is ever both a row and
//!   unresolved), a *proven* edge of any kind into a family member whose evidence span ends
//!   at the occurrence (`at.end == occurrence.end`, `at.start <= occurrence.start`: callee
//!   spans end at the member identifier), an inferred edge into a family member (unless the
//!   member-binding rule rejects the link, below), or the declaration of a family member;
//! * resolved elsewhere, with a reason (`Completeness::elsewhere_reasons`, SPEC §10.1):
//!   - `local_binding`: the occurrence is a local / parameter binding or a use of one
//!     (`FileFacts::is_local`, `Reference::local`: lexical shadowing); a non-proven link
//!     there (a name-matched candidate of another file) never names the target;
//!   - `server`: a *proven* edge to another symbol, an `external_or_ambiguous` entry
//!     without a family candidate, or a span the server recorded in
//!     `FileSemantics::resolved_elsewhere` (library / builtin / local binding);
//!   - `member_binding`: a member access (`this.x`, `obj.x`, `self.x`) of a free /
//!     exported function `x`, or a bare name of a member, where the language defines no such
//!     binding ([`Bindings`]);
//!   - `unrelated_type`: a member access whose receiver's declared / constructed /
//!     annotated type, or the type the server resolved the receiver identifier to, is
//!     provably outside the family (`trace_infer::types`);
//!   - `other_language`: the occurrence is in a language that cannot name any family
//!     member without a bridge (`trace_infer::narrow::name_interop`; contract files never);
//!     a bridge edge to the family (any tier) keeps it;
//!   - `library_object`: a call whose receiver value provably comes only from objects a
//!     library created and repository code never extended (`Index::library_receivers`,
//!     filled by value flow);
//!   - `other_scope`: a bare name that a nearer binding of its file denotes (a same-file
//!     declaration of that name visible at the occurrence, or an import binding the name
//!     that is proven to bind something outside the family: a proven `imports` edge to
//!     another symbol, or a server answer outside the index); Python, JavaScript /
//!     TypeScript modules (no wildcard import, no module-level rebinding of the name), Go
//!     (same file);
//!   - `arity`: a call whose plain positional arguments no family member of that name
//!     accepts (`trace_infer::narrow::arity_rules_out`; Python, Java, Rust, PHP);
//!   - `other_declaration`: another symbol's declaration;
//!
//!   The syntax rules (`unrelated_type`, `library_object`, `other_scope`, `arity`) never
//!   overrule conflicting evidence: an occurrence with a possible edge to a family member
//!   that value flow delivered (the site's `flow_candidates`), or a call the server reported
//!   as a dispatch through a library-declared member with a family member among its
//!   implementations (`FileSemantics::library_dispatch`), stays listed (`possible_only`).
//!   Such a library dispatch is never `server` either: it is the target (decided dispatch
//!   edge) or `possible_only`;
//! * otherwise unresolved, with the reason: the unresolved entry's kind
//!   (`no_semantic_target`, `unresolved_signature`, `external_or_ambiguous`),
//!   `possible_only` (only a possible edge reaches the family), `inferred_elsewhere` (only
//!   an inferred edge to another symbol — never counted as proof), `not_analyzed` (a file of
//!   a pending language / sub-project, set up on first use), `outside_build` (the server
//!   says the file is not part of the build on this machine), else `no_semantic_target`.
//!   Import / export statement facts whose span encloses a reference occurrence are
//!   deduplicated against it; they are resolved by edges inside their span.
//!
//! Status: `unknown` when a bound was hit (family cut at 64 members, traversal bounds) or
//! the name occurs in inventoried files without syntax facts (contract files excluded; a
//! byte-substring presence check on hash-verified sources, never fact extraction);
//! `partial` when anything is unresolved (the summary says exactly what is left:
//! `partial: 12 same-name sites unresolved (8 calls, 3 reads, 1 import; 7 in 2 files not
//! analyzed yet (Scala); run a query on one of them to set it up)`); else `complete`. Only proven evidence or a language rule can make an
//! occurrence "elsewhere", so a `complete` claim never rests on a guess.
//!
//! Unresolved sites of `uses` are all listed and ranked ([`rank_unresolved`]: same module,
//! files importing the target, name-only); `deps` keeps [`MAX_UNRESOLVED`] in (file, line)
//! order.
//!
//! **Call completeness** (`deps`): calls inside the reached set — call sites owned
//! by reached symbols; unresolved = `Index::unresolved` entries of reached owners not
//! covered by a traversable edge at tier <= min(include, inferred), except proven external
//! targets (`external_or_ambiguous` without in-index candidates: resolved elsewhere). A
//! call the server reported as a dispatch through a library-declared member with in-index
//! implementations (`FileSemantics::library_dispatch`) is never external: without a
//! covering edge it is unresolved (`possible_only`).
//!
//! **Family candidates** ([`family_candidate_edges`], query time only): call sites whose
//! every candidate is a member of the queried family are uses of the family (inferred).
//!
//! Files: the name scan here; `occurrences` (collection), `decide` (classification rules),
//! `access` / `bindings` (member-binding rule), `family` (family names and candidate edges),
//! `summary` (status, summary, ranking), `calls` (call completeness of `deps`).

use std::cell::OnceCell;
use std::collections::{BTreeSet, HashMap, HashSet};

use trace_core::model::{ByteSpan, Edge, EdgeKind, FileId, SymbolId, UnresolvedKind};
use trace_core::source::SourceStore;
use trace_core::tiers::FAMILY;
use trace_core::{Graph, Index, Language, SupportLevel, Tier};
use trace_infer::hierarchy::Hierarchy;

use crate::cards::{line_at, truncate_chars};
use crate::report::{At, Completeness, UnresolvedMatch};

mod access;
mod bindings;
mod calls;
mod decide;
mod family;
mod occurrences;
mod summary;

pub(crate) use access::access_at;
pub use access::{is_member, member_span};
pub use bindings::Bindings;
pub(crate) use calls::calls_completeness;
use decide::{arity_excludes, decide, import_edges, nearer_binding, other_language, ImportEdges};
pub(crate) use family::{
    family_candidate_edges, family_names, family_resolution, FAMILY_FLOW_REASON, FAMILY_OVERLOAD_REASON,
};
use occurrences::collect;
pub(crate) use summary::rank_unresolved;
use summary::summarize;

/// Unresolved matches listed per `deps` answer (the summary always counts all of them;
/// `uses` lists every site).
pub(crate) const MAX_UNRESOLVED: usize = 1000;
/// Characters of line text in an unresolved match.
pub(crate) const MAX_TEXT: usize = 240;

/// `Completeness::elsewhere_reasons` keys (SPEC §10.1).
pub(crate) const SERVER: &str = "server";
pub(crate) const LOCAL_BINDING: &str = "local_binding";
pub(crate) const UNRELATED_TYPE: &str = "unrelated_type";
pub(crate) const MEMBER_BINDING: &str = "member_binding";
pub(crate) const OTHER_DECLARATION: &str = "other_declaration";
pub(crate) const OTHER_LANGUAGE: &str = "other_language";
pub(crate) const LIBRARY_OBJECT: &str = "library_object";
pub(crate) const OTHER_SCOPE: &str = "other_scope";
pub(crate) const ARITY: &str = "arity";

/// Rank groups of `uses` unresolved sites (SPEC §10.1), in order.
pub(crate) const SAME_MODULE: &str = "same_module";
pub(crate) const IMPORTS_TARGET: &str = "imports_target";
pub(crate) const NAME_ONLY: &str = "name_only";

/// How one occurrence was resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Target,
    /// Resolved to something else; the reason is an `elsewhere_reasons` key.
    Elsewhere(&'static str),
    Unresolved(&'static str),
}

/// How an occurrence names its symbol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Access {
    /// A bare identifier (`f()`, `x = f`).
    Bare,
    /// The member part of a member access on a value (`obj.f`, `this.f`, `self.f`).
    Member {
        /// Root identifier of the receiver (`obj` in `obj.a.f`); `None` for the self
        /// reference and for other receiver expressions.
        receiver_root: Option<String>,
        self_receiver: bool,
    },
    /// Static paths (`Type::f`, `pkg::f`), statements, declarations, or no facts to tell.
    Unknown,
}

/// A syntax occurrence of a family name.
#[derive(Clone, Debug)]
pub(crate) struct Occurrence {
    pub file: FileId,
    /// Identifier span (import / export facts: the statement span).
    pub span: ByteSpan,
    /// 1-based line when known from facts (0 = compute from the source).
    pub line: u32,
    /// `call` | `read` | `write` | `import` | `reexport` | `callback` | `declaration`.
    pub kind: &'static str,
    /// Executing symbol containing the occurrence.
    pub owner: Option<SymbolId>,
    pub state: State,
    /// The name as written.
    pub name: String,
    pub access: Access,
    /// A local / parameter binding or a use of one (lexical shadowing).
    pub local: bool,
    /// Import / export statement fact (matched by enclosure, deduplicated).
    statement: bool,
    /// Call occurrences: index of the syntax call in its file's `FileFacts::calls`.
    call: Option<u32>,
}

/// Evidence gathered for one occurrence.
#[derive(Clone, Copy, Default)]
struct Evidence {
    /// A span the caller counts as the target (a row of the answer).
    forced: bool,
    /// A proven edge into a family member.
    target_proven: bool,
    /// A non-proven edge into a family member at tier <= min(include, inferred).
    target: bool,
    /// A proven edge to another symbol.
    elsewhere: bool,
    /// An `external_or_ambiguous` entry without a family candidate.
    external: bool,
    /// The server recorded the span as resolved outside the index / to a local binding.
    resolved_elsewhere: bool,
    possible_target: bool,
    /// A possible edge to a family member that value flow delivered (not field-name only).
    flow_target: bool,
    /// A possible bridge edge to a family member.
    possible_bridge: bool,
    /// The server reported a dispatch through a library-declared member whose
    /// implementations include a family member.
    library_dispatch: bool,
    /// The occurrence's language can name no family member without a bridge.
    other_language: bool,
    inferred_elsewhere: bool,
    unresolved: Option<&'static str>,
}

/// What a name scan needs besides the graph.
pub(crate) struct NameQuery<'a> {
    /// Target plus its family (targets first).
    pub family: &'a [SymbolId],
    pub include: Tier,
    /// Why the answer is bounded (e.g. `family cut at 64 members`), if it is.
    pub bounded: Option<String>,
    /// Spans that count as the target: the identifier span of every `uses` row (rows are
    /// never unresolved, SPEC §10.3).
    pub target_spans: &'a [(FileId, ByteSpan)],
}

/// Occurrences with their states plus the completeness summary.
pub(crate) struct NameScan {
    pub completeness: Completeness,
}

/// Classify every occurrence of the family's names and summarize (module docs).
pub(crate) fn name_scan(graph: &Graph<'_>, sources: &SourceStore<'_>, query: &NameQuery<'_>) -> NameScan {
    let index = graph.index;
    let family: HashSet<SymbolId> = query.family.iter().copied().collect();
    let family_uids: HashSet<&str> = query.family.iter().map(|&m| index.symbol(m).uid.as_str()).collect();
    let family_languages: BTreeSet<Language> =
        query.family.iter().map(|&m| index.symbol(m).language).collect();
    let names = family_names(index, query.family);
    let mut occ = collect(index, &names, &family);
    let threshold = query.include.min(Tier::Inferred);

    let mut by_end: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    let mut statements: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, o) in occ.iter().enumerate() {
        if o.statement {
            statements.entry(o.file.0).or_default().push(i);
        } else {
            by_end.entry((o.file.0, o.span.end)).or_default().push(i);
        }
    }
    let hits = |file: FileId, span: ByteSpan, occ: &[Occurrence]| -> Vec<usize> {
        let mut out: Vec<usize> = by_end
            .get(&(file.0, span.end))
            .map(|v| {
                v.iter()
                    .copied()
                    .filter(|&i| span.start <= occ[i].span.start)
                    .collect()
            })
            .unwrap_or_default();
        if let Some(list) = statements.get(&file.0) {
            out.extend(list.iter().copied().filter(|&i| occ[i].span.encloses(span)));
        }
        out
    };
    // A possible edge that value flow delivered (the site's flow candidates, not
    // field-name-only evidence).
    let flow_delivered = |e: &Edge| -> bool {
        e.site
            .and_then(|s| index.sites.get(s as usize))
            .is_some_and(|s| s.flow_candidates.contains(&e.to) && !s.field_only.contains(&e.to))
    };
    let mut ev = vec![Evidence::default(); occ.len()];
    for e in graph.edges() {
        if FAMILY.contains(e.kind) {
            continue;
        }
        for i in hits(e.at.file, e.at.bytes, &occ) {
            let x = &mut ev[i];
            if family.contains(&e.to) {
                if e.tier == Tier::Proven {
                    x.target_proven = true;
                } else if e.tier <= threshold {
                    x.target = true;
                } else {
                    x.possible_target = true;
                    x.possible_bridge |= e.kind == EdgeKind::Bridge;
                    x.flow_target |= flow_delivered(e);
                }
            } else if e.tier == Tier::Proven {
                x.elsewhere = true;
            } else {
                x.inferred_elsewhere = true;
            }
        }
    }
    for u in &index.unresolved {
        let family_candidate = u.candidates.iter().any(|c| family.contains(c));
        for i in hits(u.at.file, u.at.bytes, &occ) {
            let x = &mut ev[i];
            if u.kind == UnresolvedKind::ExternalOrAmbiguous && !family_candidate {
                x.external = true;
            } else if x.unresolved.is_none() {
                x.unresolved = Some(u.kind.as_str());
            }
        }
    }
    for &(file, span) in query.target_spans {
        for i in hits(file, span, &occ) {
            ev[i].forced = true;
        }
        // A row span may cover the whole call (`banner()`) while the occurrence is the name
        // alone: an occurrence starting at the row's start byte is that row (rows and
        // unresolved entries are keyed by file + start byte, so no site is ever both).
        for (i, o) in occ.iter().enumerate() {
            if o.file == file && o.span.start == span.start {
                ev[i].forced = true;
            }
        }
    }
    for (o, x) in occ.iter().zip(ev.iter_mut()) {
        x.other_language = other_language(index.file(o.file).language, &family_languages);
        let Some(semantic) = &index.file(o.file).semantic else { continue };
        let ends_here = |at: ByteSpan| at.end == o.span.end && at.start <= o.span.start;
        x.resolved_elsewhere = semantic.resolved_elsewhere.iter().any(|&r| ends_here(r));
        x.library_dispatch = semantic
            .library_dispatch
            .iter()
            .any(|d| ends_here(d.at) && d.implementations.iter().any(|u| family_uids.contains(u.as_str())));
    }

    // Library-created receivers (value flow): callee spans by (file, end) -> starts.
    let mut library_receivers: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    for r in &index.library_receivers {
        library_receivers
            .entry((r.at.file.0, r.at.bytes.end))
            .or_default()
            .push(r.at.bytes.start);
    }
    let bindings = Bindings::new(index, query.family);
    let hierarchy: OnceCell<Hierarchy> = OnceCell::new();
    let mut types: Option<trace_infer::types::Types<'_>> = None;
    let imports: OnceCell<ImportEdges> = OnceCell::new();
    for (o, x) in occ.iter_mut().zip(&ev) {
        if o.kind == "declaration" {
            continue;
        }
        let state = {
            let o: &Occurrence = o;
            let fallback = file_fallback(index.file(o.file));
            let member_binding = bindings.excludes_name(o.file, o.span, &o.access, &o.name);
            let mut negative = || -> Option<&'static str> {
                if o.kind == "call"
                    && library_receivers
                        .get(&(o.file.0, o.span.end))
                        .is_some_and(|starts| starts.iter().any(|&s| s <= o.span.start))
                {
                    return Some(LIBRARY_OBJECT);
                }
                if matches!(o.access, Access::Member { .. }) {
                    let h = hierarchy.get_or_init(|| Hierarchy::build(index));
                    let t = types.get_or_insert_with(|| trace_infer::types::Types::new(index, h));
                    let ty = t.receiver_type(o.file, o.span);
                    if t.unrelated_to_family(&ty, query.family) {
                        return Some(UNRELATED_TYPE);
                    }
                }
                if o.access == Access::Bare {
                    let imports = imports.get_or_init(|| import_edges(graph));
                    if nearer_binding(index, imports, &family, o) {
                        return Some(OTHER_SCOPE);
                    }
                }
                if arity_excludes(index, query.family, o) {
                    return Some(ARITY);
                }
                None
            };
            decide(x, o.local, member_binding, &mut negative, fallback)
        };
        o.state = state;
    }
    occ.sort_by_key(|o| (o.file, o.span.start, o.span.end));

    let unknown_files = files_without_facts(index, sources, &names);
    let completeness = summarize(index, sources, &occ, query.bounded.as_deref(), unknown_files, false);
    NameScan { completeness }
}

/// Unresolved reason of an occurrence nothing else decided, from its file: `not_analyzed`
/// (pending language / sub-project, or no semantic results), `outside_build` (the server
/// says the file is not part of the build on this machine), else `no_semantic_target`.
fn file_fallback(rec: &trace_core::FileRecord) -> &'static str {
    if rec.support == SupportLevel::Pending {
        return "not_analyzed";
    }
    match &rec.semantic {
        None => "not_analyzed",
        Some(s) if s.outside_build.is_some() => "outside_build",
        Some(_) => "no_semantic_target",
    }
}

/// Inventoried files without syntax facts (contract files excluded) whose verified bytes
/// contain one of `names`; unreadable files count too (they cannot be ruled out).
fn files_without_facts(index: &Index, sources: &SourceStore<'_>, names: &BTreeSet<String>) -> usize {
    index
        .files
        .iter()
        .enumerate()
        .filter(|(_, rec)| rec.facts.is_none() && !rec.language.is_contract())
        .filter(|(fi, _)| match sources.file(FileId(*fi as u32)) {
            Ok(f) => names
                .iter()
                .any(|n| !n.is_empty() && f.bytes.windows(n.len()).any(|w| w == n.as_bytes())),
            Err(_) => true,
        })
        .count()
}

/// The unresolved match of an occurrence (line text best effort: empty when the source
/// cannot be read).
fn unresolved_match(
    index: &Index,
    sources: &SourceStore<'_>,
    o: &Occurrence,
    reason: &'static str,
) -> UnresolvedMatch {
    let (line, text) = match line_at(sources, o.file, o.span.start) {
        Ok((line, _, text)) => (line, truncate_chars(text.trim(), MAX_TEXT).to_string()),
        Err(_) => (o.line, String::new()),
    };
    UnresolvedMatch {
        at: At {
            file: index.file_path(o.file).to_string(),
            line: if o.line > 0 { o.line } else { line },
            start_byte: o.span.start,
            end_byte: o.span.end,
        },
        owner: o.owner.map(|s| index.symbol(s).uid.clone()),
        kind: o.kind,
        text,
        reason,
        // (file, line) order here; `uses` re-ranks ([`rank_unresolved`]).
        rank: 0,
        scope: NAME_ONLY,
    }
}

#[cfg(test)]
#[path = "../../tests/unit/completeness/mod.rs"]
mod tests;
