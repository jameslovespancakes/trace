//! Semantic phase: backend partitions, server runs (fresh or warm sessions), failures as
//! setup errors with a log, and the per-file results.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use trace_core::config::Settings;
use trace_core::facts::FileFacts;
use trace_core::incremental::{self, StalePolicy, UpdatePlan};
use trace_core::inventory::HashedEntry;
use trace_core::model::{BackendRun, FileRecord, Index};
use trace_core::paths::RepoPaths;
use trace_core::repo_settings::RepoSettings;
use trace_core::semantics::FileSemantics;
use trace_core::setup_error::SetupError;
use trace_core::{Hash32, Language, SupportLevel};
use trace_semantic::backend::{Backend, SemanticFile, SemanticRequest};
use trace_semantic::session::SessionOutput;
use trace_semantic::{Prepared, RunPolicy, SemanticError, SemanticSessions, ToolEnv};

use super::{
    pending::pending_now, progress::IndexProgress, records::cached_semantics, records::read_source,
    setup::setup_phase, Build, FileState, Host, SemanticPlan,
};
use crate::languages::most_files;
use crate::{AnalysisError, Result};

/// Semantic phase options.
pub(super) struct SemanticOptions<'s> {
    /// Ignore the per-file semantic cache ([`IndexMode::Rebuild`]).
    pub(super) rebuild: bool,
    pub(super) sessions: &'s mut SemanticSessions,
    /// Keep analyzer processes alive between runs (`trace index --watch`).
    pub(super) persistent: bool,
    /// Files whose interface changed in this update (interface rule).
    pub(super) interfaces: &'s HashSet<String>,
    /// Declaration names that appeared or disappeared in this update.
    pub(super) declared: &'s HashSet<String>,
    /// What happens to stale dependents.
    pub(super) policy: &'s StalePolicy,
}

/// Per-file semantic accounting of one run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct SemanticCounts {
    /// Files analyzed by a language server now.
    pub(super) queried: usize,
    /// Files answered from the per-file cache or kept from the previous index.
    pub(super) reused: usize,
}

/// One backend partition to analyze in the semantic phase.
struct SemanticJob<'b> {
    /// Position in `SemanticPlan::assign` (report order).
    pub(super) bi: usize,
    pub(super) backend: &'b dyn Backend,
    pub(super) langs: Vec<Language>,
    /// Indices into `Build::files`.
    pub(super) part: Vec<usize>,
    pub(super) query: HashSet<String>,
    pub(super) version: Option<String>,
}

/// The files of a backend's partition: every non-pending file of its languages, test files
/// included (test code is analysed at index time).
pub(super) fn partition(files: &[FileState], langs: &[Language]) -> Vec<usize> {
    (0..files.len())
        .filter(|&i| {
            let f = &files[i];
            !f.skip && f.pending.is_none() && langs.contains(&f.language)
        })
        .collect()
}

/// Where the details of a backend failure are written: `<repo cache>/logs/<backend>.log`.
fn failure_log(paths: &RepoPaths, backend: &str, details: &str) -> PathBuf {
    let name: String = backend
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let dir = paths.repo_dir.join("logs");
    let log = dir.join(format!("{name}.log"));
    // Best effort: the error names the file either way.
    let _ = fs::create_dir_all(&dir).and_then(|_| fs::write(&log, details.as_bytes()));
    log
}

/// The language a backend failure names: the language with the most analysed files of that
/// backend's partition (C++ for a C / C++ repository of mostly C++ files), ties by language
/// order; the first language of the backend without files.
fn main_language(files: impl IntoIterator<Item = Language>, langs: &[Language]) -> Language {
    most_files(files)
        .or_else(|| langs.first().copied())
        .unwrap_or(Language::Python)
}

/// A backend run that failed (DESIGN §2): its setup error; a request that timed out
/// (`request_timeout`) or a session past its deadline (`session_deadline`) is
/// `ServerTimeout` (minutes of that limit, the method in the log); anything else
/// `ServerCrashed` with a log holding the details. The error names `language` (the backend's
/// main language, [`main_language`]); a crash or timeout the session already reported for
/// one of the backend's languages is named after that main language too. Errors of trace
/// itself (cache, I/O, a file changed while analysed) are not server failures:
/// `Err(AnalysisError)`.
fn backend_failure(
    paths: &RepoPaths,
    backend: &str,
    (langs, language): (&[Language], Language),
    limits: (Duration, Duration),
    error: SemanticError,
) -> std::result::Result<SetupError, AnalysisError> {
    let (request_timeout, session_deadline) = limits;
    match error {
        SemanticError::Setup(SetupError::ServerCrashed { language: l, log }) if langs.contains(&l) => {
            Ok(SetupError::ServerCrashed { language, log })
        }
        SemanticError::Setup(SetupError::ServerTimeout {
            language: l,
            minutes,
            log,
        }) if langs.contains(&l) => Ok(SetupError::ServerTimeout {
            language,
            minutes,
            log,
        }),
        SemanticError::Setup(e) => Ok(e),
        e @ (SemanticError::Core(_) | SemanticError::SourceChanged(_)) => Err(e.into()),
        SemanticError::Timeout { method } => {
            let details = format!(
                "{backend} did not answer {method} within {} s (semantic.request_timeout_secs)\n",
                request_timeout.as_secs()
            );
            Ok(SetupError::ServerTimeout {
                language,
                minutes: trace_core::setup_error::timeout_minutes(request_timeout),
                log: failure_log(paths, backend, &details),
            })
        }
        SemanticError::Deadline => {
            let details = format!(
                "{backend} did not finish within {} s (semantic.session_deadline_secs)\n",
                session_deadline.as_secs()
            );
            Ok(SetupError::ServerTimeout {
                language,
                minutes: trace_core::setup_error::timeout_minutes(session_deadline),
                log: failure_log(paths, backend, &details),
            })
        }
        other => {
            let details = format!("{backend} failed: {other}\n");
            Ok(SetupError::ServerCrashed {
                language,
                log: failure_log(paths, backend, &details),
            })
        }
    }
}

/// The result of a requested file; none means the server or its shard died (DESIGN §2):
/// `ServerCrashed` naming the file's language, with a log.
fn take_result(
    fresh: &mut HashMap<String, FileSemantics>,
    paths: &RepoPaths,
    backend: &str,
    path: &str,
    language: Language,
) -> std::result::Result<FileSemantics, SetupError> {
    fresh.remove(path).ok_or_else(|| SetupError::ServerCrashed {
        language,
        log: failure_log(
            paths,
            backend,
            &format!(
                "{backend} returned no result for {path} (the server or one of its processes stopped)\n"
            ),
        ),
    })
}

/// `trace index --watch` at launch (I-10): start the language server of every analysed
/// language of `index` in the watcher's persistent `sessions` and wait until it is ready, so
/// the first edit costs what later edits cost. Setup is checked like an update (automatic
/// installs already ran in the catch-up); a setup or server failure is the error an update
/// would report. Returns the backends started (already live ones are skipped).
pub(crate) fn warm_sessions(
    paths: &RepoPaths,
    config: &Settings,
    index: &Index,
    sessions: &mut SemanticSessions,
) -> Result<Vec<String>> {
    let tools = ToolEnv::discover(config, &paths.home, &paths.root)?;
    let settings = RepoSettings::load(paths)?;
    let host = Host {
        paths,
        config,
        settings: &settings,
        platform: trace_env::os::Platform::current(),
        vars: trace_env::os::EnvVars::from_process(),
        auto_install: false,
        stale: StalePolicy::ResolveAll,
    };
    let files: Vec<(&str, Language)> = index.files.iter().map(|f| (f.path.as_str(), f.language)).collect();
    let pending = pending_now(&files, &settings);
    let backends: Vec<Box<dyn Backend>> = trace_semantic::registry(&tools);
    let facts = |p: &str| -> Option<&FileFacts> {
        index.file_by_path(p).and_then(|id| index.file(id).facts.as_ref())
    };
    let sem = setup_phase(&backends, &host, &tools, &files, &facts, &pending)?;
    let jobs: Vec<(&dyn Backend, &[Language], &Prepared)> = sem
        .assign
        .iter()
        .zip(&sem.prepared)
        .map(|((backend, langs), prepared)| (*backend, langs.as_slice(), prepared))
        .collect();
    warm_backends(paths, &tools, index, &jobs, sessions)
}

/// Start the live session of every backend of `jobs` that has none: its whole partition
/// (the analysed files of its languages) is synced and one warm-up query is asked for the
/// smallest file (fewest calls), exactly like an update - process start, readiness wait and
/// the server's warm-up hook included. The answer is discarded and the per-file cache is not
/// touched (`use_cache: false`), so the index is unchanged.
fn warm_backends(
    paths: &RepoPaths,
    tools: &ToolEnv,
    index: &Index,
    jobs: &[(&dyn Backend, &[Language], &Prepared)],
    sessions: &mut SemanticSessions,
) -> Result<Vec<String>> {
    let configs: Vec<(String, Vec<u8>)> = index
        .configs
        .iter()
        .filter_map(|(p, _)| read_source(&paths.root, p).ok().map(|b| (p.clone(), b)))
        .collect();
    let config_refs: Vec<(&str, &[u8])> = configs.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
    let empty = FileFacts::default();
    let policy = RunPolicy {
        persistent: true,
        use_cache: false,
    };
    let live = sessions.live_backends();
    let mut started = Vec::new();
    let mut failures: Vec<SetupError> = Vec::new();
    for &(backend, langs, prepared) in jobs {
        if live.iter().any(|id| id == backend.id()) {
            continue;
        }
        let loaded: Vec<(&FileRecord, Vec<u8>)> = index
            .files
            .iter()
            .filter(|f| {
                f.support == SupportLevel::Semantic && f.pending.is_none() && langs.contains(&f.language)
            })
            .filter_map(|f| read_source(&paths.root, &f.path).ok().map(|b| (f, b)))
            .collect();
        let probe = loaded
            .iter()
            .min_by_key(|(f, b)| {
                let calls = f.facts.as_ref().map_or(usize::MAX, |x| x.calls.len());
                (calls, b.len(), f.path.clone())
            })
            .map(|(f, _)| f.path.clone());
        let Some(probe) = probe else {
            continue;
        };
        let files: Vec<SemanticFile<'_>> = loaded
            .iter()
            .map(|(f, b)| SemanticFile {
                path: &f.path,
                language: f.language,
                hash: Hash32::of(b),
                source: b,
                facts: f.facts.as_ref().unwrap_or(&empty),
            })
            .collect();
        let query: HashSet<String> = HashSet::from([probe]);
        let request = SemanticRequest {
            repo: paths,
            files: &files,
            configs: &config_refs,
            query: &query,
            tools,
            prepared,
        };
        match sessions.run(backend, &request, &policy) {
            Ok(_) => started.push(backend.id().to_string()),
            Err(e) => {
                let language = main_language(loaded.iter().map(|(f, _)| f.language), langs);
                let limits = (tools.request_timeout, tools.session_deadline);
                failures.push(backend_failure(paths, backend.id(), (langs, language), limits, e)?);
            }
        }
    }
    if !failures.is_empty() {
        return Err(SetupError::combine(failures).into());
    }
    Ok(started)
}

/// Phase 6: request every planned backend's partition through the semantic sessions (per-file
/// cache, process pool, persistent sessions). Only the files `semantic_requery` selects are
/// asked; the others keep their previous results. Every failure of every backend is collected
/// into one error.
pub(super) fn semantic_phase(
    build: &mut Build<'_>,
    plan: &UpdatePlan,
    sem: &SemanticPlan<'_>,
    configs: &[HashedEntry],
    options: SemanticOptions<'_>,
    progress: &mut dyn IndexProgress,
) -> Result<(Vec<BackendRun>, SemanticCounts, incremental::Requery)> {
    let mut counts = SemanticCounts::default();
    let tools = sem.tools;
    let SemanticOptions {
        rebuild,
        sessions,
        persistent,
        interfaces,
        declared,
        policy: stale_policy,
    } = options;
    // Union over the partitions: files re-queried now and dependents left stale.
    let mut requery_all = incremental::Requery::default();
    let current: Vec<(String, Language)> = build
        .files
        .iter()
        .filter(|f| !f.skip && f.pending.is_none())
        .map(|f| (f.path.clone(), f.language))
        .collect();
    let policy = RunPolicy {
        persistent,
        use_cache: !rebuild,
    };
    let mut config_bytes: Option<Vec<(String, Vec<u8>)>> = None;
    let total = sem.assign.len();
    // Pass 1 (sequential): partitions, queries and file loads; unchanged partitions keep
    // their results. Runs are collected with their backend position and sorted at the end,
    // so the report order does not depend on scheduling.
    let mut ordered_runs: Vec<(usize, BackendRun)> = Vec::new();
    let mut jobs: Vec<SemanticJob<'_>> = Vec::new();
    for (bi, (backend, langs)) in sem.assign.iter().enumerate() {
        progress.phase("semantic", bi, total);
        let part = partition(&build.files, langs);
        if part.is_empty() {
            continue;
        }
        let requery = incremental::semantic_requery(incremental::RequeryInput {
            prev: build.reuse_prev,
            plan,
            partition: langs,
            current_files: &current,
            tool_fingerprint: &sem.fingerprints[bi],
            declared_names: declared,
            interface_changed: interfaces,
            policy: stale_policy,
        });
        requery_all.now.extend(requery.now.iter().cloned());
        requery_all.stale.extend(requery.stale);
        let query = requery.now;
        let version = sem.version_of(backend.id());
        let reuse_prev = build.reuse_prev;
        for &i in &part {
            let f = &mut build.files[i];
            if !query.contains(f.path.as_str()) {
                f.semantic = cached_semantics(reuse_prev, &f.path);
            }
        }
        if query.is_empty() {
            counts.reused += part.len();
            ordered_runs.push((
                bi,
                BackendRun {
                    backend: backend.id().to_string(),
                    languages: langs.clone(),
                    files: part.len() as u32,
                    queried_files: 0,
                    requests: 0,
                    seconds: 0.0,
                    ok: true,
                    error: None,
                    tool_version: version,
                    ready: None,
                },
            ));
            continue;
        }
        // Every file of the partition goes into the workspace (cross-file resolution).
        build.load(&part);
        let root = &build.paths.root;
        config_bytes.get_or_insert_with(|| {
            configs
                .iter()
                .filter_map(|c| {
                    read_source(root, &c.entry.path)
                        .ok()
                        .map(|b| (c.entry.path.clone(), b))
                })
                .collect()
        });
        jobs.push(SemanticJob {
            bi,
            backend: *backend,
            langs: langs.clone(),
            part,
            query,
            version,
        });
    }

    // Pass 2: analyze. One-shot runs of different backends are independent (own caches,
    // own processes, disjoint partitions) and run concurrently; persistent sessions run in
    // order.
    let outcomes: Vec<(std::result::Result<SessionOutput, SemanticError>, f64)> = {
        let build_ref: &Build<'_> = build;
        let cfgs: &[(String, Vec<u8>)] = config_bytes.as_deref().unwrap_or(&[]);
        let config_refs: Vec<(&str, &[u8])> = cfgs.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
        let empty_facts = FileFacts::default();
        let job_files: Vec<Vec<SemanticFile<'_>>> = jobs
            .iter()
            .map(|job| {
                job.part
                    .iter()
                    .filter_map(|&i| {
                        let f = &build_ref.files[i];
                        let b = f.bytes.as_deref()?;
                        Some(SemanticFile {
                            path: &f.path,
                            language: f.language,
                            hash: f.bytes_hash?,
                            source: b,
                            facts: f.facts.as_ref().unwrap_or(&empty_facts),
                        })
                    })
                    .collect()
            })
            .collect();
        let run_one = |job: &SemanticJob<'_>, files: &[SemanticFile<'_>], sessions: &mut SemanticSessions| {
            let started = Instant::now();
            let request = SemanticRequest {
                repo: build_ref.paths,
                files,
                configs: &config_refs,
                query: &job.query,
                tools,
                prepared: &sem.prepared[job.bi],
            };
            let result = sessions.run(job.backend, &request, &policy);
            (result, started.elapsed().as_secs_f64())
        };
        if persistent || jobs.len() < 2 {
            jobs.iter()
                .zip(&job_files)
                .map(|(job, files)| run_one(job, files, &mut *sessions))
                .collect()
        } else {
            let mut forks: Vec<SemanticSessions> =
                jobs.iter().map(|job| sessions.split_off(job.backend.id())).collect();
            let outcomes = std::thread::scope(|scope| {
                let handles: Vec<_> = jobs
                    .iter()
                    .zip(&job_files)
                    .zip(forks.iter_mut())
                    .map(|((job, files), fork)| {
                        let run_one = &run_one;
                        scope.spawn(move || run_one(job, files, fork))
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|h| {
                        h.join().unwrap_or_else(|_| {
                            (
                                Err(SemanticError::Worker("the analysis thread stopped unexpectedly".into())),
                                0.0,
                            )
                        })
                    })
                    .collect::<Vec<_>>()
            });
            for fork in forks {
                sessions.merge(fork);
            }
            outcomes
        }
    };

    // Pass 3 (sequential, backend order): apply results; collect every failure.
    let mut failures: Vec<SetupError> = Vec::new();
    for (job, (result, _seconds)) in jobs.iter().zip(outcomes) {
        let SemanticJob {
            bi,
            backend,
            langs,
            part,
            query,
            version,
        } = job;
        match result {
            Ok(out) => {
                build.diagnostics.extend(out.diagnostics);
                let mut fresh = out.output.files;
                for &i in part {
                    let f = &mut build.files[i];
                    if !query.contains(f.path.as_str()) {
                        continue;
                    }
                    match take_result(&mut fresh, build.paths, backend.id(), &f.path, f.language) {
                        Ok(semantics) => f.semantic = Some(semantics),
                        Err(e) => failures.push(e),
                    }
                }
                let mut run = out.output.run;
                if run.tool_version.is_none() {
                    run.tool_version = version.clone();
                }
                counts.queried += run.queried_files as usize;
                counts.reused += out.cache_hits + (part.len() - query.len().min(part.len()));
                ordered_runs.push((*bi, run));
            }
            Err(e) => {
                let language =
                    main_language(part.iter().filter_map(|&i| build.files.get(i).map(|f| f.language)), langs);
                let limits = (tools.request_timeout, tools.session_deadline);
                failures.push(backend_failure(
                    build.paths,
                    backend.id(),
                    (langs.as_slice(), language),
                    limits,
                    e,
                )?)
            }
        }
    }
    progress.phase("semantic", total, total);
    for f in &mut build.files {
        f.bytes = None;
        f.bytes_hash = None;
    }
    if !failures.is_empty() {
        return Err(SetupError::combine(failures).into());
    }
    ordered_runs.sort_by_key(|(bi, _)| *bi);
    Ok((ordered_runs.into_iter().map(|(_, r)| r).collect(), counts, requery_all))
}

#[cfg(test)]
#[path = "../../tests/unit/pipeline/semantic.rs"]
mod tests;
