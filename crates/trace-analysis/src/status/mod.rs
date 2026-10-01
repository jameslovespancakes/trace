//! `status`: index health, freshness (metadata scan, no update), per-language setup and
//! support, backends of the last build, overview (areas, entry points, hubs; top 10 each).
//! Never modifies caches,
//! never makes network calls, never launches language servers.
//!
//! Setup rows (`setup`): one per product language from `trace_semantic::setup::report`
//! (preflight in report mode: static checks only, never a build step): server + version,
//! toolchain + version + origin, dependencies, build (`not needed` / `allowed` /
//! `needs --allow-build`), `ready` or the one-line error with its `error_type`. Pending
//! languages and sub-projects (`pending`) are listed with their reasons; they are set up by
//! the first query that needs one of their files. Also: build approval, remembered `--env`
//! paths, the user's exclusion globs, whether `trace index --watch` runs (it holds
//! `<repo cache>/watch.lock`), stale dependents, the default-language install
//! line when a default server is missing (it installs automatically on first use).
//!
//! Language rows are the ones recorded by the last build (`Index::support`): `semantic`,
//! `pending` (with the reason) or `inventoried` (no grammar / contract files).
//!
//! Also reported: the semantic analyzer pool size and cache hit rates
//! (`<repo cache>/stats.json`: per-file semantic reuse).
//!
//! Resolution health (PLAN launch targets): per language, over the calls of ANALYSED files
//! (pending files and files outside the build on this machine are not counted; they are
//! listed), a call site is `resolved` when a proven execution edge into the repository, a
//! decided inference site (the inferred edges materialised from `Index::decisions`), a
//! library call (`FileSemantics::library_calls`: the server resolved it into installed
//! library / stdlib code), a library receiver (`Index::library_receivers`: value flow proved
//! that the call runs library code) or a proven external target (`external_or_ambiguous`
//! without in-index candidates) is recorded at its callee span, else `unresolved`. Recorded spans are
//! matched to calls on the callee: a span ending where a call's callee ends is that call; a
//! span covering a whole invocation (servers whose call ranges run from the callee to the
//! closing parenthesis) is the innermost call whose expression contains it and whose callee
//! starts or ends inside it, so each recorded span counts one call and nested calls are never
//! counted twice. Resolved calls whose only answer is "external" without a library location
//! while no repository declaration carries their name (the engine's by-name rule: nothing in
//! the repository could be the target) are also counted apart as `by_name`. The
//! *in-repository* rate counts only calls whose member name equals the name of a
//! non-synthetic declaration of the index in the same language namespace (JS / TS / TSX one
//! namespace, C / C++ one). A semantic language whose in-repository rate is below
//! `analysis.resolution_warn` (setting, default 0.8) gets a `low_resolution` warning (row + diagnostic). The server state
//! per language (`ResolutionHealth::server`): `server_failed` when the last run of its
//! backend failed, `server_not_ready` when that run's readiness wait timed out, and
//! `server_missing` when its setup row says the server is not installed.
//!
//! Library behaviour coverage: callback sites whose receiving call is a library call
//! (`Site::library` is set exactly then; in-repository callees, parameters and unresolved
//! callees are not library sites), and how many of them carry library behaviour trace
//! determined itself (`Site::library` source: derived / declared type / table).
//! Synthetic scopes (`<module>`, `<lambda>`) are never entry points, hubs or area members.
//! `install` is filled by the CLI after `status --install <lang>`.
//!
//! Non-default settings (`settings`): every setting that differs from its default with its
//! origin (the user's `config.json`, or the environment variable that overrides it).
//!
//! Files: the report and index health here; `setup` (setup and pending rows), `resolution`
//! (resolution health), `dump` (unresolved dump), `overview`, `settings`.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use trace_core::model::{DecisionStatus, Diagnostic, SiteCategory};
use trace_core::repo_settings::RepoSettings;
use trace_core::{incremental, BridgeKind, Index, Tier, SCHEMA_VERSION, TRACE_VERSION};

use crate::caches::{stats_path, HitCounter, Stats};
use crate::cards::{edge_counts, language_row};
use crate::pipeline::{self, IndexMode};
use crate::report::{
    BridgeCount, CacheRate, CacheRates, EdgeCounts, IndexHealth, LibraryBehaviourStatus, SemanticInfo,
    SiteCounts, StatusReport,
};
use crate::workspace::Workspace;
use crate::Result;

mod dump;
mod overview;
mod resolution;
mod settings;
mod setup;

pub use dump::{unresolved_dump, CalleeGroup, LanguageDump, ReasonGroup};
pub use overview::{area, overview};
use resolution::rate_of;
pub(crate) use resolution::{mark_missing_servers, resolution_health};
pub(crate) use settings::settings_rows;
pub use setup::setup_rows;
pub(crate) use setup::{default_install_lines, pending_rows};

/// Changed files listed by name in the index health.
const MAX_STALE: usize = 50;

/// `status` (module docs); `install` is null.
pub fn status(ws: &Workspace) -> Result<StatusReport> {
    let started = Instant::now();
    let index = ws.index.as_ref();
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    if let Some(reason) = ws.invalidated() {
        diagnostics.push(Diagnostic::new(
            "cache_invalidated",
            None,
            format!("cached index unusable ({reason}); the next command rebuilds it"),
        ));
    }
    let health = match index {
        Some(index) => health(ws, index, &mut diagnostics),
        None => IndexHealth {
            exists: false,
            fresh: None,
            stale_files: Vec::new(),
            files: 0,
            symbols: 0,
            edges: EdgeCounts {
                proven: 0,
                inferred: 0,
                possible: 0,
            },
            unresolved: 0,
            sites: SiteCounts {
                total: 0,
                decided: 0,
                undecided: 0,
                by_category: BTreeMap::new(),
            },
            omitted: 0,
            built_unix: None,
            schema: SCHEMA_VERSION,
            full_builds: 0,
            incremental_updates: 0,
            cache_bytes: dir_bytes(&ws.paths.repo_dir),
            pending_files: 0,
            stale: 0,
            outside_build_files: 0,
            request_failed_files: 0,
        },
    };
    if let Some(index) = index {
        diagnostics.extend(index.diagnostics.iter().cloned());
        diagnostics.extend(file_diagnostics(index));
    }
    let present = index.iter().flat_map(|i| i.support.iter().map(|s| s.language));
    for e in present.filter_map(trace_syntax::grammar::grammar_error) {
        diagnostics.push(Diagnostic::new("grammar_error", None, e.to_string()));
    }
    let settings = RepoSettings::load(&ws.paths)?;
    let setup = setup_rows(ws, &settings)?;
    let warn = ws.config.analysis.resolution_warn;
    let mut resolution = index.map(|i| resolution_health(i, warn)).unwrap_or_default();
    mark_missing_servers(&mut resolution, &setup);
    for r in resolution.iter().filter(|r| r.warning.is_some()) {
        diagnostics.push(Diagnostic::new(
            "low_resolution",
            None,
            format!(
                "{}: {} of {} in-repository call sites resolved ({:.0}%, below {:.0}%; {:.0}% of all calls); callers may be missing",
                r.language,
                r.in_repo_resolved,
                r.in_repo_resolved + r.in_repo_unresolved,
                r.in_repo_rate.unwrap_or(0.0) * 100.0,
                warn * 100.0,
                r.rate.unwrap_or(0.0) * 100.0
            ),
        ));
    }
    let languages = index
        .map(|i| {
            i.support
                .iter()
                .map(|s| {
                    let mut row = language_row(s);
                    row.resolution = resolution
                        .iter()
                        .find(|r| r.language == s.language)
                        .and_then(|r| r.rate);
                    row
                })
                .collect()
        })
        .unwrap_or_default();
    let default_install = default_install_lines(&setup);
    Ok(StatusReport {
        command: "status",
        schema: crate::report::SCHEMA,
        trace_version: TRACE_VERSION,
        root: ws.paths.root_display(),
        cache: ws.paths.repo_dir.to_string_lossy().into_owned(),
        index: health,
        languages,
        setup,
        pending: index.map(pending_rows).unwrap_or_default(),
        build_approval: if settings.allow_build {
            "allowed"
        } else {
            "not given"
        },
        env_paths: settings
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.display().to_string()))
            .collect(),
        excluded: ws.config.inventory.exclude.clone(),
        settings: settings_rows(&ws.config, &ws.paths.home),
        watching: ws.paths.watching(),
        default_install,
        resolution,
        library_behaviour: index.map(library_behaviour).unwrap_or_default(),
        bridges: index.map(bridge_counts).unwrap_or_default(),
        install: None,
        backends: index.map(|i| i.backend_runs.clone()).unwrap_or_default(),
        semantic: SemanticInfo {
            pool_size: ws.config.auto().server_processes,
            memory_budget_mb: ws.config.memory.budget_mb,
            persistent_sessions_in: vec!["index --watch"],
        },
        caches: cache_rates(&Stats::load(&stats_path(&ws.paths))),
        overview: index.map(|i| overview(i, ws.include)),
        diagnostics,
        seconds: started.elapsed().as_secs_f64(),
    })
}

/// Library behaviour coverage over the callback sites into library callees (module docs):
/// only sites whose receiving call is a library call (`Site::library` is set) count.
pub fn library_behaviour(index: &Index) -> LibraryBehaviourStatus {
    let mut s = LibraryBehaviourStatus::default();
    for site in index
        .sites
        .iter()
        .filter(|x| x.category == SiteCategory::Callback && x.library.is_some())
    {
        s.sites += 1;
        match site.library.as_ref().map(|l| l.source.as_str()) {
            Some("derived") => s.derived += 1,
            Some("declared_type") => s.declared_type += 1,
            Some("table") => s.table += 1,
            _ => {}
        }
    }
    s.coverage =
        rate_of(s.derived + s.declared_type + s.table, s.sites - (s.derived + s.declared_type + s.table));
    s
}

/// Bridges per kind (in `BridgeKind::ALL` order) and tier.
pub(crate) fn bridge_counts(index: &Index) -> Vec<BridgeCount> {
    BridgeKind::ALL
        .iter()
        .filter_map(|&kind| {
            let mut row = BridgeCount {
                kind: kind.edge_label(),
                ..BridgeCount::default()
            };
            for b in index.bridges.iter().filter(|b| b.kind == kind) {
                match b.tier {
                    Tier::Proven => row.proven += 1,
                    Tier::Inferred => row.inferred += 1,
                    Tier::Possible => row.possible += 1,
                }
            }
            (row.proven + row.inferred + row.possible > 0).then_some(row)
        })
        .collect()
}

fn rate(c: &HitCounter) -> CacheRate {
    CacheRate {
        lookups: c.lookups,
        hits: c.hits,
        hit_rate: c.rate().map(|r| (r * 1000.0).round() / 1000.0),
    }
}

/// Hit rates from the persisted counters.
pub(crate) fn cache_rates(stats: &Stats) -> CacheRates {
    CacheRates {
        semantic_files: rate(&stats.semantic_files),
    }
}

/// Per-file syntax and analyzer diagnostics, one summary per kind: the number of files, the
/// first file (sorted by path) and its message.
fn file_diagnostics(index: &Index) -> Vec<Diagnostic> {
    let mut by_kind: BTreeMap<&str, (usize, &Diagnostic)> = BTreeMap::new();
    for f in &index.files {
        let semantic = f.semantic.iter().flat_map(|s| s.diagnostics.iter());
        for d in f.diagnostics.iter().chain(semantic) {
            by_kind
                .entry(d.kind.as_str())
                .and_modify(|(n, _)| *n += 1)
                .or_insert((1, d));
        }
    }
    by_kind
        .into_values()
        .map(|(n, first)| {
            let message = if n == 1 {
                first.message.clone()
            } else {
                format!("{n} files; e.g. {}", first.message)
            };
            Diagnostic::new(&first.kind, first.file.clone(), message)
        })
        .collect()
}

fn health(ws: &Workspace, index: &Index, diagnostics: &mut Vec<Diagnostic>) -> IndexHealth {
    let (fresh, stale_files) =
        match pipeline::scan(&ws.paths, &ws.config, Some(index), IndexMode::Incremental) {
            Ok(scanned) => {
                let plan = incremental::plan(Some(index), &scanned.sources, &scanned.configs);
                let mut stale: Vec<String> = plan
                    .added
                    .into_iter()
                    .chain(plan.changed)
                    .chain(plan.removed)
                    .collect();
                stale.sort();
                stale.truncate(MAX_STALE);
                (Some(pipeline::is_current(index, &scanned)), stale)
            }
            Err(e) => {
                diagnostics.push(Diagnostic::new(
                    "freshness_unknown",
                    None,
                    format!("inventory scan failed: {e}"),
                ));
                (None, Vec::new())
            }
        };
    let mut sites = SiteCounts {
        total: index.sites.len(),
        decided: 0,
        undecided: 0,
        by_category: BTreeMap::new(),
    };
    for (i, s) in index.sites.iter().enumerate() {
        *sites.by_category.entry(s.category.as_str()).or_insert(0) += 1;
        match index.decision(i as u32) {
            Some(d) if d.status == DecisionStatus::Decided => sites.decided += 1,
            _ => sites.undecided += 1,
        }
    }
    IndexHealth {
        exists: true,
        fresh,
        stale_files,
        files: index.files.len(),
        symbols: index.symbols.len(),
        edges: edge_counts(index),
        unresolved: index.unresolved.len(),
        sites,
        omitted: index.omitted.len(),
        built_unix: Some(index.header.built_unix),
        schema: index.header.schema,
        full_builds: index.header.full_builds,
        incremental_updates: index.header.incremental_updates,
        cache_bytes: dir_bytes(&ws.paths.repo_dir),
        pending_files: pipeline::pending_files(index),
        stale: index.stale.len(),
        outside_build_files: pipeline::outside_build_files(index),
        request_failed_files: index
            .files
            .iter()
            .filter(|f| {
                f.semantic
                    .as_ref()
                    .is_some_and(|s| s.diagnostics.iter().any(|d| d.kind == "request_failed"))
            })
            .count(),
    }
}

/// Total size of the files under `dir` (0 if absent).
fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

#[cfg(test)]
#[path = "../../tests/unit/status/mod.rs"]
mod tests;
