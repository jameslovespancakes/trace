use super::*;
use crate::{extract, SourceInput};

/// The language the extractor gives a file classified as C (a `.h` header may become C++).
fn header_language(path: &str, source: &[u8]) -> Language {
    let facts = extract(SourceInput {
        path,
        language: Language::C,
        source,
    })
    .expect("extracts");
    facts.language.expect("language")
}

#[test]
fn rule_c_headers_with_cpp_syntax_are_cpp() {
    assert_eq!(header_language("a.h", b"int add(int a, int b);\n"), Language::C);
    assert_eq!(
        header_language(
            "b.h",
            b"namespace ns { template <typename T> auto get(T& t) -> T& { return t; } }\n"
        ),
        Language::Cpp
    );
    assert_eq!(header_language("c.c", b"namespace ns {}\n"), Language::C, "only headers");
    let map = repo_header_languages(&[("a.h", Language::C)], &[]);
    assert_eq!(map.get("a.h"), Some(&Language::C));
}

/// Rule: a header testing whether `__cplusplus` is defined is written for C and C++: C per
/// file whatever its C++-only sections contain; a version comparison is no such test.
#[test]
fn rule_header_testing_cplusplus_is_c() {
    let dual = b"#ifdef __cplusplus\nextern \"C\" {\n#endif\n#ifdef __cplusplus\nclass _obj {};\nclass _arr : public _obj {};\n#else\nstruct _obj;\n#endif\nint on_load(void *vm);\n#ifdef __cplusplus\n}\n#endif\n";
    assert_eq!(header_language("dual.h", dual), Language::C);
    let defined = b"#if defined(__cplusplus)\nclass _obj {};\nclass _arr : public _obj {};\n#endif\nint on_load(void *vm);\n";
    assert_eq!(header_language("defined.h", defined), Language::C);
    let versioned = b"#if __cplusplus >= 201703L\nnamespace ns { template <typename T> auto get(T& t) -> T& { return t; } }\n#endif\n";
    assert_eq!(header_language("versioned.h", versioned), Language::Cpp);
}

#[test]
fn rule_header_included_by_cpp_is_cpp() {
    let headers = [
        ("include/lib/lib.h", Language::C),
        ("include/lib/inner.h", Language::C),
        ("include/shared.h", Language::C),
        ("include/cppish.h", Language::Cpp),
        ("include/unused.h", Language::C),
    ];
    let v = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let includers = vec![
        ("src/a.cpp", Language::Cpp, v(&["\"lib/lib.h\"", "\"shared.h\""])),
        ("src/b.c", Language::C, v(&["\"shared.h\"", "\"cppish.h\"", "<stdio.h>"])),
        ("include/lib/lib.h", Language::C, v(&["\"inner.h\""])),
    ];
    let map = repo_header_languages(&headers, &includers);
    assert_eq!(map["include/lib/lib.h"], Language::Cpp, "included by C++ only");
    assert_eq!(map["include/lib/inner.h"], Language::Cpp, "included by a C++ header only");
    assert_eq!(map["include/shared.h"], Language::C, "a C file includes it too");
    assert_eq!(map["include/cppish.h"], Language::Cpp, "C++ syntax stays C++");
    assert_eq!(map["include/unused.h"], Language::C, "no includers: per-file rule");
}
