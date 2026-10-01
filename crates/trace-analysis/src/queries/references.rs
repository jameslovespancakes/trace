//! The rows of `uses <symbol>`: every use of a symbol with its kind and the exact line text
//! (NEXT.md item 7, SPEC §10.3).
//!
//! 1. Resolve the selector (one grammar, exact matches only, [`Workspace::resolve_ready`]:
//!    a target in a pending language / sub-project is set up first).
//! 2. Family = [`target_family`]: the target plus overrides, implementations, declarations
//!    (prototypes, signatures, `.pyi` stubs) and overloads (<= 64 members).
//! 3. Rows are **proven and inferred only** (possible evidence is listed under
//!    `completeness.unresolved`; `--deep` does not add rows here):
//!    * index rows: incoming edges of every kind into each member at tier <= inferred
//!      (execution, deferred, reference kinds, bridges unless the workspace disables them),
//!      located at the identifier (callee spans narrowed to the member name; a callback
//!      decided at a site is located at its argument expression, [`callback_argument_span`]);
//!    * declaration rows: `declaration` for the target and its `declaration` / `overload`
//!      members, `override` / `implements` for the others (with `via`);
//!    * family rows ([`family_candidate_edges`]): value flow reaching only family members
//!      (`family_flow`) and sites / server ambiguities whose candidates are all family
//!      members (`family_overloads`).
//!
//!    Non-proven rows at a member-binding site ([`Bindings`]: a member access of a free
//!    function, a bare name of a member) or at a local-binding occurrence (the name denotes
//!    a local / parameter binding: lexical shadowing, `FileFacts::is_local`) are dropped;
//!    the name scan counts them as `member_binding` / `local_binding`. Proven rows
//!    (servers, language rules) are kept.
//! 4. One row per (file, start byte): stronger tier wins. Owner = executing symbol
//!    containing the use (synthetic `<module>` / `<lambda>` owners allowed); `via` = the
//!    family member the use resolves to when it is not the target. Rows are sorted by
//!    (file, line, column).
//! 5. Completeness ([`crate::completeness::name_scan`]) is always filled; every row's
//!    identifier span counts as the target, so no site is both a row and unresolved
//!    (asserted in debug builds). Unresolved sites are ranked
//!    ([`crate::completeness::rank_unresolved`]).

use std::collections::HashSet;

use trace_core::graph::{target_family, Family, FamilyRelation, MAX_FAMILY};
use trace_core::model::{ByteSpan, EdgeKind, FileId, SymbolId};
use trace_core::source::SourceStore;
use trace_core::tiers::FAMILY;
use trace_core::{Graph, Tier};

use crate::cards::{card, line_at, narrow_to_member, use_kind};
use crate::completeness::{
    access_at, family_candidate_edges, family_resolution, name_scan, rank_unresolved, Bindings, NameQuery,
};
use crate::report::{Card, Completeness, ReferenceRow};
use crate::workspace::Workspace;
use crate::Result;

/// The use rows of one symbol with their completeness (the first part of a `uses` report).
#[derive(Clone, Debug)]
pub struct References {
    /// The resolved target (valid for the index after a pending target was set up).
    pub target: SymbolId,
    pub symbol: Card,
    /// Family members other than the target.
    pub family: Vec<Card>,
    /// Sorted by (file, line, column).
    pub rows: Vec<ReferenceRow>,
    pub completeness: Completeness,
}

/// Every use of `symbol` (module docs).
pub fn references(ws: &mut Workspace, symbol: &str) -> Result<References> {
    // Symbol ids may change when a pending target is set up: `resolve_ready` resolves again.
    let target = ws.resolve_ready(symbol)?;
    let family = target_family(ws.index()?, target, MAX_FAMILY);
    let members = family.ids();
    let ws: &Workspace = ws;
    let graph = ws.graph()?;
    let sources = ws.sources()?;
    let index = graph.index;
    let rows = collect_rows(&graph, &sources, target, &family, ws.include)?;
    // Every row counts as the target in the name scan: no site is a row and unresolved.
    let target_spans: Vec<(FileId, ByteSpan)> = rows.iter().map(|(_, k)| *k).collect();
    let scan = name_scan(
        &graph,
        &sources,
        &NameQuery {
            family: &members,
            include: ws.include,
            bounded: family
                .truncated
                .then(|| format!("family cut at {MAX_FAMILY} members")),
            target_spans: &target_spans,
        },
    );
    let mut completeness = scan.completeness;
    rank_unresolved(index, target, &mut completeness);
    let mut rows: Vec<ReferenceRow> = rows.into_iter().map(|(r, _)| r).collect();
    debug_assert!(
        {
            let keys: HashSet<(&str, u32)> = rows.iter().map(|r| (r.file.as_str(), r.start_byte)).collect();
            completeness
                .unresolved
                .iter()
                .all(|u| !keys.contains(&(u.at.file.as_str(), u.at.start_byte)))
        },
        "a site is both a uses row and unresolved"
    );
    rows.sort_by(|a, b| {
        (&a.file, a.line, a.column, a.start_byte).cmp(&(&b.file, b.line, b.column, b.start_byte))
    });
    Ok(References {
        target,
        symbol: card(index, index.symbol(target)),
        family: members
            .iter()
            .filter(|&&m| m != target)
            .map(|&m| card(index, index.symbol(m)))
            .collect(),
        rows,
        completeness,
    })
}

fn tier_rank(tier: &str) -> u8 {
    match tier {
        "proven" => 0,
        "inferred" => 1,
        _ => 2,
    }
}

/// Kind and resolution of the declaration row of family member `m`.
fn declaration_row_kind(graph: &Graph<'_>, family: &Family, m: SymbolId) -> (&'static str, &'static str) {
    let relation = family.relation(m).unwrap_or(FamilyRelation::Target);
    let kind = match relation {
        FamilyRelation::Target | FamilyRelation::Declaration | FamilyRelation::Overload => "declaration",
        FamilyRelation::Override => "override",
        FamilyRelation::Implements => "implements",
    };
    if kind == "declaration" {
        return (kind, "declaration");
    }
    // The family edge that linked the member: its resolution (`inheritance_rule`,
    // `implementation` from a server, ...).
    let from = family.members.iter().find(|x| x.id == m).and_then(|x| x.from);
    let resolution = graph
        .index
        .edges
        .iter()
        .find(|e| {
            FAMILY.contains(e.kind)
                && from.is_some_and(|f| (e.from == m && e.to == f) || (e.from == f && e.to == m))
        })
        .map_or("inheritance_rule", |e| e.resolution.as_str());
    (kind, resolution)
}

/// Index, declaration and family rows, one per (file, start byte) (module docs, steps 3
/// and 4), each with its identifier span.
fn collect_rows(
    graph: &Graph<'_>,
    sources: &SourceStore<'_>,
    target: SymbolId,
    family: &Family,
    include: Tier,
) -> Result<Vec<(ReferenceRow, (FileId, ByteSpan))>> {
    let index = graph.index;
    let threshold = include.min(Tier::Inferred);
    let ids = family.ids();
    let members: HashSet<SymbolId> = ids.iter().copied().collect();
    let bindings = Bindings::new(index, &ids);
    let via = |m: SymbolId| (m != target).then(|| index.symbol(m).uid.clone());
    // Non-proven links at member-binding sites are not uses (SPEC §10.1 `member_binding`).
    // Non-proven links at local-binding occurrences are not uses either (the name denotes
    // the local binding: lexical shadowing, SPEC §10.1 `local_binding`).
    let rejected = |tier: Tier, file: FileId, span: ByteSpan, m: SymbolId| -> bool {
        if tier == Tier::Proven {
            return false;
        }
        let Some(facts) = &index.file(file).facts else {
            return false;
        };
        facts.is_local(span) || bindings.excludes(file, span, &access_at(facts, span), m)
    };
    let mut rows: Vec<(ReferenceRow, (FileId, ByteSpan))> = Vec::new();
    let row = |kind: &'static str,
               tier: Tier,
               resolution: &'static str,
               file: FileId,
               span: ByteSpan,
               owner: Option<SymbolId>,
               via: Option<String>|
     -> Result<(ReferenceRow, (FileId, ByteSpan))> {
        let rec = index.file(file);
        let (line, column, text) = line_at(sources, file, span.start)?;
        Ok((
            ReferenceRow {
                kind,
                tier: tier.as_str(),
                file: rec.path.clone(),
                language: rec.language,
                line,
                column,
                start_byte: span.start,
                end_byte: span.end,
                text,
                owner: owner.map(|o| index.symbol(o).uid.clone()),
                via,
                source: "index".to_string(),
                resolution,
                when: Vec::new(),
            },
            (file, span),
        ))
    };

    // Declarations: the target and the other family members.
    for &m in &ids {
        let s = index.symbol(m);
        if s.is_synthetic() {
            continue;
        }
        let (kind, resolution) = declaration_row_kind(graph, family, m);
        rows.push(row(kind, Tier::Proven, resolution, s.file, s.name_span, Some(m), via(m))?);
    }

    // Index rows: incoming edges of every kind, proven and inferred only.
    for &m in &ids {
        let name = index.symbol(m).name.clone();
        for (_, e) in graph.incoming_all(m) {
            if FAMILY.contains(e.kind) || e.tier > threshold {
                continue;
            }
            // Stub declarations of the family are listed as declarations already.
            if e.kind == EdgeKind::StubImplementation && members.contains(&e.from) {
                continue;
            }
            if e.kind == EdgeKind::Bridge && !graph.bridges_enabled() {
                continue;
            }
            let kind = use_kind(e.kind);
            let span = match callback_argument_span(index, e) {
                Some(arg) if kind == "callback" => arg,
                _ if kind == "call" || kind == "read" || kind == "callback" => {
                    narrow_to_member(index, sources, &e.at, &name)
                }
                _ => e.at.bytes,
            };
            if rejected(e.tier, e.at.file, span, m) {
                continue;
            }
            rows.push(row(kind, e.tier, e.resolution.as_str(), e.at.file, span, Some(e.from), via(m))?);
        }
    }

    // Sites whose candidates / value flow are all family members (query-time rules).
    if ids.len() > 1 {
        for e in family_candidate_edges(index, &members, threshold) {
            let name = index.symbol(e.to).name.clone();
            let span = match callback_argument_span(index, &e) {
                Some(arg) if use_kind(e.kind) == "callback" => arg,
                _ => narrow_to_member(index, sources, &e.at, &name),
            };
            if rejected(e.tier, e.at.file, span, e.to) {
                continue;
            }
            rows.push(row(
                use_kind(e.kind),
                e.tier,
                family_resolution(&e),
                e.at.file,
                span,
                Some(e.from),
                via(e.to),
            )?);
        }
    }

    rows.sort_by(|(a, ka), (b, kb)| {
        (ka.0, ka.1.start, tier_rank(a.tier)).cmp(&(kb.0, kb.1.start, tier_rank(b.tier)))
    });
    rows.dedup_by(|(_, kb), (_, ka)| ka.0 == kb.0 && ka.1.start == kb.1.start);
    debug_assert!(rows.iter().all(|(r, _)| r.tier != Tier::Possible.as_str()));
    Ok(rows)
}

/// The argument expression of a callback edge decided at a site (TS `errorHandler` rule
/// (c)): the site is located at the receiving call's callee (`compose(...)`), but the use of
/// the passed function is the argument (`this.errorHandler`). Returns the identifier span of
/// the syntax `CallbackArg` of that call (the attribute name for `obj.method`), so the
/// member-binding and local-binding rules see the real occurrence:
/// * the argument whose text is the site's argument;
/// * else the only argument of that call whose last member segment (or bound name) is the
///   site's argument: sites name the flowed value (`errorHandler`) while the syntax argument
///   is the member expression (`this.errorHandler`, `app.errorHandler`).
///
/// `None` for edges without a site, without a matching argument fact, or with several
/// arguments of that call matching by member segment.
pub(crate) fn callback_argument_span(index: &trace_core::Index, e: &trace_core::Edge) -> Option<ByteSpan> {
    let site = index.sites.get(e.site? as usize)?;
    let argument = site.argument.as_deref()?;
    let facts = index.file(site.at.file).facts.as_ref()?;
    let same_call = move || {
        facts
            .callbacks
            .iter()
            .filter(move |c| c.call_callee_span == site.at.bytes)
    };
    if let Some(exact) = same_call().find(|c| c.argument == argument) {
        return Some(exact.arg_span);
    }
    let mut by_member = same_call().filter(|c| c.name == argument || member_tail(&c.argument) == argument);
    let only = by_member.next()?;
    by_member.next().is_none().then_some(only.arg_span)
}

/// Last member segment of a member expression (`errorHandler` of `this.errorHandler`,
/// `app?.errorHandler`, `$this->errorHandler`, `obj:errorHandler`); the text itself when it
/// has no member separator.
fn member_tail(text: &str) -> &str {
    text.rsplit(['.', '>', ':']).next().map(str::trim).unwrap_or(text)
}

#[cfg(test)]
#[path = "../../tests/unit/queries/references.rs"]
mod tests;
