use crate::{extract, SourceInput};
use trace_core::facts::{FileFacts, ImportKind, Scope};
use trace_core::Language;

fn facts(path: &str, language: Language, src: &str) -> FileFacts {
    extract(SourceInput {
        path,
        language,
        source: src.as_bytes(),
    })
    .expect("extract")
}

fn path_of(f: &FileFacts, callee: &str) -> Option<String> {
    let i = f.calls.iter().position(|c| c.callee == callee).expect("call");
    f.call_detail(i).and_then(|d| d.callee_path.clone())
}

#[test]
fn python_imports_and_aliases() {
    let src = "import os.path\nimport numpy as np\nfrom functools import partial as p, wraps\nfrom . import sibling\nfrom ..pkg.mod import thing\nfrom itertools import *\n\ndef f():\n    from asyncio import to_thread\n    return to_thread\n";
    let f = facts("m.py", Language::Python, src);
    let got: Vec<(&str, &str, ImportKind)> = f
        .imports
        .iter()
        .map(|i| (i.local.as_str(), i.target.as_str(), i.kind))
        .collect();
    assert_eq!(
        got,
        vec![
            ("os", "os", ImportKind::Module),
            ("np", "numpy", ImportKind::Module),
            ("p", "functools.partial", ImportKind::Member),
            ("wraps", "functools.wraps", ImportKind::Member),
            ("sibling", ".sibling", ImportKind::Member),
            ("thing", "..pkg.mod.thing", ImportKind::Member),
            ("*", "itertools", ImportKind::Wildcard),
            ("to_thread", "asyncio.to_thread", ImportKind::Member),
        ]
    );
    assert_eq!(f.imports[0].scope, Scope::Module);
    assert_eq!(f.imports[7].scope, Scope::Decl(0));
    assert_eq!(f.imports[2].line, 3);
}

#[test]
fn callee_paths_follow_python_scoping() {
    // more-itertools 04-intersperse: `iter(partial(take, n, it), [])`.
    let src = "from functools import partial\nimport itertools as it2\nimport asyncio\n\n\
                   def take(n, iterable):\n    return list(it2.islice(iterable, n))\n\n\
                   def chunked(iterable, n):\n    return iter(partial(take, n, iter(iterable)), [])\n\n\
                   def shadow(iter):\n    return iter(1)\n\n\
                   class K:\n    partial = 1\n    x = partial(2)\n    def m(self):\n        return partial(3)\n\n\
                   async def run(fn):\n    await asyncio.to_thread(fn)\n    asyncio.get_running_loop().run_in_executor(None, fn)\n";
    let f = facts("m.py", Language::Python, src);
    assert_eq!(path_of(&f, "partial").as_deref(), Some("functools.partial"));
    assert_eq!(path_of(&f, "it2.islice").as_deref(), Some("itertools.islice"));
    assert_eq!(path_of(&f, "list").as_deref(), Some("builtins.list"));
    assert_eq!(path_of(&f, "asyncio.to_thread").as_deref(), Some("asyncio.to_thread"));
    assert_eq!(path_of(&f, "asyncio.get_running_loop").as_deref(), Some("asyncio.get_running_loop"));
    assert_eq!(path_of(&f, "asyncio.get_running_loop().run_in_executor"), None);
    // Both `iter` calls in `chunked` are the builtin; the parameter shadows it in `shadow`.
    let iters: Vec<Option<String>> = f
        .calls
        .iter()
        .enumerate()
        .filter(|(_, c)| c.callee == "iter")
        .map(|(i, _)| f.call_detail(i).unwrap().callee_path.clone())
        .collect();
    assert_eq!(iters, vec![Some("builtins.iter".to_string()), Some("builtins.iter".to_string()), None]);
    // Class scope: visible directly in the class body, not inside its methods.
    let partials: Vec<Option<String>> = f
        .calls
        .iter()
        .enumerate()
        .filter(|(_, c)| c.callee == "partial")
        .map(|(i, _)| f.call_detail(i).unwrap().callee_path.clone())
        .collect();
    assert_eq!(
        partials,
        vec![
            Some("functools.partial".to_string()),
            None,
            Some("functools.partial".to_string())
        ]
    );
}

/// Bare names bound only as parameters/variables of their function are local variables
/// (no definition request can reach a declaration through them).
#[test]
fn references_to_local_variables_are_marked() {
    let src = "from functools import partial\n\ndef helper():\n    pass\n\n\
                   def run(fn, items):\n    alias = helper\n    total = 0\n    def inner():\n        return fn, total\n    for x in items:\n        use(x, fn, alias, helper, partial, inner)\n    return lambda: fn\n\n\
                   def outer():\n    global state\n    state = helper\n    return state\n\n\
                   def shadow(helper):\n    helper = helper or 1\n    return helper\n\n\
                   top = helper\nuse(top)\n";
    let f = facts("m.py", Language::Python, src);
    // Store targets are `write` references since extractor 5; the reads keep their order.
    let local = |name: &str| -> Vec<bool> {
        f.references
            .iter()
            .filter(|r| r.name == name && r.kind != trace_core::facts::RefKind::Write)
            .map(|r| r.local)
            .collect()
    };
    // Parameters and variables (also read from nested scopes) are local variables.
    assert!(local("fn").iter().all(|&l| l), "{:?}", local("fn"));
    assert!(local("x").iter().all(|&l| l));
    assert!(local("alias").iter().all(|&l| l));
    assert!(local("total").iter().all(|&l| l));
    // Declarations, imports, nested `def`s, `global` names and module variables are not.
    // `helper`: two reads in `run`, one in `outer`, two of the parameter in `shadow`,
    // one at module level.
    assert_eq!(local("helper"), vec![false, false, false, true, true, false]);
    assert!(local("partial").iter().all(|&l| !l));
    assert!(local("inner").iter().all(|&l| !l));
    assert!(local("state").iter().all(|&l| !l));
    assert!(local("top").iter().all(|&l| !l));
}

#[test]
fn wildcard_imports_make_unbound_names_unknown() {
    let src = "from itertools import *\nislice([], 1)\nlen([])\n";
    let f = facts("m.py", Language::Python, src);
    assert_eq!(path_of(&f, "islice"), None);
    assert_eq!(path_of(&f, "len"), None);
}

#[test]
fn qualified_names_for_decorators_arguments_and_values() {
    let src = "import functools\nfrom contextlib import contextmanager\nimport threading\n\n\
                   @contextmanager\ndef cm():\n    yield\n\n\
                   @functools.cached_property\ndef prop(self):\n    return 1\n\n\
                   def work():\n    pass\n\n\
                   def start():\n    t = threading.Thread(target=work, name=str)\n    runner = functools.partial\n    return t, runner\n";
    let f = facts("m.py", Language::Python, src);
    let paths: Vec<&str> = f.qualified_names.iter().map(|q| q.path.as_str()).collect();
    assert!(paths.contains(&"contextlib.contextmanager"), "{paths:?}");
    assert!(paths.contains(&"functools.cached_property"), "{paths:?}");
    assert!(paths.contains(&"builtins.str"), "{paths:?}");
    assert!(paths.contains(&"functools.partial"), "{paths:?}");
    let cm = f
        .qualified_names
        .iter()
        .find(|q| q.path == "contextlib.contextmanager")
        .unwrap();
    assert_eq!(&src[cm.span.range()], "contextmanager");
    assert_eq!(f.qualified_name_at(cm.span), Some("contextlib.contextmanager"));
    // `work` is a local function: no qualified path.
    assert!(!paths.iter().any(|p| p.ends_with("work")));
    assert_eq!(path_of(&f, "threading.Thread").as_deref(), Some("threading.Thread"));
}

#[test]
fn javascript_imports() {
    let src = "import fs, { readFile as rf, stat } from 'node:fs';\nimport * as path from \"path\";\n\
                   rf('x', cb);\nsetTimeout(tick, 5);\npath.join('a');\n";
    let f = facts("a.js", Language::JavaScript, src);
    let got: Vec<(&str, &str)> = f
        .imports
        .iter()
        .map(|i| (i.local.as_str(), i.target.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("fs", "node:fs.default"),
            ("rf", "node:fs.readFile"),
            ("stat", "node:fs.stat"),
            ("path", "path"),
        ]
    );
    assert_eq!(path_of(&f, "rf").as_deref(), Some("node:fs.readFile"));
    assert_eq!(path_of(&f, "setTimeout").as_deref(), Some("globalThis.setTimeout"));
    assert_eq!(path_of(&f, "path.join").as_deref(), Some("path.join"));
}

/// rule-test-code: inline unit tests next to product code (Rust `#[cfg(test)] mod tests`)
/// do not make the file test code; a file of tests only is test code.
#[test]
fn rule_test_code_needs_every_callable_to_be_a_test() {
    let mixed = "pub fn add(a: i32, b: i32) -> i32 { a + b }\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn adds() { assert_eq!(add(1, 2), 3); }\n}\n";
    assert!(!facts("src/lib.rs", Language::Rust, mixed).is_test_code());
    let only =
        "#[test]\nfn adds() { assert_eq!(1 + 2, 3); }\n\n#[test]\nfn subtracts() { assert_eq!(3 - 2, 1); }\n";
    assert!(facts("tests/math.rs", Language::Rust, only).is_test_code());
    let plain = "pub fn add(a: i32, b: i32) -> i32 { a + b }\n";
    assert!(!facts("src/lib.rs", Language::Rust, plain).is_test_code());
}

#[test]
fn bash_source_keeps_the_literal_tail_of_a_variable_path() {
    let src = "#!/bin/bash\n. ./lib.sh\n\\. \"$NVM_DIR/nvm.sh\" --no-use\nsource \"${ROOT}/scripts/util.sh\"\nsource \"$ONLY_VAR\"\n";
    let f = facts("update_test_mocks.sh", Language::Bash, src);
    let targets: Vec<&str> = f.imports.iter().map(|i| i.target.as_str()).collect();
    assert_eq!(targets, vec!["./lib.sh", "nvm.sh", "scripts/util.sh"]);
    assert!(f.imports.iter().all(|i| i.kind == ImportKind::Wildcard));
}

/// A module-level CommonJS `module.exports = require('<spec>')` (also inside a chain
/// `exports = module.exports = require(..)`) is a whole-module re-export (`*`, spanning
/// the loader call); a member store, a non-literal argument or the same statement inside
/// a function is not.
#[test]
fn rule_commonjs_whole_module_reexport() {
    let src = "module.exports = require('./lib/app');
exports = module.exports = require('../core');
exports.x = require('./y');
module.exports = require(name);
function f() { module.exports = require('./z'); }
";
    let f = facts("index.js", Language::JavaScript, src);
    let got: Vec<(&str, &str)> = f
        .exports
        .iter()
        .map(|e| (e.exported.as_str(), e.target.as_str()))
        .collect();
    assert_eq!(got, vec![("*", "./lib/app"), ("*", "../core")]);
    let first = &f.exports[0];
    assert_eq!(&src[first.span.start as usize..first.span.end as usize], "require('./lib/app')");
    // Other languages keep assignments out of the re-exports.
    let ts = facts(
        "index.ts",
        Language::TypeScript,
        "module.exports = require('./lib/app');
",
    );
    assert!(ts.exports.is_empty());
}
