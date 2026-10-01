use super::*;

#[test]
fn path_helpers() {
    assert_eq!(join_rel("web/src", "../api/client"), Some("web/api/client".into()));
    assert_eq!(join_rel("", "../x"), None);
    assert_eq!(python_module_of_path("app/api/__init__.py"), "app.api");
    assert_eq!(python_module_of_path("app/main.py"), "app.main");
    assert_eq!(file_stem("pkg/_core.pyi"), "_core");
    assert_eq!(segments("/users/{id}/posts/{}"), vec!["users", "{}", "posts", "{}"]);
    assert_eq!(lower_first("SayHello"), "sayHello");
    assert_eq!(clamp(BridgeKind::Http, Tier::Proven), Tier::Inferred);
    assert_eq!(clamp(BridgeKind::Message, Tier::Inferred), Tier::Possible);
    assert_eq!(clamp(BridgeKind::CAbi, Tier::Proven), Tier::Proven);
}
