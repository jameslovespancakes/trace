//! Semantic runs with a per-file cache, per-declaration answer reuse and warm
//! language-server sessions (the entry point for `index` and
//! `index --watch`).
//!
//! [`SemanticSessions::run`] for one backend partition:
//! 1. Requested files (`SemanticRequest::query`) whose cached result is still valid
//!    (`cache`) are answered from the cache (no process, no workspace, no
//!    readiness wait when every requested file is cached: `queried_files: 0`, `ready: None`).
//! 2. For the other files the cache offers the answers of unchanged declarations
//!    (`cache::FileReuse`: the file's interface and dependencies are unchanged); the
//!    server is asked only for the rest.
//! 3. The misses are analysed through a [`BackendSession`] - ONE mechanism for every run:
//!    with [`RunPolicy::persistent`] the live session is kept between calls (processes stay
//!    warm; only changed files are re-sent, only requested files re-queried; `index --watch` owns
//!    the sessions for its lifetime), otherwise the session is opened for this run and closed
//!    after it. Backends without sessions answer through `Backend::run`. A live session
//!    whose processes died is restarted once; a session whose tool fingerprint changed is
//!    replaced. Watcher hints ([`SemanticSessions::hint_changes`]) let a mirror workspace
//!    sync only the changed paths.
//! 4. Fresh results and raw per-declaration answers are stored in the cache (persisted
//!    under `<repo cache>/semantic/`).
//!
//! What an incremental update needs (`IndexDelta`, DESIGN §1.14.5) is reported in
//! [`SessionOutput`]: the files analysed now (`requeried`), the files answered from the cache
//! (`cache_hits`) and, per requeried file, how many reuse units were answered from reuse and
//! how many were asked (`units`).
//!
//! Build files (`is_build_file_for`): an edit of a build or project file of a backend's
//! languages (pom.xml, build.gradle*, *.csproj, *.cabal, Cargo.toml, go.mod, package.json,
//! pyproject.toml, ...) restarts only that backend's processes, which then wait for their
//! import-finished readiness signal (`pool.rs`); other backends stay warm.
//!
//! Passing every partition file in `query` is valid and cheap on warm runs: the cache
//! decides what really runs.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use trace_core::model::{BackendRun, Diagnostic};
use trace_core::paths::{ensure_outside, RepoPaths};
use trace_core::semantics::FileSemantics;
use trace_core::{Language, SetupError};

use crate::backend::{Backend, BackendOutput, SemanticFile, SemanticRequest};
use crate::cache::{environment_key, CacheContext, FileAnswers, FileReuse, SemanticCache};
use crate::SemanticError;

/// What one session update produced.
pub struct SessionUpdate {
    pub output: BackendOutput,
    /// Raw per-declaration answers of the analysed files (declaration reuse).
    pub answers: HashMap<String, FileAnswers>,
    /// Per analysed file: (reuse units answered from reuse, reuse units asked).
    pub units: HashMap<String, (u32, u32)>,
    /// Notes of the server's load validation (`Server::check_loaded`).
    pub diagnostics: Vec<Diagnostic>,
}

/// A live analyzer session (process pool + workspace) kept between updates.
pub trait BackendSession: Send {
    fn backend_id(&self) -> &str;
    /// Tool fingerprint the session was started with.
    fn fingerprint(&self) -> &str;
    /// Running analyzer processes.
    fn processes(&self) -> usize;
    /// Bring the workspace up to date with `request.files` / `request.configs` (notifying
    /// the servers) and analyze `request.query`.
    fn update(&mut self, request: &SemanticRequest<'_>) -> Result<BackendOutput, SemanticError>;
    /// [`BackendSession::update`] with the answers of unchanged declarations (`reuse`) and
    /// watcher hints (changed repository paths, `None` = unknown). Sessions without
    /// declaration reuse answer every requested file.
    fn update_reusing(
        &mut self,
        request: &SemanticRequest<'_>,
        reuse: &HashMap<String, FileReuse>,
        hints: Option<&[String]>,
    ) -> Result<SessionUpdate, SemanticError> {
        let _ = (reuse, hints);
        Ok(SessionUpdate {
            output: self.update(request)?,
            answers: HashMap::new(),
            units: HashMap::new(),
            diagnostics: Vec::new(),
        })
    }
    /// Live find-references over the session's (synced) workspace. `Ok(None)` when the
    /// backend cannot answer (the caller uses the index).
    fn references(
        &mut self,
        request: &SemanticRequest<'_>,
        query: &crate::references::ReferenceQuery,
    ) -> Result<Option<crate::references::LiveReferences>, SemanticError> {
        let _ = (request, query);
        Ok(None)
    }
    /// Graceful shutdown (bounded); the workspace stays for the next session.
    fn close(self: Box<Self>);
}

/// How [`SemanticSessions::run`] schedules work.
#[derive(Clone, Copy, Debug, Default)]
pub struct RunPolicy {
    /// Keep analyzer processes alive between runs (`index --watch`).
    pub persistent: bool,
    /// Answer unchanged files from the per-file cache, reuse unchanged declarations and
    /// store fresh results.
    pub use_cache: bool,
}

/// Declaration-reuse counts of one run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReuseStats {
    /// Requeried files that reused at least one unit.
    pub files: usize,
    /// Units answered from reuse.
    pub reused: usize,
    /// Units asked.
    pub asked: usize,
}

/// Result of [`SemanticSessions::run`].
pub struct SessionOutput {
    /// Results for every requested file (fresh or cached). `run` describes the analyzer
    /// work actually done (`queried_files` = files analysed now).
    pub output: BackendOutput,
    /// Requested files answered from the cache.
    pub cache_hits: usize,
    /// Requested files analysed now (sorted): `IndexDelta::requeried` of this backend.
    pub requeried: Vec<String>,
    /// Per requeried file: (reuse units answered from reuse, reuse units asked).
    pub units: BTreeMap<String, (u32, u32)>,
    pub reuse: ReuseStats,
    /// Whether a live session from an earlier run was reused.
    pub reused_session: bool,
    /// Cache, session and load notes (`semantic_cache_discarded`, `semantic_cache_unsaved`,
    /// `semantic_session_restarted`, `Server::check_loaded` notes).
    pub diagnostics: Vec<Diagnostic>,
}

/// Caches and live sessions of one repository.
pub struct SemanticSessions {
    cache_dir: Option<PathBuf>,
    caches: HashMap<String, SemanticCache>,
    live: HashMap<String, Box<dyn BackendSession>>,
    /// Per live backend: repository paths changed since its last update (watcher hints),
    /// `None` = unknown (its next sync walks the workspace).
    hints: HashMap<String, Option<Vec<String>>>,
}

impl SemanticSessions {
    /// Sessions whose caches persist under `<repo cache>/semantic/` (outside the root).
    pub fn new(repo: &RepoPaths) -> Result<SemanticSessions, SemanticError> {
        let dir = repo.repo_dir.join("semantic");
        ensure_outside(&dir, &[&repo.root])?;
        Ok(SemanticSessions {
            cache_dir: Some(dir),
            caches: HashMap::new(),
            live: HashMap::new(),
            hints: HashMap::new(),
        })
    }

    /// Sessions with in-memory caches only.
    pub fn in_memory() -> SemanticSessions {
        SemanticSessions {
            cache_dir: None,
            caches: HashMap::new(),
            live: HashMap::new(),
            hints: HashMap::new(),
        }
    }

    /// Watcher hints (the `index --watch` watcher): repository-relative paths changed since the last
    /// update, `None` when unknown (watcher overflow: the next sync walks the tree). Hints
    /// accumulate per live backend until its next update consumes them; a fresh session
    /// always syncs its whole workspace.
    pub fn hint_changes(&mut self, changed: Option<Vec<String>>) {
        for entry in self.hints.values_mut() {
            match (entry.as_mut(), &changed) {
                (Some(known), Some(more)) => {
                    known.extend(more.iter().cloned());
                    known.sort();
                    known.dedup();
                }
                _ => *entry = None,
            }
        }
    }

    /// Move one backend's cache and live session into a separate set, so backends of
    /// different partitions can run concurrently; [`SemanticSessions::merge`] takes them back.
    pub fn split_off(&mut self, backend_id: &str) -> SemanticSessions {
        let mut caches = HashMap::new();
        if let Some(c) = self.caches.remove(backend_id) {
            caches.insert(backend_id.to_string(), c);
        }
        let mut live = HashMap::new();
        if let Some(s) = self.live.remove(backend_id) {
            live.insert(backend_id.to_string(), s);
        }
        let mut hints = HashMap::new();
        if let Some(h) = self.hints.remove(backend_id) {
            hints.insert(backend_id.to_string(), h);
        }
        SemanticSessions {
            cache_dir: self.cache_dir.clone(),
            caches,
            live,
            hints,
        }
    }

    /// Take back the caches and live sessions of a set made by [`SemanticSessions::split_off`].
    pub fn merge(&mut self, mut other: SemanticSessions) {
        self.caches.extend(std::mem::take(&mut other.caches));
        self.live.extend(std::mem::take(&mut other.live));
        self.hints.extend(std::mem::take(&mut other.hints));
    }

    /// Backend ids with a live session (sorted).
    pub fn live_backends(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.live.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Stop a backend's live session.
    pub fn close(&mut self, backend_id: &str) {
        self.hints.remove(backend_id);
        if let Some(session) = self.live.remove(backend_id) {
            session.close();
        }
    }

    /// Stop every live session.
    pub fn close_all(&mut self) {
        for id in self.live_backends() {
            self.close(&id);
        }
    }

    /// Live find-references (SPEC §8.9): through the backend's live session when one exists
    /// (or `policy.persistent` asks for one), else a one-shot `Backend::references`.
    /// `Ok(None)` = unsupported by this backend (callers use the index).
    ///
    /// A live session with another tool fingerprint is replaced; a live session that fails
    /// in a restartable way (process died, broken stream) is dropped and the query is
    /// answered by a fresh session (`policy.persistent`) or one-shot.
    pub fn references(
        &mut self,
        backend: &dyn Backend,
        request: &SemanticRequest<'_>,
        query: &crate::references::ReferenceQuery,
        policy: &RunPolicy,
    ) -> Result<Option<crate::references::LiveReferences>, SemanticError> {
        let id = backend.id().to_string();
        let fingerprint = backend.fingerprint(request.tools, request.prepared);
        if self.live.get(&id).is_some_and(|s| s.fingerprint() != fingerprint) {
            self.close(&id);
        }
        if let Some(mut session) = self.live.remove(&id) {
            match session.references(request, query) {
                Ok(Some(found)) => {
                    self.live.insert(id, session);
                    return Ok(Some(found));
                }
                Ok(None) => {
                    // The live session cannot answer; keep it for `run`.
                    self.live.insert(id, session);
                    return backend.references(request, query);
                }
                Err(e) if restartable(&e) => session.close(),
                Err(e) => {
                    self.live.insert(id, session);
                    return Err(e);
                }
            }
        }
        if policy.persistent {
            if let Some(mut session) = backend.open_session(request)? {
                let found = session.references(request, query)?;
                self.live.insert(id, session);
                if found.is_some() {
                    return Ok(found);
                }
            }
        }
        backend.references(request, query)
    }

    /// Analyze `request.query` of `backend`'s partition under `policy`.
    pub fn run(
        &mut self,
        backend: &dyn Backend,
        request: &SemanticRequest<'_>,
        policy: &RunPolicy,
    ) -> Result<SessionOutput, SemanticError> {
        let started = Instant::now();
        let id = backend.id().to_string();
        let fingerprint = backend.fingerprint(request.tools, request.prepared);
        let files: Vec<&SemanticFile<'_>> = request
            .files
            .iter()
            .filter(|f| backend.languages().contains(&f.language))
            .collect();
        let mut diagnostics = Vec::new();
        let mut wanted: Vec<&str> = files
            .iter()
            .filter(|f| request.query.contains(f.path))
            .map(|f| f.path)
            .collect();
        wanted.sort_unstable();

        // 1. Whole-file cache hits, then declaration reuse for the misses.
        let env = environment_key(&id, &fingerprint, request.configs, backend.snapshot_configs());
        let mut ctx = CacheContext::new(env, &files);
        let mut results: HashMap<String, FileSemantics> = HashMap::new();
        let mut misses: HashSet<String> = HashSet::new();
        let mut reuse: HashMap<String, FileReuse> = HashMap::new();
        if policy.use_cache {
            let cache = self.cache_for(&id, &mut diagnostics);
            for path in &wanted {
                match cache.lookup(&mut ctx, path) {
                    Some(sem) => {
                        results.insert(path.to_string(), sem);
                    }
                    None => {
                        if let Some(r) = cache.reuse(&mut ctx, path) {
                            reuse.insert(path.to_string(), r);
                        }
                        misses.insert(path.to_string());
                    }
                }
            }
        } else {
            misses.extend(wanted.iter().map(|p| p.to_string()));
        }
        let cache_hits = results.len();

        // 2. Analyze the misses.
        let mut reused_session = false;
        let mut units: BTreeMap<String, (u32, u32)> = BTreeMap::new();
        let run = if misses.is_empty() {
            BackendRun {
                backend: id.clone(),
                languages: backend.languages().to_vec(),
                files: files.len() as u32,
                queried_files: 0,
                requests: 0,
                seconds: started.elapsed().as_secs_f64(),
                ok: true,
                error: None,
                tool_version: None,
                ready: None,
            }
        } else {
            let narrowed = SemanticRequest {
                repo: request.repo,
                files: request.files,
                configs: request.configs,
                query: &misses,
                tools: request.tools,
                prepared: request.prepared,
            };
            let update = if policy.persistent {
                let hints = self.hints.remove(&id).flatten();
                let update = self.run_live(
                    backend,
                    &fingerprint,
                    &narrowed,
                    &reuse,
                    hints.as_deref(),
                    &mut reused_session,
                    &mut diagnostics,
                )?;
                if self.live.contains_key(&id) {
                    // Synced now: later hints are exact again.
                    self.hints.insert(id.clone(), Some(Vec::new()));
                }
                update
            } else {
                run_session(backend, &narrowed, &reuse)?
            };
            diagnostics.extend(update.diagnostics);
            // 3. Cache fresh results and raw answers (the whole fresh output: some backends
            //    analyze more than asked).
            if policy.use_cache {
                let cache = self.cache_for(&id, &mut diagnostics);
                for (path, sem) in &update.output.files {
                    cache.store(&mut ctx, path, sem);
                }
                for (path, answers) in update.answers {
                    if let Some(sem) = update.output.files.get(&path) {
                        cache.store_answers(&mut ctx, &path, answers, sem);
                    }
                }
            }
            units.extend(update.units.into_iter().filter(|(p, _)| misses.contains(p)));
            for (path, sem) in update.output.files {
                if misses.contains(&path) || !results.contains_key(&path) {
                    results.insert(path, sem);
                }
            }
            update.output.run
        };
        if policy.use_cache {
            let partition: HashSet<&str> = files.iter().map(|f| f.path).collect();
            let cache = self.cache_for(&id, &mut diagnostics);
            cache.retain_paths(|p| partition.contains(p));
            if let Err(e) = cache.save() {
                diagnostics.push(Diagnostic::new(
                    "semantic_cache_unsaved",
                    None,
                    format!("{id} semantic cache not saved: {e}"),
                ));
            }
        }
        let mut requeried: Vec<String> = misses.into_iter().collect();
        requeried.sort();
        let reuse_stats = ReuseStats {
            files: units.values().filter(|(r, _)| *r > 0).count(),
            reused: units.values().map(|(r, _)| *r as usize).sum(),
            asked: units.values().map(|(_, a)| *a as usize).sum(),
        };
        Ok(SessionOutput {
            output: BackendOutput { files: results, run },
            cache_hits,
            requeried,
            units,
            reuse: reuse_stats,
            reused_session,
            diagnostics,
        })
    }

    /// Update the live session (starting or restarting it when needed).
    #[allow(clippy::too_many_arguments)]
    fn run_live(
        &mut self,
        backend: &dyn Backend,
        fingerprint: &str,
        request: &SemanticRequest<'_>,
        reuse: &HashMap<String, FileReuse>,
        hints: Option<&[String]>,
        reused: &mut bool,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<SessionUpdate, SemanticError> {
        let id = backend.id().to_string();
        if self.live.get(&id).is_some_and(|s| s.fingerprint() != fingerprint) {
            self.close(&id);
        }
        if let Some(mut session) = self.live.remove(&id) {
            match session.update_reusing(request, reuse, hints) {
                Ok(update) => {
                    self.live.insert(id, session);
                    *reused = true;
                    return Ok(update);
                }
                Err(e) if restartable(&e) => {
                    session.close();
                    diagnostics.push(Diagnostic::new(
                        "semantic_session_restarted",
                        None,
                        format!("{id} session restarted after: {e}"),
                    ));
                }
                Err(e) => return Err(e),
            }
        }
        match backend.open_session(request)? {
            Some(mut session) => {
                // A fresh session syncs its whole workspace: no hints.
                let update = session.update_reusing(request, reuse, None)?;
                self.live.insert(id, session);
                Ok(update)
            }
            None => Ok(one_shot(backend.run(request)?)),
        }
    }

    fn cache_for(&mut self, id: &str, diagnostics: &mut Vec<Diagnostic>) -> &mut SemanticCache {
        let dir = self.cache_dir.clone();
        self.caches.entry(id.to_string()).or_insert_with(|| match dir {
            Some(dir) => {
                let (cache, warning) = SemanticCache::load(&dir, id);
                if let Some(warning) = warning {
                    diagnostics.push(Diagnostic::new("semantic_cache_discarded", None, warning));
                }
                cache
            }
            None => SemanticCache::in_memory(id),
        })
    }
}

impl Drop for SemanticSessions {
    fn drop(&mut self) {
        self.close_all();
    }
}

/// A run without a kept session: open one for this run and close it after (the same
/// mechanism as warm runs), or `Backend::run` for backends without sessions.
fn run_session(
    backend: &dyn Backend,
    request: &SemanticRequest<'_>,
    reuse: &HashMap<String, FileReuse>,
) -> Result<SessionUpdate, SemanticError> {
    match backend.open_session(request)? {
        Some(mut session) => {
            let update = session.update_reusing(request, reuse, None);
            session.close();
            update
        }
        None => Ok(one_shot(backend.run(request)?)),
    }
}

fn one_shot(output: BackendOutput) -> SessionUpdate {
    SessionUpdate {
        output,
        answers: HashMap::new(),
        units: HashMap::new(),
        diagnostics: Vec::new(),
    }
}

/// A live session failed in a way a fresh session may not (process died, broken stream).
fn restartable(error: &SemanticError) -> bool {
    matches!(
        error,
        SemanticError::ServerExited(_)
            | SemanticError::Setup(SetupError::ServerCrashed { .. })
            | SemanticError::Protocol(_)
            | SemanticError::Io(_)
            | SemanticError::Worker(_)
    )
}

/// Build and project files whose edit changes a server's project model, per language
/// (DESIGN §1.14.1). Name patterns match anywhere in the tree (`crate::mirror::glob_match`).
const BUILD_FILES: &[(&str, &[Language])] = {
    use Language::*;
    const JVM: &[Language] = &[Java, Scala];
    const WEB: &[Language] = &[JavaScript, TypeScript, Tsx];
    &[
        ("pom.xml", JVM),
        ("build.gradle*", JVM),
        ("settings.gradle*", JVM),
        ("gradle.properties", JVM),
        ("*.csproj", &[CSharp]),
        ("*.sln", &[CSharp]),
        ("*.slnx", &[CSharp]),
        ("Directory.Build.*", &[CSharp]),
        ("Directory.Packages.props", &[CSharp]),
        ("*.cabal", &[Haskell]),
        ("cabal.project*", &[Haskell]),
        ("stack.yaml", &[Haskell]),
        ("package.yaml", &[Haskell]),
        ("DESCRIPTION", &[R]),
        ("build.sbt", &[Scala]),
        ("**/project/*.sbt", &[Scala]),
        ("**/project/build.properties", &[Scala]),
        ("Cargo.toml", &[Rust]),
        ("Cargo.lock", &[Rust]),
        ("CMakeLists.txt", &[C, Cpp]),
        ("*.cmake", &[C, Cpp]),
        ("meson.build", &[C, Cpp]),
        ("go.mod", &[Go]),
        ("go.sum", &[Go]),
        ("go.work", &[Go]),
        ("package.json", WEB),
        ("tsconfig*.json", WEB),
        ("jsconfig*.json", WEB),
        ("pyproject.toml", &[Python]),
        ("requirements*.txt", &[Python]),
        ("composer.json", &[Php]),
        ("composer.lock", &[Php]),
    ]
};

/// Whether `path` (repository-relative) is a build or project file of one of `languages`:
/// its edit restarts that backend's session (re-import), and only that one.
pub(crate) fn is_build_file_for(languages: &[Language], path: &str) -> bool {
    BUILD_FILES.iter().any(|(pattern, owners)| {
        owners.iter().any(|l| languages.contains(l)) && crate::mirror::glob_match(pattern, path)
    })
}

#[cfg(test)]
#[path = "../tests/unit/session.rs"]
mod tests;
