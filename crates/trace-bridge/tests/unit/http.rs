use super::*;

fn s(p: &str) -> Vec<String> {
    segments(p)
}

#[test]
fn matching_rules() {
    assert_eq!(path_match(&s("/users/{}"), false, &s("/users/{}"), false, false), Some(0));
    assert_eq!(path_match(&s("/users/me"), false, &s("/users/{}"), false, false), Some(1));
    assert_eq!(path_match(&s("/users"), false, &s("/users/{}"), false, false), None);
    // Unknown client base: suffix of the route.
    assert_eq!(path_match(&s("/users"), true, &s("/api/v1/users"), false, false), Some(0));
    // Unknown mount prefix: the route is a suffix of the client path.
    assert_eq!(path_match(&s("/api/v1/users"), false, &s("/users"), true, false), Some(0));
    assert_eq!(path_match(&s("/api/Users"), false, &s("/api/users"), false, true), Some(0));
    assert!(method_ok("GET", "*") && !method_ok("GET", "POST"));
    assert!(id_matches("users-read_users", "read_users"));
    assert!(id_matches("read_users_api_v1_users__get", "read_users"));
    assert!(id_matches("readUsers", "read_users"));
    assert!(!id_matches("users-read_user_me", "read_users"));
}
