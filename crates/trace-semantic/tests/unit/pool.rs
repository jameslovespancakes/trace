use super::*;
use crate::test_support::facts::{decl_at, find};
use crate::test_support::session::{call, prepared, range, FakeSession, FakeUris, Handler};
use serde_json::{json, Value};
use trace_core::model::SymbolKind;

#[test]
fn pool_size_follows_the_work() {
    assert_eq!(desired_processes(0, 8), 1);
    assert_eq!(desired_processes(1, 8), 1);
    assert_eq!(desired_processes(10, 8), 1);
    assert_eq!(desired_processes(11, 8), 2);
    assert_eq!(desired_processes(400, 8), 8);
    assert_eq!(desired_processes(400, 0), 1);
}

/// Rule 16: K is the largest pool (<= 4, <= ceil(queried / 10)) whose estimated memory
/// fits the budget; the Node heap cap appears only when one process is estimated above
/// the budget; budget 0 = unbounded. Partition sizes of the lspbench measurements.
#[test]
fn rule_pool_size_follows_memory_budget() {
    let small = (332, 3_800); // 0.56 GB with one process
    let medium = (674, 31_800); // 2.5 GB / 3.6 GB (2) / 4.4 GB (4)
    let large = (2_452, 55_800); // 4.1 GB with one process
    let size = |(files, callables): (usize, usize), budget| pool_size(files, 4, files, callables, budget);
    let cap = |(files, callables): (usize, usize), budget| heap_cap_mb(files, callables, budget);
    assert_eq!(size(small, 4_096), 4);
    assert_eq!(cap(small, 4_096), None);
    assert_eq!(size(medium, 4_096), 2);
    assert_eq!(cap(medium, 4_096), None);
    assert_eq!(size(large, 4_096), 1);
    assert_eq!(cap(large, 4_096), Some(4_096), "one process above the budget: heap cap");
    for p in [small, medium, large] {
        assert_eq!(size(p, 0), 4, "0 = unbounded: the maximum pool");
        assert_eq!(cap(p, 0), None);
    }
    // Never above the work (10 files per process) or the configured maximum.
    assert_eq!(pool_size(15, 4, 332, 3_800, 4_096), 2);
    assert_eq!(pool_size(332, 1, 332, 3_800, 0), 1);
    assert_eq!(pool_size(332, 4, 332, 3_800, 10), 1, "never below one process");
    // The model is monotone and reproduces the measured single-process sizes (+-5%).
    for (files, callables, measured) in [(332, 3_800, 560u64), (674, 31_800, 2_500), (2_452, 55_800, 4_100)] {
        let one = estimate_mb(files, callables, 1);
        assert!(one.abs_diff(measured) * 100 <= measured * 5, "{files}: {one} vs {measured}");
        assert!(estimate_mb(files, callables, 2) > one);
        assert!(estimate_mb(files + 1, callables + 1_000, 1) > one);
    }
}

/// Rule 16: whole directories go to one process, an oversized directory is split into
/// contiguous runs, sticky files stay, and the result is a deterministic partition.
#[test]
fn rule_directory_grouped_sharding_is_deterministic() {
    let paths = [
        "pkg/a/1.py",
        "pkg/a/2.py",
        "pkg/a/3.py",
        "pkg/a/4.py",
        "pkg/a/5.py",
        "pkg/a/6.py",
        "pkg/b/1.py",
        "pkg/b/2.py",
        "pkg/c/1.py",
        "pkg/c/2.py",
        "pkg/d/1.py",
        "pkg/d/2.py",
    ];
    let weights = [1u64; 12];
    let none = HashMap::new();
    let shards = assign(&paths, &weights, 3, &none);
    assert_eq!(shards, assign(&paths, &weights, 3, &none), "deterministic");
    let mut all: Vec<usize> = shards.concat();
    all.sort_unstable();
    assert_eq!(all, (0..12).collect::<Vec<_>>(), "a partition of the files");
    let owner = |i: usize| shards.iter().position(|s| s.contains(&i)).unwrap();
    // pkg/a (weight 6) exceeds the target 4: split into contiguous runs [1..4], [5, 6].
    assert_eq!(owner(0), owner(3));
    assert_ne!(owner(3), owner(4));
    assert_eq!(owner(4), owner(5));
    // Directories within the target stay whole.
    for (x, y) in [(6, 7), (8, 9), (10, 11)] {
        assert_eq!(owner(x), owner(y), "{} and {} share a process", paths[x], paths[y]);
    }
    let loads: Vec<usize> = shards.iter().map(Vec::len).collect();
    assert!(loads.iter().max().unwrap() - loads.iter().min().unwrap() <= 2, "{loads:?}");
    // Sticky files stay where they are open, whatever their directory.
    let sticky: HashMap<String, usize> = [("pkg/c/2.py".to_string(), 2)].into_iter().collect();
    let shards = assign(&paths, &weights, 3, &sticky);
    assert!(shards[2].contains(&9));
    // One process: everything, in order.
    assert_eq!(assign(&paths, &weights, 1, &none), vec![(0..12).collect::<Vec<_>>()]);
}

#[test]
fn assignment_is_contiguous_balanced_and_deterministic() {
    let paths = ["a/1.py", "a/2.py", "a/3.py", "b/1.py", "b/2.py", "b/3.py"];
    let weights = [1, 1, 1, 1, 1, 1];
    let shards = assign(&paths, &weights, 2, &HashMap::new());
    assert_eq!(shards, vec![vec![0, 1, 2], vec![3, 4, 5]]);
    assert_eq!(shards, assign(&paths, &weights, 2, &HashMap::new()));
    // Heavy files spread out.
    let shards = assign(&paths, &[10, 1, 1, 1, 1, 10], 2, &HashMap::new());
    let load = |s: &Vec<usize>| s.iter().map(|&i| [10u64, 1, 1, 1, 1, 10][i]).sum::<u64>();
    assert!(load(&shards[0]).abs_diff(load(&shards[1])) <= 10);
    assert_eq!(shards.iter().map(Vec::len).sum::<usize>(), 6);
    // Sticky files stay where they are open.
    let sticky: HashMap<String, usize> = [("b/3.py".to_string(), 0)].into_iter().collect();
    let shards = assign(&paths, &weights, 2, &sticky);
    assert!(shards[0].contains(&5));
    let mut all: Vec<usize> = shards.concat();
    all.sort_unstable();
    assert_eq!(all, vec![0, 1, 2, 3, 4, 5]);
}

/// Rule (request sharding): the requests of ONE file are spread round-robin over the
/// processes of a request-sharded pool, answers come back in order, and the result
/// equals a single-process analysis. The shard count follows cores, the entry, the
/// memory budget and the work.
#[test]
fn rule_request_sharding_splits_one_file() {
    let body: String = (0..9).map(|_| "  greet\n").collect();
    let src = format!("greet() {{\n  true\n}}\nrun() {{\n{body}}}\n").into_bytes();
    let greet_at = find(&src, "greet() {");
    let run_at = find(&src, "run() {");
    let mut calls = Vec::new();
    let mut from = run_at as usize;
    while let Some(at) = src[from..].windows(7).position(|w| w == b"  greet") {
        let at = (from + at + 2) as u32;
        calls.push(call(at, "greet", Some(1), 0));
        from = at as usize + 1;
    }
    assert_eq!(calls.len(), 9);
    let facts = FileFacts {
        declarations: vec![
            decl_at(&src, "greet", "greet", SymbolKind::Function, (greet_at, run_at), greet_at),
            decl_at(&src, "run", "run", SymbolKind::Function, (run_at, src.len() as u32), run_at),
        ],
        calls,
        ..FileFacts::default()
    };
    let file = SemanticFile {
        path: "main.go",
        language: Language::Go,
        hash: Hash32::of(&src),
        source: &src,
        facts: &facts,
    };
    let decls = DeclTable::new([(file.path, file.source, file.facts)]);
    let handler = || -> Handler {
        Box::new(|method, _| {
            assert_eq!(method, "textDocument/definition");
            Ok(json!([{"uri": "file:///ws/main.go", "range": range(0, 0, 5)}]))
        })
    };
    let caps = json!({"definitionProvider": true});
    let opts = Options {
        provider: Provider::Lsp("test".into()),
        tool_fingerprint: "fp",
        python: false,
        syntax_answers: false,
        hooks: &crate::languages::DefaultServer,
        prepared: crate::test_support::session::empty_prepared(),
        calls_by_definition: false,
        reuse: None,
    };
    let mut single = FakeSession::new(caps.clone(), handler());
    let whole = engine::analyze(&mut single, &[&file], &decls, &FakeUris, &opts).unwrap();
    let mut a = FakeSession::new(caps.clone(), handler());
    let mut b = FakeSession::new(caps.clone(), handler());
    let mut c = FakeSession::new(caps, handler());
    let split = {
        let parts: Vec<&mut (dyn Session + Send)> = vec![&mut a, &mut b, &mut c];
        let mut sharded = Sharded::new(parts).unwrap();
        engine::analyze(&mut sharded, &[&file], &decls, &FakeUris, &opts).unwrap()
    };
    assert_eq!(
        (a.requests.len(), b.requests.len(), c.requests.len()),
        (3, 3, 3),
        "one file, three processes"
    );
    assert_eq!(split.files, whole.files);
    assert_eq!(split.files["main.go"].edges.len(), 9);
    assert_eq!(request_shards(8, 8, 4_096, 500), 8);
    assert_eq!(request_shards(8, 2, 4_096, 500), 2, "cores / 2");
    assert_eq!(request_shards(0, 8, 300, 500), 2, "memory budget");
    assert_eq!(request_shards(8, 8, 0, 30), 1, "little work: one process");
    assert!(Sharded::new(Vec::new()).is_err());
}

/// A change sink recording the order of operations.
#[derive(Default)]
struct Recorder {
    open: HashMap<String, i32>,
    log: Vec<String>,
}

impl ChangeSink for Recorder {
    fn watched(&mut self, changes: &[(String, FileChange)]) -> Result<(), SemanticError> {
        self.log.push(format!("watched {}", changes.len()));
        Ok(())
    }
    fn version(&self, uri: &str) -> Option<i32> {
        self.open.get(uri).copied()
    }
    fn change(&mut self, uri: &str, version: i32, _text: &str) -> Result<(), SemanticError> {
        self.open.insert(uri.to_string(), version);
        self.log.push(format!("change {uri} {version}"));
        Ok(())
    }
    fn close(&mut self, uri: &str) -> Result<(), SemanticError> {
        self.open.remove(uri);
        self.log.push(format!("close {uri}"));
        Ok(())
    }
    fn confirm(&mut self, uris: &[String]) -> Result<(), SemanticError> {
        self.log.push(format!("confirm {}", uris.join(",")));
        Ok(())
    }
    fn settle(&mut self, changed: &[String]) -> Result<(), SemanticError> {
        self.log.push(format!("settle {}", changed.join(",")));
        // A re-index that does not finish (the hook's bounded wait ran out).
        if changed.iter().any(|u| u.ends_with("slow.php")) {
            return Err(SemanticError::Setup(trace_core::SetupError::ServerTimeout {
                language: Language::Php,
                minutes: 1,
                log: std::path::PathBuf::from("lsp.log"),
            }));
        }
        Ok(())
    }
}

/// Rule (warm sessions): a changed file is sent (watched-file event, `didChange` of open
/// documents), then confirmed by an ordered request on every changed source before any
/// re-query; removed documents are closed.
#[test]
fn rule_did_change_is_confirmed_before_requery() {
    let mut sink = Recorder::default();
    sink.open.insert("file:///ws/a.py".into(), 1);
    sink.open.insert("file:///ws/gone.py".into(), 3);
    let changes = vec![
        ("file:///ws/a.py".to_string(), FileChange::Changed),
        ("file:///ws/c.py".to_string(), FileChange::Created),
        ("file:///ws/gone.py".to_string(), FileChange::Deleted),
    ];
    let sources = vec![
        ("file:///ws/a.py".to_string(), Some("x = 2\n")),
        ("file:///ws/c.py".to_string(), Some("y = 1\n")),
    ];
    apply_changes(&mut sink, &changes, &sources, &["file:///ws/gone.py".to_string()]).unwrap();
    assert_eq!(
        sink.log,
        vec![
            "watched 3".to_string(),
            "change file:///ws/a.py 2".to_string(),
            "close file:///ws/gone.py".to_string(),
            "confirm file:///ws/a.py,file:///ws/c.py".to_string(),
            "settle file:///ws/a.py,file:///ws/c.py".to_string(),
        ]
    );
    // Invalid UTF-8 now: the open document is closed, nothing to confirm for it.
    let mut sink = Recorder::default();
    sink.open.insert("file:///ws/a.py".into(), 1);
    apply_changes(&mut sink, &[], &[("file:///ws/a.py".to_string(), None)], &[]).unwrap();
    assert_eq!(sink.log, vec!["watched 0", "close file:///ws/a.py", "confirm ", "settle "]);
}

/// Rule (servers that re-index after an edit): the backend's settle hook runs after the
/// ordered confirmation and before any re-query, over every changed source; its setup
/// error (a re-index that does not finish) stops the update.
#[test]
fn rule_server_settles_changes_after_the_confirmation() {
    let mut sink = Recorder::default();
    let changes = vec![("file:///ws/b.php".to_string(), FileChange::Changed)];
    let sources = vec![("file:///ws/b.php".to_string(), Some("<?php\n"))];
    apply_changes(&mut sink, &changes, &sources, &[]).unwrap();
    let at = |prefix: &str| sink.log.iter().position(|l| l.starts_with(prefix)).unwrap();
    assert!(at("confirm") < at("settle"), "{:?}", sink.log);
    assert_eq!(sink.log.last().map(String::as_str), Some("settle file:///ws/b.php"));
    // A settle failure stops the update with its setup error.
    let mut sink = Recorder::default();
    let sources = vec![("file:///ws/slow.php".to_string(), Some("<?php\n"))];
    let err = apply_changes(&mut sink, &[], &sources, &[]).unwrap_err();
    assert!(matches!(err, SemanticError::Setup(trace_core::SetupError::ServerTimeout { .. })));
}

/// Sharded analysis over several (fake) processes merges to exactly the single-process
/// result.
#[test]
fn sharded_results_equal_single_process_results() {
    let sources: Vec<(String, Vec<u8>)> = (0..6)
        .map(|i| {
            let next = (i + 1) % 6;
            (format!("m{i}.py"), format!("def f{i}():\n    f{next}()\n    missing{i}()\n").into_bytes())
        })
        .collect();
    let facts: Vec<FileFacts> = sources
        .iter()
        .enumerate()
        .map(|(i, (_, src))| {
            let next = (i + 1) % 6;
            let call_next = find(src, &format!("f{next}()"));
            let call_missing = find(src, "missing");
            FileFacts {
                declarations: vec![decl_at(
                    src,
                    &format!("f{i}"),
                    &format!("f{i}"),
                    SymbolKind::Function,
                    (0, src.len() as u32 - 1),
                    4,
                )],
                calls: vec![
                    call(call_next, &format!("f{next}"), Some(0), 2),
                    call(call_missing, &format!("missing{i}"), Some(0), 3),
                ],
                ..FileFacts::default()
            }
        })
        .collect();
    let files: Vec<SemanticFile<'_>> = sources
        .iter()
        .zip(&facts)
        .map(|((path, src), facts)| SemanticFile {
            path,
            language: Language::Python,
            hash: Hash32::of(src),
            source: src,
            facts,
        })
        .collect();
    let refs: Vec<&SemanticFile<'_>> = files.iter().collect();
    let decls = DeclTable::new(files.iter().map(|f| (f.path, f.source, f.facts)));
    let handler = || -> Handler {
        Box::new(|method, params| {
            Ok(match method {
                "textDocument/prepareCallHierarchy" => prepared(params),
                "callHierarchy/outgoingCalls" => {
                    let uri = params["item"]["uri"].as_str().unwrap();
                    let i: usize = uri
                        .trim_start_matches("file:///ws/m")
                        .trim_end_matches(".py")
                        .parse()
                        .unwrap();
                    let next = (i + 1) % 6;
                    json!([{"to": {"name": format!("f{next}"), "kind": 12,
                                       "uri": format!("file:///ws/m{next}.py"),
                                       "range": range(0, 0, 9), "selectionRange": range(0, 4, 6)},
                                "fromRanges": [range(1, 4, 6)]}])
                }
                _ => Value::Null,
            })
        })
    };
    let caps = json!({"callHierarchyProvider": true});
    let opts = Options {
        provider: Provider::Pyright,
        tool_fingerprint: "fp",
        python: true,
        syntax_answers: true,
        hooks: &crate::languages::DefaultServer,
        prepared: crate::test_support::session::empty_prepared(),
        calls_by_definition: false,
        reuse: None,
    };
    let mut single = FakeSession::new(caps.clone(), handler());
    let whole = engine::analyze(&mut single, &refs, &decls, &FakeUris, &opts).unwrap();

    let paths: Vec<&str> = refs.iter().map(|f| f.path).collect();
    let weights: Vec<u64> = refs.iter().map(|f| weight(f.facts)).collect();
    let shards = assign(&paths, &weights, 3, &HashMap::new());
    let mut merged = Analysis::default();
    thread::scope(|scope| {
        let handles: Vec<_> = shards
            .iter()
            .map(|shard| {
                let shard_files: Vec<&SemanticFile<'_>> = shard.iter().map(|&i| refs[i]).collect();
                let (decls, opts, caps) = (&decls, &opts, caps.clone());
                let handler = handler();
                scope.spawn(move || {
                    let mut session = FakeSession::new(caps, handler);
                    engine::analyze(&mut session, &shard_files, decls, &FakeUris, opts)
                })
            })
            .collect();
        for handle in handles {
            merged.merge(handle.join().unwrap().unwrap());
        }
    });
    assert_eq!(merged.files.len(), 6);
    assert_eq!(merged.files, whole.files);
    let m0 = &merged.files["m0.py"];
    assert_eq!(m0.edges.len(), 1);
    assert_eq!(m0.edges[0].target, "m1.py:f1");
    assert_eq!(m0.unresolved.len(), 1, "blind call recorded");
}
