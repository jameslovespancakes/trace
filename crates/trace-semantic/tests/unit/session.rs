use super::*;
use crate::cache::{decl_uids, UnitAnswers};
use crate::tools::ToolEnv;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use trace_core::facts::FileFacts;
use trace_core::model::{Provider, SymbolKind};
use trace_core::Hash32;

/// Live analyzer processes of a backend (0 when no session is live).
fn live_processes(sessions: &SemanticSessions, backend_id: &str) -> usize {
    sessions.live.get(backend_id).map_or(0, |s| s.processes())
}

/// Records which files each run/update was asked to analyze.
#[derive(Default)]
struct Log {
    runs: Mutex<Vec<Vec<String>>>,
    reuse: Mutex<Vec<Vec<String>>>,
    sessions_opened: AtomicUsize,
    sessions_closed: AtomicUsize,
    updates: AtomicUsize,
    fail_next_update: Mutex<bool>,
}

struct FakeBackend {
    log: Arc<Log>,
    sessions: bool,
}

fn semantics() -> FileSemantics {
    FileSemantics {
        provider: Provider::Pyright,
        tool_fingerprint: "fp".into(),
        edges: Vec::new(),
        unresolved: Vec::new(),
        value_refs: Vec::new(),
        diagnostics: Vec::new(),
        implementations: Vec::new(),
        resolved_elsewhere: Vec::new(),
        callback_params: Vec::new(),
        library_files: Vec::new(),
        library_calls: Vec::new(),
        outside_build: None,
        expanded: Vec::new(),
        library_dispatch: Vec::new(),
        library_bases: Vec::new(),
    }
}

fn answer(request: &SemanticRequest<'_>) -> BackendOutput {
    let mut queried: Vec<String> = request.query.iter().cloned().collect();
    queried.sort();
    let files = queried.iter().map(|p| (p.clone(), semantics())).collect();
    BackendOutput {
        files,
        run: BackendRun {
            backend: "fake".into(),
            languages: vec![Language::Python],
            files: request.files.len() as u32,
            queried_files: queried.len() as u32,
            requests: 1,
            seconds: 0.0,
            ok: true,
            error: None,
            tool_version: None,
            ready: None,
        },
    }
}

/// Raw answers of a queried file: one unit per function (empty answers).
fn answers_for(request: &SemanticRequest<'_>) -> HashMap<String, FileAnswers> {
    request
        .files
        .iter()
        .filter(|f| request.query.contains(f.path))
        .map(|f| {
            let uids = decl_uids(f.path, f.facts);
            let units = crate::cache::reuse_units(f.facts)
                .into_iter()
                .map(|u| {
                    let span = f.facts.declarations[u as usize].span.bytes;
                    UnitAnswers {
                        uid: uids[u as usize].clone(),
                        body: Hash32::of(&f.source[span.range()]),
                        start: span.start,
                        line: 1,
                        ..UnitAnswers::default()
                    }
                })
                .collect();
            let answers = FileAnswers {
                hash: f.hash,
                interface: f.facts.interface,
                decl_uids: uids,
                units,
                ..FileAnswers::default()
            };
            (f.path.to_string(), answers)
        })
        .collect()
}

impl FakeBackend {
    fn record(&self, request: &SemanticRequest<'_>) {
        let mut queried: Vec<String> = request.query.iter().cloned().collect();
        queried.sort();
        self.log.runs.lock().unwrap().push(queried);
    }
}

struct FakeSessionImpl {
    log: Arc<Log>,
}

impl BackendSession for FakeSessionImpl {
    fn backend_id(&self) -> &str {
        "fake"
    }
    fn fingerprint(&self) -> &str {
        "fp"
    }
    fn processes(&self) -> usize {
        2
    }
    fn update(&mut self, request: &SemanticRequest<'_>) -> Result<BackendOutput, SemanticError> {
        Ok(self.update_reusing(request, &HashMap::new(), None)?.output)
    }
    fn update_reusing(
        &mut self,
        request: &SemanticRequest<'_>,
        reuse: &HashMap<String, FileReuse>,
        _hints: Option<&[String]>,
    ) -> Result<SessionUpdate, SemanticError> {
        self.log.updates.fetch_add(1, Ordering::SeqCst);
        let mut fail = self.log.fail_next_update.lock().unwrap();
        if *fail {
            *fail = false;
            return Err(SemanticError::Setup(SetupError::ServerCrashed {
                language: Language::Python,
                log: PathBuf::from("lsp-0.stderr.log"),
            }));
        }
        let mut queried: Vec<String> = request.query.iter().cloned().collect();
        queried.sort();
        self.log.runs.lock().unwrap().push(queried);
        let mut reused: Vec<String> = reuse.keys().cloned().collect();
        reused.sort();
        self.log.reuse.lock().unwrap().push(reused);
        Ok(SessionUpdate {
            output: answer(request),
            answers: answers_for(request),
            units: HashMap::new(),
            diagnostics: vec![Diagnostic::new("loaded", None, "server loaded")],
        })
    }
    fn references(
        &mut self,
        _request: &SemanticRequest<'_>,
        query: &crate::references::ReferenceQuery,
    ) -> Result<Option<crate::references::LiveReferences>, SemanticError> {
        Ok(Some(found("live", query)))
    }
    fn close(self: Box<Self>) {
        self.log.sessions_closed.fetch_add(1, Ordering::SeqCst);
    }
}

fn found(backend: &str, query: &crate::references::ReferenceQuery) -> crate::references::LiveReferences {
    crate::references::LiveReferences {
        references: vec![crate::references::LiveReference {
            path: query.path.clone(),
            at: trace_core::model::ByteSpan::new(query.byte, query.byte + 1),
            line: 1,
            is_declaration: true,
        }],
        dropped: 0,
        complete: true,
        backend: backend.into(),
    }
}

impl Backend for FakeBackend {
    fn id(&self) -> &str {
        "fake"
    }
    fn languages(&self) -> &[Language] {
        &[Language::Python]
    }
    fn fingerprint(&self, _tools: &ToolEnv, _prepared: &crate::languages::Prepared) -> String {
        "fp".into()
    }
    fn run(&self, request: &SemanticRequest<'_>) -> Result<BackendOutput, SemanticError> {
        self.record(request);
        Ok(answer(request))
    }
    fn open_session(
        &self,
        _request: &SemanticRequest<'_>,
    ) -> Result<Option<Box<dyn BackendSession>>, SemanticError> {
        if !self.sessions {
            return Ok(None);
        }
        self.log.sessions_opened.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Box::new(FakeSessionImpl {
            log: Arc::clone(&self.log),
        })))
    }
    fn references(
        &self,
        _request: &SemanticRequest<'_>,
        query: &crate::references::ReferenceQuery,
    ) -> Result<Option<crate::references::LiveReferences>, SemanticError> {
        Ok(Some(found("one-shot", query)))
    }
}

fn tools() -> ToolEnv {
    crate::test_support::setup::tool_env(None)
}

struct Repo {
    paths: Vec<String>,
    sources: Vec<Vec<u8>>,
    facts: Vec<FileFacts>,
    repo: RepoPaths,
}

impl Repo {
    fn new(files: &[(&str, &str)]) -> Repo {
        let base = std::env::temp_dir()
            .join("trace-tests")
            .join("trace-semantic-tests")
            .join(format!("sessions-{}", uuid::Uuid::new_v4().simple()));
        let root = base.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let repo = RepoPaths::resolve_in(&root, &base.join("home")).unwrap();
        let mut repo = Repo {
            paths: files.iter().map(|(p, _)| p.to_string()).collect(),
            sources: files.iter().map(|(_, s)| s.as_bytes().to_vec()).collect(),
            facts: files.iter().map(|_| FileFacts::default()).collect(),
            repo,
        };
        for i in 0..files.len() {
            repo.refacts(i);
        }
        repo
    }

    /// Syntax facts: one function per `def` line (up to the next one), a stable
    /// interface of the names.
    fn refacts(&mut self, i: usize) {
        let src = self.sources[i].clone();
        let mut facts = FileFacts::default();
        let mut defs: Vec<(u32, String)> = Vec::new();
        let mut start = 0usize;
        for line in src.split_inclusive(|b| *b == b'\n') {
            if let Some(rest) = line.strip_prefix(b"def ") {
                let name: String = rest
                    .iter()
                    .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_')
                    .map(|b| *b as char)
                    .collect();
                defs.push((start as u32, name));
            }
            start += line.len();
        }
        for (k, (at, name)) in defs.iter().enumerate() {
            let end = defs.get(k + 1).map_or(src.len() as u32, |(next, _)| *next);
            facts.declarations.push(crate::test_support::facts::decl_at(
                &src,
                name,
                name,
                SymbolKind::Function,
                (*at, end),
                at + 4,
            ));
        }
        let names: Vec<&str> = defs.iter().map(|(_, n)| n.as_str()).collect();
        facts.interface = Hash32::of(names.join(",").as_bytes());
        self.facts[i] = facts;
    }

    fn edit(&mut self, i: usize, text: &str) {
        self.sources[i] = text.as_bytes().to_vec();
        self.refacts(i);
    }

    fn files(&self) -> Vec<SemanticFile<'_>> {
        (0..self.paths.len())
            .map(|i| SemanticFile {
                path: &self.paths[i],
                language: Language::Python,
                hash: Hash32::of(&self.sources[i]),
                source: &self.sources[i],
                facts: &self.facts[i],
            })
            .collect()
    }

    fn run(
        &self,
        sessions: &mut SemanticSessions,
        backend: &FakeBackend,
        policy: &RunPolicy,
    ) -> SessionOutput {
        let files = self.files();
        let tools = tools();
        let query: HashSet<String> = self.paths.iter().cloned().collect();
        let request = SemanticRequest {
            repo: &self.repo,
            files: &files,
            configs: &[],
            query: &query,
            tools: &tools,
            prepared: &crate::languages::Prepared::default(),
        };
        sessions.run(backend, &request, policy).unwrap()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        if let Some(base) = self.repo.home.parent() {
            let _ = std::fs::remove_dir_all(base);
        }
    }
}

#[test]
fn warm_runs_answer_from_the_cache_and_edits_requery_only_changed_files() {
    let log = Arc::new(Log::default());
    let backend = FakeBackend {
        log: Arc::clone(&log),
        sessions: false,
    };
    let mut repo = Repo::new(&[("a.py", "a = 1\n"), ("b.py", "b = 1\n")]);
    let mut sessions = SemanticSessions::new(&repo.repo).unwrap();
    let policy = RunPolicy {
        use_cache: true,
        ..RunPolicy::default()
    };
    let cold = repo.run(&mut sessions, &backend, &policy);
    assert_eq!(cold.cache_hits, 0);
    assert_eq!(cold.requeried, vec!["a.py".to_string(), "b.py".to_string()]);
    assert_eq!(cold.output.files.len(), 2);
    let warm = repo.run(&mut sessions, &backend, &policy);
    assert_eq!(warm.cache_hits, 2);
    assert!(warm.requeried.is_empty());
    assert_eq!(warm.output.run.queried_files, 0);
    assert_eq!(log.runs.lock().unwrap().len(), 1, "warm run did not start the analyzer");

    repo.edit(1, "b = 2\n");
    let edited = repo.run(&mut sessions, &backend, &policy);
    assert_eq!(edited.cache_hits, 1);
    assert_eq!(edited.requeried, vec!["b.py".to_string()]);
    assert_eq!(log.runs.lock().unwrap().last().unwrap(), &vec!["b.py".to_string()]);
    assert_eq!(edited.output.files.len(), 2);

    // The cache persists: a new process (new sessions object) starts warm.
    drop(sessions);
    let mut fresh = SemanticSessions::new(&repo.repo).unwrap();
    let restarted = repo.run(&mut fresh, &backend, &policy);
    assert_eq!(restarted.cache_hits, 2);
    assert!(repo.repo.repo_dir.join("semantic").join("fake.bin").is_file());
}

/// One mechanism: a run without `persistent` opens a session, updates it once (load
/// notes and raw answers come back) and closes it; the next edit of one function
/// offers the unchanged functions of that file for reuse.
#[test]
fn one_shot_runs_use_a_session_and_offer_reuse() {
    let log = Arc::new(Log::default());
    let backend = FakeBackend {
        log: Arc::clone(&log),
        sessions: true,
    };
    let mut repo = Repo::new(&[("m.py", "def f():\n    return 1\ndef g():\n    return 2\n")]);
    let mut sessions = SemanticSessions::in_memory();
    let policy = RunPolicy {
        use_cache: true,
        ..RunPolicy::default()
    };
    let first = repo.run(&mut sessions, &backend, &policy);
    assert!(first.diagnostics.iter().any(|d| d.kind == "loaded"), "load notes are returned");
    assert_eq!(log.sessions_opened.load(Ordering::SeqCst), 1);
    assert_eq!(log.sessions_closed.load(Ordering::SeqCst), 1, "closed after the run");
    assert_eq!(live_processes(&sessions, "fake"), 0);
    repo.edit(0, "def f():\n    return 10\ndef g():\n    return 2\n");
    let second = repo.run(&mut sessions, &backend, &policy);
    assert_eq!(second.requeried, vec!["m.py".to_string()]);
    assert_eq!(
        log.reuse.lock().unwrap().last().unwrap(),
        &vec!["m.py".to_string()],
        "g is offered for reuse"
    );
    // A changed interface (a new function) offers nothing.
    repo.edit(0, "def f():\n    return 10\ndef g():\n    return 2\ndef h():\n    return 3\n");
    repo.run(&mut sessions, &backend, &policy);
    assert!(log.reuse.lock().unwrap().last().unwrap().is_empty());
}

/// Live references: one-shot without a live session; `persistent` opens (and keeps) a
/// session that later queries and `run` reuse.
#[test]
fn live_references_prefer_warm_sessions() {
    let log = Arc::new(Log::default());
    let backend = FakeBackend {
        log: Arc::clone(&log),
        sessions: true,
    };
    let repo = Repo::new(&[("a.py", "def f(): pass\n")]);
    let files = repo.files();
    let tools = tools();
    let query_set: HashSet<String> = repo.paths.iter().cloned().collect();
    let request = SemanticRequest {
        repo: &repo.repo,
        files: &files,
        configs: &[],
        query: &query_set,
        tools: &tools,
        prepared: &crate::languages::Prepared::default(),
    };
    let query = crate::references::ReferenceQuery {
        path: "a.py".into(),
        byte: 4,
        include_declaration: true,
    };
    let mut sessions = SemanticSessions::in_memory();
    let cold = sessions
        .references(&backend, &request, &query, &RunPolicy::default())
        .unwrap()
        .unwrap();
    assert_eq!(cold.backend, "one-shot");
    assert_eq!(live_processes(&sessions, "fake"), 0);
    let persistent = RunPolicy {
        persistent: true,
        ..RunPolicy::default()
    };
    let warm = sessions
        .references(&backend, &request, &query, &persistent)
        .unwrap()
        .unwrap();
    assert_eq!(warm.backend, "live");
    assert_eq!(log.sessions_opened.load(Ordering::SeqCst), 1);
    assert_eq!(live_processes(&sessions, "fake"), 2);
    let again = sessions
        .references(&backend, &request, &query, &RunPolicy::default())
        .unwrap()
        .unwrap();
    assert_eq!(again.backend, "live", "a live session answers even without persistent");
    assert_eq!(log.sessions_opened.load(Ordering::SeqCst), 1);
    // `run` reuses the session opened by the references query.
    let out = sessions.run(&backend, &request, &persistent).unwrap();
    assert!(out.reused_session);
    sessions.close_all();
}

#[test]
fn persistent_sessions_are_reused_and_restarted_after_a_crash() {
    let log = Arc::new(Log::default());
    let backend = FakeBackend {
        log: Arc::clone(&log),
        sessions: true,
    };
    let mut repo = Repo::new(&[("a.py", "a = 1\n"), ("b.py", "b = 1\n")]);
    let mut sessions = SemanticSessions::in_memory();
    let policy = RunPolicy {
        persistent: true,
        use_cache: true,
    };
    let first = repo.run(&mut sessions, &backend, &policy);
    assert!(!first.reused_session);
    assert_eq!(live_processes(&sessions, "fake"), 2);
    repo.edit(0, "a = 2\n");
    let second = repo.run(&mut sessions, &backend, &policy);
    assert!(second.reused_session);
    assert_eq!(log.sessions_opened.load(Ordering::SeqCst), 1);
    assert_eq!(log.runs.lock().unwrap().last().unwrap(), &vec!["a.py".to_string()]);

    *log.fail_next_update.lock().unwrap() = true;
    repo.edit(0, "a = 3\n");
    let third = repo.run(&mut sessions, &backend, &policy);
    assert!(!third.reused_session);
    assert!(third
        .diagnostics
        .iter()
        .any(|d| d.kind == "semantic_session_restarted"));
    assert_eq!(log.sessions_opened.load(Ordering::SeqCst), 2);
    assert_eq!(third.output.files.len(), 2);
    sessions.close_all();
    assert_eq!(live_processes(&sessions, "fake"), 0);
}

/// Rule (warm sessions): a build-file edit restarts only the backend whose languages it
/// belongs to (Maven for the JVM backends, go.mod for gopls, ...); a source edit or
/// another ecosystem's build file never does.
#[test]
fn rule_build_file_edit_restarts_only_its_backend() {
    use Language::*;
    assert!(is_build_file_for(&[Java], "pom.xml"));
    assert!(is_build_file_for(&[Java], "app/build.gradle.kts"));
    assert!(is_build_file_for(&[Scala], "project/plugins.sbt"));
    assert!(is_build_file_for(&[Scala], "sub/project/build.properties"));
    assert!(is_build_file_for(&[CSharp], "src/App/App.csproj"));
    assert!(is_build_file_for(&[Rust], "crates/core/Cargo.toml"));
    assert!(is_build_file_for(&[Go], "go.mod"));
    assert!(is_build_file_for(&[TypeScript, Tsx], "web/tsconfig.base.json"));
    assert!(is_build_file_for(&[Python], "requirements-dev.txt"));
    assert!(is_build_file_for(&[Haskell], "cabal.project.local"));
    assert!(!is_build_file_for(&[Python], "pom.xml"), "another ecosystem's build file");
    assert!(!is_build_file_for(&[Java], "go.mod"));
    assert!(!is_build_file_for(&[Java], "src/main/java/App.java"), "sources are not build files");
    assert!(!is_build_file_for(&[Bash], "Makefile"));
}

#[test]
fn watcher_hints_accumulate_until_a_run() {
    let log = Arc::new(Log::default());
    let backend = FakeBackend {
        log: Arc::clone(&log),
        sessions: true,
    };
    let repo = Repo::new(&[("a.py", "a = 1\n"), ("b.py", "b = 1\n")]);
    let mut sessions = SemanticSessions::in_memory();
    sessions.hint_changes(Some(vec!["a.py".into()]));
    assert!(sessions.hints.is_empty(), "no live session: nothing to hint");
    let persistent = RunPolicy {
        persistent: true,
        use_cache: false,
    };
    repo.run(&mut sessions, &backend, &persistent);
    assert_eq!(sessions.hints.get("fake"), Some(&Some(Vec::new())), "synced by the run");
    sessions.hint_changes(Some(vec!["b.py".into()]));
    sessions.hint_changes(Some(vec!["a.py".into(), "b.py".into()]));
    assert_eq!(sessions.hints["fake"], Some(vec!["a.py".to_string(), "b.py".to_string()]));
    sessions.hint_changes(None);
    assert_eq!(sessions.hints["fake"], None, "an overflow makes the next sync walk the tree");
    sessions.close_all();
    assert!(sessions.hints.is_empty());
}
