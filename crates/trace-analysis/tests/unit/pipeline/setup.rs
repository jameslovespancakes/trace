use super::*;
use std::cell::RefCell;
use trace_core::config::Settings;

/// PLAN decision 10: the default languages whose server is missing are installed (one call
/// with exactly those languages) and reported as installed, so `run` resolves the tools
/// again BEFORE the preflight; nothing missing -> no install call.
#[test]
fn rule_default_language_auto_installs_before_preflight() {
    let calls: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let product = [Language::Python, Language::Scala, Language::Go];
    let missing = |languages: &[Language]| -> Vec<Language> {
        assert!(languages.iter().all(|l| l.is_default()), "only default languages are asked");
        languages.iter().copied().filter(|l| *l == Language::Python).collect()
    };
    let mut install = |languages: &[Language]| {
        calls.borrow_mut().push(format!("install {languages:?}"));
        Ok(())
    };
    assert!(install_missing(true, &product, &missing, &mut install).unwrap());
    assert_eq!(*calls.borrow(), vec!["install [Python]".to_string()]);
    let none = |_: &[Language]| -> Vec<Language> { Vec::new() };
    assert!(!install_missing(true, &product, &none, &mut install).unwrap());
    assert_eq!(calls.borrow().len(), 1, "nothing missing: no install call");
    // An install failure is the error (no preflight, no fallback).
    let mut failing = |_: &[Language]| {
        Err(SetupError::Install {
            language: Some(Language::Python),
            failure: trace_core::setup_error::InstallFailure::Download {
                what: "Python language server".into(),
            },
        })
    };
    let err = install_missing(true, &product, &missing, &mut failing).unwrap_err();
    assert_eq!(err.kind(), "install_failed");
    let line = install_progress_line(&trace_semantic::install::InstallProgress {
        what: "Python language server".into(),
        product: "Pyright".into(),
        version: "1.1.414".into(),
        step: "start",
        done: 0,
        total: None,
    });
    assert_eq!(line.as_deref(), Some("Installing the Python language server (Pyright 1.1.414)..."));
    // The order inside `run`: install, resolve the tools again, then the preflight.
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/pipeline/mod.rs"));
    let body = &source[source.find("// 3-5. Languages").unwrap()..];
    let install_at = body.find("auto_install(&host").unwrap();
    let preflight_at = body.find("setup_phase(&backends").unwrap();
    assert!(install_at < preflight_at);
}

/// `TRACE_OFFLINE=1` (and TRACE_NO_AUTO_INSTALL=1 / `semantic.auto_install = false`) turns the
/// automatic install off: nothing is asked or installed; the preflight then reports the
/// missing server.
#[test]
fn rule_auto_install_is_off_with_offline_flag() {
    assert!(!trace_core::config::auto_install_enabled(&Settings::default(), true));
    let product = [Language::Python];
    let asked = RefCell::new(0);
    let missing = |l: &[Language]| -> Vec<Language> {
        *asked.borrow_mut() += 1;
        l.to_vec()
    };
    let mut install = |_: &[Language]| -> std::result::Result<(), SetupError> {
        panic!("no install with TRACE_OFFLINE=1");
    };
    assert!(!install_missing(false, &product, &missing, &mut install).unwrap());
    assert_eq!(*asked.borrow(), 0);
}

/// PLAN decision 10: a non-default language is never installed automatically; its missing
/// server is the approved error naming `trace status --install <lang>`.
#[test]
fn rule_non_default_language_missing_server_is_an_error() {
    let product = [Language::Scala, Language::Haskell];
    let missing = |l: &[Language]| -> Vec<Language> { l.to_vec() };
    let mut install = |_: &[Language]| -> std::result::Result<(), SetupError> {
        panic!("Scala and Haskell are installed only on request");
    };
    assert!(!install_missing(true, &product, &missing, &mut install).unwrap());
    assert_eq!(
        SetupError::ServerMissing {
            language: Language::Scala
        }
        .to_string(),
        "The Scala language server is not installed. Install it: trace status --install scala"
    );
}
