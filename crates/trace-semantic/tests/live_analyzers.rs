//! Real-analyzer tests on the `tests/fixtures/{sem-*,lang-*}` fixtures (NEXT.md items 1, 3, 4
//! and 7; cross-file calls for every registry server). Every test runs exactly like the
//! pipeline: the language's registry entry, its preflight (`setup::preflight_all`, build
//! approval given: the fixtures are trusted), then the backend over the fixture. A test is
//! skipped (with a notice on stderr) when the preflight reports the server or the toolchain
//! missing on this machine (`server_missing`, `toolchain_missing`, `server_unavailable`);
//! every other setup error fails the test. The fixtures are only read (workspace copies).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use trace_core::config::Settings;
use trace_core::facts::FileFacts;
use trace_core::model::EdgeKind;
use trace_core::paths::RepoPaths;
use trace_core::repo_settings::RepoSettings;
use trace_core::semantics::FileSemantics;
use trace_core::{Hash32, Language, SetupError};
use trace_env::os::{EnvVars, Platform};
use trace_semantic::setup::{preflight_all, SetupInputs};
use trace_semantic::{Backend, Prepared, ReferenceQuery, SemanticFile, SemanticRequest, ToolEnv};

/// A fixture read into memory with syntax facts.
struct Fixture {
    root: PathBuf,
    base: PathBuf,
    paths: Vec<String>,
    languages: Vec<Language>,
    sources: Vec<Vec<u8>>,
    facts: Vec<FileFacts>,
    configs: Vec<(String, Vec<u8>)>,
}

impl Fixture {
    /// `None` when the fixture directory does not exist (reported as skipped).
    fn load(name: &str) -> Option<Fixture> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name);
        if !root.is_dir() {
            eprintln!("SKIPPED: fixture {name} not present");
            return None;
        }
        let root =
            trace_core::inventory::strip_verbatim(std::fs::canonicalize(&root).expect("fixture exists"));
        // A short base (the system temp directory): some servers nest cache directories
        // below the workspace and fail beyond MAX_PATH under long paths.
        let id = uuid::Uuid::new_v4().simple().to_string();
        let base = std::env::temp_dir().join("trace-lt").join(&id[..12]);
        std::fs::create_dir_all(&base).unwrap();
        let mut files: Vec<PathBuf> = Vec::new();
        collect(&root, &mut files);
        files.sort();
        let mut fixture = Fixture {
            root: root.clone(),
            base,
            paths: Vec::new(),
            languages: Vec::new(),
            sources: Vec::new(),
            facts: Vec::new(),
            configs: Vec::new(),
        };
        for file in files {
            let rel = file
                .strip_prefix(&root)
                .unwrap()
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let bytes = std::fs::read(&file).unwrap();
            let Some(language) = trace_core::languages::from_path(&file) else {
                // Every other file is a configuration candidate; each backend copies the
                // ones its entry names (`workspace.configs`).
                fixture.configs.push((rel, bytes));
                continue;
            };
            let Ok(facts) = trace_syntax::extract(trace_syntax::SourceInput {
                path: &rel,
                language,
                source: &bytes,
            }) else {
                continue;
            };
            fixture.paths.push(rel);
            fixture.languages.push(language);
            fixture.sources.push(bytes);
            fixture.facts.push(facts);
        }
        Some(fixture)
    }

    fn files(&self) -> Vec<SemanticFile<'_>> {
        (0..self.paths.len())
            .map(|i| SemanticFile {
                path: &self.paths[i],
                language: self.languages[i],
                hash: Hash32::of(&self.sources[i]),
                source: &self.sources[i],
                facts: &self.facts[i],
            })
            .collect()
    }

    fn index_of(&self, path: &str) -> usize {
        self.paths.iter().position(|p| p == path).expect("fixture file")
    }

    /// Byte offset of the name of the declaration `name` in `path`.
    fn name_byte(&self, path: &str, name: &str) -> u32 {
        let facts = &self.facts[self.index_of(path)];
        let decl = facts
            .declarations
            .iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("{name} declared in {path}"));
        decl.name_span.start
    }

    /// Syntax call sites (path, callee start) whose member is `member`.
    fn calls_to(&self, member: &str) -> HashSet<(String, u32)> {
        let mut out = HashSet::new();
        for (i, facts) in self.facts.iter().enumerate() {
            for c in &facts.calls {
                if c.member.as_deref() == Some(member) {
                    out.insert((self.paths[i].clone(), c.callee_span.start));
                }
            }
        }
        out
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// A backend set up for a fixture exactly as the pipeline does it.
struct Live {
    repo: RepoPaths,
    tools: ToolEnv,
    backend: Box<dyn Backend>,
    prepared: Prepared,
}

/// Setup errors that mean "not installed on this machine": the test is skipped.
fn missing_here(error: &SetupError) -> bool {
    match error {
        SetupError::Several { items } => items.iter().all(missing_here),
        other => matches!(other.kind(), "server_missing" | "toolchain_missing" | "server_unavailable"),
    }
}

/// Registry entry + preflight for `language` over `fixture` (`None` = skipped, reported).
fn setup(fixture: &Fixture, language: Language) -> Option<Live> {
    let home = fixture.base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = RepoPaths::resolve_in(&fixture.root, &home).unwrap();
    // The repository's own tools folder when it exists (TRACE_SEMANTIC_TOOLS still wins).
    let mut config = Settings::default();
    let lab_tools = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools");
    if config.semantic.tools_dir.is_none() && lab_tools.is_dir() {
        config.semantic.tools_dir = std::fs::canonicalize(&lab_tools)
            .ok()
            .map(trace_core::inventory::strip_verbatim);
    }
    let tools = ToolEnv::discover(&config, &home, &fixture.root).unwrap();
    let Some(entry) = tools.registry.entry_for(language).cloned() else {
        eprintln!("SKIPPED: no registry entry serves {language:?}");
        return None;
    };
    let languages: Vec<Language> = entry
        .languages
        .iter()
        .copied()
        .filter(|l| fixture.languages.contains(l))
        .collect();
    let files: Vec<(&str, Language)> = fixture
        .paths
        .iter()
        .zip(&fixture.languages)
        .filter(|(_, l)| languages.contains(l))
        .map(|(p, l)| (p.as_str(), *l))
        .collect();
    let facts_of = |path: &str| {
        fixture
            .paths
            .iter()
            .position(|p| p == path)
            .map(|i| &fixture.facts[i])
    };
    let settings = RepoSettings {
        allow_build: true,
        ..RepoSettings::default()
    };
    let platform = Platform::current();
    let vars = EnvVars::from_process();
    let inputs = SetupInputs {
        repo: &repo,
        settings: &settings,
        tools: &tools,
        files: &files,
        facts: &facts_of,
        platform: &platform,
        vars: &vars,
    };
    let prepared = match preflight_all(&[(&entry, languages)], &inputs) {
        Ok(mut all) => all.remove(0),
        Err(e) if missing_here(&e) => {
            eprintln!("SKIPPED: {} ({})", entry.id, e.item_line());
            return None;
        }
        Err(e) => panic!("{}: setup failed: {e}", entry.id),
    };
    let backend = trace_semantic::backend::registry(&tools)
        .into_iter()
        .find(|b| b.id() == entry.id)
        .expect("registry backend");
    Some(Live {
        repo,
        tools,
        backend,
        prepared,
    })
}

impl Live {
    /// A request over the whole fixture (`query` = every file of the partition).
    fn run(&self, fixture: &Fixture) -> HashMap<String, FileSemantics> {
        let files = fixture.files();
        let configs: Vec<(&str, &[u8])> = fixture
            .configs
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_slice()))
            .collect();
        let query: HashSet<String> = fixture.paths.iter().cloned().collect();
        let request = SemanticRequest {
            repo: &self.repo,
            files: &files,
            configs: &configs,
            query: &query,
            tools: &self.tools,
            prepared: &self.prepared,
        };
        self.backend
            .run(&request)
            .unwrap_or_else(|e| panic!("{}: {e}", self.backend.id()))
            .files
    }

    fn references(&self, fixture: &Fixture, query: &ReferenceQuery) -> trace_semantic::LiveReferences {
        let files = fixture.files();
        let configs: Vec<(&str, &[u8])> = fixture
            .configs
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_slice()))
            .collect();
        let none: HashSet<String> = HashSet::new();
        let request = SemanticRequest {
            repo: &self.repo,
            files: &files,
            configs: &configs,
            query: &none,
            tools: &self.tools,
            prepared: &self.prepared,
        };
        self.backend
            .references(&request, query)
            .unwrap()
            .expect("references supported")
    }
}

/// Edge sites (path, evidence start) into `target_suffix` of the given kinds.
fn edge_sites(
    files: &HashMap<String, FileSemantics>,
    target_suffix: &str,
    kinds: &[EdgeKind],
) -> Vec<(String, u32)> {
    let mut out: Vec<(String, u32)> = files
        .iter()
        .flat_map(|(path, sem)| {
            sem.edges
                .iter()
                .filter(|e| e.target.ends_with(target_suffix) && kinds.contains(&e.kind))
                .map(move |e| (path.clone(), e.at.start))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// A call site is found when an edge's evidence lies inside its callee.
fn found(sites: &[(String, u32)], call: &(String, u32), fixture: &Fixture) -> bool {
    let facts = &fixture.facts[fixture.index_of(&call.0)];
    let callee = facts
        .calls
        .iter()
        .find(|c| c.callee_span.start == call.1)
        .expect("call")
        .callee_span;
    sites
        .iter()
        .any(|(p, at)| *p == call.0 && callee.start <= *at && *at < callee.end)
}

/// Run `language`'s registry server over fixture `name`; assert a `calls` edge from the
/// caller file's `caller` declaration to a declaration whose uid ends with `target`.
fn cross_file_call(language: Language, name: &str, caller: &str, target: &str) {
    let Some(fixture) = Fixture::load(name) else { return };
    let Some(live) = setup(&fixture, language) else { return };
    let files = live.run(&fixture);
    let owner_of = |path: &str, owner: u32| -> String {
        let qualified = fixture
            .paths
            .iter()
            .position(|p| p == path)
            .and_then(|i| fixture.facts[i].declarations.get(owner as usize))
            .map(|d| d.qualified_name.clone())
            .unwrap_or_default();
        format!("{path}:{qualified}")
    };
    let edges: Vec<(String, EdgeKind, String)> = files
        .iter()
        .flat_map(|(path, sem)| {
            sem.edges
                .iter()
                .map(|e| (owner_of(path, e.owner), e.kind, e.target.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    let found = edges
        .iter()
        .any(|(from, kind, to)| *kind == EdgeKind::Calls && from.ends_with(caller) && to.ends_with(target));
    if !found {
        for (path, sem) in &files {
            eprintln!("{path}: unresolved {:?}", sem.unresolved);
            eprintln!("{path}: diagnostics {:?}", sem.diagnostics);
        }
    }
    assert!(found, "{}: no calls edge {caller} -> {target}; edges: {edges:?}", live.backend.id());
}

#[test]
fn gopls_resolves_a_cross_file_call() {
    cross_file_call(Language::Go, "lang-go", "main.go:main", "util.go:Greet");
}

#[test]
fn clangd_resolves_a_cross_file_call_in_c() {
    // The definition (util.c) or its prototype (util.h), both compiler facts.
    cross_file_call(Language::C, "lang-c", "main.c:main", ":add_numbers");
}

#[test]
fn clangd_resolves_a_cross_file_call_in_cpp() {
    cross_file_call(Language::Cpp, "lang-cpp", "main.cpp:main", ":Greeter.greet");
}

#[test]
fn jdtls_resolves_a_cross_file_call() {
    cross_file_call(Language::Java, "lang-java", "Main.java:Main.main", "Util.java:Util.greet");
}

#[test]
fn intelephense_resolves_a_cross_file_call() {
    cross_file_call(Language::Php, "lang-php", "main.php:run", "Util.php:Util.greet");
}

#[test]
fn bash_language_server_resolves_a_cross_file_call() {
    cross_file_call(Language::Bash, "lang-bash", "main.sh:run", "lib.sh:greet");
}

#[test]
fn rust_analyzer_resolves_assoc_fns_builder_chains_and_feature_gated_modules() {
    let Some(fixture) = Fixture::load("sem-rust-ripgrep") else { return };
    let Some(live) = setup(&fixture, Language::Rust) else { return };
    let files = live.run(&fixture);
    let calls = [EdgeKind::Calls];
    for (member, suffix, expected) in [
        ("from_bytes", "Data.from_bytes", 4usize),
        ("current_dir", "WalkBuilder.current_dir", 1),
        ("clear", "Replacer.clear", 3),
    ] {
        let sites = edge_sites(&files, suffix, &calls);
        let wanted: Vec<(String, u32)> = fixture
            .calls_to(member)
            .into_iter()
            .filter(|c| found(&sites, c, &fixture))
            .collect();
        assert_eq!(wanted.len(), expected, "{suffix}: {sites:?}");
        assert_eq!(sites.len(), expected, "{suffix}: no decoys ({sites:?})");
    }
    let path = "crates/printer/src/jsont.rs";
    let query = ReferenceQuery {
        path: path.into(),
        byte: fixture.name_byte(path, "from_bytes"),
        include_declaration: false,
    };
    let live_refs = live.references(&fixture, &query);
    assert_eq!(live_refs.references.len(), 4, "{live_refs:?}");
    assert!(live_refs.complete);
    assert_eq!(live_refs.backend, live.backend.id());
}

/// Rule 6 (server side): rust-analyzer's `textDocument/implementation` on trait methods
/// records the `impl Trait for Type` methods of other files / crates on the trait's file
/// (`FileSemantics::implementations`), each named like its base member.
#[test]
fn rust_analyzer_records_trait_implementations_across_crates() {
    let Some(fixture) = Fixture::load("rule-family-trait-crates") else { return };
    let Some(live) = setup(&fixture, Language::Rust) else { return };
    let files = live.run(&fixture);
    let mut total = 0usize;
    let mut cross_file = 0usize;
    for (path, sem) in &files {
        let facts = &fixture.facts[fixture.index_of(path)];
        for imp in &sem.implementations {
            total += 1;
            let base = &facts.declarations[imp.base as usize];
            let parent = base.parent.map(|p| facts.declarations[p as usize].kind);
            assert_eq!(parent, Some(trace_core::model::SymbolKind::Interface), "{path}: {imp:?}");
            assert!(
                imp.implementor.ends_with(&format!(".{}", base.name))
                    || imp.implementor.ends_with(&format!(":{}", base.name)),
                "{path}: implementor named like the base: {imp:?}"
            );
            assert_eq!(imp.kind, EdgeKind::Implements, "{imp:?}");
            if !imp.implementor.starts_with(&format!("{path}:")) {
                cross_file += 1;
            }
        }
    }
    assert!(total > 0, "no implementations recorded");
    assert!(cross_file > 0, "implementations in other files / crates are found");
}

#[test]
fn typescript_callbacks_module_code_and_class_field_arrows_are_owned() {
    let Some(fixture) = Fixture::load("sem-ts-hono") else { return };
    let Some(live) = setup(&fixture, Language::TypeScript) else { return };
    let files = live.run(&fixture);
    for sem in files.values() {
        assert!(!sem.diagnostics.iter().any(|d| d.kind == "unmapped_owner"), "{:?}", sem.diagnostics);
    }
    let calls = [EdgeKind::Calls];
    for (member, suffix, expected) in
        [("notFound", "Context.notFound", 4usize), ("redirect", "Context.redirect", 5)]
    {
        let sites = edge_sites(&files, suffix, &calls);
        let syntax = fixture.calls_to(member);
        assert_eq!(syntax.len(), expected, "fixture call sites of {member}");
        for call in &syntax {
            assert!(found(&sites, call, &fixture), "{member} at {call:?}: {sites:?}");
        }
    }
    // Non-call uses: the read of `notFound` and the write of `redirect` (module level).
    assert_eq!(
        edge_sites(&files, "Context.notFound", &[EdgeKind::References, EdgeKind::PassesCallback]).len(),
        1
    );
    assert_eq!(edge_sites(&files, "Context.redirect", &[EdgeKind::Writes]).len(), 1);
    // Live references (worker references mode): 4 calls + 1 read.
    let path = "src/context.ts";
    let query = ReferenceQuery {
        path: path.into(),
        byte: fixture.name_byte(path, "notFound"),
        include_declaration: false,
    };
    let live_refs = live.references(&fixture, &query);
    assert_eq!(live_refs.references.len(), 5, "{live_refs:?}");
    assert!(live_refs.complete);
}

/// Rules "assigned function" and "returned function" (assets/ts-worker/assigned.mjs): a call
/// through a typed holder reaches the one repository function its initializer and every
/// repository write assign (fixture README: constructor writes by literal-typed keys,
/// identifier aliases, `let` without initializer, a call returning one function); shadowed,
/// doubly written, unknown, `any`-written and dynamically keyed holders give nothing.
#[test]
fn rule_assigned_function_resolves_calls_through_held_functions() {
    let Some(fixture) = Fixture::load("rule-ts-assigned-function") else { return };
    let Some(live) = setup(&fixture, Language::TypeScript) else { return };
    let files = live.run(&fixture);
    let main = files.get("src/main.ts").expect("main.ts analysed");
    let calls_at = |line: u32| -> Vec<&str> {
        main.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls && e.line == line)
            .map(|e| e.target.as_str())
            .collect()
    };
    let first = |line: u32| {
        let t = calls_at(line);
        assert_eq!(t.len(), 1, "line {line}: {t:?}; edges {:?}", main.edges);
        t[0].to_string()
    };
    // `app.get` / `app.post`: the one arrow `this[method] = ...` (constructor of App).
    let verb = first(12);
    assert!(verb.starts_with("src/router.ts:"), "{verb}");
    assert_eq!(first(13), verb);
    assert!(first(14).starts_with("src/router.ts:"), "use arrow");
    assert_ne!(first(14), verb);
    assert!(first(16).ends_with("src/router.ts:first"), "patched.run -> first");
    assert!(first(19).starts_with("src/main.ts:"), "later -> its arrow");
    assert!(first(20).ends_with("src/router.ts:first"), "alias -> first");
    for (line, what) in [
        (22, "base.hook: shadowed by Derived.hook"),
        (24, "twice.fn: first and second"),
        (27, "replaced: call result"),
        (29, "external.exec: written through any"),
    ] {
        assert!(calls_at(line).is_empty(), "{what}: {:?}", calls_at(line));
        assert!(main.unresolved.iter().any(|u| u.line == line), "{what}: reported unresolved");
    }
    // Rule "returned function": `made = makeHandler('m')`, `makeHandler('n')('y')`.
    let made = first(31);
    assert!(made.starts_with("src/router.ts:"), "{made}");
    assert!(calls_at(32).contains(&made.as_str()), "{:?}", calls_at(32));
    // `either(true)('z')`: two functions returned -> only the `either` call itself.
    assert!(calls_at(33).iter().all(|t| t.ends_with(":either")), "{:?}", calls_at(33));
    assert!(main.unresolved.iter().any(|u| u.line == 33), "either(true)('z') unresolved");
    // `table.run`: an object literal written with a non-literal key holds nothing.
    assert!(calls_at(36).is_empty(), "table.run: {:?}", calls_at(36));
    assert!(main.unresolved.iter().any(|u| u.line == 36), "table.run reported unresolved");
}

/// Rule "tagged template" (assets/ts-worker/analyze.mjs): f`x` is a call of its tag `f`,
/// resolved like any call (checker signature, then the checker rules).
#[test]
fn rule_tagged_template_is_a_call_of_its_tag() {
    let Some(fixture) = Fixture::load("rule-ts-assigned-function") else { return };
    let Some(live) = setup(&fixture, Language::TypeScript) else { return };
    let files = live.run(&fixture);
    let main = files.get("src/main.ts").expect("main.ts analysed");
    let calls_at = |line: u32| -> Vec<&str> {
        main.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls && e.line == line)
            .map(|e| e.target.as_str())
            .collect()
    };
    // tag`a${1}`: the function `tag` (checker signature).
    assert_eq!(calls_at(34).len(), 1, "{:?}", calls_at(34));
    assert!(calls_at(34)[0].ends_with("src/router.ts:tag"), "{:?}", calls_at(34));
    // tagged`b`: the arrow `tagged` is initialised with (assigned function).
    assert_eq!(calls_at(35).len(), 1, "{:?}", calls_at(35));
    assert!(calls_at(35)[0].starts_with("src/router.ts:"), "{:?}", calls_at(35));
    assert!(!calls_at(35)[0].ends_with(":tag"), "{:?}", calls_at(35));
}

#[test]
fn pyright_records_module_calls_writes_and_value_uses() {
    let Some(fixture) = Fixture::load("sem-py-flask") else { return };
    let Some(live) = setup(&fixture, Language::Python) else { return };
    let files = live.run(&fixture);
    let test = &files["tests/test_helpers.py"];
    let facts = &fixture.facts[fixture.index_of("tests/test_helpers.py")];
    let module = facts.module_decl.expect("extractor 5 appends <module>");
    let to_redirect: Vec<(u32, EdgeKind)> = test
        .edges
        .iter()
        .filter(|e| e.target.ends_with("App.redirect"))
        .map(|e| (e.owner, e.kind))
        .collect();
    assert!(to_redirect.contains(&(module, EdgeKind::Calls)), "{to_redirect:?}");
    assert!(to_redirect.contains(&(module, EdgeKind::References)), "{to_redirect:?}");
    assert!(
        to_redirect
            .iter()
            .any(|(owner, kind)| *owner != module && *kind == EdgeKind::Writes),
        "app.redirect = redirect is a writes edge: {to_redirect:?}"
    );
    let app = &files["src/flask/sansio/app.py"];
    assert!(app
        .edges
        .iter()
        .any(|e| e.target.ends_with("JSONProvider.dumps") && e.kind == EdgeKind::References));
    let path = "src/flask/sansio/app.py";
    let query = ReferenceQuery {
        path: path.into(),
        byte: fixture.name_byte(path, "redirect"),
        include_declaration: true,
    };
    let live_refs = live.references(&fixture, &query);
    let lines: Vec<(&str, bool)> = live_refs
        .references
        .iter()
        .map(|r| (r.path.as_str(), r.is_declaration))
        .collect();
    assert!(lines.contains(&(path, true)), "{lines:?}");
    assert!(
        live_refs
            .references
            .iter()
            .filter(|r| r.path == "tests/test_helpers.py")
            .count()
            >= 3,
        "{lines:?}"
    );
}
