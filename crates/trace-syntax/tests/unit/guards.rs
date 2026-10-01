use trace_core::facts::{Expr, FileFacts};
use trace_core::Language;

use crate::{extract, SourceInput};

fn facts(src: &str) -> FileFacts {
    extract(SourceInput {
        path: "m.py",
        language: Language::Python,
        source: src.as_bytes(),
    })
    .expect("extract")
}

fn guards(f: &FileFacts, line: u32) -> Vec<String> {
    let i = f
        .calls
        .iter()
        .position(|c| c.line == line && c.callee == "visit")
        .expect("call");
    let mut out: Vec<String> = f.call_details[i]
        .not_identical
        .iter()
        .map(|e| match e {
            Expr::Name { name, .. } => name.clone(),
            other => format!("{other:?}"),
        })
        .collect();
    out.sort();
    out
}

/// boltons 07-remap: the default callback is skipped by an identity test.
#[test]
fn else_branch_of_an_identity_test_excludes_the_object() {
    let src = "\
def default_visit(p):
    return p
_orig = default_visit

def remap(root, visit=default_visit):
    for x in root:
        if visit is _orig:
            y = x
        else:
            try:
                y = visit(x)
            except Exception:
                y = x
    if visit is not _orig:
        visit(root)
    if visit is _orig:
        visit(root)
    return visit(root) if not (visit is _orig) else None
";
    let f = facts(src);
    assert_eq!(guards(&f, 11), vec!["_orig"]);
    assert_eq!(guards(&f, 15), vec!["_orig"]);
    assert!(guards(&f, 17).is_empty());
    assert_eq!(guards(&f, 18), vec!["_orig"]);
}

#[test]
fn early_exits_elif_chains_and_boolean_operators() {
    let src = "\
def run(visit, a, b):
    if visit is a:
        return None
    visit(1)
    if visit is b:
        pass
    elif a:
        visit(2)
    ok = visit is b or visit(3)
    both = visit is not a and visit is not b and visit(4)
";
    let f = facts(src);
    assert_eq!(guards(&f, 4), vec!["a"]);
    assert_eq!(guards(&f, 8), vec!["a", "b"]);
    assert_eq!(guards(&f, 9), vec!["a", "b"]);
    assert_eq!(guards(&f, 10), vec!["a", "a", "b"]);
}

#[test]
fn rebound_names_are_never_narrowed() {
    let src = "\
def run(visit, a):
    if visit is a:
        visit = a
    else:
        visit(1)
";
    let f = facts(src);
    assert!(guards(&f, 5).is_empty());
}
