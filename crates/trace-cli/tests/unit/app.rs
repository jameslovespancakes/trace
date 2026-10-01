use super::*;

fn ambiguous_err() -> anyhow::Error {
    AnalysisError::Ambiguous {
        reference: "login".into(),
        candidates: vec![
            CandidateRef {
                n: 1,
                id: "src/a.py:login".into(),
                kind: "function",
                file: "src/a.py".into(),
                line: 12,
            },
            CandidateRef {
                n: 2,
                id: "src/b.py:login".into(),
                kind: "method",
                file: "src/b.py".into(),
                line: 3,
            },
        ],
    }
    .into()
}

#[test]
fn json_error_shape_with_numbered_candidates() {
    let v: serde_json::Value = serde_json::from_str(&error_json("path", &ambiguous_err())).unwrap();
    assert_eq!(v["command"], "path");
    assert_eq!(v["error_type"], "ambiguous_symbol");
    assert_eq!(
        v["candidates"],
        json!([
            {"n": 1, "id": "src/a.py:login", "kind": "function", "file": "src/a.py", "line": 12},
            {"n": 2, "id": "src/b.py:login", "kind": "method", "file": "src/b.py", "line": 3}
        ])
    );
    assert!(v["error"].as_str().unwrap().contains("matches 2 symbols"));
    // A core ambiguity (no index details) still gets numbered ids.
    let core: anyhow::Error = CoreError::AmbiguousSymbol {
        reference: "x".into(),
        candidates: vec!["a.py:x".into()],
    }
    .into();
    let v = error_value("deps", &core);
    assert_eq!(v["command"], "deps");
    assert_eq!(v["candidates"][0]["n"], 1);
    assert_eq!(v["candidates"][0]["file"], "a.py");
}

#[test]
fn text_error_lists_numbered_candidates() {
    let text = error_text(&ambiguous_err());
    assert_eq!(text, "\"login\" matches 2 symbols. Use one of:\n  1. src/a.py:login\n  2. src/b.py:login");
}

/// DESIGN §1.3: the approved texts of the catalogue, as `Error: ` + text and as one JSON
/// line; setup errors exit 3, usage errors 2.
#[test]
fn rule_error_texts_follow_the_catalogue() {
    let not_indexed: anyhow::Error = AnalysisError::NotIndexed("C:/repo".into()).into();
    assert_eq!(error_text(&not_indexed), "No index yet. Run: trace index");
    let missing: anyhow::Error = AnalysisError::SymbolNotFound {
        reference: "parse_config".into(),
        suggestions: vec!["load_config".into(), "parse_args".into()],
    }
    .into();
    assert_eq!(
        error_text(&missing),
        "No symbol named \"parse_config\". Did you mean: load_config, parse_args?"
    );
    assert_eq!(error_kind(&missing), "symbol_not_found");
    assert_eq!(exit_code(&missing), 2);
    let folder: anyhow::Error = CoreError::InvalidRoot(PathBuf::from("proj")).into();
    assert!(error_text(&folder).starts_with("Folder not found: "));
    let excluded: anyhow::Error = CoreError::Excluded(PathBuf::from("proj")).into();
    assert_eq!(error_text(&excluded), "This folder is excluded in your trace settings.");
    assert_eq!(error_kind(&excluded), "invalid_root");
    let line: anyhow::Error = AnalysisError::Core(CoreError::NoNamedSymbolAt {
        reference: "www/index.js:185".into(),
        nearest: vec!["drawCells (line 128)".into(), "getIndex (line 124)".into()],
    })
    .into();
    assert_eq!(
        error_text(&line),
        "Line 185 of www/index.js is not inside a named function.\n       Nearest: drawCells (line 128), getIndex (line 124)"
    );
    let v = error_value("deps", &line);
    assert_eq!(
        v["error"],
        "Line 185 of www/index.js is not inside a named function. Nearest: drawCells (line 128), getIndex (line 124)"
    );
    let setup: anyhow::Error = AnalysisError::Setup(trace_core::SetupError::BuildNotAllowed {
        language: trace_core::Language::Java,
        tool: "Gradle".into(),
        runs: "this project's build scripts".into(),
    })
    .into();
    assert_eq!(
        error_text(&setup),
        "Java needs Gradle, which runs this project's build scripts.\n       Only allow this for projects you trust: trace index --allow-build"
    );
    assert_eq!(error_kind(&setup), "build_not_allowed");
    assert_eq!(exit_code(&setup), 3);
    let several: anyhow::Error = AnalysisError::Setup(trace_core::SetupError::combine(vec![
        trace_core::SetupError::ServerMissing {
            language: trace_core::Language::Scala,
        },
        trace_core::SetupError::DepsMissing {
            language: trace_core::Language::Python,
            hint: "pip install -r requirements.txt".into(),
        },
    ]))
    .into();
    assert!(error_text(&several)
        .starts_with("This repository needs 2 things before trace can analyze it:\n  1. The Scala"));
    let v = error_value("index", &several);
    assert_eq!(v["error_type"], "setup_incomplete");
    assert_eq!(v["errors"].as_array().map(Vec::len), Some(2));
    assert_eq!(v["errors"][1]["error_type"], "deps_missing");
    let locked: anyhow::Error = CoreError::Locked(PathBuf::from("x")).into();
    assert_eq!(error_text(&locked), "Another trace process is updating this index. Try again in a moment.");
}

#[test]
fn error_kinds_and_exit_codes() {
    let not_indexed: anyhow::Error = AnalysisError::NotIndexed("C:/repo".into()).into();
    assert_eq!(error_kind(&not_indexed), "not_indexed");
    assert_eq!(exit_code(&not_indexed), 3);
    let core: anyhow::Error = CoreError::SymbolNotFound("x".into()).into();
    assert_eq!(error_kind(&core), "symbol_not_found");
    assert_eq!(exit_code(&core), 2);
    assert_eq!(exit_code(&ambiguous_err()), 2);
    let cli: anyhow::Error = CliError::InvalidArgument("x".into()).into();
    assert_eq!(error_kind(&cli), "invalid_argument");
    assert_eq!(exit_code(&cli), 2);
    // An unknown `status --install` language is an invalid argument: exit 2.
    let lang: anyhow::Error = AnalysisError::InvalidArgument("unknown language".into()).into();
    assert_eq!(exit_code(&lang), 2);
    let config: anyhow::Error = CoreError::Config("x".into()).into();
    assert_eq!(exit_code(&config), 3);
    let locked: anyhow::Error = CoreError::Locked(PathBuf::from("x")).into();
    assert_eq!(exit_code(&locked), 3);
    let other = anyhow::anyhow!("something else");
    assert_eq!(error_kind(&other), "error");
    assert_eq!(exit_code(&other), 3);
    let v: serde_json::Value = serde_json::from_str(&error_json("status", &other)).unwrap();
    assert!(v.get("candidates").is_none());
}

/// Rule 18: a `file:line` selector on module-level code is `symbol_not_found` (exit 2)
/// with the nearest named symbols, never a silent `<module>`.
#[test]
fn rule_selector_line_error_names_the_nearest_symbols() {
    let err: anyhow::Error = AnalysisError::Core(CoreError::NoNamedSymbolAt {
        reference: "app/main.py:9".into(),
        nearest: vec!["helper (line 6)".into()],
    })
    .into();
    assert_eq!(error_kind(&err), "symbol_not_found");
    assert_eq!(exit_code(&err), 2);
    let v = error_value("deps", &err);
    assert_eq!(v["nearest"], json!(["helper (line 6)"]));
    assert!(error_text(&err).contains("Nearest: helper (line 6)"));
    assert!(error_value("deps", &ambiguous_err()).get("nearest").is_none());
}

#[test]
fn empty_arguments_are_rejected() {
    let bad = Command::Deps {
        symbol: "  ".into(),
        deep: false,
    };
    assert_eq!(validate(&bad).unwrap_err().kind(), "invalid_argument");
    let ok = Command::Path {
        from: "a.py:f".into(),
        to: "b.py:g".into(),
        deep: true,
    };
    assert!(validate(&ok).is_ok());
    let bad_to = Command::Path {
        from: "a.py:f".into(),
        to: String::new(),
        deep: false,
    };
    assert!(validate(&bad_to).is_err());
    let bad_uses = Command::Uses {
        symbol: String::new(),
        deep: true,
    };
    assert!(validate(&bad_uses).is_err());
    let bad_show = Command::Show {
        symbols: vec!["a".into(), " ".into()],
    };
    assert!(validate(&bad_show).is_err());
    let bad_context = Command::Context {
        symbol: String::new(),
        deep: false,
    };
    assert!(validate(&bad_context).is_err());
    let bad_install = Command::Status {
        install: Some(String::new()),
        yes: false,
    };
    assert!(validate(&bad_install).is_err());
    assert!(validate(&Command::Status {
        install: None,
        yes: false
    })
    .is_ok());
    assert!(validate(&Command::Index {
        watch: true,
        allow_build: false,
        env: Vec::new()
    })
    .is_ok());
}

#[test]
fn include_tier_from_deep() {
    assert_eq!(include(true), Tier::Possible);
    assert_eq!(include(false), Tier::Inferred);
}
