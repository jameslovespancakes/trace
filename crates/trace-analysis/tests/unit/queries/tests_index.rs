use super::*;

#[test]
fn rows_use_python_style_ids() {
    let r = row("tests/test_a.py", "test_f", 10, "f", 0);
    assert_eq!(r.test, "tests/test_a.py::test_f");
    assert_eq!(r.directness, "direct");
    assert_eq!(row("a.ts", "logs in", 3, "login", 1).directness, "via_caller");
}
