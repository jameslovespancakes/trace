use super::*;

#[test]
fn proto_services() {
    let src = r#"
syntax = "proto3";
// service Fake { rpc No(A) returns (B); }
package hipstershop;
import "google/protobuf/empty.proto";
option go_package = "x";
message Req { string id = 1; message Inner { int32 x = 1; } }
service CartService {
    rpc AddItem(AddItemRequest) returns (Empty) {}
    rpc GetCart(GetCartRequest) returns (Cart);
    /* rpc Hidden(A) returns (B); */
    rpc Watch(stream A) returns (stream B) { option (google.api.http) = { get: "/v1/x" }; }
}
enum E { A = 0; }
"#;
    let s = parse_proto(src, FileId(0));
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].package, "hipstershop");
    assert_eq!(s[0].name, "CartService");
    assert_eq!(s[0].rpcs, vec!["AddItem", "GetCart", "Watch"]);
}

#[test]
fn graphql_schema() {
    let src = r#"
"""Root"""
schema { query: RootQuery mutation: Mutation }
type RootQuery implements Node @key(fields: "id") {
  "the user"
  user(id: ID!): User
  users(first: Int = 10): [User!]! @deprecated(reason: "x")
}
extend type Mutation { createUser(name: String!): User }
input UserInput { name: String }
type User { id: ID! name: String }
"#;
    let mut g = GraphqlSchema::default();
    assert!(parse_graphql_schema(src, FileId(1), &mut g));
    assert_eq!(g.roots.get("RootQuery").map(String::as_str), Some("Query"));
    assert!(g.fields.contains_key(&("RootQuery".into(), "user".into())));
    assert!(g.fields.contains_key(&("RootQuery".into(), "users".into())));
    assert!(g.fields.contains_key(&("Mutation".into(), "createUser".into())));
    assert!(!g.fields.contains_key(&("UserInput".into(), "name".into())));
    assert!(g.fields.contains_key(&("User".into(), "name".into())));
}

#[test]
fn openapi_json_operations() {
    let v: serde_json::Value = serde_json::from_str(
        r#"{"openapi":"3.1.0","servers":[{"url":"https://api.example.com/api/v1"}],
                "paths":{"/users/{user_id}":{"get":{"operationId":"users-read_user"},"parameters":[]},
                         "/login":{"post":{}}}}"#,
    )
    .unwrap();
    let ops = openapi_operations(&v, FileId(2));
    assert_eq!(ops.len(), 2);
    let get = ops.iter().find(|o| o.method == "GET").unwrap();
    assert_eq!(get.full, vec!["api", "v1", "users", "{}"]);
    assert_eq!(get.relative, vec!["users", "{}"]);
    assert_eq!(get.operation_id.as_deref(), Some("users-read_user"));
}
