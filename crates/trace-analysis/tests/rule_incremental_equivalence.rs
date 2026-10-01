//! Accuracy guard (PLAN decision 13, DESIGN §1.14.6): after any sequence of edits the
//! incremental graph equals a full rebuild of the same files.
//!
//! A deterministic fake backend stands in for the language servers: a call resolves to every
//! callable declaration of the repository with the called name (one -> a proven edge, several
//! -> an ambiguous unknown with candidates, none -> an unknown). Its answer for a file depends
//! only on the file's own facts and on the declarations of the others, exactly what the
//! requery rules (`trace_core::incremental`: interface rule, dangling targets, changed
//! declaration names, stale dependents) track, so any difference found here is a bug of the
//! incremental phases (link, families, library, bridges, inference, decisions) or of the
//! requery rules. The repository mixes Python, TypeScript and Go
//! (`tests/fixtures/rule-speed-*`); files are held in memory (sources are read from the
//! fixtures, never written there); caches live in `<temp>/trace-tests`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use trace_analysis::equivalence::compare;
use trace_analysis::pipeline::update::{apply, persist, PostSemantic};
use trace_analysis::pipeline::{Profile, Quiet};
use trace_analysis::report::PhaseSeconds;
use trace_core::config::Settings;
use trace_core::delta::IndexDelta;
use trace_core::facts::FileFacts;
use trace_core::incremental::{
    self, changed_declaration_names, index_delta, interface_changed, semantic_requery, DeltaInput,
    RequeryInput, StalePolicy,
};
use trace_core::inventory::{HashedEntry, InventoryEntry};
use trace_core::model::{EdgeKind, FileRecord, Index, IndexHeader, Provider, Resolution, UnresolvedKind};
use trace_core::paths::RepoPaths;
use trace_core::semantics::{FileSemantics, SemEdge, SemUnresolved};
use trace_core::{Hash32, Language, SupportLevel};
use trace_library::installed::InstalledPackages;
use trace_library::Library;

const TOOL: &str = "fake-1";

/// Repository contents: path -> bytes; configuration files separately.
#[derive(Clone, Debug, Default)]
struct Repo {
    files: BTreeMap<String, Vec<u8>>,
    configs: BTreeMap<String, Vec<u8>>,
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

impl Repo {
    fn load() -> Repo {
        let mut repo = Repo::default();
        for dir in ["rule-speed-python", "rule-speed-ts", "rule-speed-go"] {
            let base = fixtures().join(dir);
            let mut names: Vec<_> = std::fs::read_dir(&base)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            for name in names {
                let bytes = std::fs::read(base.join(&name)).unwrap();
                repo.files.insert(format!("{dir}/{name}"), bytes);
            }
        }
        repo.configs
            .insert("pyproject.toml".into(), b"[project]\nname = \"x\"\n".to_vec());
        repo
    }

    fn set(&mut self, path: &str, text: &str) {
        self.files.insert(path.to_string(), text.as_bytes().to_vec());
    }

    fn text(&self, path: &str) -> String {
        String::from_utf8(self.files[path].clone()).unwrap()
    }

    /// Replace the first occurrence of `from` in `path`.
    fn edit(&mut self, path: &str, from: &str, to: &str) {
        let text = self.text(path);
        assert!(text.contains(from), "{path} has no {from:?}");
        self.set(path, &text.replacen(from, to, 1));
    }

    fn entries(map: &BTreeMap<String, Vec<u8>>) -> Vec<HashedEntry> {
        map.iter()
            .map(|(path, bytes)| HashedEntry {
                entry: InventoryEntry {
                    path: path.clone(),
                    abs: PathBuf::from(path),
                    language: trace_core::languages::from_path(Path::new(path)),
                    is_config: false,
                    size: bytes.len() as u64,
                    mtime_ns: 0,
                },
                hash: Hash32::of(bytes),
            })
            .collect()
    }

    fn sources(&self) -> Vec<HashedEntry> {
        Repo::entries(&self.files)
    }

    fn config_entries(&self) -> Vec<HashedEntry> {
        Repo::entries(&self.configs)
    }

    fn config_hashes(&self) -> Vec<(String, Hash32)> {
        self.configs.iter().map(|(p, b)| (p.clone(), Hash32::of(b))).collect()
    }

    fn language(path: &str) -> Language {
        trace_core::languages::from_path(Path::new(path)).unwrap()
    }
}

fn extract(path: &str, bytes: &[u8]) -> FileFacts {
    trace_syntax::extract(trace_syntax::SourceInput {
        path,
        language: Repo::language(path),
        source: bytes,
    })
    .unwrap()
}

/// Called name -> uids of every callable declaration with that name (sorted).
type Names = BTreeMap<String, Vec<String>>;

fn names_of<'a>(facts: impl IntoIterator<Item = (&'a str, &'a FileFacts)>) -> Names {
    let mut out = Names::new();
    for (path, f) in facts {
        let uids = incremental::uids_of(path, f);
        for (d, uid) in f.declarations.iter().zip(uids) {
            if d.kind.is_callable() && !d.name.starts_with('<') {
                out.entry(d.name.clone()).or_default().push(uid);
            }
        }
    }
    for v in out.values_mut() {
        v.sort();
    }
    out
}

/// The fake backend (module docs).
fn fake(facts: &FileFacts, names: &Names) -> FileSemantics {
    let mut edges = Vec::new();
    let mut unresolved = Vec::new();
    for c in &facts.calls {
        let Some(owner) = facts.executing_owner(c.owner) else {
            continue;
        };
        let name = c
            .member
            .clone()
            .unwrap_or_else(|| c.callee.rsplit('.').next().unwrap_or(&c.callee).to_string());
        match names.get(&name).map(Vec::as_slice) {
            Some([one]) => edges.push(SemEdge {
                owner,
                target: one.clone(),
                kind: EdgeKind::Calls,
                at: c.callee_span,
                line: c.line,
                resolution: Resolution::Definition,
            }),
            Some(many) => unresolved.push(SemUnresolved {
                owner: Some(owner),
                kind: UnresolvedKind::ExternalOrAmbiguous,
                at: c.callee_span,
                line: c.line,
                callee: c.callee.clone(),
                candidates: many.to_vec(),
            }),
            None => unresolved.push(SemUnresolved {
                owner: Some(owner),
                kind: UnresolvedKind::NoSemanticTarget,
                at: c.callee_span,
                line: c.line,
                callee: c.callee.clone(),
                candidates: Vec::new(),
            }),
        }
    }
    FileSemantics {
        provider: Provider::Lsp("fake".into()),
        tool_fingerprint: TOOL.into(),
        edges,
        unresolved,
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

fn record(path: &str, bytes: &[u8], facts: FileFacts, semantic: Option<FileSemantics>) -> FileRecord {
    FileRecord {
        path: path.to_string(),
        language: Repo::language(path),
        hash: Hash32::of(bytes),
        size: bytes.len() as u64,
        mtime_ns: 0,
        support: SupportLevel::Semantic,
        facts: Some(facts),
        semantic,
        first_symbol: 0,
        symbol_count: 0,
        diagnostics: Vec::new(),
        pending: None,
    }
}

/// A temporary cache home + root; removed on drop.
struct Env {
    _dir: tempfile::TempDir,
    paths: RepoPaths,
    library: Library,
}

impl Env {
    fn new(name: &str) -> Env {
        let base = std::env::temp_dir().join("trace-tests");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::Builder::new().prefix(name).tempdir_in(&base).unwrap();
        let root = dir.path().join("repo");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let paths = RepoPaths::resolve_in(&root, &home).unwrap();
        paths.ensure_repo_dir().unwrap();
        let library = Library::open(&home).unwrap();
        Env {
            _dir: dir,
            paths,
            library,
        }
    }

    fn header(&self, repo: &Repo) -> IndexHeader {
        let sources = repo.sources();
        let configs = repo.config_entries();
        IndexHeader {
            schema: trace_core::SCHEMA_VERSION,
            trace_version: trace_core::TRACE_VERSION.into(),
            root: self.paths.root_display(),
            built_unix: 0.0,
            syntax_version: trace_syntax::EXTRACTOR_VERSION,
            infer_version: trace_infer::INFER_VERSION,
            bridge_version: trace_bridge::BRIDGE_VERSION,
            inventory_fingerprint: trace_core::inventory::inventory_fingerprint(
                sources.iter().map(|e| (e.entry.path.as_str(), &e.hash)),
                configs.iter().map(|e| (e.entry.path.as_str(), &e.hash)),
            ),
            full_builds: 1,
            incremental_updates: 0,
        }
    }

    fn run(
        &self,
        repo: &Repo,
        prev: Option<Index>,
        files: Vec<FileRecord>,
        removed: Vec<String>,
        delta: IndexDelta,
    ) -> Index {
        let config = Settings::default();
        let installed = InstalledPackages::default();
        let mut secs = PhaseSeconds::default();
        let (index, _) = apply(
            PostSemantic {
                config: &config,
                prev,
                header: self.header(repo),
                files,
                removed,
                configs: repo.config_hashes(),
                omitted: Vec::new(),
                delta,
                support: Vec::new(),
                backend_runs: Vec::new(),
                diagnostics: Vec::new(),
                library: &self.library,
                installed: &installed,
                secs: &mut secs,
            },
            &mut Quiet,
            &mut Profile::from_env(),
        )
        .unwrap();
        index
    }

    /// A clean full index of `repo`.
    fn full(&self, repo: &Repo) -> Index {
        let facts: BTreeMap<&str, FileFacts> =
            repo.files.iter().map(|(p, b)| (p.as_str(), extract(p, b))).collect();
        let names = names_of(facts.iter().map(|(p, f)| (*p, f)));
        let records = repo
            .files
            .iter()
            .map(|(p, b)| {
                let f = facts[p.as_str()].clone();
                let sem = fake(&f, &names);
                record(p, b, f, Some(sem))
            })
            .collect();
        self.run(repo, None, records, Vec::new(), IndexDelta::full())
    }

    /// One incremental update from `prev` to the contents of `repo` (the pipeline's steps:
    /// plan, re-parse, interface changes, requery with `policy`, delta, post-semantic).
    fn update(&self, prev: Index, repo: &Repo, policy: &StalePolicy) -> Index {
        let sources = repo.sources();
        let configs = repo.config_entries();
        let plan = incremental::plan(Some(&prev), &sources, &configs);
        let dirty: HashSet<&str> = plan.dirty().collect();
        let facts: BTreeMap<&str, FileFacts> = repo
            .files
            .iter()
            .map(|(p, b)| {
                let f = if dirty.contains(p.as_str()) {
                    extract(p, b)
                } else {
                    let id = prev.file_by_path(p).unwrap();
                    prev.file(id).facts.clone().unwrap()
                };
                (p.as_str(), f)
            })
            .collect();
        let dirty_facts = || {
            facts
                .iter()
                .filter(|(p, _)| dirty.contains(**p))
                .map(|(p, f)| (*p, Some(f)))
        };
        let interfaces = interface_changed(Some(&prev), &plan, dirty_facts());
        let declared = changed_declaration_names(Some(&prev), &plan, dirty_facts());
        let current: Vec<(String, Language)> =
            repo.files.keys().map(|p| (p.clone(), Repo::language(p))).collect();
        let languages: BTreeSet<Language> = current.iter().map(|(_, l)| *l).collect();
        let languages: Vec<Language> = languages.into_iter().collect();
        let requery = semantic_requery(RequeryInput {
            prev: Some(&prev),
            plan: &plan,
            partition: &languages,
            current_files: &current,
            tool_fingerprint: TOOL,
            declared_names: &declared,
            interface_changed: &interfaces,
            policy,
        });
        let names = names_of(facts.iter().map(|(p, f)| (*p, f)));
        let records: Vec<FileRecord> = repo
            .files
            .iter()
            .map(|(p, b)| {
                let f = facts[p.as_str()].clone();
                let sem = if requery.now.contains(p) {
                    fake(&f, &names)
                } else {
                    let id = prev.file_by_path(p).unwrap();
                    prev.file(id).semantic.clone().unwrap()
                };
                record(p, b, f, Some(sem))
            })
            .collect();
        let delta = index_delta(DeltaInput {
            prev: Some(&prev),
            plan: &plan,
            full: false,
            records: &records,
            requeried: &requery.now,
            interface_changed: &interfaces,
            stale: &requery.stale,
        });
        let removed = plan.removed.clone();
        self.run(repo, Some(prev), records, removed, delta)
    }
}

/// No difference between the incremental index and a clean full index of `repo`.
fn assert_equivalent(env: &Env, incremental: &Index, repo: &Repo) {
    let full = env.full(repo);
    let differences = compare(incremental, &full);
    assert!(differences.is_empty(), "incremental != full: {differences:#?}");
    incremental.validate().unwrap();
}

const PY: &str = "rule-speed-python";

#[test]
fn rule_incremental_equals_full_after_body_edit() {
    let env = Env::new("eq-body");
    let mut repo = Repo::load();
    let index = env.full(&repo);
    // A body edit that keeps the interface (a local variable).
    repo.edit(&format!("{PY}/util.py"), "    return 2", "    unused = 1\n    return 2");
    let index = env.update(index, &repo, &StalePolicy::ResolveAll);
    assert!(index.stale.is_empty());
    assert_equivalent(&env, &index, &repo);
    // A body edit in another language.
    repo.edit("rule-speed-go/serve.go", "\thandle()", "\thandle()\n\thandle()");
    let index = env.update(index, &repo, &StalePolicy::ResolveAll);
    assert_equivalent(&env, &index, &repo);
}

#[test]
fn rule_incremental_equals_full_after_interface_change() {
    let env = Env::new("eq-interface");
    let mut repo = Repo::load();
    let index = env.full(&repo);
    // `helper` renamed: its callers in app.py dangle now.
    repo.edit(&format!("{PY}/util.py"), "def helper()", "def helper2()");
    // `index --watch` leaves the dependent stale ...
    let deferred = env.update(
        index,
        &repo,
        &StalePolicy::Defer {
            resolve: BTreeSet::new(),
        },
    );
    assert!(
        deferred.stale.contains(&format!("{PY}/app.py")),
        "the caller is a stale dependent: {:?}",
        deferred.stale
    );
    // ... and resolves it before any query reads it (a no-change update resolving it).
    let resolved = env.update(deferred, &repo, &StalePolicy::ResolveAll);
    assert!(resolved.stale.is_empty());
    assert_equivalent(&env, &resolved, &repo);
    // In-process: resolved in the same update.
    repo.edit("rule-speed-ts/greet.ts", "function format(", "function formatName(");
    let index = env.update(resolved, &repo, &StalePolicy::ResolveAll);
    assert_equivalent(&env, &index, &repo);
}

#[test]
fn rule_incremental_equals_full_after_file_add() {
    let env = Env::new("eq-add");
    let mut repo = Repo::load();
    let index = env.full(&repo);
    // A second `save`: `s.save()` in app.py becomes ambiguous.
    repo.set(&format!("{PY}/extra.py"), "class Other:\n    def save(self):\n        return 0\n");
    let index = env.update(index, &repo, &StalePolicy::ResolveAll);
    assert_equivalent(&env, &index, &repo);
}

#[test]
fn rule_incremental_equals_full_after_file_remove() {
    let env = Env::new("eq-remove");
    let mut repo = Repo::load();
    let index = env.full(&repo);
    repo.files.remove(&format!("{PY}/store.py"));
    let index = env.update(index, &repo, &StalePolicy::ResolveAll);
    assert_equivalent(&env, &index, &repo);
}

#[test]
fn rule_incremental_equals_full_after_rename() {
    let env = Env::new("eq-rename");
    let mut repo = Repo::load();
    let index = env.full(&repo);
    let moved = repo.files.remove(&format!("{PY}/util.py")).unwrap();
    repo.files.insert(format!("{PY}/tools_util.py"), moved);
    let index = env.update(index, &repo, &StalePolicy::ResolveAll);
    assert_equivalent(&env, &index, &repo);
}

#[test]
fn rule_incremental_equals_full_after_build_file_edit() {
    let env = Env::new("eq-build");
    let mut repo = Repo::load();
    let index = env.full(&repo);
    repo.configs
        .insert("pyproject.toml".into(), b"[project]\nname = \"y\"\n".to_vec());
    let index = env.update(index, &repo, &StalePolicy::ResolveAll);
    assert_equivalent(&env, &index, &repo);
}

#[test]
fn rule_incremental_equals_full_after_watch_restart() {
    let env = Env::new("eq-restart");
    let mut repo = Repo::load();
    let index = env.full(&repo);
    trace_core::cache::save_index(&env.paths.index_file, &index).unwrap();
    // An update persisted as a journal segment, then `index --watch` exits.
    repo.edit(&format!("{PY}/store.py"), "        return 1", "        kept = 1\n        return 1");
    let index = env.update(index, &repo, &StalePolicy::ResolveAll);
    let delta = IndexDelta {
        modified: BTreeSet::from([format!("{PY}/store.py")]),
        requeried: BTreeSet::from([format!("{PY}/store.py")]),
        ..IndexDelta::default()
    };
    persist(&env.paths, &index, &delta).unwrap();
    drop(index);
    // Cold catch-up: the persisted index (base + journal) plus a change made meanwhile.
    let loaded = trace_core::cache::load_index(&env.paths.index_file, &env.paths.root_display()).unwrap();
    repo.edit(&format!("{PY}/util.py"), "def helper()", "def helper(x=None)");
    let index = env.update(loaded, &repo, &StalePolicy::ResolveAll);
    assert_equivalent(&env, &index, &repo);
}

/// One edit of the random sequence (all keep the fixtures parseable).
fn random_edit(repo: &mut Repo, choice: u64, step: usize) {
    let py = |name: &str| format!("{PY}/{name}");
    match choice % 8 {
        0 => {
            // Body edit.
            let p = py("util.py");
            let text = repo.text(&p);
            repo.set(&p, &text.replacen("    return", &format!("    step{step} = {step}\n    return"), 1));
        }
        1 => {
            // New declaration (interface change) in a new or existing module.
            repo.set(
                &py(&format!("gen{}.py", step % 3)),
                &format!("def helper():\n    return {step}\n\ndef g{step}():\n    return helper()\n"),
            );
        }
        2 => {
            // Remove a generated module (if any).
            let p = py(&format!("gen{}.py", step % 3));
            repo.files.remove(&p);
        }
        3 => {
            // Rename a TypeScript function back and forth.
            let p = "rule-speed-ts/greet.ts";
            let text = repo.text(p);
            let next = if text.contains("function format(") {
                text.replace("function format(", "function formatName(")
            } else {
                text.replace("function formatName(", "function format(")
            };
            repo.set(p, &next);
        }
        4 => {
            // Go: a call to a function that may or may not exist.
            let p = "rule-speed-go/main.go";
            let text = repo.text(p);
            repo.set(p, &text.replacen("\tServe()", "\tServe()\n\thandle()", 1));
        }
        5 => {
            // Toggle `make_store`'s return (an inferred return: interface change).
            let p = py("store.py");
            let text = repo.text(p.as_str());
            let next = if text.contains("return Store()") {
                text.replace("return Store()", "return None")
            } else {
                text.replace("return None", "return Store()")
            };
            repo.set(&p, &next);
        }
        6 => {
            // A new caller of an ambiguous name.
            repo.set(&py("caller.py"), &format!("def call{step}():\n    helper()\n    save()\n"));
        }
        _ => {
            // Configuration change.
            repo.configs
                .insert("pyproject.toml".into(), format!("[project]\nname = \"n{step}\"\n").into_bytes());
        }
    }
}

#[test]
fn rule_incremental_equals_full_after_edit_sequence() {
    for seed in [1u64, 7, 42] {
        let env = Env::new("eq-sequence");
        let mut repo = Repo::load();
        let mut index = env.full(&repo);
        // A small linear congruential generator: reproducible sequences, no dependency.
        let mut state = seed;
        for step in 0..20 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            random_edit(&mut repo, state >> 33, step);
            // Every third update leaves dependents stale (`index --watch`), the next resolves them.
            let policy = if step % 3 == 0 {
                StalePolicy::Defer {
                    resolve: BTreeSet::new(),
                }
            } else {
                StalePolicy::ResolveAll
            };
            index = env.update(index, &repo, &policy);
            if index.stale.is_empty() {
                assert_equivalent(&env, &index, &repo);
            }
        }
        let index = env.update(index, &repo, &StalePolicy::ResolveAll);
        assert!(index.stale.is_empty());
        assert_equivalent(&env, &index, &repo);
    }
}
