use super::*;
use crate::test_support::fixture_paths;

/// Test files are analysed at index time: they are part of their backend's partition like
/// every other product file (no deferral).
#[test]
fn rule_test_files_are_analyzed_at_index() {
    let state = |path: &str, language: Language, pending: Option<&str>| FileState {
        path: path.into(),
        language,
        size: 1,
        mtime_ns: 0,
        hash: Hash32::of(path.as_bytes()),
        facts: Some(FileFacts::default()),
        semantic: None,
        diagnostics: Vec::new(),
        bytes: None,
        bytes_hash: None,
        skip: false,
        pending: pending.map(str::to_string),
    };
    let files = vec![
        state("src/app.py", Language::Python, None),
        state("tests/test_app.py", Language::Python, None),
        state("src/app_test.go", Language::Go, None),
        state("examples/demo.py", Language::Python, Some("sub-project examples")),
    ];
    assert_eq!(partition(&files, &[Language::Python]), vec![0, 1]);
    assert_eq!(partition(&files, &[Language::Go]), vec![2]);
}

/// DESIGN §2: a requested file without a result means the server or one of its shards
/// died: `ServerCrashed` naming the file's language, with a log that exists.
#[test]
fn rule_missing_file_result_is_a_server_crash() {
    let (base, paths) = fixture_paths("missing-result");
    let mut fresh: HashMap<String, FileSemantics> = HashMap::new();
    let err = take_result(&mut fresh, &paths, "lsp:gopls", "cmd/main.go", Language::Go).unwrap_err();
    assert_eq!(err.kind(), "server_crashed");
    let SetupError::ServerCrashed { language, log } = &err else {
        panic!("expected a crash, got {err:?}");
    };
    assert_eq!(*language, Language::Go);
    assert!(log.starts_with(&paths.repo_dir));
    assert!(fs::read_to_string(log).unwrap().contains("cmd/main.go"));
    assert!(err
        .to_string()
        .starts_with("The Go language server stopped unexpectedly. Details: "));
    // Any other backend error is a crash with the details in the log; a setup error stays itself.
    let python: (&[Language], Language) = (&[Language::Python], Language::Python);
    let limits = (Duration::from_secs(60), Duration::from_secs(900));
    let crash =
        backend_failure(&paths, "pyright", python, limits, SemanticError::Worker("boom".into())).unwrap();
    assert_eq!(crash.kind(), "server_crashed");
    let setup = backend_failure(
        &paths,
        "pyright",
        python,
        limits,
        SemanticError::Setup(SetupError::ServerMissing {
            language: Language::Python,
        }),
    )
    .unwrap();
    assert_eq!(setup.kind(), "server_missing");
    assert!(backend_failure(&paths, "pyright", python, limits, SemanticError::SourceChanged("a.py".into()))
        .is_err());
    let _ = fs::remove_dir_all(&base);
}

/// A request that timed out is a timeout (not a crash): `server_timeout` with the minutes
/// of `semantic.request_timeout_secs` (rounded up) and the method in the log.
#[test]
fn rule_request_timeout_is_a_timeout_error() {
    let (base, paths) = fixture_paths("request-timeout");
    let go: (&[Language], Language) = (&[Language::Go], Language::Go);
    let limits = (Duration::from_secs(1), Duration::from_secs(900));
    let err = backend_failure(
        &paths,
        "lsp:gopls",
        go,
        limits,
        SemanticError::Timeout {
            method: "textDocument/definition".into(),
        },
    )
    .unwrap();
    assert_eq!(err.kind(), "server_timeout");
    let SetupError::ServerTimeout {
        language,
        minutes,
        log,
    } = &err
    else {
        panic!("expected a timeout, got {err:?}");
    };
    assert_eq!((*language, *minutes), (Language::Go, 1));
    assert!(fs::read_to_string(log).unwrap().contains("textDocument/definition"));
    assert!(err
        .to_string()
        .starts_with("The Go language server did not finish in 1 minute. Details: "));
    let _ = fs::remove_dir_all(&base);
}

/// A session past its deadline is a timeout with the minutes of
/// `semantic.session_deadline_secs`.
#[test]
fn rule_session_deadline_is_a_timeout_error() {
    let (base, paths) = fixture_paths("session-deadline");
    let java: (&[Language], Language) = (&[Language::Java], Language::Java);
    let limits = (Duration::from_secs(60), Duration::from_secs(600));
    let err = backend_failure(&paths, "lsp:jdtls", java, limits, SemanticError::Deadline).unwrap();
    assert_eq!(err.kind(), "server_timeout");
    assert!(err
        .to_string()
        .starts_with("The Java language server did not finish in 10 minutes. Details: "));
    let _ = fs::remove_dir_all(&base);
}

/// A backend serving several languages names the language with the most analysed files
/// of its partition, also for a crash or timeout the session reported itself.
#[test]
fn rule_backend_error_names_its_main_language() {
    let (base, paths) = fixture_paths("main-language");
    // src/format.c, src/os.cc, src/format.cc, test/a.cc
    let files = [Language::C, Language::Cpp, Language::Cpp, Language::Cpp];
    let langs = [Language::C, Language::Cpp];
    let main = main_language(files, &langs);
    assert_eq!(main, Language::Cpp);
    assert_eq!(
        main_language(std::iter::empty::<Language>(), &langs),
        Language::C,
        "no files: the backend's first language"
    );
    let limits = (Duration::from_secs(60), Duration::from_secs(900));
    let err = backend_failure(
        &paths,
        "lsp:clangd",
        (&langs, main),
        limits,
        SemanticError::ServerExited("exit code 3".into()),
    )
    .unwrap();
    assert_eq!(err.language(), Some(Language::Cpp));
    let reported = SemanticError::Setup(SetupError::ServerTimeout {
        language: Language::C,
        minutes: 15,
        log: PathBuf::from("clangd.log"),
    });
    let err = backend_failure(&paths, "lsp:clangd", (&langs, main), limits, reported).unwrap();
    assert_eq!(err.language(), Some(Language::Cpp));
    assert_eq!(err.kind(), "server_timeout");
    let _ = fs::remove_dir_all(&base);
}

/// I-10: `trace index --watch` starts the language servers at launch: every backend without
/// a live session opens it (process start and readiness) with one warm-up query on its
/// smallest file before the first edit; the index and the per-file cache are untouched,
/// and a backend whose session already runs is not started again.
#[test]
fn rule_watch_warms_servers_before_the_first_edit() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use trace_semantic::backend::BackendOutput;
    use trace_semantic::BackendSession;

    struct Live {
        asked: Arc<Mutex<Vec<String>>>,
    }
    impl BackendSession for Live {
        fn backend_id(&self) -> &str {
            "fake"
        }
        fn fingerprint(&self) -> &str {
            "fp"
        }
        fn processes(&self) -> usize {
            1
        }
        fn update(
            &mut self,
            request: &SemanticRequest<'_>,
        ) -> std::result::Result<BackendOutput, SemanticError> {
            let mut asked: Vec<String> = request.query.iter().cloned().collect();
            asked.sort();
            self.asked.lock().unwrap().extend(asked);
            Ok(BackendOutput {
                files: HashMap::new(),
                run: BackendRun {
                    backend: "fake".into(),
                    languages: vec![Language::Python],
                    files: request.files.len() as u32,
                    queried_files: request.query.len() as u32,
                    requests: 0,
                    seconds: 0.0,
                    ok: true,
                    error: None,
                    tool_version: None,
                    ready: Some(true),
                },
            })
        }
        fn close(self: Box<Self>) {}
    }
    struct Fake {
        opened: Arc<AtomicUsize>,
        asked: Arc<Mutex<Vec<String>>>,
    }
    impl Backend for Fake {
        fn id(&self) -> &str {
            "fake"
        }
        fn languages(&self) -> &[Language] {
            &[Language::Python]
        }
        fn fingerprint(&self, _tools: &ToolEnv, _prepared: &Prepared) -> String {
            "fp".into()
        }
        fn run(&self, _request: &SemanticRequest<'_>) -> std::result::Result<BackendOutput, SemanticError> {
            Err(SemanticError::Worker("the warm-up opens a session".into()))
        }
        fn open_session(
            &self,
            _request: &SemanticRequest<'_>,
        ) -> std::result::Result<Option<Box<dyn BackendSession>>, SemanticError> {
            self.opened.fetch_add(1, Ordering::SeqCst);
            Ok(Some(Box::new(Live {
                asked: Arc::clone(&self.asked),
            })))
        }
    }

    let (base, paths) = fixture_paths("warm-servers");
    fs::write(paths.root.join("big.py"), "def a():\n    b()\n    c()\n").unwrap();
    fs::write(paths.root.join("small.py"), "X = 1\n").unwrap();
    let mut index = crate::test_support::index(&["big.py", "small.py"], Vec::new(), &[]);
    index.files[1].facts = Some(FileFacts::default());
    let tools = ToolEnv::discover(&Settings::default(), &paths.home, &paths.root).unwrap();
    let fake = Fake {
        opened: Arc::new(AtomicUsize::new(0)),
        asked: Arc::new(Mutex::new(Vec::new())),
    };
    let prepared = Prepared::default();
    let backend: &dyn Backend = &fake;
    let langs: &[Language] = &[Language::Python];
    let jobs = vec![(backend, langs, &prepared)];
    let mut sessions = SemanticSessions::in_memory();
    let started = warm_backends(&paths, &tools, &index, &jobs, &mut sessions).unwrap();
    assert_eq!(started, vec!["fake".to_string()]);
    assert_eq!(fake.opened.load(Ordering::SeqCst), 1, "started before any edit");
    assert_eq!(*fake.asked.lock().unwrap(), vec!["small.py".to_string()], "one warm-up query");
    assert_eq!(sessions.live_backends(), vec!["fake".to_string()]);
    // A later check (new language) does not start the live session again.
    assert!(warm_backends(&paths, &tools, &index, &jobs, &mut sessions)
        .unwrap()
        .is_empty());
    assert_eq!(fake.opened.load(Ordering::SeqCst), 1);
    sessions.close_all();
    let _ = fs::remove_dir_all(&base);
}
