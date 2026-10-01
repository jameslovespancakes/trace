use super::*;

fn root(symbol: &str) -> ChannelRow {
    ChannelRow {
        symbol: Some(symbol.to_string()),
        ..ChannelRow::default()
    }
}

#[test]
fn rule_meta_annotation_chain_reaches_reflection_root() {
    let root_src = b"package p;\npublic @interface Mapped {\n  String[] path() default {};\n  Verb[] method() default {};\n}\n";
    let get_src = b"package p;\n@Mapped(method = Verb.GET)\npublic @interface GetRoute {\n  @Alias(annotation = Mapped.class)\n  String[] value() default {};\n  String name() default \"\";\n}\n";
    let chain = annotation_chain(
        Language::Java,
        &[root_src.as_slice(), get_src.as_slice()],
        "p.GetRoute",
        &[root("p.Mapped")],
    )
    .expect("chain");
    assert_eq!(chain.root, "Mapped");
    assert_eq!(chain.verb.as_deref(), Some("GET"));
    assert_eq!(chain.path_elements, vec!["value".to_string()]);
    // No root row: no chain (never a guess).
    assert!(annotation_chain(Language::Java, &[get_src.as_slice()], "GetRoute", &[root("p.Other")]).is_none());
}

#[test]
fn rule_expanded_macro_export_is_exports() {
    let expansion = "#[no_mangle]\npub extern \"C\" fn add_numbers(a: i32, b: i32) -> i32 { a + b }\n";
    assert_eq!(expansion_exports(Language::Rust, expansion), vec!["add_numbers".to_string()]);
    assert!(expansion_exports(Language::Rust, "fn private_helper() {}\n").is_empty());
}

#[test]
fn rule_io_entry_pattern_is_name_and_arity() {
    let row = ChannelRow {
        pattern: Some("__call__/2".into()),
        ..ChannelRow::default()
    };
    assert_eq!(row.entry_pattern(), Some(("__call__", Some(2))));
}
