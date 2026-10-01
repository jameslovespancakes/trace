//! Tests for rule "provider of an installed plugin" ([`Injection::installed`]): a parameter
//! no repository provider serves holds a library object of the installed provider's value
//! class, so member calls on it run library code (`Flow::library_receivers`).

use super::*;
use crate::test_support::{Decl, Fixture};

const PROVIDER: &str = "runner.fixture";
const SHARED: &str = "shared_fixtures.py";

/// The active injection rule with installed providers `installed` (name -> value class).
fn rule(installed: &[(&str, &str)]) -> Injection {
    let row = trace_library::table::IrreducibleRow {
        symbol: Some(PROVIDER.into()),
        pattern: Some(INJECT_BY_PARAMETER_NAME.into()),
        glob: Some(SHARED.into()),
        activated_by: Some("runner".into()),
        why_not_derivable: "reflection".into(),
        describe: "the runner injects provider values into parameters of the same name".into(),
        ..Default::default()
    };
    let mut rules = injections([(Language::Python, &row)], &|_, package| package == "runner");
    assert_eq!(rules.len(), 1);
    let mut rule = rules.remove(0);
    rule.installed = installed
        .iter()
        .map(|(n, c)| (n.to_string(), c.to_string()))
        .collect();
    rule
}

/// `def test_x(patcher): patcher.setenv(..)`, `patcher` served only by an installed
/// provider: the call runs the library class's member (evidence: the value class). A
/// repository provider of the same name visible to the test wins; a parameter no provider
/// serves stays unknown.
#[test]
fn rule_installed_provider_value_is_a_library_object() {
    let mut fx = Fixture::new();
    let lib = fx.file("src/app.py", Language::Python, None);
    let shared = fx.file("tests/own/shared_fixtures.py", Language::Python, None);
    let plain = fx.file("tests/test_env.py", Language::Python, None);
    let own = fx.file("tests/own/test_own.py", Language::Python, None);
    let class = fx.decl(lib, Decl::class("Patcher"));
    let setenv = fx.decl(lib, Decl::method("setenv", class).params(&["self"]));
    fx.receiver(lib, setenv, "self", class, false);
    // tests/own/shared_fixtures.py: @runner.fixture def patcher(): return Patcher()
    let fixture = fx.decl(shared, Decl::function("patcher").decorators(&[PROVIDER]));
    let k = fx.name_ref(shared, "Patcher", class);
    let made = fx.call(k, vec![]);
    fx.ret(shared, fixture, made);
    let mut ats = Vec::new();
    for (file, name, param) in [
        (plain, "test_env", "patcher"),
        (own, "test_own", "patcher"),
        (plain, "test_other", "unknown"),
    ] {
        let t = fx.decl(file, Decl::function(name).params(&[param]).test());
        let recv = fx.name(param);
        let func = fx.attr(recv, "setenv");
        let call = fx.call(func, vec![]);
        ats.push(fx.eval(file, t, call, &format!("{param}.setenv")));
    }
    let index = fx.build();
    let h = Hierarchy::build(&index);
    let rules = [rule(&[("patcher", "lib.patching.Patcher")])];
    let flow = Flow::solve_rules(&index, &h, &LibraryKnowledge::default(), &rules);
    let receivers = flow.library_receivers();
    let found: Vec<(ByteSpan, &str)> = receivers.iter().map(|r| (r.at.bytes, r.library.as_str())).collect();
    assert_eq!(found, vec![(ats[0], "lib.patching.Patcher")], "{receivers:?}");
    let own_row: Vec<_> = flow.candidates().into_iter().filter(|c| c.span == ats[1]).collect();
    assert_eq!(own_row.len(), 1, "the repository provider wins: {own_row:?}");
    assert_eq!(own_row[0].candidates, vec![fx.id(setenv)]);
    // Without installed providers nothing is known about `patcher` in tests/test_env.py.
    let flow = Flow::solve_rules(&index, &h, &LibraryKnowledge::default(), &[rule(&[])]);
    assert!(flow.library_receivers().is_empty());
}

/// Installed providers enter the rule only for names exactly one installed provider serves
/// with a known value class.
#[test]
fn rule_installed_provider_needs_one_provider_with_a_known_value() {
    use trace_library::injected::InstalledProvider;
    let mut knowledge = LibraryKnowledge::default();
    let provider = |symbol: &str, value: Option<&str>| InstalledProvider {
        symbol: symbol.into(),
        value: value.map(str::to_string),
    };
    knowledge.providers.insert(
        PROVIDER.into(),
        [
            ("patcher".to_string(), vec![provider("lib.patcher", Some("lib.Patcher"))]),
            ("config".to_string(), vec![provider("lib.config", None)]),
            ("twice".to_string(), vec![provider("a.twice", Some("a.T")), provider("b.twice", Some("b.T"))]),
        ]
        .into_iter()
        .collect(),
    );
    let values = installed_values(&rule(&[]), &knowledge);
    assert_eq!(
        values.into_iter().collect::<Vec<_>>(),
        vec![("patcher".to_string(), "lib.Patcher".to_string())]
    );
}
