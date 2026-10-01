use super::*;

#[test]
fn rule_every_registry_backend_has_its_hooks() {
    let registry = crate::registry::Registry::builtin();
    for entry in &registry.backends {
        // A registry backend never falls back to the default hooks (their route is TableOnly
        // for every language, which only Bash and R use).
        let hooks = server_for(&entry.id);
        for l in &entry.languages {
            let route = hooks.fn_type_route(*l);
            if !matches!(l, Language::Bash | Language::R) {
                assert_ne!(route, FnTypeRoute::TableOnly, "{} {l}", entry.id);
            }
        }
    }
    assert_eq!(server_for("unknown").fn_type_route(Language::Python), FnTypeRoute::TableOnly);
}
