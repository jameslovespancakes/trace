use super::*;
use trace_core::facts::{Import, ImportKind, Scope};
use trace_core::model::{ByteSpan, EdgeKind, Provider, Resolution, UnresolvedKind};
use trace_core::semantics::SemEdge;

#[test]
fn module_keys_and_resolution() {
    assert_eq!(module_key("pkg/mod.py").as_deref(), Some("pkg/mod"));
    assert_eq!(module_key("pkg/__init__.pyi").as_deref(), Some("pkg"));
    assert_eq!(module_key("src/util/index.ts").as_deref(), Some("src/util"));
    assert_eq!(module_key("types.d.ts").as_deref(), Some("types"));
    assert_eq!(module_key("README.md"), None);
    let index = ModuleIndex::new([
        "src/app/models.py",
        "src/app/__init__.py",
        "src/app/views.py",
        "other/models.py",
        "web/util/index.ts",
        "web/main.ts",
    ]);
    let py = |from: &str, target: &str| index.resolve(from, Language::Python, target);
    assert_eq!(
        py("src/app/views.py", "app.models.User"),
        vec!["src/app/__init__.py".to_string(), "src/app/models.py".to_string()]
    );
    assert_eq!(
        py("src/app/views.py", ".models.User"),
        vec!["src/app/__init__.py".to_string(), "src/app/models.py".to_string()]
    );
    assert!(py("src/app/views.py", "json").is_empty());
    assert_eq!(py("x.py", "models"), vec!["other/models.py".to_string(), "src/app/models.py".to_string()]);
    let js = |target: &str| index.resolve("web/main.ts", Language::TypeScript, target);
    assert_eq!(js("./util.helper"), vec!["web/util/index.ts".to_string()]);
    assert_eq!(js("./util/index.js.default"), vec!["web/util/index.ts".to_string()]);
    assert!(js("react.useState").is_empty());
}

#[derive(Clone)]
struct Fixture {
    paths: Vec<String>,
    sources: Vec<Vec<u8>>,
    facts: Vec<FileFacts>,
    language: Language,
}

impl Fixture {
    fn new(files: &[(&str, &str)]) -> Self {
        Fixture {
            paths: files.iter().map(|(p, _)| p.to_string()).collect(),
            sources: files.iter().map(|(_, s)| s.as_bytes().to_vec()).collect(),
            facts: files.iter().map(|_| FileFacts::default()).collect(),
            language: Language::Python,
        }
    }

    fn declare(&mut self, file: usize, name: &str) {
        let src = &self.sources[file];
        let at = crate::test_support::facts::find(src, name);
        self.facts[file]
            .declarations
            .push(crate::test_support::facts::decl_at(
                src,
                name,
                name,
                trace_core::model::SymbolKind::Function,
                (0, src.len() as u32),
                at,
            ));
    }

    fn import(&mut self, file: usize, target: &str) {
        self.facts[file].imports.push(Import {
            local: target.rsplit('.').next().unwrap_or(target).to_string(),
            target: target.to_string(),
            kind: ImportKind::Member,
            scope: Scope::Module,
            span: ByteSpan::new(0, 1),
            line: 1,
        });
    }

    fn files(&self) -> Vec<SemanticFile<'_>> {
        (0..self.paths.len())
            .map(|i| SemanticFile {
                path: &self.paths[i],
                language: self.language,
                hash: Hash32::of(&self.sources[i]),
                source: &self.sources[i],
                facts: &self.facts[i],
            })
            .collect()
    }
}

fn semantics(edges: &[&str], unresolved: &[&str]) -> FileSemantics {
    FileSemantics {
        provider: Provider::Pyright,
        tool_fingerprint: "fp".into(),
        edges: edges
            .iter()
            .map(|t| SemEdge {
                owner: 0,
                target: t.to_string(),
                kind: EdgeKind::Calls,
                at: ByteSpan::new(0, 1),
                line: 1,
                resolution: Resolution::CallHierarchy,
            })
            .collect(),
        unresolved: unresolved
            .iter()
            .map(|callee| SemUnresolved {
                owner: Some(0),
                kind: UnresolvedKind::NoSemanticTarget,
                at: ByteSpan::new(2, 3),
                line: 1,
                callee: callee.to_string(),
                candidates: Vec::new(),
            })
            .collect(),
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

fn lookup(cache: &SemanticCache, fixture: &Fixture, path: &str) -> bool {
    let files = fixture.files();
    let refs: Vec<&SemanticFile<'_>> = files.iter().collect();
    let mut ctx = CacheContext::new(Hash32::of(b"env"), &refs);
    cache.lookup(&mut ctx, path).is_some()
}

fn store(cache: &mut SemanticCache, fixture: &Fixture, path: &str, sem: &FileSemantics) {
    let files = fixture.files();
    let refs: Vec<&SemanticFile<'_>> = files.iter().collect();
    let mut ctx = CacheContext::new(Hash32::of(b"env"), &refs);
    cache.store(&mut ctx, path, sem);
}

#[test]
fn entries_follow_content_dependencies_imports_and_names() {
    let mut fx = Fixture::new(&[
        ("app/main.py", "from app.svc import run\ndef main():\n    run()\n    helper.go()\n"),
        ("app/svc.py", "def run(): pass\n"),
        ("app/unrelated.py", "def other(): pass\n"),
    ]);
    fx.declare(0, "main");
    fx.declare(1, "run");
    fx.declare(2, "other");
    fx.import(0, "app.svc.run");
    let mut cache = SemanticCache::in_memory("pyright");
    let sem = semantics(&["app/svc.py:run"], &["helper.go"]);
    store(&mut cache, &fx, "app/main.py", &sem);
    assert!(lookup(&cache, &fx, "app/main.py"), "warm hit");
    assert!(!lookup(&cache, &fx, "app/svc.py"), "never stored");

    // An unrelated file changes: still a hit.
    let mut unrelated = fx.clone();
    unrelated.sources[2] = b"def other(): return 1\n".to_vec();
    assert!(lookup(&cache, &unrelated, "app/main.py"));

    // The target file changes: miss.
    let mut dep_changed = fx.clone();
    dep_changed.sources[1] = b"def run(): return 2\n".to_vec();
    assert!(!lookup(&cache, &dep_changed, "app/main.py"));

    // The file itself changes: miss; reverting hits again (content-addressed history).
    let mut edited = fx.clone();
    edited.sources[0].extend_from_slice(b"# edit\n");
    assert!(!lookup(&cache, &edited, "app/main.py"));
    store(&mut cache, &edited, "app/main.py", &sem);
    assert!(lookup(&cache, &edited, "app/main.py"));
    assert!(lookup(&cache, &fx, "app/main.py"), "reverted version still cached");

    // A new file declaring the unresolved member `go`: miss (it may resolve now).
    let mut declared = Fixture::new(&[
        ("app/main.py", "from app.svc import run\ndef main():\n    run()\n    helper.go()\n"),
        ("app/svc.py", "def run(): pass\n"),
        ("app/unrelated.py", "def other(): pass\n"),
        ("app/helper.py", "def go(): pass\n"),
    ]);
    declared.facts[..3].clone_from_slice(&fx.facts);
    declared.declare(3, "go");
    assert!(!lookup(&cache, &declared, "app/main.py"));
}

#[test]
fn a_new_module_matching_an_import_invalidates_importers() {
    let mut fx = Fixture::new(&[("app/main.py", "import app.extra\ndef main():\n    pass\n")]);
    fx.declare(0, "main");
    fx.import(0, "app.extra");
    let mut cache = SemanticCache::in_memory("pyright");
    store(&mut cache, &fx, "app/main.py", &semantics(&[], &[]));
    assert!(lookup(&cache, &fx, "app/main.py"));
    let mut added = Fixture::new(&[
        ("app/main.py", "import app.extra\ndef main():\n    pass\n"),
        ("app/extra.py", "X = 1\n"),
    ]);
    added.facts[0] = fx.facts[0].clone();
    assert!(!lookup(&cache, &added, "app/main.py"));
}

#[test]
fn environment_and_facts_are_part_of_the_key() {
    let mut fx = Fixture::new(&[("a.py", "def f(): pass\n")]);
    fx.declare(0, "f");
    let mut cache = SemanticCache::in_memory("pyright");
    store(&mut cache, &fx, "a.py", &semantics(&[], &[]));
    let files = fx.files();
    let refs: Vec<&SemanticFile<'_>> = files.iter().collect();
    let mut other_env = CacheContext::new(Hash32::of(b"other"), &refs);
    assert!(cache.lookup(&mut other_env, "a.py").is_none());
    // Same bytes, different facts (new extractor): miss.
    let mut refacted = fx.clone();
    refacted.facts[0].error_count = 1;
    assert!(!lookup(&cache, &refacted, "a.py"));
    // History is bounded.
    for i in 0..5 {
        let mut v = fx.clone();
        v.sources[0] = format!("def f(): return {i}\n").into_bytes();
        store(&mut cache, &v, "a.py", &semantics(&[], &[]));
    }
    assert_eq!(cache.data.files["a.py"].len(), MAX_ENTRIES_PER_FILE);
    let a = environment_key("pyright", "fp", &[("go.mod", &b"x"[..])], &[]);
    let b = environment_key("pyright", "fp", &[("go.mod", &b"y"[..])], &[]);
    let c = environment_key("lsp:gopls", "fp", &[("go.mod", &b"y"[..])], &["go.mod"]);
    let d = environment_key("lsp:gopls", "fp", &[("go.mod", &b"z"[..])], &["go.mod"]);
    assert_eq!(a, b, "configs the backend does not copy are irrelevant");
    assert_ne!(c, d);
}

/// Rule (bash-language-server scoping): a script depends only on the declarations of the
/// files it sources; a new declaration of its unresolved name in a script it does not
/// source keeps its entry, the same declaration in a sourced file invalidates it.
#[test]
fn shell_scripts_depend_only_on_sourced_declarations() {
    let mut fx = Fixture::new(&[
        ("main.sh", "source ./lib.sh\nrun() {\n  greet\n  missing\n}\n"),
        ("lib.sh", "greet() {\n  echo hi\n}\n"),
        ("other.sh", "other() {\n  true\n}\n"),
    ]);
    fx.language = Language::Bash;
    fx.declare(0, "run");
    fx.declare(1, "greet");
    fx.declare(2, "other");
    fx.import(0, "./lib.sh");
    let mut cache = SemanticCache::in_memory("lsp:bash-language-server");
    store(&mut cache, &fx, "main.sh", &semantics(&[], &["missing"]));
    assert!(lookup(&cache, &fx, "main.sh"));
    let mut unrelated = fx.clone();
    unrelated.sources[2] = b"other() {\n  true\n}\nmissing() {\n  true\n}\n".to_vec();
    unrelated.declare(2, "missing");
    assert!(lookup(&cache, &unrelated, "main.sh"), "other.sh is not sourced: no re-query");
    let mut sourced = fx.clone();
    sourced.sources[1] = b"greet() {\n  echo hi\n}\nmissing() {\n  true\n}\n".to_vec();
    sourced.declare(1, "missing");
    assert!(!lookup(&cache, &sourced, "main.sh"), "lib.sh is sourced: re-query");
    let files = fx.files();
    let scopes = ShellScopes::new(files.iter().map(|f| (f.path, f.facts)));
    assert_eq!(scopes.closure("main.sh").into_iter().collect::<Vec<_>>(), vec!["lib.sh", "main.sh"]);
    assert_eq!(scopes.closure("other.sh").into_iter().collect::<Vec<_>>(), vec!["other.sh"]);
}

#[test]
fn caches_persist_outside_the_target() {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join("trace-semantic-tests")
        .join(format!("cache-{}", uuid::Uuid::new_v4().simple()));
    let mut fx = Fixture::new(&[("a.py", "def f(): pass\n")]);
    fx.declare(0, "f");
    let (mut cache, warning) = SemanticCache::load(&dir, "lsp:gopls");
    assert!(warning.is_none());
    assert!(cache.data.files.is_empty());
    store(&mut cache, &fx, "a.py", &semantics(&[], &[]));
    cache.save().unwrap();
    assert!(dir.join("lsp-gopls.bin").is_file());
    let (loaded, warning) = SemanticCache::load(&dir, "lsp:gopls");
    assert!(warning.is_none());
    assert!(lookup(&loaded, &fx, "a.py"));
    std::fs::write(dir.join("lsp-gopls.bin"), b"garbage").unwrap();
    let (broken, warning) = SemanticCache::load(&dir, "lsp:gopls");
    assert!(warning.is_some());
    assert!(broken.data.files.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Rule (declaration reuse, library dispatch): a unit's library dispatch answers move
/// with the unit (byte and line offsets, owner re-indexed); an implementor that no longer
/// exists in the file makes the unit be asked again.
#[test]
fn rule_library_dispatch_answers_move_with_their_unit() {
    let old = UnitAnswers {
        uid: "a.go:run".into(),
        start: 100,
        line: 10,
        library_dispatch: vec![SemLibraryDispatch {
            owner: 3,
            at: ByteSpan::new(120, 131),
            line: 12,
            library_symbol: Some("example.com/web.ServeHTTP".into()),
            implementations: vec!["b.go:Engine.ServeHTTP".into()],
        }],
        ..UnitAnswers::default()
    };
    let owner = |old: u32| Some(old + 1);
    let known = |_: &str| true;
    let moved = translate_unit(&old, 110, 11, &owner, &known).expect("reused");
    assert_eq!(
        moved.library_dispatch,
        vec![SemLibraryDispatch {
            owner: 4,
            at: ByteSpan::new(130, 141),
            line: 13,
            library_symbol: Some("example.com/web.ServeHTTP".into()),
            implementations: vec!["b.go:Engine.ServeHTTP".into()],
        }]
    );
    let gone = |uid: &str| uid != "b.go:Engine.ServeHTTP";
    assert!(
        translate_unit(&old, 110, 11, &owner, &gone).is_none(),
        "a vanished implementor re-asks the unit"
    );
}
