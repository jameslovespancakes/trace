use super::*;

#[test]
fn unrelated_roots_are_allowed() {
    let lab = std::env::temp_dir().join("trace-tests");
    assert!(ensure_allowed(&lab).is_ok());
    assert!(ensure_allowed(Path::new("/home/user/project")).is_ok());
}

#[test]
fn protected_error_kind() {
    let err = CliError::ProtectedRoot(PathBuf::from("x"));
    assert_eq!(err.kind(), "invalid_root");
    assert_eq!(err.to_string(), "This folder is excluded in your trace settings.");
}
