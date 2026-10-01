use super::*;

#[test]
fn rule_jsonc_comments_and_trailing_commas_are_ignored() {
    let text = "{\n  // line\n  \"a\": \"x//y\", /* block\n */ \"b\": [1, 2,],\n  \"c\": \"q\\\"/*\",\n}\n";
    let v = parse(text).unwrap();
    assert_eq!(v["a"], "x//y");
    assert_eq!(v["b"], serde_json::json!([1, 2]));
    assert_eq!(v["c"], "q\"/*");
}

#[test]
fn rule_jsonc_invalid_documents_are_none() {
    assert!(parse("{\"a\": }").is_none());
    assert!(parse("\u{feff}{}").is_some());
}
