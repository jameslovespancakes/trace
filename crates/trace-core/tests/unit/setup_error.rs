use super::*;

#[test]
fn rule_setup_error_texts_follow_the_catalogue() {
    let missing = SetupError::ServerMissing {
        language: Language::Scala,
    };
    assert_eq!(
        missing.to_string(),
        "The Scala language server is not installed. Install it: trace status --install scala"
    );
    assert_eq!(missing.kind(), "server_missing");
    assert_eq!(missing.exit_code(), 3);

    let jdk = SetupError::ToolchainMissing {
        language: Language::Scala,
        needs: "a JDK 17 or newer".into(),
        install: "Install one from https://adoptium.net".into(),
    };
    assert_eq!(
        jdk.to_string(),
        "Scala needs a JDK 17 or newer, which is not installed. Install one from https://adoptium.net and run trace again."
    );
    let go = SetupError::ToolchainMissing {
        language: Language::Go,
        needs: "Go".into(),
        install: "Install it from https://go.dev/dl".into(),
    };
    assert_eq!(go.to_string(), "Go is not installed. Install it from https://go.dev/dl and run trace again.");

    let deps = SetupError::DepsMissing {
        language: Language::Python,
        hint: "pip install -r requirements.txt".into(),
    };
    assert_eq!(
        deps.lines(),
        vec![
            "Dependencies not installed. Install your project's dependencies (pip install -r requirements.txt)".to_string(),
            "       or point trace to them: trace index --env <path>".to_string(),
        ]
    );
    let build = SetupError::BuildNotAllowed {
        language: Language::Java,
        tool: "Gradle".into(),
        runs: "this project's build scripts".into(),
    };
    assert_eq!(
        build.to_string(),
        "Java needs Gradle, which runs this project's build scripts.\n       Only allow this for projects you trust: trace index --allow-build"
    );
    assert_eq!(build.kind(), "build_not_allowed");

    let crash = SetupError::ServerCrashed {
        language: Language::Java,
        log: PathBuf::from("x.log"),
    };
    assert_eq!(crash.to_string(), "The Java language server stopped unexpectedly. Details: x.log");
    let timeout = SetupError::ServerTimeout {
        language: Language::Java,
        minutes: 10,
        log: PathBuf::from("x.log"),
    };
    assert_eq!(timeout.to_string(), "The Java language server did not finish in 10 minutes. Details: x.log");
    let env = SetupError::EnvNotFound {
        language: Some(Language::Python),
        path: PathBuf::from("venv"),
    };
    assert_eq!(env.to_string(), "No Python environment found at venv");
    let env = SetupError::EnvNotFound {
        language: None,
        path: PathBuf::from("venv"),
    };
    assert_eq!(env.to_string(), "No environment found at venv");
    let version = SetupError::ToolchainVersion {
        language: Language::Go,
        needs: "Go 1.26 or newer".into(),
        source: "go.mod".into(),
        tool: "Go".into(),
        found: "1.25.3".into(),
        install: "Install it from https://go.dev/dl".into(),
    };
    assert_eq!(
        version.to_string(),
        "This Go project needs Go 1.26 or newer (go.mod); the installed Go is 1.25.3. Install it from https://go.dev/dl and run trace again."
    );
    let unavailable = SetupError::ServerUnavailable {
        language: Language::C,
        platform: "Linux on aarch64".into(),
        advice: Some("Install clangd with your system package manager and run trace again.".into()),
    };
    assert_eq!(unavailable.lines().len(), 2);
    assert!(unavailable.lines()[1].starts_with(CONTINUATION));
    let failed = SetupError::BuildFailed {
        language: Language::Haskell,
        what: "cabal build failed".into(),
        log: PathBuf::from("b.log"),
    };
    assert_eq!(
        failed.to_string(),
        "This Haskell project could not be built (cabal build failed). Details: b.log"
    );
}

#[test]
fn rule_one_minute_is_singular() {
    let timeout = SetupError::ServerTimeout {
        language: Language::Go,
        minutes: 1,
        log: PathBuf::from("x.log"),
    };
    assert_eq!(timeout.to_string(), "The Go language server did not finish in 1 minute. Details: x.log");
    assert_eq!(timeout.kind(), "server_timeout");
    assert_eq!(minutes_text(2), "2 minutes");
    assert_eq!(timeout_minutes(std::time::Duration::from_secs(1)), 1);
    assert_eq!(timeout_minutes(std::time::Duration::from_secs(60)), 1);
    assert_eq!(timeout_minutes(std::time::Duration::from_secs(61)), 2);
    assert_eq!(timeout_minutes(std::time::Duration::from_secs(900)), 15);
}

#[test]
fn rule_install_failure_texts_name_the_install_id() {
    let download = SetupError::Install {
        language: Some(Language::Scala),
        failure: InstallFailure::Download {
            what: "Scala language server".into(),
        },
    };
    assert_eq!(
        download.to_string(),
        "Could not download the Scala language server. Check your internet connection and run: trace status --install scala"
    );
    assert_eq!(download.kind(), "install_failed");
    let checksum = SetupError::Install {
        language: None,
        failure: InstallFailure::Checksum {
            what: "Node.js runtime".into(),
        },
    };
    assert_eq!(
        checksum.to_string(),
        "The download of the Node.js runtime was damaged (checksum mismatch). Run trace status --install all again."
    );
    let unknown = SetupError::Install {
        language: None,
        failure: InstallFailure::UnknownLanguage { arg: "cobol".into() },
    };
    assert_eq!(unknown.kind(), "invalid_argument");
    assert_eq!(unknown.exit_code(), 2);
    assert!(unknown.to_string().starts_with(
        "Unknown language \"cobol\". Use one of: default, all, python, javascript, typescript,"
    ));
    let licence = SetupError::Install {
        language: Some(Language::Php),
        failure: InstallFailure::LicenceNotAccepted {
            what: "PHP language server".into(),
            url: "https://intelephense.com/legal".into(),
        },
    };
    assert_eq!(
        licence.lines(),
        vec![
            "The PHP language server is installed only after you accept its licence (https://intelephense.com/legal).".to_string(),
            "       To accept it without a question (scripts): trace status --install php --yes".to_string(),
        ]
    );
    assert_eq!(licence.kind(), "install_failed");
    assert_eq!(licence.exit_code(), 3);
    let disk = InstallFailure::DiskSpace {
        what: "Java runtime".into(),
        needs_mb: 300,
        dir: PathBuf::from("tools"),
    };
    assert_eq!(
        disk.text("java"),
        "Not enough disk space to install the Java runtime (needs 300 MB in tools)."
    );
}

#[test]
fn rule_mixed_repository_errors_are_combined_into_one() {
    let deps = SetupError::DepsMissing {
        language: Language::Python,
        hint: "pip install -r requirements.txt".into(),
    };
    let scala = SetupError::ServerMissing {
        language: Language::Scala,
    };
    // One item stays that item.
    assert_eq!(SetupError::combine(vec![deps.clone()]), deps);
    // Nested + duplicates flatten, discovery order kept.
    let combined =
        SetupError::combine(vec![deps.clone(), SetupError::combine(vec![scala.clone(), deps.clone()])]);
    assert_eq!(
        combined,
        SetupError::Several {
            items: vec![deps.clone(), scala.clone()]
        }
    );
    assert_eq!(combined.kind(), "setup_incomplete");
    assert_eq!(combined.exit_code(), 3);
    assert_eq!(combined.language(), None);
    assert_eq!(
        combined.lines(),
        vec![
            "This repository needs 2 things before trace can analyze it:".to_string(),
            "  1. Python: Dependencies not installed. Install your project's dependencies (pip install -r requirements.txt) or point trace to them: trace index --env <path>".to_string(),
            "  2. The Scala language server is not installed. Install it: trace status --install scala".to_string(),
        ]
    );
    // The language is not prefixed twice; C does not match inside another word.
    let c = SetupError::ServerMissing {
        language: Language::C,
    };
    assert!(c.item_line().starts_with("The C language server"));
    let cpp_deps = SetupError::DepsMissing {
        language: Language::Cpp,
        hint: "vcpkg install".into(),
    };
    assert!(cpp_deps.item_line().starts_with("C++: Dependencies"));
}

#[test]
#[should_panic]
fn rule_combine_needs_at_least_one_failure() {
    let _ = SetupError::combine(Vec::new());
}

#[test]
fn rule_setup_errors_round_trip_as_json() {
    let e = SetupError::BuildNotAllowed {
        language: Language::Rust,
        tool: "Cargo".into(),
        runs: "this project's build scripts".into(),
    };
    let text = serde_json::to_string(&e).unwrap();
    assert_eq!(serde_json::from_str::<SetupError>(&text).unwrap(), e);
}
