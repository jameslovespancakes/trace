use super::*;
use trace_core::model::ByteSpan;

fn library(dir: &Path) -> Library {
    Library::open(&dir.join("cache")).expect("library")
}

fn lib_file(path: &Path) -> LibraryFile {
    LibraryFile {
        path: path.to_string_lossy().into_owned(),
        package: "pkg".to_string(),
        version: Some("1.0".to_string()),
        stdlib: false,
        readable: true,
        language: Language::Python,
    }
}

fn request<'a>(file: &'a LibraryFile, line: u32, column: u32) -> BehaviourRequest<'a> {
    BehaviourRequest {
        file: "app.py",
        language: Language::Python,
        callee: ByteSpan::new(10, 13),
        spelling: "run",
        qualifier: None,
        positional_args: 1,
        keywords: Vec::new(),
        target: Some((file, line, column)),
        symbol: None,
        callback_params: Vec::new(),
        args: vec![RequestArg {
            span: ByteSpan::new(14, 18),
            index: Some(0),
            keyword: None,
        }],
    }
}

#[test]
fn rule_derived_below_gate_is_possible() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("lib.py");
    std::fs::write(&src, "def run(fn):\n    return fn()\n").expect("write");
    let file = lib_file(&src);
    let lib = library(dir.path());
    let mut closed = gate::Gate::load_builtin().expect("gate");
    closed.languages.insert(
        Language::Python,
        gate::GateResult {
            passed: false,
            ..gate::GateResult::default()
        },
    );
    let lib = lib.with_gate(closed.clone());
    let k = lib.knowledge(&[request(&file, 0, 4)]);
    let b = k.by_call.get(&("app.py".to_string(), 10)).expect("behaviour");
    assert_eq!(b.source, BehaviourSource::Derived);
    assert!(!b.inferred, "below the gate: possible");
    assert_eq!(b.effects, vec![Effect::Calls(ArgSel::PosOrKw(0, "fn".to_string()))]);
    assert_eq!(k.stats.derived, 1);
    assert_eq!(k.stats.sites, 1);
    let mut open = closed;
    open.languages.insert(
        Language::Python,
        gate::GateResult {
            precision: 0.95,
            facts: 40,
            sample: "test".to_string(),
            passed: true,
        },
    );
    let lib = library(dir.path()).with_gate(open);
    let k = lib.knowledge(&[request(&file, 0, 4)]);
    assert!(k.by_call.values().all(|b| b.inferred), "gate passed: inferred");
    assert_eq!(k.stats.cache_hits, 1, "second library reads the per-machine cache");
}

#[test]
fn rule_declared_function_type_answers_without_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lib = library(dir.path());
    let param = trace_core::semantics::SemCallbackParam {
        call: ByteSpan::new(10, 13),
        arg: ByteSpan::new(14, 18),
        param_name: Some("callback".to_string()),
        param_type: "Callable[[], None]".to_string(),
        verdict: FnTypeVerdict::FunctionType,
        route: "declaration".to_string(),
        library_symbol: Some("lib.schedule".to_string()),
    };
    let missing = lib_file(Path::new("missing.pyi"));
    let mut r = BehaviourRequest {
        target: None,
        ..request(&missing, 0, 0)
    };
    r.callback_params = vec![&param];
    let k = lib.knowledge(&[r]);
    let b = k.by_call.values().next().expect("behaviour");
    assert_eq!(b.source, BehaviourSource::DeclaredType);
    assert!(b.inferred);
    assert_eq!(b.effects, vec![Effect::Calls(ArgSel::PosOrKw(0, "callback".to_string()))]);
    assert_eq!(b.symbol.as_deref(), Some("lib.schedule"));
}

fn stdlib_root(path: &Path, ecosystem: EcosystemId) -> LibraryRoot {
    LibraryRoot {
        path: path.to_path_buf(),
        kind: LibraryKind::Stdlib,
        ecosystem,
        layout: "toolchain_stdlib",
        version: Some("1.0".to_string()),
    }
}

fn located_nowhere<'a>(language: Language, module: &'a str, spelling: &'a str) -> BehaviourRequest<'a> {
    BehaviourRequest {
        file: "app",
        language,
        callee: ByteSpan::new(10, 20),
        spelling,
        qualifier: Some(module),
        positional_args: 2,
        keywords: Vec::new(),
        target: None,
        symbol: None,
        callback_params: Vec::new(),
        args: vec![RequestArg {
            span: ByteSpan::new(24, 28),
            index: Some(1),
            keyword: None,
        }],
    }
}

#[test]
fn rule_stdindex_is_not_built_when_servers_give_locations() {
    let dir = tempfile::tempdir().expect("tempdir");
    let go_src = dir.path().join("goroot").join("src");
    std::fs::create_dir_all(go_src.join("codec")).expect("dirs");
    std::fs::write(
        go_src.join("codec").join("codec.go"),
        "package codec\n\nfunc Dumps(obj int, fn func(int)) {\n\tfn(obj)\n}\n",
    )
    .expect("write");
    let py_lib = dir.path().join("Lib");
    std::fs::create_dir_all(py_lib.join("codec")).expect("dirs");
    std::fs::write(
        py_lib.join("codec").join("__init__.py"),
        "def dumps(obj, default):\n    return default(obj)\n",
    )
    .expect("write");
    let lib = library(dir.path())
        .with_roots(vec![stdlib_root(&go_src, EcosystemId::Go), stdlib_root(&py_lib, EcosystemId::Python)]);
    // Go: gopls locates every standard-library call, so a call without a location is not
    // looked up in a module index (none is built).
    let k = lib.knowledge(&[located_nowhere(Language::Go, "codec", "Dumps")]);
    assert!(k.by_call.is_empty());
    assert!(!lib.cache_dir().join("stdindex").exists(), "no Go stdlib index");
    // Python: a stdlib call the server gave no location is found by module + name.
    let k = lib.knowledge(&[located_nowhere(Language::Python, "codec", "dumps")]);
    let b = k.by_call.values().next().expect("behaviour through the module index");
    assert_eq!(b.source, BehaviourSource::Derived);
    assert!(b
        .effects
        .contains(&Effect::Calls(ArgSel::PosOrKw(1, "default".to_string()))));
    assert!(lib.cache_dir().join("stdindex").exists());
}

#[test]
fn rule_declaration_file_callee_is_derived_from_its_implementation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pkg = dir.path().join("node_modules").join("runner");
    std::fs::create_dir_all(pkg.join("dist").join("types")).expect("dirs");
    std::fs::write(
        pkg.join("package.json"),
        r#"{"name":"runner","version":"1.0.0","types":"./dist/types/index.d.ts","main":"./dist/index.js"}"#,
    )
    .expect("write");
    let stub_text = "export declare function run(fn: () => void): void;\n";
    let stub = pkg.join("dist").join("types").join("run.d.ts");
    std::fs::write(&stub, stub_text).expect("write");
    std::fs::write(pkg.join("dist").join("run.js"), "function run(fn) {\n  fn();\n}\nexports.run = run;\n")
        .expect("write");
    let file = LibraryFile {
        path: stub.to_string_lossy().into_owned(),
        package: "runner".to_string(),
        version: Some("1.0.0".to_string()),
        stdlib: false,
        readable: false,
        language: Language::TypeScript,
    };
    let column = stub_text.find("run(").expect("name") as u32;
    let r = BehaviourRequest {
        language: Language::TypeScript,
        ..request(&file, 0, column)
    };
    let k = library(dir.path()).knowledge(&[r]);
    let b = k.by_call.values().next().expect("derived through the implementation");
    assert_eq!(b.source, BehaviourSource::Derived);
    assert_eq!(b.effects, vec![Effect::Calls(ArgSel::Pos(0))]);
}

#[test]
fn rule_java_sources_jar_is_derived() {
    use std::io::Write;
    let dir = tempfile::tempdir().expect("tempdir");
    let version = dir
        .path()
        .join("repository")
        .join("com")
        .join("acme")
        .join("pool")
        .join("1.0");
    std::fs::create_dir_all(&version).expect("dirs");
    let jar = version.join("pool-1.0.jar");
    std::fs::write(&jar, b"").expect("jar placeholder");
    let sources = std::fs::File::create(version.join("pool-1.0-sources.jar")).expect("create");
    let mut w = zip::ZipWriter::new(sources);
    w.start_file("com/acme/Pool.java", zip::write::SimpleFileOptions::default())
        .expect("entry");
    w.write_all(
        b"package com.acme;\n\npublic class Pool {\n    public void submit(Runnable task) {\n        task.run();\n    }\n\n    public void keep(Object value) {\n        this.value = value;\n    }\n\n    private Object value;\n}\n",
    )
    .expect("write");
    w.finish().expect("finish");
    let jar_text = jar.to_string_lossy().replace('\\', "/");
    let file = LibraryFile {
        path: format!("jar:file:///{}!/com/acme/Pool.class", jar_text.trim_start_matches('/')),
        package: "com.acme:pool".to_string(),
        version: Some("1.0".to_string()),
        stdlib: false,
        readable: false,
        language: Language::Java,
    };
    let submit = BehaviourRequest {
        language: Language::Java,
        spelling: "submit",
        symbol: Some("com.acme.Pool"),
        args: vec![RequestArg {
            span: ByteSpan::new(14, 18),
            index: Some(0),
            keyword: None,
        }],
        ..request(&file, 0, 0)
    };
    let keep = BehaviourRequest {
        callee: ByteSpan::new(40, 44),
        spelling: "keep",
        ..submit.clone()
    };
    let k = library(dir.path()).knowledge(&[submit, keep]);
    let b = k
        .by_call
        .get(&("app.py".to_string(), 10))
        .expect("derived from the sources jar");
    assert_eq!(b.source, BehaviourSource::Derived);
    assert_eq!(b.effects, vec![Effect::Calls(ArgSel::Pos(0))]);
    assert!(!k.by_call.contains_key(&("app.py".to_string(), 40)), "data parameters are not run");
}

#[test]
fn rule_no_evidence_is_no_behaviour() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("lib.py");
    std::fs::write(&src, "def keep(value):\n    return None\n").expect("write");
    let file = lib_file(&src);
    let k = library(dir.path()).knowledge(&[request(&file, 0, 4)]);
    assert!(k.by_call.is_empty());
    assert_eq!(k.stats.unknown, 1);
}
