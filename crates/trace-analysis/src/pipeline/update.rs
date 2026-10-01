//! The one post-semantic code path (PLAN decision 13, DESIGN §1.14.5; owner speed): link ->
//! families -> library knowledge -> bridges -> inference -> decisions, full or incremental
//! per [`IndexDelta`], then persistence (full or delta journal).
//!
//! Full mode (`delta.full`, or no previous index): every phase runs its full function; the
//! incremental states of the phases are seeded for the next update.
//!
//! Incremental mode: the previous index is taken apart first (its rule edges, bridges,
//! sites, decisions and phase states), then
//! * link: `trace_core::assemble::assemble_delta` re-links only the changed / removed /
//!   re-queried records and the files that name their uids; everything else moves by the
//!   returned [`IdRemap`];
//! * families: `family_edges_delta` / `import_path_edges_delta` with the previous rule edges
//!   remapped;
//! * library: `library::knowledge_delta` with the previous knowledge (PhaseState `library`);
//! * bridges: `trace_bridge::detect_delta` with the previous bridges remapped and the
//!   `BridgeState` (PhaseState `bridges`);
//! * inference: `trace_infer::sites::generate_delta` with the `SitesState` (PhaseState
//!   `sites`);
//! * decisions: `trace_infer::decide::decide` over every site (one pass, no state).
//!
//! Every `*_delta` produces exactly what its full function produces on the same inputs (the
//! equivalence guard, `crate::equivalence`). A phase whose previous state is missing (an
//! index written by another version) runs in full mode; the result is the same.
//! `TRACE_PROFILE=1` adds one `profile: <phase> affected=<n>` line per incremental phase.

use std::time::Instant;

use serde::de::DeserializeOwned;
use serde::Serialize;
use trace_core::assemble::{assemble, assemble_delta, AssembleDeltaInput, AssembleInput};
use trace_core::cache::{save_index_delta, RepoMeta};
use trace_core::config::Settings;
use trace_core::delta::{IdRemap, IndexDelta, PhaseState};
use trace_core::model::{
    BackendRun, Bridge, Diagnostic, Edge, FileRecord, Index, IndexHeader, Location, OmittedFile, Provider,
};
use trace_core::paths::RepoPaths;
use trace_core::source::SourceStore;
use trace_core::{Hash32, LanguageSupport};
use trace_infer::hierarchy::Hierarchy;
use trace_library::installed::InstalledPackages;
use trace_library::{Library, LibraryKnowledge};

use crate::pipeline::{IndexProgress, Profile};
use crate::report::PhaseSeconds;
use crate::Result;

/// Everything the post-semantic phases read.
pub struct PostSemantic<'a> {
    pub config: &'a Settings,
    /// The previous index (incremental mode; `None` for a full build).
    pub prev: Option<Index>,
    /// Header of the new index (versions, inventory fingerprint, build counters).
    pub header: IndexHeader,
    /// File records of this build (full mode: every file; incremental: at least every added,
    /// changed and re-queried file; records of other files are accepted).
    pub files: Vec<FileRecord>,
    pub removed: Vec<String>,
    /// Configuration files fingerprinted (path, hash).
    pub configs: Vec<(String, Hash32)>,
    pub omitted: Vec<OmittedFile>,
    pub delta: IndexDelta,
    pub support: Vec<LanguageSupport>,
    pub backend_runs: Vec<BackendRun>,
    pub diagnostics: Vec<Diagnostic>,
    pub library: &'a Library,
    /// Installed dependency packages (`activated_by` rows of the bridges' irreducible table).
    pub installed: &'a InstalledPackages,
    /// Phase timings of the report.
    pub secs: &'a mut PhaseSeconds,
}

/// PhaseState names of the post-link phases.
pub(crate) const LIBRARY_PHASE: &str = "library";
pub(crate) const BRIDGES_PHASE: &str = "bridges";
pub(crate) const SITES_PHASE: &str = "sites";

/// Provider rule name of import-path edges (`trace_infer::imports::RULE`).
fn is_import_rule(edge: &Edge) -> bool {
    matches!(&edge.provider, Provider::Rule(name) if name == trace_infer::imports::RULE)
}

/// The previous index's parts the incremental phases need, taken out before linking.
#[derive(Default)]
struct Previous {
    family_edges: Vec<Edge>,
    import_edges: Vec<Edge>,
    bridges: Vec<Bridge>,
    knowledge: Option<LibraryKnowledge>,
    bridge_state: Option<trace_bridge::BridgeState>,
    sites_state: Option<trace_infer::sites::SitesState>,
}

/// Decode the global block of phase `phase` at `version` (`None`: absent, other version or
/// undecodable).
fn take_state<T: DeserializeOwned>(states: &[PhaseState], phase: &str, version: u32) -> Option<T> {
    states
        .iter()
        .find(|s| s.phase == phase && s.version == version)
        .and_then(|s| postcard::from_bytes(&s.global).ok())
}

/// A phase state holding `value` in its global block.
fn state_of<T: Serialize>(phase: &str, version: u32, value: &T) -> Option<PhaseState> {
    postcard::to_stdvec(value).ok().map(|global| PhaseState {
        phase: phase.to_string(),
        version,
        global,
        files: Default::default(),
    })
}

fn profile_enabled() -> bool {
    trace_core::env::profile()
}

/// `profile: <phase> affected=<n>` (incremental phases, `TRACE_PROFILE=1`).
fn affected_line(phase: &str, affected: usize) {
    if profile_enabled() {
        eprintln!("profile: {phase} affected={affected}");
    }
}

/// link -> families -> library knowledge -> bridges -> inference -> decisions, full or delta.
pub fn apply(
    input: PostSemantic<'_>,
    progress: &mut dyn IndexProgress,
    profile: &mut Profile,
) -> Result<(Index, IndexDelta)> {
    let PostSemantic {
        config,
        prev,
        header,
        files,
        removed,
        configs,
        omitted,
        mut delta,
        support,
        backend_runs,
        diagnostics,
        library,
        installed,
        secs,
    } = input;
    let prev = prev.filter(|_| !delta.full);
    if prev.is_none() && !delta.full {
        delta = IndexDelta {
            stale: std::mem::take(&mut delta.stale),
            ..IndexDelta::full()
        };
    }

    // 4. Link.
    let t = Instant::now();
    progress.phase("assemble", 0, 1);
    let (mut index, remap, previous) = match prev {
        None => {
            let index = assemble(AssembleInput {
                header,
                files,
                configs,
                omitted,
                support,
                backend_runs,
                diagnostics,
            });
            let remap = IdRemap::identity(0, 0);
            (index, remap, None)
        }
        Some(mut prev) => {
            let previous = take_previous(&mut prev);
            let files = changed_records(&prev, files, &mut delta);
            let mut removed: Vec<String> = removed;
            removed.extend(delta.removed.iter().cloned());
            removed.sort();
            removed.dedup();
            let relinked = files.len();
            let (index, remap) = assemble_delta(
                prev,
                AssembleDeltaInput {
                    header,
                    files,
                    removed,
                    configs,
                    omitted,
                    support,
                    backend_runs,
                    diagnostics,
                },
                &delta,
            );
            affected_line("assemble", relinked);
            (index, remap, Some(previous))
        }
    };
    secs.assemble = t.elapsed().as_secs_f64();
    progress.phase("assemble", 1, 1);
    profile.index(&index);
    profile.line("assemble", secs.assemble);

    // Post-link phases run incrementally only when every previous state is present.
    let previous =
        previous.filter(|p| p.knowledge.is_some() && p.bridge_state.is_some() && p.sites_state.is_some());
    let phase_delta = match &previous {
        Some(_) => delta.clone(),
        None => IndexDelta::full(),
    };
    let mut previous = previous.unwrap_or_default();

    // 4b. Family edges (language rules, proven) and import-path edges. The hierarchy is
    // built once here and reused by inference (family edges do not change it: hierarchy
    // construction evidence ignores family kinds).
    let t = Instant::now();
    progress.phase("family", 0, 1);
    let hierarchy = Hierarchy::build(&index);
    let family = {
        let store = SourceStore::new(&index);
        let line_of: &dyn Fn(trace_core::FileId, u32) -> Option<u32> =
            &|file, byte| store.file(file).ok().map(|src| src.lines.line1(byte));
        if phase_delta.full {
            let mut edges = trace_infer::family::family_edges_with(&index, &hierarchy, Some(line_of));
            // Import / use / re-export statements naming exactly one declaration (SPEC §7.12).
            edges.extend(trace_infer::imports::import_path_edges(&index));
            edges
        } else {
            let prev_family = remap_edges(&previous.family_edges, &remap);
            let prev_imports = remap_edges(&previous.import_edges, &remap);
            let mut edges = trace_infer::family::family_edges_delta(
                &index,
                &hierarchy,
                &phase_delta,
                &prev_family,
                Some(line_of),
            );
            edges.extend(trace_infer::imports::import_path_edges_delta(&index, &phase_delta, &prev_imports));
            affected_line("family", edges.len());
            edges
        }
    };
    if !family.is_empty() {
        index.edges.extend(family);
        trace_core::assemble::sort_edges(&mut index.edges);
    }
    secs.family = t.elapsed().as_secs_f64();
    progress.phase("family", 1, 1);
    profile.index(&index);
    profile.line("family", secs.family);

    // Library knowledge (trace-library): what library callees do with the functions passed
    // to them, incl. channel effects the bridges read.
    let t = Instant::now();
    let knowledge = {
        let views = crate::pipeline::library::views(&index);
        if phase_delta.full {
            crate::pipeline::library::knowledge(&views, library)
        } else {
            let prev_knowledge = previous.knowledge.take().unwrap_or_default();
            let k = crate::pipeline::library::knowledge_delta(&views, library, prev_knowledge, &phase_delta);
            affected_line(
                "library",
                phase_delta.added.len() + phase_delta.modified.len() + phase_delta.requeried.len(),
            );
            k
        }
    };
    secs.library = t.elapsed().as_secs_f64();
    profile.line("library", secs.library);

    // Bridges (after library: they read its channel effects).
    let t = Instant::now();
    let mut bridge_state = previous.bridge_state.take().unwrap_or_default();
    {
        let store = SourceStore::new(&index);
        let bridge_input = trace_bridge::BridgeInput {
            index: &index,
            sources: &store,
            config: &config.bridges,
            knowledge: &knowledge,
            tables: library.tables(),
            installed,
        };
        let prev_output = trace_bridge::BridgeOutput {
            bridges: if phase_delta.full {
                Vec::new()
            } else {
                remap_bridges(&previous.bridges, &remap)
            },
            diagnostics: Vec::new(),
        };
        // `detect_delta` with a full delta is the full detection and seeds the state.
        let found = trace_bridge::detect_delta(bridge_input, prev_output, &phase_delta, &mut bridge_state);
        drop(store);
        if !phase_delta.full {
            affected_line("bridges", found.bridges.len());
        }
        index.bridges = found.bridges;
        index.diagnostics.extend(found.diagnostics);
    }
    secs.bridges = t.elapsed().as_secs_f64();
    profile.index(&index);
    profile.line("bridges", secs.bridges);

    // 5. Infer.
    let t = Instant::now();
    progress.phase("infer", 0, 1);
    let prev_sites_state = previous.sites_state.take().unwrap_or_default();
    let generated = {
        let store = SourceStore::new(&index);
        trace_infer::sites::generate_delta(
            &index,
            &store,
            &hierarchy,
            trace_infer::LibraryInputs {
                knowledge: &knowledge,
                tables: library.tables(),
                installed,
            },
            prev_sites_state,
            &remap,
            &phase_delta,
        )
    };
    let mut sites_state = None;
    match generated {
        Ok((g, state)) => {
            index.sites = g.sites;
            // Filled on every infer run, full and incremental alike (the assembled index
            // starts with an empty list; on an error it stays empty).
            index.library_receivers = g.library_receivers;
            index.diagnostics.extend(g.diagnostics);
            profile.flow = g.stats.flow;
            sites_state = Some(state);
        }
        Err(e) => index.diagnostics.push(Diagnostic::new(
            "sites_failed",
            None,
            format!("edge-case sites not generated: {e}"),
        )),
    }
    secs.infer = t.elapsed().as_secs_f64();
    progress.phase("infer", 1, 1);
    profile.index(&index);
    profile.line("infer", secs.infer);

    drop(hierarchy);

    // 6. Decisions.
    let t = Instant::now();
    progress.phase("decide", 0, 1);
    index.decisions = trace_infer::decide::decide(&index);
    secs.decide = t.elapsed().as_secs_f64();
    progress.phase("decide", 1, 1);
    profile.line("decide", secs.decide);

    // Incremental states for the next update, and the stale set.
    let states = [
        state_of(LIBRARY_PHASE, trace_library::ENGINE_VERSION, &knowledge),
        state_of(BRIDGES_PHASE, trace_bridge::BRIDGE_VERSION, &bridge_state),
        sites_state.and_then(|s| state_of(SITES_PHASE, trace_infer::INFER_VERSION, &s)),
    ];
    index.phase_state.extend(states.into_iter().flatten());
    index.stale = delta.stale.clone();
    Ok((index, delta))
}

/// Take the parts of the previous index the incremental phases read (the linked facts and
/// the link state stay for `assemble_delta`).
fn take_previous(prev: &mut Index) -> Previous {
    let states = std::mem::take(&mut prev.phase_state);
    let mut previous = Previous {
        knowledge: take_state(&states, LIBRARY_PHASE, trace_library::ENGINE_VERSION),
        bridge_state: take_state(&states, BRIDGES_PHASE, trace_bridge::BRIDGE_VERSION),
        sites_state: take_state(&states, SITES_PHASE, trace_infer::INFER_VERSION),
        bridges: std::mem::take(&mut prev.bridges),
        ..Previous::default()
    };
    for e in &prev.edges {
        if is_import_rule(e) {
            previous.import_edges.push(e.clone());
        } else if matches!(e.provider, Provider::Rule(_)) {
            previous.family_edges.push(e.clone());
        }
    }
    // The link state goes back for `assemble_delta`.
    prev.phase_state = states
        .into_iter()
        .filter(|s| s.phase == trace_core::assemble::LINK_PHASE)
        .collect();
    previous
}

/// The records `assemble_delta` must replace: every file the delta names, plus any record
/// whose persisted metadata differs from the previous one (e.g. test code whose semantics
/// were deferred or completed); the latter are added to `delta.requeried` so the journal
/// carries their blocks. Records identical to the previous ones are dropped (the previous
/// record is kept by the link).
fn changed_records(prev: &Index, files: Vec<FileRecord>, delta: &mut IndexDelta) -> Vec<FileRecord> {
    let mut out = Vec::new();
    for rec in files {
        if delta.file_changed(&rec.path) {
            out.push(rec);
            continue;
        }
        let differs = match prev.file_by_path(&rec.path).map(|id| prev.file(id)) {
            None => true,
            Some(old) => {
                old.hash != rec.hash
                    || old.size != rec.size
                    || old.mtime_ns != rec.mtime_ns
                    || old.language != rec.language
                    || old.support != rec.support
                    || old.pending != rec.pending
                    || old.diagnostics != rec.diagnostics
                    || old.facts.is_some() != rec.facts.is_some()
                    || old.semantic.is_some() != rec.semantic.is_some()
                    || old.semantic.as_ref().map(|s| &s.tool_fingerprint)
                        != rec.semantic.as_ref().map(|s| &s.tool_fingerprint)
            }
        };
        if differs {
            delta.requeried.insert(rec.path.clone());
            out.push(rec);
        }
    }
    out
}

fn remap_location(at: &Location, remap: &IdRemap) -> Option<Location> {
    Some(Location {
        file: remap.file(at.file)?,
        ..*at
    })
}

/// Previous edges with remapped ids (edges touching a removed symbol or file are dropped).
fn remap_edges(edges: &[Edge], remap: &IdRemap) -> Vec<Edge> {
    edges
        .iter()
        .filter_map(|e| {
            let mut e = e.clone();
            e.from = remap.symbol(e.from)?;
            e.to = remap.symbol(e.to)?;
            e.at = remap_location(&e.at, remap)?;
            Some(e)
        })
        .collect()
}

/// Previous bridges with remapped ids (bridges touching a removed symbol or file are
/// dropped).
fn remap_bridges(bridges: &[Bridge], remap: &IdRemap) -> Vec<Bridge> {
    bridges
        .iter()
        .filter_map(|b| {
            let mut b = b.clone();
            b.from = remap.symbol(b.from)?;
            b.to = remap.symbol(b.to)?;
            b.from_at = remap_location(&b.from_at, remap)?;
            b.to_at = remap_location(&b.to_at, remap)?;
            b.contract = match b.contract {
                Some(f) => Some(remap.file(f)?),
                None => None,
            };
            Some(b)
        })
        .collect()
}

/// Full or journal persistence per the delta, then `meta.json`.
pub fn persist(paths: &RepoPaths, index: &Index, delta: &IndexDelta) -> Result<()> {
    save_index_delta(&paths.index_file, index, delta)?;
    RepoMeta::record_index(&paths.meta_file, &paths.root_display())?;
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/pipeline/update.rs"]
mod tests;
