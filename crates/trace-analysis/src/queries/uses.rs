//! `uses <symbol> [--deep]`: every use of a symbol, the provenance of its links and, with
//! `--deep`, callers of callers and tests (SPEC §10 `uses`).
//!
//! Order of work (each step sees the index the previous one left):
//! 1. [`crate::queries::references::references`]: resolves the selector (exact matches only; a
//!    target in a pending file sets up its language first, `Workspace::resolve_ready`),
//!    classifies the reverse frontier lazily, and yields the
//!    rows (proven and inferred only, family always included, every kind, one per site),
//!    the family and the name completeness (ranked unresolved sites, never also a row).
//! 2. Evidence ([`crate::queries::evidence::evidence_of`]) of the target, re-resolved by uid (symbol
//!    ids are valid for the index after a pending language was set up).
//! 3. Site facts of every use ([`crate::queries::sites`]): the conditions it runs under (`when`).
//! 4. The summary (always): per direct caller in product code the entry points above it
//!    ([`crate::queries::sites::entry_points`]); the tests that set an option a use depends on
//!    ([`crate::queries::sites::option_tests`]) first, then the tests mentioning the symbol or its
//!    callers ([`crate::queries::tests_index`]).
//! 5. With `deep`: [`crate::queries::impact::impact`] at [`crate::DEPTH`] (callers, other
//!    references, transitive callers with `through`, result uses, similar code, tests,
//!    unknowns).
//!
//! Envelope: command `uses`, completeness = the rows' name completeness, `tiers_used` = the
//! rows' tiers (plus the deep rows' tiers).
//! Counts: `uses` = rows whose kind is not `declaration`; `overrides` = rows of kind
//! `override` / `implements`; `declarations` = rows of kind `declaration`; `unresolved` =
//! unresolved same-name sites; with `deep`, `callers_of_callers` = `transitive_total` and
//! `tests` = the number of tests.

use std::collections::HashMap;

use trace_core::model::{ByteSpan, FileId, Location};

use crate::cards::tiers_used;
use crate::queries::sites::{entry_points, option_tests, Sites, OPTION_TESTS};
use crate::report::{GuardTest, ReferenceRow, UsesCounts, UsesReport, UsesSummary};
use crate::workspace::Workspace;
use crate::Result;

/// Tests kept in the summary.
pub(crate) const SUMMARY_TESTS: usize = 10;

/// Use kinds whose owner runs the symbol (entry points are walked from them).
const RUNNING_KINDS: [&str; 4] = ["call", "read", "callback", "bridge"];

/// Every use of `symbol` (module docs).
pub fn uses(ws: &mut Workspace, symbol: &str, deep: bool) -> Result<UsesReport> {
    let mut refs = crate::queries::references::references(ws, symbol)?;
    let uid = refs.symbol.id.clone();
    let (evidence, summary) = {
        let ws: &Workspace = ws;
        let id = ws.resolve(&uid)?;
        let graph = ws.graph()?;
        let sources = ws.sources()?;
        let evidence = crate::queries::evidence::evidence_of(&graph, &sources, id);
        let index = graph.index;
        let files: HashMap<&str, FileId> = index
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.as_str(), FileId(i as u32)))
            .collect();
        let location = |r: &ReferenceRow| {
            Some(Location {
                file: *files.get(r.file.as_str())?,
                bytes: ByteSpan::new(r.start_byte, r.end_byte),
                line: r.line,
            })
        };
        let locations: Vec<(usize, Location)> = refs
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.kind != "declaration")
            .filter_map(|(i, r)| Some((i, location(r)?)))
            .collect();
        let sites = Sites::load(index, &sources, locations.iter().map(|(_, l)| (l.file, l.bytes.start)));
        for (i, l) in &locations {
            refs.rows[*i].when = sites.when(l);
        }
        let guards = sites.guards(locations.iter().map(|(_, l)| l));
        let mut tests: Vec<GuardTest> = option_tests(index, &sources, &guards, OPTION_TESTS);
        let mentions = crate::queries::tests_index::tests_with_graph(
            &graph,
            ws.include,
            &[id],
            crate::queries::impact::MAX_TESTS,
        )?;
        let extra = tests
            .iter()
            .filter(|t| !mentions.iter().any(|m| m.test == t.test))
            .count();
        for m in &mentions {
            if tests.len() >= SUMMARY_TESTS {
                break;
            }
            if !tests.iter().any(|t| t.test == m.test) {
                tests.push(GuardTest {
                    test: m.test.clone(),
                    file: m.file.clone(),
                    line: m.line,
                    sets: String::new(),
                });
            }
        }
        let mut callers = Vec::new();
        for r in refs.rows.iter().filter(|r| RUNNING_KINDS.contains(&r.kind)) {
            if let Some(owner) = r.owner.as_deref().and_then(|o| graph.symbol_by_uid(o)) {
                if !callers.contains(&owner) {
                    callers.push(owner);
                }
            }
        }
        let summary = UsesSummary {
            impact: entry_points(&graph, ws.include, &callers),
            tests_total: mentions.len() + extra,
            tests,
        };
        (evidence, summary)
    };
    let impact = if deep {
        Some(crate::queries::impact::impact(ws, &uid, crate::DEPTH)?)
    } else {
        None
    };
    let mut counts = row_counts(&refs.rows, refs.completeness.unresolved.len());
    if let Some(i) = &impact {
        counts.callers_of_callers = Some(i.transitive_total);
        counts.tests = Some(i.tests.len());
    }
    let mut envelope = ws.envelope("uses");
    let row_tiers = refs.rows.iter().map(|r| r.tier);
    envelope.tiers_used = match &impact {
        Some(i) => tiers_used(
            row_tiers.chain(
                i.callers
                    .iter()
                    .chain(&i.other_references)
                    .chain(&i.transitive)
                    .map(|r| r.tier),
            ),
        ),
        None => tiers_used(row_tiers),
    };
    envelope.completeness = Some(refs.completeness);
    Ok(UsesReport {
        envelope,
        symbol: refs.symbol,
        family: refs.family,
        deep,
        uses: refs.rows,
        counts,
        evidence,
        summary,
        impact,
    })
}

/// Row counts (module docs); the deep counts are filled by the caller.
pub(crate) fn row_counts(rows: &[ReferenceRow], unresolved: usize) -> UsesCounts {
    UsesCounts {
        uses: rows.iter().filter(|r| r.kind != "declaration").count(),
        overrides: rows
            .iter()
            .filter(|r| matches!(r.kind, "override" | "implements"))
            .count(),
        declarations: rows.iter().filter(|r| r.kind == "declaration").count(),
        unresolved,
        callers_of_callers: None,
        tests: None,
    }
}

#[cfg(test)]
#[path = "../../tests/unit/queries/uses.rs"]
mod tests;
