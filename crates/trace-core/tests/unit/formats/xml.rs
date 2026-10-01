use super::*;

#[test]
fn rule_xml_documents_read_as_element_trees() {
    let doc = r#"<?xml version="1.0"?>
<!-- comment -->
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <parent><version>1.2 &amp; 3</version></parent>
  <modules><module>a</module><module>b</module></modules>
  <PackageReference Include="Newtonsoft.Json" Version="13.0.3" />
</project>"#;
    let root = parse(doc).unwrap();
    assert_eq!(root.local_name(), "project");
    assert_eq!(root.text_at(&["parent", "version"]), Some("1.2 & 3"));
    let modules: Vec<&str> = root
        .child("modules")
        .unwrap()
        .children_named("module")
        .map(|m| m.text.as_str())
        .collect();
    assert_eq!(modules, vec!["a", "b"]);
    assert_eq!(root.child("PackageReference").unwrap().attr("Version"), Some("13.0.3"));
}

#[test]
fn rule_malformed_xml_is_none() {
    assert!(parse("<a><b></a>").is_none());
    assert!(parse("<a>").is_none());
}
