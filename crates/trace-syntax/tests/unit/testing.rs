use super::*;

fn cfg(files: &[(&str, &str)]) -> TestConfig {
    let owned: Vec<(&str, &[u8])> = files.iter().map(|(p, t)| (*p, t.as_bytes())).collect();
    TestConfig::from_files(&owned)
}

#[test]
fn rule_language_spec_test_rows_always_apply() {
    // A project whose manifests declare no runner at all.
    let project = cfg(&[("package.json", "{\"dependencies\": {}}"), ("requirements.txt", "requests\n")]);
    assert!(is_test_path("pkg/auth_test.go", Language::Go, &project));
    assert!(is_test_path("crate/tests/it.rs", Language::Rust, &project));
    assert!(!is_test_path("pkg/auth.go", Language::Go, &project));
    assert!(!is_test_path("crate/src/lib.rs", Language::Rust, &project));
}

#[test]
fn rule_runner_rows_need_the_declared_runner() {
    let unknown = TestConfig::default();
    assert!(is_test_path("tests/test_auth.py", Language::Python, &unknown));
    assert!(is_test_path("src/box.test.ts", Language::TypeScript, &unknown));
    assert!(is_test_path("src/__tests__/box.js", Language::JavaScript, &unknown));
    assert!(!is_test_path("src/auth.py", Language::Python, &unknown));
    assert!(!is_test_path("src/contest.py", Language::Python, &unknown));
    assert!(!is_test_path("src/latest.ts", Language::TypeScript, &unknown));
    // Manifests read, runner not declared: its defaults do not apply.
    let without = cfg(&[("package.json", "{\"devDependencies\": {\"typescript\": \"5\"}}")]);
    assert!(!is_test_path("src/box.test.ts", Language::TypeScript, &without));
    let with = cfg(&[("package.json", "{\"devDependencies\": {\"jest\": \"29\"}}")]);
    assert!(is_test_path("src/box.test.ts", Language::TypeScript, &with));
    // unittest (standard library) needs no declaration.
    let py = cfg(&[("requirements.txt", "requests==2\n")]);
    assert!(is_test_path("tests/test_auth.py", Language::Python, &py));
    assert!(!is_test_path("tests/auth_test.py", Language::Python, &py));
}

#[test]
fn rule_test_path_follows_runner_config() {
    // pytest settings replace the runner's default globs.
    let project = cfg(&[
        ("requirements-dev.txt", "pytest>=7\n"),
        ("pytest.ini", "[pytest]\npython_files = check_*.py\ntestpaths = checks\n"),
    ]);
    assert!(is_test_path("checks/check_auth.py", Language::Python, &project));
    assert!(!is_test_path("src/check_auth.py", Language::Python, &project));
    assert!(!is_test_path("checks/auth_test.py", Language::Python, &project));
    // Jest `testMatch` in package.json.
    let js = cfg(&[(
        "web/package.json",
        "{\"devDependencies\": {\"jest\": \"29\"}, \"jest\": {\"testMatch\": [\"**/specs/**/*.js\"]}}",
    )]);
    assert!(is_test_path("web/specs/a/box.js", Language::JavaScript, &js));
    assert!(!is_test_path("web/src/__tests__/box.js", Language::JavaScript, &js));
    // Vitest `test.include` in a TypeScript config module.
    let vite = cfg(&[
        ("package.json", "{\"devDependencies\": {\"vitest\": \"1\"}}"),
        ("vitest.config.ts", "export default defineConfig({ test: { include: ['checks/**/*.ts'] } });\n"),
    ]);
    assert!(is_test_path("checks/a.ts", Language::TypeScript, &vite));
    assert!(!is_test_path("src/a.test.ts", Language::TypeScript, &vite));
    // Maven / Gradle test source sets.
    let jvm = cfg(&[("svc/pom.xml", "<project><artifactId>svc</artifactId></project>")]);
    assert!(is_test_path("svc/src/test/java/FooTest.java", Language::Java, &jvm));
    assert!(!is_test_path("svc/src/main/java/Foo.java", Language::Java, &jvm));
    let gradle = cfg(&[(
        "app/build.gradle.kts",
        "dependencies { testImplementation(\"org.junit.jupiter:junit-jupiter-api:5.10.0\") }\n",
    )]);
    assert!(is_test_path("app/src/test/java/FooTest.java", Language::Java, &gradle));
    assert!(!is_test_path("app/src/main/java/Foo.java", Language::Java, &gradle));
}

#[test]
fn rule_test_declarations_follow_rows() {
    let facts = crate::extract(crate::SourceInput {
        path: "app.py",
        language: Language::Python,
        source: b"def login():
    pass
",
    })
    .expect("extract");
    let mut d = facts
        .declarations
        .iter()
        .find(|d| d.name == "login")
        .cloned()
        .expect("declaration");
    d.name = "test_login".into();
    assert!(is_test_declaration(true, &d));
    assert!(!is_test_declaration(false, &d));
    d.name = "login".into();
    d.decorators = vec!["tokio::test".into()];
    assert!(is_test_declaration(false, &d), "`#[test]` row by its last segment");
    d.decorators = vec!["Fact".into()];
    assert!(is_test_declaration(false, &d), "C# attribute class without its suffix");
    d.decorators = vec!["org.junit.jupiter.api.Test".into()];
    assert!(is_test_declaration(false, &d));
    d.decorators = vec!["cache".into()];
    assert!(!is_test_declaration(false, &d));
}

#[test]
fn rule_convention_globs_match_structurally() {
    assert!(glob_matches("**/*.{test,spec}.{js,ts}", "a/b/c.spec.ts"));
    assert!(glob_matches("**/*.{test,spec}.{js,ts}", "c.test.js"));
    assert!(!glob_matches("**/*.{test,spec}.{js,ts}", "c.js"));
    assert_eq!(glob_matches_within("tests/*.rs", "crates/x/tests/a.rs").as_deref(), Some("crates/x"));
    assert_eq!(glob_matches_within("test_*.py", "a/test_b.py").as_deref(), Some("a"));
    assert!(glob_matches_within("tests/*.rs", "crates/x/tests/sub/a.rs").is_none());
    assert!(glob_matches("[a-c]x?.py", "bxy.py"));
    assert_eq!(attribute_name("Test(timeout = 5)"), "Test");
    assert_eq!(attribute_name("#[test]"), "test");
}
