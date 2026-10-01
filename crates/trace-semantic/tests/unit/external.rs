use super::*;
use trace_env::EcosystemId;

fn root(path: &Path, ecosystem: EcosystemId, layout: &'static str) -> LibraryRoot {
    LibraryRoot {
        path: path.to_path_buf(),
        kind: LibraryKind::Dependency,
        ecosystem,
        layout,
        version: None,
    }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
    std::fs::write(path, text).expect("write");
}

#[test]
fn rule_external_location_package_from_layout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();
    let site = d.join("venv").join("Lib").join("site-packages");
    write(&site.join("web_kit").join("app.py"), "def run(fn):\n    fn()\n");
    write(&site.join("Web_Kit-2.1.0.dist-info").join("top_level.txt"), "web_kit\n");
    let nm = d.join("proj").join("node_modules");
    write(&nm.join("@scope").join("pkg").join("package.json"), r#"{"name":"@scope/pkg","version":"4.5.6"}"#);
    write(&nm.join("@scope").join("pkg").join("index.js"), "module.exports = function (f) { f(); };\n");
    let modcache = d.join("go").join("pkg").join("mod");
    write(
        &modcache
            .join("example.com")
            .join("!acme")
            .join("lib@v1.2.0")
            .join("lib.go"),
        "package lib\n",
    );
    let registry = d.join("cargo").join("registry").join("src");
    write(
        &registry
            .join("index.crates.io-6f17")
            .join("serde_kit-1.0.200")
            .join("src")
            .join("lib.rs"),
        "pub fn f() {}\n",
    );
    write(
        &registry
            .join("index.crates.io-6f17")
            .join("serde_kit-1.0.200")
            .join("Cargo.toml"),
        "[package]\nname = \"serde_kit\"\n",
    );

    let roots = vec![
        root(&site, EcosystemId::Python, "site_packages"),
        root(&nm, EcosystemId::Node, "node_modules"),
        root(&modcache, EcosystemId::Go, "go_modcache"),
        root(&registry, EcosystemId::Rust, "cargo_registry"),
    ];
    let expect = |path: PathBuf, package: &str, version: Option<&str>| {
        let got = locate(&path, &roots).unwrap_or_else(|| panic!("located {}", path.display()));
        assert_eq!(got.package, package, "{}", path.display());
        assert_eq!(got.version.as_deref(), version, "{}", path.display());
        assert!(!got.stdlib);
        // The same layouts are recognised without prepared roots.
        let bare = locate(&path, &[]).unwrap_or_else(|| panic!("marker layout {}", path.display()));
        assert_eq!(bare.package, package, "marker layout {}", path.display());
    };
    expect(site.join("web_kit").join("app.py"), "Web_Kit", Some("2.1.0"));
    expect(nm.join("@scope").join("pkg").join("index.js"), "@scope/pkg", Some("4.5.6"));
    expect(
        modcache
            .join("example.com")
            .join("!acme")
            .join("lib@v1.2.0")
            .join("lib.go"),
        "example.com/Acme/lib",
        Some("v1.2.0"),
    );
    expect(
        registry
            .join("index.crates.io-6f17")
            .join("serde_kit-1.0.200")
            .join("src")
            .join("lib.rs"),
        "serde_kit",
        Some("1.0.200"),
    );
    // Outside every library: not a library call.
    assert!(locate(&d.join("elsewhere").join("x.py"), &roots).is_none());
}

#[test]
fn rule_external_classify_builds_the_library_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let site = dir.path().join("site-packages");
    let file = site.join("kit").join("core.py");
    write(&file, "def run(fn):\n    fn()\n");
    write(&site.join("kit-0.3.0.dist-info").join("top_level.txt"), "kit\n");
    let prepared = Prepared {
        languages: vec![Language::Python],
        library_roots: vec![root(&site, EcosystemId::Python, "site_packages")],
        ..Prepared::default()
    };
    let cx = ExternalContext {
        prepared: &prepared,
        hooks: &crate::languages::DefaultServer,
    };
    let uri = crate::lsp::path_to_uri(&file).expect("uri");
    let (lib, target) = classify(&uri, 0, 4, &cx).expect("library call");
    assert_eq!(lib.package, "kit");
    assert_eq!(lib.version.as_deref(), Some("0.3.0"));
    assert!(lib.readable);
    assert_eq!(lib.language, Language::Python);
    assert_eq!((target.decl_line, target.decl_column), (0, 4));
    // A virtual document without a hook answer is not classified.
    assert!(classify("jdt://contents/rt.jar/java.lang/Thread.class", 0, 0, &cx).is_none());
}

/// Rule (I-30): a declaration file the language server bundles (inside its install
/// directory, `Prepared::vars[SERVER_DIR_VAR]`) is the language's standard library as the
/// server sees it: `<language>-stdlib`, stdlib, not readable (stubs without bodies), with
/// the declaration position kept. Without the server directory the same file is an
/// ordinary package location; a typeshed stub distribution inside a server keeps its
/// package; project dependencies are unaffected.
#[test]
fn rule_server_bundled_stubs_are_the_standard_library() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = dir.path().join("tools").join("php-server").join("1.0.0");
    let stub = server
        .join("node_modules")
        .join("php-server")
        .join("lib")
        .join("stub")
        .join("standard")
        .join("basic.php");
    write(
        &stub,
        "<?php\nfunction array_map(?callable $callback, array $array, array ...$arrays): array {}\n",
    );
    write(
        &server.join("node_modules").join("php-server").join("package.json"),
        r#"{"name":"php-server","version":"1.0.0"}"#,
    );
    let typeshed = server
        .join("dist")
        .join("typeshed-fallback")
        .join("stubs")
        .join("web-kit")
        .join("web_kit")
        .join("api.pyi");
    write(&typeshed, "def get(url: str) -> None: ...\n");
    let vendor = dir.path().join("proj").join("vendor");
    write(
        &vendor.join("composer").join("installed.json"),
        r#"{"packages":[{"name":"acme/http","version":"7.0.1"}]}"#,
    );
    let dependency = vendor.join("acme").join("http").join("src").join("Client.php");
    write(&dependency, "<?php\nclass Client {}\n");

    let mut prepared = Prepared {
        languages: vec![Language::Php],
        ..Prepared::default()
    };
    prepared
        .vars
        .insert(SERVER_DIR_VAR.to_string(), server.to_string_lossy().into_owned());
    let cx = ExternalContext {
        prepared: &prepared,
        hooks: &crate::languages::DefaultServer,
    };
    let uri = crate::lsp::path_to_uri(&stub).expect("uri");
    let (lib, target) = classify(&uri, 1, 9, &cx).expect("bundled stub");
    assert_eq!(lib.package, "php-stdlib");
    assert!(lib.stdlib);
    assert!(!lib.readable, "stubs have no bodies to derive from");
    assert_eq!(lib.language, Language::Php);
    assert_eq!((target.decl_line, target.decl_column), (1, 9));

    // A stub distribution naming its package keeps it.
    let uri = crate::lsp::path_to_uri(&typeshed).expect("uri");
    let (lib, _) = classify(&uri, 0, 4, &cx).expect("typeshed stub");
    assert_eq!(lib.package, "web-kit");
    assert!(!lib.stdlib);

    // Project dependencies are unaffected.
    let uri = crate::lsp::path_to_uri(&dependency).expect("uri");
    let (lib, _) = classify(&uri, 1, 6, &cx).expect("vendor dependency");
    assert_eq!(lib.package, "acme/http");
    assert!(!lib.stdlib);

    // Negative: without the server directory the stub is an ordinary package location.
    let bare = Prepared {
        languages: vec![Language::Php],
        ..Prepared::default()
    };
    let cx = ExternalContext {
        prepared: &bare,
        hooks: &crate::languages::DefaultServer,
    };
    let uri = crate::lsp::path_to_uri(&stub).expect("uri");
    let (lib, _) = classify(&uri, 1, 9, &cx).expect("package location");
    assert_ne!(lib.package, "php-stdlib");
    assert!(!lib.stdlib);
}
