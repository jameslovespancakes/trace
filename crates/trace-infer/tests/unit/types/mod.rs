use super::*;

#[test]
fn spellings_that_prove_nothing() {
    for s in ["any", "Object", "table", "T", "K2", "self", "Self"] {
        assert!(uninformative(s), "{s}");
    }
    for s in ["Picker", "Greeter", "String"] {
        assert!(!uninformative(s), "{s}");
    }
    assert_eq!(split_path("Foo::new"), (Some("Foo"), "new"));
    assert_eq!(split_path("a.b.c"), (Some("a.b"), "c"));
    assert_eq!(split_path("f"), (None, "f"));
    assert!(same_var("$p", "p"));
}
