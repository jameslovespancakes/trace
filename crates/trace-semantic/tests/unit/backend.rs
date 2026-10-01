use super::*;

struct Fake {
    id: &'static str,
    languages: Vec<Language>,
}

impl Backend for Fake {
    fn id(&self) -> &str {
        self.id
    }
    fn languages(&self) -> &[Language] {
        &self.languages
    }
    fn fingerprint(&self, _tools: &ToolEnv, _prepared: &crate::languages::Prepared) -> String {
        String::new()
    }
    fn run(&self, _request: &SemanticRequest<'_>) -> Result<BackendOutput, SemanticError> {
        Err(SemanticError::Worker("not runnable".into()))
    }
}

fn tools() -> ToolEnv {
    let config = trace_core::config::Settings::default();
    let base = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("backend-plan-{}", std::process::id()));
    std::fs::create_dir_all(base.join("root")).expect("temp root");
    std::fs::create_dir_all(base.join("home")).expect("temp home");
    ToolEnv::discover(&config, &base.join("home"), &base.join("root"))
        .unwrap_or_else(|e| panic!("tool env: {e}"))
}

/// No fallback (PLAN decision 3): every language gets exactly its one registry entry,
/// whether or not that server is installed; languages nobody serves are absent (the
/// setup phase reports them).
#[test]
fn rule_plan_assigns_one_entry_per_language() {
    let backends: Vec<Box<dyn Backend>> = vec![
        Box::new(Fake {
            id: "lsp:clangd",
            languages: vec![Language::C, Language::Cpp],
        }),
        Box::new(Fake {
            id: "pyright",
            languages: vec![Language::Python],
        }),
        Box::new(Fake {
            id: "alt-python",
            languages: vec![Language::Python],
        }),
    ];
    let present = [Language::Python, Language::C, Language::Haskell, Language::Python];
    let chosen = plan(&backends, &present);
    let ids: Vec<(&str, Vec<Language>)> = chosen.iter().map(|(b, l)| (b.id(), l.clone())).collect();
    assert_eq!(ids, vec![("lsp:clangd", vec![Language::C]), ("pyright", vec![Language::Python])]);
}

#[test]
fn registry_order_is_stable() {
    let ids: Vec<String> = registry(&tools()).iter().map(|b| b.id().to_string()).collect();
    assert_eq!(&ids[..3], ["pyright", "typescript", "lsp:rust-analyzer"]);
    assert!(ids.contains(&"lsp:gopls".to_string()));
    assert!(ids.contains(&"lsp:clangd".to_string()));
    // Exactly the registry's entries, in file order.
    let file: Vec<String> = crate::registry::Registry::builtin()
        .backends
        .iter()
        .map(|b| b.id.clone())
        .collect();
    assert_eq!(ids, file);
}

#[test]
fn plan_follows_the_registry_order() {
    // A registry override that serves Go by another id and drops PHP: every language is
    // served by at most one entry; unserved languages are not planned.
    let mut t = tools();
    let gopls = t.registry.entry("lsp:gopls").cloned().unwrap();
    let mut alt = gopls.clone();
    alt.id = "lsp:gopls-alt".into();
    t.registry
        .backends
        .retain(|b| b.id != "lsp:intelephense" && b.id != "lsp:gopls");
    t.registry.backends.push(alt);
    t.registry.validate().unwrap();
    let backends = registry(&t);
    let ids: Vec<&str> = backends.iter().map(|b| b.id()).collect();
    assert_eq!(ids.last(), Some(&"lsp:gopls-alt"));
    let chosen = plan(&backends, &[Language::Go, Language::Php]);
    assert_eq!(chosen.len(), 1, "installed or not, Go is planned on its one entry");
    assert_eq!(chosen[0].0.id(), "lsp:gopls-alt");
    assert_eq!(chosen[0].1, vec![Language::Go]);
}
