use super::*;

#[test]
fn extensions_map_case_insensitively() {
    assert_eq!(from_extension("PY"), Some(Language::Python));
    assert_eq!(from_extension("pyi"), Some(Language::Python));
    assert_eq!(from_extension("tsx"), Some(Language::Tsx));
    assert_eq!(from_extension("R"), Some(Language::R));
    assert_eq!(from_extension("txt"), None);
    assert_eq!(from_path(Path::new("src/lib.rs")), Some(Language::Rust));
    assert_eq!(from_path(Path::new("Makefile")), None);
    assert_eq!(from_path(Path::new("api/openapi.yaml")), Some(Language::OpenApi));
    assert_eq!(from_path(Path::new("spec/pets.swagger.json")), Some(Language::OpenApi));
    assert_eq!(from_path(Path::new("package.json")), None);
    assert_eq!(from_path(Path::new("proto/greeter.proto")), Some(Language::Proto));
}

#[test]
fn names_round_trip() {
    for lang in Language::ALL {
        assert_eq!(info(lang).language, lang);
        assert_eq!(lang.as_str().parse::<Language>(), Ok(lang));
        assert_eq!(serde_json::to_string(&lang).unwrap(), format!("\"{}\"", lang.as_str()));
    }
}

#[test]
fn rule_install_ids_and_aliases_parse() {
    assert_eq!(Language::Tsx.install_id(), "typescript");
    assert_eq!(Language::Tsx.display_name(), "TypeScript");
    assert_eq!(Language::Cpp.display_name(), "C++");
    assert_eq!(Language::from_install_arg("C#"), Some(Language::CSharp));
    assert_eq!(Language::from_install_arg("golang"), Some(Language::Go));
    assert_eq!(Language::from_install_arg("typescript"), Some(Language::TypeScript));
    assert_eq!(Language::from_install_arg("tsx"), None);
    assert_eq!(Language::from_install_arg("julia"), None);
    assert_eq!(Language::from_install_arg("cobol"), None);
    assert_eq!(Language::ALL.iter().filter(|l| l.is_code()).count(), 15);
}

#[test]
fn rule_default_languages_are_the_plan_set() {
    for l in DEFAULT_LANGUAGES {
        assert!(l.is_default() && l.is_code());
    }
    assert!(Language::Tsx.is_default());
    assert!(!Language::Scala.is_default());
    assert!(!Language::Php.is_default());
    assert_eq!(Language::ALL.iter().filter(|l| l.is_default()).count(), DEFAULT_LANGUAGES.len() + 1);
}

#[test]
fn rule_family_is_a_shared_call_namespace() {
    assert!(same_family(Language::TypeScript, Language::JavaScript));
    assert!(same_family(Language::Cpp, Language::C));
    assert!(same_family(Language::Scala, Language::Java));
    assert!(same_family(Language::Python, Language::Python));
    assert!(!same_family(Language::Python, Language::Rust));
    assert!(!same_family(Language::C, Language::Rust));
    assert!(!same_family(Language::Java, Language::CSharp));
}

#[test]
fn rule_lsp_language_id_is_the_name_unless_the_protocol_names_it_otherwise() {
    let ids: Vec<_> = Language::ALL
        .iter()
        .filter(|&&l| info(l).lsp_id != l.as_str())
        .map(|&l| (l, info(l).lsp_id))
        .collect();
    assert_eq!(
        ids,
        [
            (Language::Tsx, "typescriptreact"),
            (Language::Bash, "shellscript"),
            (Language::VisualBasic, "vb")
        ]
    );
}

#[test]
fn stub_paths_are_byte_safe() {
    assert!(Language::is_python_stub_path("pkg/mod.pyi"));
    assert!(Language::is_python_stub_path("pkg/MOD.PYI"));
    assert!(!Language::is_python_stub_path("pkg/mod.py"));
    assert!(!Language::is_python_stub_path(".pyi"));
    // Multi-byte characters near the end must not panic.
    assert!(!Language::is_python_stub_path("ééé"));
    assert!(Language::is_python_stub_path("é/é.pyi"));
}

#[test]
fn rule_non_shebang_lines_have_no_language() {
    assert_eq!(from_shebang(b"all: build"), None);
    assert_eq!(from_shebang(b""), None);
    assert_eq!(from_shebang(b"# bash"), None);
    assert_eq!(from_shebang(b"#!"), None);
}

#[test]
fn rule_extensionless_shell_script_is_bash() {
    for line in [
        &b"#!/bin/sh"[..],
        b"#!/bin/bash -e",
        b"#! /usr/bin/env bash",
        b"#!/usr/bin/env -S bash -euo pipefail",
        b"#!/usr/bin/env -i PATH=/bin dash",
        b"#!/bin/ksh93",
        b"#!/usr/bin/zsh\r",
    ] {
        assert_eq!(from_shebang(line), Some(Language::Bash), "{}", String::from_utf8_lossy(line));
    }
}

#[test]
fn rule_shebang_selects_the_interpreter_language() {
    assert_eq!(from_shebang(b"#!/usr/bin/python3.12"), Some(Language::Python));
    assert_eq!(from_shebang(b"#!/usr/bin/env python3 -u"), Some(Language::Python));
    assert_eq!(from_shebang(b"#!/usr/bin/env node"), Some(Language::JavaScript));
    assert_eq!(from_shebang(b"#!/usr/bin/env -u HOME node"), Some(Language::JavaScript));
    assert_eq!(from_shebang(b"#!C:\\Tools\\node.exe"), Some(Language::JavaScript));
}

#[test]
fn rule_unknown_interpreters_are_not_code() {
    assert_eq!(from_shebang(b"#!/usr/bin/env perl"), None);
    assert_eq!(from_shebang(b"#!/usr/bin/make -f"), None);
    assert_eq!(from_shebang(b"#!/usr/bin/env"), None);
    assert_eq!(from_shebang(b"#!/usr/bin/awk -f"), None);
}
