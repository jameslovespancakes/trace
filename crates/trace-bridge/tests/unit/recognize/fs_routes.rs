use super::*;

#[test]
fn rule_fs_route_key_follows_the_glob_root() {
    assert_eq!(
        fs_route_key("pages/api/**/*.{js,ts}", Some("pages"), "pages/api/users/[id].ts").as_deref(),
        Some("/api/users/[id]")
    );
    assert_eq!(
        fs_route_key("app/**/route.{js,ts}", None, "app/users/[id]/route.ts").as_deref(),
        Some("/users/[id]")
    );
    assert_eq!(
        fs_route_key("src/routes/**/+server.{js,ts}", None, "src/routes/items/+server.ts").as_deref(),
        Some("/items")
    );
    assert_eq!(
        fs_route_key("server/api/**/*.{js,ts}", Some("server"), "server/api/index.ts").as_deref(),
        Some("/api")
    );
    // Without an explicit root the glob's static prefix is the URL root.
    assert_eq!(
        fs_route_key("server/routes/**/*.{js,ts}", None, "server/routes/hello.ts").as_deref(),
        Some("/hello")
    );
    assert_eq!(fs_route_key("pages/api/**/*.{js,ts}", Some("pages"), "lib/x.ts"), None);
}
