use super::*;

#[test]
fn rule_rust_analyzer_health_warning_maps_to_build_error() {
    let status = |health: &str, message: &str| ServerStatus {
        health: health.into(),
        quiescent: true,
        message: Some(message.into()),
    };
    assert_eq!(
        classify_status(&status("warning", "Failed to run build scripts of some packages.")),
        Some(LoadProblem::BuildScripts)
    );
    assert_eq!(
        classify_status(&status(
            "warning",
            "Failed to read Cargo metadata with dependencies for sysroot of `C:/x`"
        )),
        Some(LoadProblem::SysrootDependencies)
    );
    assert_eq!(
            classify_status(&status(
                "warning",
                "Failed to read Cargo metadata with dependencies for `Cargo.toml`: attempting to make an HTTP request, but --offline was specified"
            )),
            Some(LoadProblem::Dependencies)
        );
    assert!(matches!(
        classify_status(&status("error", "Failed to load workspaces")),
        Some(LoadProblem::Workspace(_))
    ));
    assert_eq!(classify_status(&status("ok", "")), None);
    let notes = vec![
        (SERVER_STATUS.to_string(), json!({"health": "ok", "quiescent": false})),
        (
            SERVER_STATUS.to_string(),
            json!({"health": "warning", "quiescent": true, "message": "Failed to run build scripts"}),
        ),
        ("window/logMessage".to_string(), json!({})),
    ];
    let last = last_status(&notes).unwrap();
    assert!(last.quiescent);
    assert_eq!(last.health, "warning");
}

#[test]
fn rule_attribute_macros_on_items_are_expansion_targets() {
    let src = "#[derive(Debug)]\nstruct S;\n\n#[tokio::main]\nasync fn main() {}\n\n#[inline]\nfn f() {}\n\n// note\n#[async_trait]\nimpl T for S {}\n\n#[rustfmt::skip]\nmod m {}\n\n#[my_macro(arg)]\nconst C: u8 = 1;\n";
    let targets = expansion_targets(src.as_bytes());
    let paths: Vec<&str> = targets.iter().map(|t| t.path.as_str()).collect();
    assert_eq!(paths, vec!["tokio::main", "async_trait"]);
    assert_eq!((targets[0].line, targets[0].character), (3, 2));
    let params = expand_params("file:///a.rs", &targets[0]);
    assert_eq!(params["position"]["line"], 3);
    assert_eq!(
        parse_expansion(&json!({"name": "main", "expansion": "fn main() {}"})).as_deref(),
        Some("fn main() {}")
    );
    assert_eq!(parse_expansion(&Value::Null), None);
}
