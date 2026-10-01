//! Rules of the tiny native tables and the irreducible table (DESIGN §1.12, §4.12).

use std::path::{Path, PathBuf};
use std::process::Command;

use trace_core::Language;
use trace_library::table::{parse_table, Basis, Section, TableEntry, Tables, WHY_NOT_DERIVABLE};
use trace_library::{ArgSel, Effect};

fn tables() -> Tables {
    Tables::load_builtin().expect("embedded tables are valid")
}

/// The key (first served language) of every embedded table.
fn table_keys() -> Vec<&'static str> {
    trace_library::languages::ALL
        .iter()
        .map(|s| s.languages[0].as_str())
        .collect()
}

fn language(key: &str) -> Language {
    key.parse().expect("table key is a language")
}

fn effects_of(entries: &[&TableEntry]) -> Vec<Effect> {
    entries.iter().flat_map(|e| e.effects.iter().cloned()).collect()
}

#[test]
fn rule_every_embedded_table_parses() {
    let tables = tables();
    assert_eq!(table_keys().len(), 13);
    for key in table_keys() {
        let table = tables.table(language(key)).expect("table present");
        assert_eq!(table.language, key);
    }
    assert!(tables.table(Language::Tsx).is_some(), "TSX uses the javascript table");
    let gate = trace_library::gate::Gate::load_builtin().expect("gate parses");
    assert!((gate.threshold - 0.9).abs() < 1e-9);
}

/// Every native entry says why it cannot be derived (`why` + its `basis`) and every
/// irreducible row says it with `why_not_derivable` (one of the allowed reasons).
#[test]
fn rule_every_table_row_states_why_it_cannot_be_derived() {
    let tables = tables();
    let mut entries = 0;
    let mut rows = 0;
    for key in table_keys() {
        let l = language(key);
        for e in tables.entries(l) {
            entries += 1;
            assert!(!e.why.trim().is_empty(), "{key} {}: why", e.symbol);
            assert!(!e.describe.trim().is_empty(), "{key} {}: describe", e.symbol);
            assert!(Basis::ALL.contains(&e.basis), "{key} {}", e.symbol);
            assert!(!e.effects.is_empty(), "{key} {}: effects", e.symbol);
        }
        for section in Section::ALL {
            for row in tables.irreducible(l, section) {
                rows += 1;
                assert!(
                    WHY_NOT_DERIVABLE.contains(&row.why_not_derivable.as_str()),
                    "{key} {} {:?}: why_not_derivable {:?}",
                    section.key(),
                    row.anchor(),
                    row.why_not_derivable
                );
                assert!(
                    !row.describe.trim().is_empty(),
                    "{key} {} {:?}: describe",
                    section.key(),
                    row.anchor()
                );
            }
        }
    }
    assert!(entries > 100 && rows > 100, "entries {entries}, rows {rows}");
}

/// The engine built-ins that copy members or delegate member lookups (I-25): no readable
/// source and no type that says it, so they are table rows the value flow applies.
#[test]
fn rule_member_copy_rows_parse() {
    let tables = tables();
    for l in [Language::JavaScript, Language::TypeScript, Language::Tsx] {
        for symbol in ["ObjectConstructor.assign", "Object.assign"] {
            assert_eq!(
                effects_of(&tables.by_symbol(l, symbol)),
                vec![
                    Effect::CopiesMembers {
                        from: ArgSel::Rest(1),
                        to: ArgSel::Pos(0)
                    },
                    Effect::Returns(ArgSel::Pos(0))
                ],
                "{symbol}"
            );
        }
        for symbol in ["ObjectConstructor.create", "Object.create"] {
            assert_eq!(
                effects_of(&tables.by_symbol(l, symbol)),
                vec![Effect::DelegatesMembers {
                    object: ArgSel::Result,
                    to: ArgSel::Pos(0)
                }],
                "{symbol}"
            );
        }
        for symbol in ["ObjectConstructor.setPrototypeOf", "Object.setPrototypeOf"] {
            assert_eq!(
                effects_of(&tables.by_symbol(l, symbol)),
                vec![
                    Effect::DelegatesMembers {
                        object: ArgSel::Pos(0),
                        to: ArgSel::Pos(1)
                    },
                    Effect::Returns(ArgSel::Pos(0))
                ],
                "{symbol}"
            );
        }
        // Reflect.setPrototypeOf returns a boolean: the delegation only.
        assert_eq!(
            effects_of(&tables.by_symbol(l, "Reflect.setPrototypeOf")),
            vec![Effect::DelegatesMembers {
                object: ArgSel::Pos(0),
                to: ArgSel::Pos(1)
            }]
        );
    }
    // Member copy / delegation never runs a function: not a call effect.
    for e in tables.by_symbol(Language::JavaScript, "Object.assign") {
        assert!(e.effects.iter().all(|x| !x.runs_argument()), "{}", e.symbol);
        assert!(e.is_method(), "typed by the server, never matched by spelling");
    }
}

/// The tiny native tables hold the irreducible rows of every language that has them
/// (LANGUAGE-FIXES tiny-table column): the rows the language-specific tests below check
/// (Python C builtins and pytest injection, R `.Internal` / `.Primitive`, Bash `trap` /
/// `eval`, JavaScript `bind`).
#[test]
fn rule_tiny_tables_hold_each_languages_irreducible_rows() {
    let tables = tables();
    for (l, symbol) in [
        (Language::Python, "builtins.sorted"),
        (Language::Python, "_signal.signal"),
        (Language::Python, "atexit.register"),
        (Language::R, ".Primitive(\"on.exit\")"),
        (Language::JavaScript, "Function.bind"),
    ] {
        assert!(!tables.by_symbol(l, symbol).is_empty(), "{l:?} {symbol}");
    }
    // Languages whose runtime ships readable source or useful types keep no native rows.
    assert!(tables.entries(Language::Rust).is_empty());
}

/// Every `syntax_conventions` row names a rule trace implements, a bridge kind, and the
/// fields its rule needs; package-specific syntax conventions exist only as such rows.
#[test]
fn rule_syntax_convention_rows_name_a_known_rule() {
    let tables = tables();
    let mut rows = 0;
    for key in table_keys() {
        for row in tables.irreducible(language(key), Section::SyntaxConventions) {
            rows += 1;
            let rule = row.rule.as_deref().expect("rule");
            assert!(trace_library::table::syntax_rule_shape(rule).is_some(), "{key}: {rule}");
            let bridge = row.bridge.as_deref().expect("bridge");
            assert!(bridge.parse::<trace_core::model::BridgeKind>().is_ok(), "{key}: {bridge}");
        }
    }
    assert!(rows >= 50, "syntax convention rows: {rows}");
    let rust = tables.irreducible(Language::Rust, Section::SyntaxConventions);
    assert!(rust.iter().any(
        |r| r.rule.as_deref() == Some("export_function_attribute") && r.bridge.as_deref() == Some("pyo3")
    ));
    let go = tables.irreducible(Language::Go, Section::SyntaxConventions);
    assert!(go
        .iter()
        .any(|r| r.pattern.as_deref() == Some("Register<Service>Server")));
    // Rows of other sections carry no syntax rule.
    for key in table_keys() {
        for section in Section::ALL {
            if section == Section::SyntaxConventions {
                continue;
            }
            for row in tables.irreducible(language(key), section) {
                assert!(row.rule.is_none() && row.bridge.is_none(), "{key} {}", section.key());
            }
        }
    }
}

#[test]
fn rule_table_rows_require_why() {
    let head = r#""schema":1,"language":"bash","runtime":"Bash","roots":["eval"],"function_types":["function"],"top_types":["any"]"#;
    let no_why = format!(
        r#"{{{head},"entries":[{{"symbol":"eval","kind":"function","basis":"no_source","describe":"d","effects":[{{"calls":{{"pos":0}}}}]}}]}}"#
    );
    assert!(parse_table("bash", &no_why).is_err());
    let no_basis = format!(
        r#"{{{head},"entries":[{{"symbol":"eval","kind":"function","why":"C","describe":"d","effects":[{{"calls":{{"pos":0}}}}]}}]}}"#
    );
    assert!(parse_table("bash", &no_basis).is_err());
    let row_without_reason = format!(r#"{{{head},"io_send":[{{"symbol":"exec","describe":"d"}}]}}"#);
    assert!(parse_table("bash", &row_without_reason).is_err());
    let ok = format!(
        r#"{{{head},"entries":[{{"symbol":"eval","kind":"function","basis":"no_source","why":"C","describe":"d","effects":[{{"calls":{{"pos":0}}}}]}}]}}"#
    );
    assert!(parse_table("bash", &ok).is_ok());
}

#[test]
fn rule_irreducible_row_has_reason() {
    let tables = tables();
    let mut rows = 0;
    for key in table_keys() {
        for section in Section::ALL {
            for row in tables.irreducible(language(key), section) {
                rows += 1;
                assert!(
                    WHY_NOT_DERIVABLE.contains(&row.why_not_derivable.as_str()),
                    "{key} {} {:?}",
                    section.key(),
                    row.anchor()
                );
                assert!(!row.describe.trim().is_empty(), "{key} {:?}", row.anchor());
                assert!(!row.anchor().is_empty());
            }
        }
    }
    assert!(rows > 0);
}

#[test]
fn rule_every_selector_is_valid() {
    let tables = tables();
    for key in table_keys() {
        let l = language(key);
        for section in Section::ALL {
            for row in tables.irreducible(l, section) {
                assert!(row.key_sel().is_ok(), "{key} key {:?}", row.anchor());
                assert!(row.handler_sel().is_ok(), "{key} handler {:?}", row.anchor());
                assert!(row.verb_sel().is_ok(), "{key} verb {:?}", row.anchor());
            }
        }
        for e in tables.entries(l) {
            for effect in &e.effects {
                // Channel effects are derived, never table rows.
                assert!(
                    !matches!(
                        effect,
                        Effect::Sends { .. }
                            | Effect::Registers { .. }
                            | Effect::Mounts { .. }
                            | Effect::Decorates { .. }
                            | Effect::Exports { .. }
                    ),
                    "{key} {}",
                    e.symbol
                );
            }
        }
    }
}

#[test]
fn rule_function_and_top_types_present_for_every_language() {
    let tables = tables();
    for key in table_keys() {
        let l = language(key);
        assert!(!tables.function_types(l).is_empty(), "{key} function_types");
        assert!(!tables.top_types(l).is_empty(), "{key} top_types");
    }
    assert!(tables
        .function_types(Language::Python)
        .iter()
        .any(|t| t == "typing.Callable"));
    assert!(tables.top_types(Language::Go).iter().any(|t| t == "any"));
    assert!(tables
        .data_types(Language::CSharp)
        .iter()
        .any(|t| t == "System.Linq.Expressions.Expression"));
}

/// Well-known third-party packages that must never appear as native rows (their behaviour is
/// derived from their installed source).
const THIRD_PARTY: [&str; 16] = [
    "anyio",
    "starlette",
    "flask",
    "django",
    "fastapi",
    "trio",
    "express",
    "lodash",
    "react",
    "rails",
    "sinatra",
    "tokio",
    "serde",
    "gin",
    "spring",
    "phoenix",
];

#[test]
fn rule_tables_have_no_third_party_rows() {
    let tables = tables();
    for key in table_keys() {
        let table = tables.table(language(key)).expect("table");
        for e in &table.entries {
            for name in e.names() {
                let head = name
                    .trim_start_matches(':')
                    .split(['.', ':', '#', '/', '('])
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                assert!(!THIRD_PARTY.contains(&head.as_str()), "{key}: third-party row {name}");
            }
        }
        for root in &table.roots {
            assert!(
                !THIRD_PARTY.contains(&root.to_ascii_lowercase().as_str()),
                "{key}: third-party root {root}"
            );
        }
    }
}

#[test]
fn rule_python_table_keeps_only_native_facts() {
    let tables = tables();
    let entries = tables.entries(Language::Python);
    for e in entries {
        assert!(
            matches!(e.basis, Basis::NoSource | Basis::RuntimeDispatch | Basis::NeverCalls),
            "{}: {:?}",
            e.symbol,
            e.basis
        );
    }
    // Rows whose behaviour has Python source are derived, never listed.
    for derived in [
        "functools.partial",
        "functools.partialmethod",
        "functools.reduce",
        "functools.cache",
        "functools.lru_cache",
        "functools.singledispatch",
        "functools.cached_property",
        "threading.Thread",
        "threading.Timer",
        "asyncio.to_thread",
        "asyncio.BaseEventLoop.call_soon",
        "concurrent.futures.Executor.submit",
        "contextlib.contextmanager",
        "contextlib.ExitStack.callback",
        "signal.signal",
        "weakref.finalize",
        "collections.Counter",
        "collections.OrderedDict",
        "anyio.to_thread.run_sync",
        "starlette.concurrency.run_in_threadpool",
    ] {
        assert!(
            tables.by_symbol(Language::Python, derived).is_empty(),
            "{derived} has Python source and must be derived"
        );
    }
    let native = entries.iter().filter(|e| e.basis != Basis::NeverCalls).count();
    assert!(native <= 40, "native Python rows: {native}");
    let map = tables.by_symbol(Language::Python, "builtins.map");
    assert_eq!(effects_of(&map), vec![Effect::Calls(ArgSel::Pos(0)), Effect::Iterates(ArgSel::Rest(1))]);
    let signature = tables.by_symbol(Language::Python, "inspect.signature");
    assert!(signature.iter().all(|e| e.never_calls()) && !signature.is_empty());
}

#[test]
fn rule_pytest_injection_row_matches_the_inference_contract() {
    let tables = tables();
    let rows = tables.irreducible(Language::Python, Section::RuntimeDispatch);
    let row = rows
        .iter()
        .find(|r| r.pattern.as_deref() == Some("inject_by_parameter_name"))
        .expect("pytest injection row");
    assert_eq!(row.symbol.as_deref(), Some("pytest.fixture"));
    assert_eq!(row.glob.as_deref(), Some("conftest.py"));
    assert_eq!(row.activated_by.as_deref(), Some("pytest"));
    assert_eq!(row.why_not_derivable, "reflection");
    // `name=` of the provider decorator renames what it provides (flow `Injection::name_keyword`).
    assert_eq!(row.key_sel(), Ok(Some(trace_library::table::RowSel::Arg(ArgSel::Kw("name".into())))));
    assert!(!row.active(&|_| false));
}

#[test]
fn rule_bash_trap_string_is_code() {
    let tables = tables();
    let trap = tables.by_spelling(Language::Bash, None, "trap", 2);
    assert_eq!(effects_of(&trap), vec![Effect::StoredThenCalled(ArgSel::Code(0))]);
    let eval = tables.by_spelling(Language::Bash, None, "eval", 1);
    assert_eq!(effects_of(&eval), vec![Effect::Calls(ArgSel::Code(0))]);
    let complete = tables.by_spelling(Language::Bash, None, "complete", 3);
    assert_eq!(effects_of(&complete), vec![Effect::StoredThenCalled(ArgSel::Kw("-F".into()))]);
    // External programs never call shell functions.
    let xargs = tables.by_spelling(Language::Bash, None, "xargs", 1);
    assert!(xargs.iter().all(|e| e.never_calls()) && !xargs.is_empty());
    // Commands are never matched with a qualifier.
    assert!(tables.by_spelling(Language::Bash, Some("x"), "trap", 2).is_empty());
}

#[test]
fn rule_r_internal_lapply_leaf_calls_fun() {
    let tables = tables();
    let leaf = tables.by_symbol(Language::R, ".Internal(lapply)");
    assert_eq!(effects_of(&leaf), vec![Effect::Calls(ArgSel::Pos(1))]);
    let spelled = tables.by_spelling(Language::R, None, ".Internal(lapply)", 2);
    assert_eq!(effects_of(&spelled), vec![Effect::Calls(ArgSel::Pos(1))]);
    // R-level functions are R code: derived, never rows.
    for derived in ["lapply", "sapply", "Reduce", "Filter", "Map", "do.call", "tryCatch"] {
        assert!(tables.by_spelling(Language::R, None, derived, 2).is_empty(), "{derived}");
        assert!(tables.by_symbol(Language::R, derived).is_empty(), "{derived}");
    }
    for e in tables.entries(Language::R) {
        assert!(
            e.names().all(|n| n.starts_with(".Internal(")
                || n.starts_with(".Primitive(")
                || n.starts_with(".External")),
            "R rows are native leaves only: {}",
            e.symbol
        );
    }
}

#[test]
fn rule_cpp_sort_calls_the_comparator() {
    let tables = tables();
    let sort = tables.by_symbol(Language::Cpp, "std::sort");
    assert_eq!(effects_of(&sort), vec![Effect::Calls(ArgSel::Rest(0))]);
    let spelled = tables.by_spelling(Language::Cpp, Some("std"), "sort", 3);
    assert_eq!(effects_of(&spelled), vec![Effect::Calls(ArgSel::Rest(0))]);
    let thread = tables.by_symbol(Language::Cpp, "std::thread");
    assert_eq!(effects_of(&thread), vec![Effect::StoredThenCalled(ArgSel::Pos(0))]);
}

#[test]
fn rule_js_bind_returns_a_wrapper() {
    let tables = tables();
    for symbol in ["Function.bind", "CallableFunction.bind"] {
        for l in [Language::JavaScript, Language::TypeScript, Language::Tsx] {
            let bind = tables.by_symbol(l, symbol);
            assert_eq!(effects_of(&bind), vec![Effect::Partial(ArgSel::Receiver)], "{symbol}");
        }
    }
    let call = tables.by_symbol(Language::TypeScript, "Function.call");
    assert_eq!(effects_of(&call), vec![Effect::Calls(ArgSel::Receiver)]);
}

#[test]
fn rule_spelling_fallback_never_matches_methods() {
    let tables = tables();
    for key in table_keys() {
        let l = language(key);
        for e in tables.entries(l) {
            let (namespace, name) = trace_library::table::split_symbol(&e.symbol);
            let qualifier = (!namespace.is_empty()).then_some(namespace);
            for found in tables.by_spelling(l, qualifier, name, e.arity.unwrap_or(1)) {
                assert!(!found.is_method(), "{key}: method {} matched by spelling", found.symbol);
                assert_ne!(found.kind, "protocol");
            }
        }
    }
}

/// The Python interpreter's standard-library directory, when an interpreter is installed
/// (`TRACE_TEST_PYTHON` overrides the program).
fn python_stdlib() -> Option<PathBuf> {
    let programs: Vec<String> = match trace_core::env::test::python().and_then(|p| p.into_string().ok()) {
        Some(p) if !p.trim().is_empty() => vec![p],
        _ => vec!["python3".into(), "python".into()],
    };
    for program in programs {
        let Ok(out) = Command::new(&program)
            .args(["-c", "import sysconfig; print(sysconfig.get_paths()['stdlib'])"])
            .output()
        else {
            continue;
        };
        if !out.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let dir = PathBuf::from(text);
        if dir.join("os.py").is_file() {
            return Some(dir);
        }
    }
    None
}

/// Whether `symbol` (module.qualified.name) is declared in readable Python source under
/// `stdlib`: the longest module prefix with a `.py` file (or package `__init__.py`) must not
/// declare the rest of the symbol.
fn python_source_declares(stdlib: &Path, symbol: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = symbol.split('.').collect();
    for i in (1..parts.len()).rev() {
        let module = parts[..i].join("/");
        let candidates = [stdlib.join(format!("{module}.py")), stdlib.join(&module).join("__init__.py")];
        let Some(file) = candidates.into_iter().find(|c| c.is_file()) else {
            continue;
        };
        let rest = parts[i..].join(".");
        let source = std::fs::read(&file).ok()?;
        let facts = trace_syntax::extract(trace_syntax::SourceInput {
            path: "lib.py",
            language: Language::Python,
            source: &source,
        })
        .ok()?;
        return facts
            .declarations
            .iter()
            .any(|d| d.qualified_name == rest)
            .then_some(file);
    }
    None
}

#[test]
fn rule_table_row_has_no_readable_source() {
    let tables = tables();
    let Some(stdlib) = python_stdlib() else {
        eprintln!("skipped: no Python interpreter installed");
        return;
    };
    let mut checked = 0;
    for e in tables.entries(Language::Python) {
        if e.basis != Basis::NoSource {
            continue;
        }
        for name in e.names() {
            checked += 1;
            if let Some(file) = python_source_declares(&stdlib, name) {
                panic!("{name} has readable source in {}: derive it instead", file.display());
            }
        }
    }
    for section in Section::ALL {
        for row in tables.irreducible(Language::Python, section) {
            let Some(symbol) = row.symbol.as_deref() else { continue };
            if !matches!(row.why_not_derivable.as_str(), "compiled_runtime" | "no_source") {
                continue;
            }
            checked += 1;
            if let Some(file) = python_source_declares(&stdlib, symbol) {
                panic!("{symbol} has readable source in {}: derive it instead", file.display());
            }
        }
    }
    assert!(checked > 20);
}
