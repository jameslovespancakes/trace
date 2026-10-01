use super::*;

#[test]
fn reads_openapi_shapes() {
    let doc = r#"
openapi: 3.0.0   # comment
info:
  title: "Pets # not a comment"
  description: |
    Multi-line
    text
servers:
  - url: http://localhost:8000/api/v1
paths:
  /pets/{petId}:
    get:
      operationId: showPetById
      tags: [pets, 'read']
      parameters:
      - name: petId
        in: path
    post: {operationId: updatePet, deprecated: true}
  '/users':
    delete:
      operationId: "deleteUser"
"#;
    let v = parse(doc).unwrap();
    assert_eq!(v["info"]["title"], "Pets # not a comment");
    assert_eq!(v["servers"][0]["url"], "http://localhost:8000/api/v1");
    assert_eq!(v["paths"]["/pets/{petId}"]["get"]["operationId"], "showPetById");
    assert_eq!(v["paths"]["/pets/{petId}"]["get"]["tags"][1], "read");
    assert_eq!(v["paths"]["/pets/{petId}"]["get"]["parameters"][0]["in"], "path");
    assert_eq!(v["paths"]["/pets/{petId}"]["post"]["operationId"], "updatePet");
    assert_eq!(v["paths"]["/users"]["delete"]["operationId"], "deleteUser");
    assert_eq!(v["info"]["description"], "Multi-line\ntext");
}

#[test]
fn scalars() {
    assert_eq!(scalar("~"), Value::Null);
    assert_eq!(scalar("'it''s'"), Value::String("it's".into()));
    assert_eq!(scalar("\"a\\\"b\""), Value::String("a\"b".into()));
    assert_eq!(split_key("a: b"), Some(("a".into(), "b".into())));
    assert_eq!(split_key("http://x"), None);
    assert_eq!(strip_comment("a: b # c"), "a: b ");
}
