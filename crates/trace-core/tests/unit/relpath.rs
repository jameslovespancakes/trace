use super::*;

#[test]
fn rule_relative_paths_are_slash_separated_and_never_leave_the_root() {
    assert_eq!(join("", "a.rs"), "a.rs");
    assert_eq!(join("src", "a.rs"), "src/a.rs");
    assert_eq!(parent("a/b/c.rs"), "a/b");
    assert_eq!(parent("c.rs"), "");
    assert_eq!(file_name("a/b/c.rs"), "c.rs");
    assert!(within("a/b", "a") && within("a", "") && !within("ab", "a"));
    assert_eq!(normalize("a/b", "../c\\d.rs").as_deref(), Some("a/c/d.rs"));
    assert_eq!(normalize("", "../x"), None);
    assert_eq!(relative_to("a/b/c.rs", "a"), "b/c.rs");
    assert_eq!(ancestors("a/b"), ["a", ""]);
    assert_eq!(lexical("a/./b/../c").as_deref(), Some("a/c"));
    assert_eq!(lexical("../c"), None);
    assert_eq!(last_component("C:\\Tools\\node.exe"), "node.exe");
    assert_eq!(last_component("/usr/bin/python3"), "python3");
}
