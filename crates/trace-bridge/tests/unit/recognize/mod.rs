use super::*;

#[test]
fn rule_rpc_wire_path_names_service_and_method() {
    assert_eq!(
        rpc_key("/helloworld.Greeter/SayHello"),
        Some(("helloworld".into(), "Greeter".into(), "SayHello".into()))
    );
    assert_eq!(rpc_key("/Greeter/SayHello"), Some((String::new(), "Greeter".into(), "SayHello".into())));
    assert_eq!(rpc_key("no-method"), None);
}

#[test]
fn rule_selectors_map_onto_syntax_arguments() {
    assert_eq!(
        arg_ref(&ArgSel::Field {
            arg: 1,
            field: "method".into()
        }),
        Some(ArgRef::Field(1, "method".into()))
    );
    assert_eq!(arg_ref(&ArgSel::PosOrKw(0, "path".into())), Some(ArgRef::PosOrKw(0, "path".into())));
    assert_eq!(arg_ref(&ArgSel::Result), None);
    assert_eq!(arg_ref(&ArgSel::Member), None);
}
