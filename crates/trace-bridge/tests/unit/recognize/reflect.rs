use super::*;
use trace_core::facts::Scope;
use trace_core::ByteSpan;

fn import(local: &str, target: &str, kind: ImportKind) -> Import {
    Import {
        local: local.into(),
        target: target.into(),
        kind,
        scope: Scope::Module,
        span: ByteSpan::new(0, 0),
        line: 1,
    }
}

#[test]
fn rule_annotation_names_resolve_through_imports_then_on_demand_imports_then_the_package() {
    let imports = vec![
        import("GetRoute", "lib.web.GetRoute", ImportKind::Member),
        import("*", "lib.more", ImportKind::Wildcard),
    ];
    assert_eq!(candidates("GetRoute", &imports, Some("app")), vec!["lib.web.GetRoute".to_string()]);
    assert_eq!(
        candidates("PostRoute", &imports, Some("app")),
        vec!["lib.more.PostRoute".to_string(), "app.PostRoute".to_string()]
    );
    assert_eq!(candidates("lib.x.Route", &[], None), vec!["lib.x.Route".to_string()]);
}

#[test]
fn rule_compiled_attribute_reaching_template_and_verb_roots_gives_path_root_and_loaded_verb() {
    use trace_library::model::{ArgSel, VerbSel};
    let hex = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/rule-derive-clr-metadata/Lib.Web.dll.hex"),
    )
    .expect("fixture hex");
    let digits: Vec<u8> = hex.bytes().filter(u8::is_ascii_hexdigit).collect();
    let bytes: Vec<u8> = digits
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).expect("ascii"), 16).expect("hex"))
        .collect();
    let dir = tempfile::tempdir().expect("temp");
    let dll = dir.path().join("lib.web/1.0.0/lib/net10.0/Lib.Web.dll");
    std::fs::create_dir_all(dll.parent().expect("parent")).expect("dirs");
    std::fs::write(&dll, bytes).expect("dll");
    let roots = [LibraryRoot {
        path: dir.path().to_path_buf(),
        kind: trace_env::LibraryKind::Dependency,
        ecosystem: trace_env::EcosystemId::Dotnet,
        layout: "nuget_packages",
        version: None,
    }];
    let rows = [
        ChannelRow {
            symbol: Some("Lib.Web.Routing.ITemplateSource".into()),
            key: Some(ArgSel::Kw("Template".into())),
            ..ChannelRow::default()
        },
        ChannelRow {
            symbol: Some("Lib.Web.Routing.IVerbSource".into()),
            verb: Some(VerbSel::Arg(ArgSel::Kw("Verbs".into()))),
            ..ChannelRow::default()
        },
    ];
    let facts = FileFacts {
        imports: vec![import("*", "Lib.Web", ImportKind::Wildcard)],
        ..FileFacts::default()
    };
    let written: BTreeSet<String> = ["Read".to_string(), "Plain".to_string()].into();
    let chains = compiled_attribute_chains(&roots, &facts, &written, &rows);
    let read = chains.get("Read").expect("Read reaches the roots");
    assert_eq!(read.root, "ITemplateSource");
    assert_eq!(read.verb.as_deref(), Some("GET"));
    // An attribute whose lineage reaches no root row has no chain.
    assert!(!chains.contains_key("Plain"));
}

#[test]
fn rule_decorator_metadata_with_one_method_token_and_one_argument_value_is_a_route() {
    let w = |key: &str, value: MetaValue| MetaWrite {
        key: key.into(),
        value,
        on_member: true,
    };
    let path = MetaValue::Choice(vec![MetaValue::Arg(0), MetaValue::Str("/".into())]);
    let writes = vec![w("route", path.clone()), w("verb", MetaValue::Member("GET".into()))];
    let written = vec![Some(":id".to_string())];
    assert_eq!(member_route(&written, &writes), Some(("route".into(), "GET".into(), ":id".into())));
    // No argument: the constant default.
    assert_eq!(member_route(&[], &writes).map(|r| r.2), Some("/".into()));
    // A non-string argument, no method token, or two keys built from arguments: no route.
    assert_eq!(member_route(&[None], &writes), None);
    assert_eq!(
        member_route(&written, &[w("route", path.clone()), w("kind", MetaValue::Member("ITEM".into()))]),
        None
    );
    assert_eq!(
        member_route(
            &written,
            &[
                w("route", path.clone()),
                w("other", MetaValue::Arg(0)),
                w("verb", MetaValue::Str("POST".into()))
            ]
        ),
        None
    );
    // Class metadata resolves the same way (the prefix).
    let prefix = MetaValue::Choice(vec![
        MetaValue::Str("/".into()),
        MetaValue::Arg(0),
        MetaValue::ArgField(0, "path".into()),
    ]);
    assert_eq!(resolve(&prefix, &[Some("users".into())]), Some(vec!["users".to_string()]));
    assert_eq!(resolve(&prefix, &[None]), None);
}

#[test]
fn rule_declared_package_comes_from_the_package_declaration() {
    assert_eq!(declared_package(b"package lib.web;\nclass A {}\n").as_deref(), Some("lib.web"));
    assert_eq!(declared_package(b"class A {}\n"), None);
}
