use super::*;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[test]
fn rule_collect_combines_every_failure() {
    let mut c = Collect::default();
    assert_eq!(c.check::<u32>(Ok(1)), Some(1));
    assert!(c.is_empty());
    assert_eq!(
        c.check::<u32>(Err(SetupError::ServerMissing {
            language: Language::Scala
        })),
        None
    );
    c.push(SetupError::DepsMissing {
        language: Language::Python,
        hint: "pip install -r requirements.txt".into(),
    });
    let err = c.finish(()).unwrap_err();
    assert_eq!(err.kind(), "setup_incomplete");
    assert_eq!(err.lines().len(), 3);
    let ok: Result<u8, SetupError> = Collect::default().finish(7);
    assert_eq!(ok, Ok(7));
}

#[test]
fn rule_shared_checks_use_the_catalogue_texts() {
    let report = trace_env::DepsReport {
        status: trace_env::DepsStatus::Missing,
        hint: "go mod download".into(),
        ..trace_env::DepsReport::none_declared()
    };
    assert_eq!(
            deps_error(Language::Go, &report).unwrap().to_string(),
            "Dependencies not installed. Install your project's dependencies (go mod download)\n       or point trace to them: trace index --env <path>"
        );
    assert!(deps_error(Language::Go, &trace_env::DepsReport::none_declared()).is_none());
    let spec = ToolchainSpec {
        ecosystem: "go".into(),
        needs: "Go".into(),
        install: "Install it from https://go.dev/dl".into(),
        optional: false,
    };
    let missing = trace_env::ToolchainStatus::Missing { searched: vec![] };
    assert_eq!(
        toolchain_error(Language::Go, &spec, &missing).unwrap().to_string(),
        "Go is not installed. Install it from https://go.dev/dl and run trace again."
    );
    let found = trace_env::Toolchain {
        id: "go",
        root: PathBuf::from("go"),
        version: trace_env::os::Version::parse("1.25.3"),
        executables: BTreeMap::new(),
        origin: trace_env::Origin::Path,
        facts: BTreeMap::new(),
    };
    let old = trace_env::ToolchainStatus::TooOld {
        found,
        needed: trace_env::os::VersionReq::at_least(trace_env::os::Version::parse("1.26").unwrap()),
        source: "go.mod".into(),
    };
    assert_eq!(
            toolchain_error(Language::Go, &spec, &old).unwrap().to_string(),
            "This Go project needs Go 1.26 or newer (go.mod); the installed Go is 1.25.3. Install it from https://go.dev/dl and run trace again."
        );
    let optional = ToolchainSpec {
        optional: true,
        ..spec
    };
    assert!(toolchain_error(Language::Go, &optional, &missing).is_none());
}

use crate::backends::fntype::FnTypeRoute;
use crate::languages::{default_prepared, DefaultServer, Server};

/// A backend whose preflight finds two independent failures (server + approval).
struct ScalaMissingTwo;
impl Server for ScalaMissingTwo {
    fn preflight(&self, cx: &SetupContext<'_>) -> Result<Prepared, SetupError> {
        let mut c = Collect::default();
        c.push(SetupError::ServerMissing {
            language: cx.language(),
        });
        c.push(SetupError::BuildNotAllowed {
            language: cx.language(),
            tool: "Gradle".into(),
            runs: "this project's build scripts".into(),
        });
        c.finish(default_prepared(cx))
    }
    fn fn_type_route(&self, _language: Language) -> FnTypeRoute {
        FnTypeRoute::TableOnly
    }
}

/// A backend whose project dependencies are not installed.
struct PythonDepsMissing;
impl Server for PythonDepsMissing {
    fn preflight(&self, cx: &SetupContext<'_>) -> Result<Prepared, SetupError> {
        Err(SetupError::DepsMissing {
            language: cx.language(),
            hint: "pip install -r requirements.txt".into(),
        })
    }
    fn fn_type_route(&self, _language: Language) -> FnTypeRoute {
        FnTypeRoute::TableOnly
    }
}

fn fake_hooks(id: &str) -> &'static dyn Server {
    match id {
        "test:scala" => &ScalaMissingTwo,
        "test:python" => &PythonDepsMissing,
        _ => &DefaultServer,
    }
}

struct Fixture {
    base: PathBuf,
    repo: RepoPaths,
    tools: ToolEnv,
    registry: crate::registry::Registry,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let base = std::env::temp_dir()
            .join("trace-tests")
            .join(format!("setup-{name}-{}", uuid::Uuid::new_v4().simple()));
        let root = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let repo = RepoPaths::resolve_in(&root, &home).unwrap();
        Fixture {
            base,
            repo,
            tools: crate::test_support::setup::tool_env(None),
            registry: crate::registry::Registry::builtin(),
        }
    }

    /// A registry entry copied from a builtin one under another id and languages, with
    /// nothing to install and no build (the generic checks of `DefaultServer` pass).
    fn entry(&self, from: &str, id: &str, languages: &[Language]) -> BackendEntry {
        let mut e = self.registry.entry(from).cloned().unwrap();
        e.id = id.into();
        e.languages = languages.to_vec();
        e.install = None;
        e.runtime.clear();
        e.requires_build = None;
        e
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn inputs<'a>(
    fx: &'a Fixture,
    settings: &'a RepoSettings,
    files: &'a [(&'a str, Language)],
    facts: &'a dyn Fn(&str) -> Option<&'a FileFacts>,
    platform: &'a Platform,
    vars: &'a EnvVars,
) -> SetupInputs<'a> {
    SetupInputs {
        repo: &fx.repo,
        settings,
        tools: &fx.tools,
        files,
        facts,
        platform,
        vars,
    }
}

#[test]
fn rule_preflight_all_follows_the_plan_order() {
    let fx = Fixture::new("order");
    let settings = RepoSettings::default();
    let files = [("a.go", Language::Go), ("b.py", Language::Python)];
    let facts = |_: &str| -> Option<&FileFacts> { None };
    let platform = Platform::current();
    let vars = EnvVars::default();
    let cx = inputs(&fx, &settings, &files, &facts, &platform, &vars);
    let py = fx.entry("pyright", "test:ok-python", &[Language::Python]);
    let go = fx.entry("lsp:gopls", "test:ok-go", &[Language::Go]);
    let plan = vec![(&py, vec![Language::Python]), (&go, vec![Language::Go])];
    let prepared = preflight_with(&plan, &cx, &fake_hooks, Vec::new()).unwrap();
    let ids: Vec<&str> = prepared.iter().map(|p| p.backend.as_str()).collect();
    assert_eq!(ids, ["test:ok-python", "test:ok-go"]);
    assert_eq!(prepared[1].languages, vec![Language::Go]);
    let rows = report_with(&plan, &cx, &fake_hooks);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.status == "ready"));
}

/// PLAN decision 15: two languages, three missing items -> ONE error listing all three,
/// numbered, each on one line with its fix.
#[test]
fn rule_setup_reports_everything_missing_at_once() {
    let fx = Fixture::new("several");
    let settings = RepoSettings::default();
    let files = [("app/Main.scala", Language::Scala), ("tools/gen.py", Language::Python)];
    let facts = |_: &str| -> Option<&FileFacts> { None };
    let platform = Platform::current();
    let vars = EnvVars::default();
    let cx = inputs(&fx, &settings, &files, &facts, &platform, &vars);
    let py = fx.entry("pyright", "test:python", &[Language::Python]);
    let sc = fx.entry("lsp:gopls", "test:scala", &[Language::Scala]);
    let plan = vec![(&py, vec![Language::Python]), (&sc, vec![Language::Scala])];
    let err = preflight_with(&plan, &cx, &fake_hooks, Vec::new()).unwrap_err();
    assert_eq!(err.kind(), "setup_incomplete");
    assert_eq!(
            err.lines(),
            vec![
                "This repository needs 3 things before trace can analyze it:".to_string(),
                "  1. Python: Dependencies not installed. Install your project's dependencies (pip install -r requirements.txt) or point trace to them: trace index --env <path>".to_string(),
                "  2. The Scala language server is not installed. Install it: trace status --install scala".to_string(),
                "  3. Scala needs Gradle, which runs this project's build scripts. Only allow this for projects you trust: trace index --allow-build".to_string(),
            ]
        );
    // Languages no registry entry serves come first, in the same error.
    let mut empty = fx.registry.clone();
    empty.backends.clear();
    let earlier = unserved(&[Language::Haskell, Language::Proto, Language::Haskell], &empty, &platform);
    assert_eq!(earlier.len(), 1, "only code languages need a server, each once");
    let err = preflight_with(&plan, &cx, &fake_hooks, earlier).unwrap_err();
    match &err {
        SetupError::Several { items } => {
            assert_eq!(items.len(), 4);
            assert_eq!(items[0].kind(), "server_unavailable");
        }
        other => panic!("expected the combined error, got {other:?}"),
    }
    // Status rows: one per language, each with its own one-line error.
    let rows = report_with(&plan, &cx, &fake_hooks);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].error_type, Some("deps_missing"));
    assert_eq!(rows[0].dependencies.as_deref(), Some("not installed"));
    assert_eq!(rows[1].error_type, Some("setup_incomplete"));
    assert!(rows[1].error.as_deref().unwrap().contains("; Scala needs Gradle"));
}

/// A single failure keeps its own text and error type (never wrapped in the list).
#[test]
fn rule_single_setup_failure_keeps_its_own_text() {
    let fx = Fixture::new("single");
    let settings = RepoSettings::default();
    let files = [("tools/gen.py", Language::Python)];
    let facts = |_: &str| -> Option<&FileFacts> { None };
    let platform = Platform::current();
    let vars = EnvVars::default();
    let cx = inputs(&fx, &settings, &files, &facts, &platform, &vars);
    let py = fx.entry("pyright", "test:python", &[Language::Python]);
    let plan = vec![(&py, vec![Language::Python])];
    let err = preflight_with(&plan, &cx, &fake_hooks, Vec::new()).unwrap_err();
    assert_eq!(err.kind(), "deps_missing");
    assert_eq!(
            err.to_string(),
            "Dependencies not installed. Install your project's dependencies (pip install -r requirements.txt)\n       or point trace to them: trace index --env <path>"
        );
}

#[test]
fn rule_approval_is_read_from_the_repository_settings() {
    let fx = Fixture::new("approval");
    let files = [("app/Main.scala", Language::Scala)];
    let platform = Platform::current();
    let vars = EnvVars::default();
    let entry = fx.entry("lsp:gopls", "test:ok-scala", &[Language::Scala]);
    let spec = BuildSpec {
        tool: "Gradle".into(),
        runs: "this project's build scripts".into(),
        when: crate::registry::BuildWhen::Always,
    };
    for allow in [false, true] {
        let settings = RepoSettings {
            allow_build: allow,
            ..RepoSettings::default()
        };
        let languages = [Language::Scala];
        let facts = |_: &str| -> Option<&FileFacts> { None };
        let cx = SetupContext {
            repo: &fx.repo,
            entry: &entry,
            languages: &languages,
            files: &files,
            facts: &facts,
            settings: &settings,
            tools: &fx.tools,
            platform: &platform,
            vars: &vars,
            report_only: false,
        };
        let r = require_approval(&cx, &spec);
        assert_eq!(r.is_ok(), allow);
        if let Err(e) = r {
            assert_eq!(e.kind(), "build_not_allowed");
        }
    }
}

/// A registry entry without a language module (a `semantic.registry` override) still gets
/// the checks it declares: its build approval is required, and remembered approval passes.
#[test]
fn rule_registry_entry_checks_apply_without_a_language_module() {
    let fx = Fixture::new("default-hooks");
    let files = [("app/Main.scala", Language::Scala)];
    let facts = |_: &str| -> Option<&FileFacts> { None };
    let platform = Platform::current();
    let vars = EnvVars::default();
    let mut entry = fx.entry("lsp:gopls", "lsp:test-scala", &[Language::Scala]);
    entry.requires_build = Some(BuildSpec {
        tool: "Gradle".into(),
        runs: "this project's build scripts".into(),
        when: crate::registry::BuildWhen::Always,
    });
    let plan = vec![(&entry, vec![Language::Scala])];
    let denied = RepoSettings::default();
    let cx = inputs(&fx, &denied, &files, &facts, &platform, &vars);
    let err = preflight_all(&plan, &cx).unwrap_err();
    assert_eq!(err.kind(), "build_not_allowed");
    let allowed = RepoSettings {
        allow_build: true,
        ..RepoSettings::default()
    };
    let cx = inputs(&fx, &allowed, &files, &facts, &platform, &vars);
    let prepared = preflight_all(&plan, &cx).unwrap();
    assert!(prepared[0].runs_project_code);
}

#[test]
fn rule_one_line_status_error_joins_continuations() {
    let e = SetupError::BuildNotAllowed {
        language: Language::Scala,
        tool: "Gradle".into(),
        runs: "this project's build scripts".into(),
    };
    assert_eq!(
            one_line(&e),
            "Scala needs Gradle, which runs this project's build scripts. Only allow this for projects you trust: trace index --allow-build"
        );
}
