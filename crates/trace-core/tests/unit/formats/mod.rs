#[test]
fn rule_toml_documents_read_structurally() {
    let v = super::toml_value("[package]\nname = \"x\"\nversion = \"0.1.0\"\n").unwrap();
    assert_eq!(v["package"]["name"], "x");
    assert!(super::toml_value("[package\nname=").is_none());
}
