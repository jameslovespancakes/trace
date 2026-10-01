use super::*;
use serde_json::json;

const HEAD: &str = r#""schema":1,"language":"python","runtime":"CPython","roots":["builtins","itertools","functools"],"prelude":["builtins"],"function_types":["typing.Callable"],"top_types":["builtins.object"]"#;

fn table(rest: &str) -> Result<LanguageTable, LibraryError> {
    parse_table("python", &format!("{{{HEAD}{rest}}}"))
}

#[test]
fn rule_table_selectors_and_effects_parse() {
    assert_eq!(parse_selector(&json!({"pos": 1})), Ok(ArgSel::Pos(1)));
    assert_eq!(parse_selector(&json!({"pos": 0, "kw": "target"})), Ok(ArgSel::PosOrKw(0, "target".into())));
    assert!(parse_selector(&json!("block")).is_err());
    assert!(parse_selector(&json!({"mfa": [0, 1, 2]})).is_err());
    assert_eq!(
        parse_selector(&json!({"field": [0, "__index"]})),
        Ok(ArgSel::Field {
            arg: 0,
            field: "__index".into()
        })
    );
    assert!(parse_selector(&json!({"nope": 1})).is_err());
    assert!(parse_selector(&json!({"kw": ""})).is_err());
    assert_eq!(
        parse_effect(&json!({"calls_method": {"arg": {"pos": 0}, "method": "run"}})),
        Ok(Effect::CallsMethod {
            arg: ArgSel::Pos(0),
            method: "run".into()
        })
    );
    assert_eq!(parse_effect(&json!("receiver")), Ok(Effect::Receiver));
}

#[test]
fn rule_member_copy_and_delegation_effects_parse() {
    assert_eq!(parse_selector(&json!({"result": true})), Ok(ArgSel::Result));
    assert!(parse_selector(&json!({"result": false})).is_err());
    assert_eq!(
        parse_effect(&json!({"copies_members": {"from": {"rest": 1}, "to": {"pos": 0}}})),
        Ok(Effect::CopiesMembers {
            from: ArgSel::Rest(1),
            to: ArgSel::Pos(0)
        })
    );
    assert_eq!(
        parse_effect(&json!({"delegates_members": {"object": {"result": true}, "to": {"pos": 0}}})),
        Ok(Effect::DelegatesMembers {
            object: ArgSel::Result,
            to: ArgSel::Pos(0)
        })
    );
    assert!(parse_effect(&json!({"copies_members": {"from": {"pos": 1}}})).is_err());
    assert!(!ArgSel::Result.picks(Some(0), None));
    assert_eq!(
        Effect::CopiesMembers {
            from: ArgSel::Pos(1),
            to: ArgSel::Pos(0)
        }
        .name(),
        "copies_members"
    );
}

#[test]
fn rule_table_entry_needs_why() {
    let bare = r#","entries":[{"symbol":"builtins.map","kind":"function","basis":"no_source","describe":"d","effects":[{"calls":{"pos":0}}]}]"#;
    assert!(table(bare).is_err());
    let ok = r#","entries":[{"symbol":"builtins.map","kind":"function","basis":"no_source","describe":"d","effects":[{"calls":{"pos":0}}],"why":"C builtin"}]"#;
    let t = table(ok).unwrap();
    assert_eq!(t.entries[0].effects, vec![Effect::Calls(ArgSel::Pos(0))]);
    assert_eq!(t.entries[0].basis, Basis::NoSource);
}

#[test]
fn rule_irreducible_row_has_reason() {
    let bad =
        r#","io_send":[{"symbol":"_socket.socket.connect","describe":"d","why_not_derivable":"because"}]"#;
    assert!(table(bad).is_err());
    let ok = r#","io_send":[{"symbol":"_socket.socket.connect","describe":"d","key":{"pos":0},"why_not_derivable":"compiled_runtime"}]"#;
    let t = table(ok).unwrap();
    assert_eq!(t.irreducible[&Section::IoSend].len(), 1);
    let row = &t.irreducible[&Section::IoSend][0];
    assert_eq!(row.key_sel(), Ok(Some(RowSel::Arg(ArgSel::Pos(0)))));
    assert_eq!(row.anchor(), "_socket.socket.connect");
}

#[test]
fn rule_table_symbols_stay_under_runtime_roots() {
    let third_party = r#","entries":[{"symbol":"anyio.to_thread.run_sync","kind":"function","basis":"no_source","describe":"d","why":"w","effects":[{"calls":{"pos":0}}]}]"#;
    assert!(table(third_party).is_err());
    let alias = r#","entries":[{"symbol":"builtins.map","aliases":["starlette.map"],"kind":"function","basis":"no_source","describe":"d","why":"w","effects":[{"calls":{"pos":0}}]}]"#;
    assert!(table(alias).is_err());
}

#[test]
fn rule_duplicate_table_rows_are_rejected() {
    let dup = r#","entries":[
        {"symbol":"builtins.iter","kind":"function","arity":2,"basis":"no_source","describe":"d","why":"w","effects":[{"calls":{"pos":0}}]},
        {"symbol":"builtins.iter","kind":"function","arity":1,"basis":"no_source","describe":"d","why":"w","effects":[{"iterates":{"pos":0}}]},
        {"symbol":"builtins.iter","kind":"function","arity":2,"basis":"no_source","describe":"d","why":"w","effects":[{"calls":{"pos":0}}]}]"#;
    assert!(table(dup).is_err());
    let rows = r#","runtime_dispatch":[
        {"pattern":"p","describe":"d","why_not_derivable":"reflection"},
        {"pattern":"p","describe":"d","why_not_derivable":"reflection"}]"#;
    assert!(table(rows).is_err());
}

#[test]
fn rule_never_calls_entries_hold_only_never_calls() {
    let mixed = r#","entries":[{"symbol":"builtins.id","kind":"function","basis":"never_calls","describe":"d","why":"w","effects":[{"never_calls":{"pos":0}},{"calls":{"pos":0}}]}]"#;
    assert!(table(mixed).is_err());
}

#[test]
fn rule_fs_routes_need_a_glob_and_an_activating_package() {
    let bare = r#","fs_routes":[{"glob":"pages/api/**/*.js","describe":"d","key":"file_path","handler":"default_export","verb":"any","why_not_derivable":"filesystem_convention"}]"#;
    assert!(table(bare).is_err());
    let ok = r#","fs_routes":[{"glob":"pages/api/**/*.js","activated_by":"next","describe":"d","key":"file_path","handler":"default_export","verb":"any","why_not_derivable":"filesystem_convention"}]"#;
    let t = table(ok).unwrap();
    let row = &t.irreducible[&Section::FsRoutes][0];
    assert_eq!(row.key_sel(), Ok(Some(RowSel::FilePath)));
    assert_eq!(row.handler_sel(), Ok(Some(RowSel::DefaultExport)));
    assert_eq!(row.verb_sel(), Ok(Some(RowVerb::Sel(VerbSel::Any))));
    assert!(!row.active(&|_| false));
    assert!(row.active(&|p| p == "next"));
    let bad_verb = r#","io_send":[{"symbol":"fetch","describe":"d","verb":"sometimes","why_not_derivable":"compiled_runtime"}]"#;
    assert!(table(bad_verb).is_err());
    let verbs = r#","io_send":[
        {"symbol":"a","describe":"d","verb":"GET","why_not_derivable":"compiled_runtime"},
        {"symbol":"b","describe":"d","verb":{"field":[1,"method"]},"why_not_derivable":"compiled_runtime"}]"#;
    let t = table(verbs).unwrap();
    let rows = &t.irreducible[&Section::IoSend];
    assert_eq!(rows[0].verb_sel(), Ok(Some(RowVerb::Sel(VerbSel::Const("GET".into())))));
    assert_eq!(
        rows[1].verb_sel(),
        Ok(Some(RowVerb::Sel(VerbSel::Arg(ArgSel::Field {
            arg: 1,
            field: "method".into()
        }))))
    );
}

#[test]
fn rule_syntax_convention_rows_name_a_known_rule_and_bridge() {
    let ok = r#","syntax_conventions":[
        {"rule":"export_function_attribute","bridge":"pyo3","symbol":"pyfunction","describe":"d","why_not_derivable":"generated_code"},
        {"rule":"rpc_client_type","bridge":"grpc","pattern":"<Service>Stub","describe":"d","why_not_derivable":"generated_code"},
        {"rule":"graphql_resolver_base","bridge":"graphql","symbol":"lib.ObjectType","pattern":"resolve_<field>","describe":"d","why_not_derivable":"reflection"},
        {"rule":"registration_call","bridge":"napi","symbol":"REGISTER","key":{"pos":0},"handler":{"pos":1},"describe":"d","why_not_derivable":"generated_code"}]"#;
    let t = table(ok).unwrap();
    let rows = &t.irreducible[&Section::SyntaxConventions];
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].rule.as_deref(), Some("export_function_attribute"));
    assert_eq!(rows[1].bridge.as_deref(), Some("grpc"));
    for bad in [
        // unknown rule
        r#"{"rule":"guess","bridge":"pyo3","symbol":"x","describe":"d","why_not_derivable":"generated_code"}"#,
        // unknown bridge kind
        r#"{"rule":"export_function_attribute","bridge":"magic","symbol":"x","describe":"d","why_not_derivable":"generated_code"}"#,
        // no rule
        r#"{"bridge":"pyo3","symbol":"x","describe":"d","why_not_derivable":"generated_code"}"#,
        // naming pattern without the placeholder / without fixed text
        r#"{"rule":"rpc_client_type","bridge":"grpc","pattern":"Stub","describe":"d","why_not_derivable":"generated_code"}"#,
        r#"{"rule":"rpc_client_type","bridge":"grpc","pattern":"<Service>","describe":"d","why_not_derivable":"generated_code"}"#,
        // symbol rule with only a pattern
        r#"{"rule":"name_key","bridge":"pyo3","pattern":"name","describe":"d","why_not_derivable":"generated_code"}"#,
        // registration call without argument selectors
        r#"{"rule":"registration_call","bridge":"napi","symbol":"REGISTER","describe":"d","why_not_derivable":"generated_code"}"#,
        // no reason
        r#"{"rule":"name_key","bridge":"pyo3","symbol":"name","describe":"d"}"#,
    ] {
        assert!(table(&format!(r#","syntax_conventions":[{bad}]"#)).is_err(), "{bad}");
    }
    // `rule` / `bridge` / `value` only in syntax_conventions.
    let misplaced = r#","io_send":[{"symbol":"a","rule":"name_key","describe":"d","why_not_derivable":"compiled_runtime"}]"#;
    assert!(table(misplaced).is_err());
}

#[test]
fn rule_spelling_matches_bare_names_and_last_segment_qualifiers() {
    let rows = r#","entries":[
        {"symbol":"builtins.map","kind":"function","basis":"no_source","describe":"d","why":"w","effects":[{"calls":{"pos":0}}]},
        {"symbol":"builtins.iter","kind":"function","arity":2,"basis":"no_source","describe":"d","why":"w","effects":[{"calls":{"pos":0}}]},
        {"symbol":"itertools.starmap","kind":"function","basis":"no_source","describe":"d","why":"w","effects":[{"calls":{"pos":0}}]},
        {"symbol":"builtins.str.join","kind":"method","basis":"no_source","describe":"d","why":"w","effects":[{"iterates":{"pos":0}}]}]"#;
    let mut tables = Tables::default();
    tables.by_file.insert("python".into(), table(rows).unwrap());
    let names = |q: Option<&str>, n: &str, k: u32| -> Vec<String> {
        tables
            .by_spelling(Language::Python, q, n, k)
            .iter()
            .map(|e| e.symbol.clone())
            .collect()
    };
    assert_eq!(names(None, "map", 2), vec!["builtins.map"]);
    assert_eq!(names(Some("builtins"), "map", 2), vec!["builtins.map"]);
    assert_eq!(names(Some("itertools"), "starmap", 2), vec!["itertools.starmap"]);
    assert_eq!(names(Some("it.itertools"), "starmap", 2), vec!["itertools.starmap"]);
    assert!(names(None, "starmap", 2).is_empty(), "not in the prelude");
    assert!(names(None, "iter", 1).is_empty(), "arity");
    assert_eq!(names(None, "iter", 2), vec!["builtins.iter"]);
    assert!(names(Some("sep"), "join", 1).is_empty(), "methods never match by spelling");
}

#[test]
fn rule_symbols_split_at_their_last_separator() {
    assert_eq!(split_symbol("builtins.map"), ("builtins", "map"));
    assert_eq!(split_symbol("std::sort"), ("std", "sort"));
    assert_eq!(split_symbol("Array#each"), ("Array", "each"));
    assert_eq!(split_symbol("encoding/json.Marshal"), ("encoding/json", "Marshal"));
    assert_eq!(split_symbol(":erlang.apply"), (":erlang", "apply"));
    assert_eq!(split_symbol(".Internal(lapply)"), ("", ".Internal(lapply)"));
    assert_eq!(split_symbol("trap"), ("", "trap"));
    assert_eq!(last_segment("encoding/json"), "json");
}
