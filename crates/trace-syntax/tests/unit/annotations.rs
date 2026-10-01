use super::*;

fn names(s: &str) -> Vec<String> {
    type_names(s.as_bytes(), 0, s.len(), 0)
        .into_iter()
        .map(|t| t.name)
        .collect()
}

#[test]
fn type_expression_grammar() {
    assert_eq!(names("Picker|nil"), vec!["Picker"]);
    assert_eq!(names("?Foo"), vec!["Foo"]);
    assert_eq!(names("Map<string, Foo>"), vec!["Map"]);
    assert_eq!(names("Foo[]"), Vec::<String>::new());
    assert_eq!(names("function(number): boolean"), Vec::<String>::new());
    assert_eq!(names("\\GuzzleHttp\\Client|null"), vec!["GuzzleHttp\\Client"]);
    assert_eq!(names("(A|B)"), vec!["A"]);
    assert_eq!(names("module.Type="), vec!["module.Type"]);
    let spans = type_names(b"x Foo", 2, 5, 100);
    assert_eq!(spans[0].span, ByteSpan::new(102, 105));
}
