use super::*;

#[test]
fn skipped_and_name_kinds() {
    assert!(skipped_type("predefined_type"));
    assert!(skipped_type("array_type"));
    assert!(!skipped_type("user_type"));
    assert!(not_name("type_arguments"));
    assert!(not_name("lifetime"));
    assert_eq!(crate::syntax(Language::Rust).unwrap().type_path_separator, "::");
    assert_eq!(crate::syntax(Language::Php).unwrap().type_path_separator, "\\");
}
