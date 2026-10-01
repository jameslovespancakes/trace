//! Rule tests of [`crate::engine::rules::scala_apply`] over real syntax facts.

use trace_core::facts::FileFacts;

use super::*;

const SRC: &str = "package p\n\
\n\
trait Codec[A] { self =>\n\
\x20 def apply(c: Int): A\n\
\x20 def run(c: Int): A = self(c)\n\
}\n\
\n\
object Codec {\n\
\x20 val int: Codec[Int] = null\n\
\x20 def apply[A](implicit c: Codec[A]): Codec[A] = c\n\
}\n\
\n\
sealed abstract class Failure\n\
\n\
object Failure {\n\
\x20 def apply(message: String): Failure = null\n\
}\n\
\n\
case class Wub(x: Long)\n\
\n\
object Wub {\n\
\x20 def zero: Wub = Wub(0L)\n\
}\n\
\n\
class Other\n\
\n\
case class Ext(x: Int)\n\
\n\
object Ext extends Other\n\
\n\
object Use {\n\
\x20 def a = Failure(\"x\")\n\
\x20 def b = Codec.int(1)\n\
\x20 def c = other(1)\n\
}\n";

fn facts() -> FileFacts {
    trace_syntax::extract(trace_syntax::SourceInput {
        path: "p/A.scala",
        language: Language::Scala,
        source: SRC.as_bytes(),
    })
    .expect("fixture parses")
}

/// The declarations with qualified name `q`, in source order.
fn named<'a>(decls: &DeclTable<'a>, f: &FileFacts, q: &str) -> Vec<DeclRef<'a>> {
    let path = decls.path_key("p/A.scala").unwrap();
    f.declarations
        .iter()
        .enumerate()
        .filter(|(_, d)| d.qualified_name == q)
        .map(|(i, _)| DeclRef { path, decl: i as u32 })
        .collect()
}

fn call<'f>(f: &'f FileFacts, callee: &str) -> &'f CallSite {
    f.calls
        .iter()
        .find(|c| c.callee == callee)
        .unwrap_or_else(|| panic!("no call {callee}: {:?}", f.calls))
}

/// `apply` sugar: `Failure("x")` answered by the companion object and its `apply` runs the
/// `apply`; `Codec.int(1)` (the qualifier object and the trait's `apply`) and `self(c)` (the
/// enclosing trait and its `apply`) too.
#[test]
fn rule_scala_application_runs_the_apply_method() {
    let f = facts();
    let decls = DeclTable::new([("p/A.scala", SRC.as_bytes(), &f)]);
    let failure = named(&decls, &f, "Failure");
    let failure_apply = named(&decls, &f, "Failure.apply");
    assert_eq!((failure.len(), failure_apply.len()), (2, 1), "the class and its companion object");
    let targets = vec![failure[1], failure_apply[0]];
    let got = narrow_application(targets, call(&f, "Failure"), Some(Language::Scala), &decls);
    assert_eq!(got, failure_apply);

    let codec = named(&decls, &f, "Codec");
    let trait_apply = named(&decls, &f, "Codec.apply");
    assert_eq!(codec.len(), 2, "the trait and its companion object");
    // Codec.int(1): the companion object (named by the receiver) + the trait's apply.
    let got = narrow_application(
        vec![codec[1], trait_apply[0]],
        call(&f, "Codec.int"),
        Some(Language::Scala),
        &decls,
    );
    assert_eq!(got, vec![trait_apply[0]]);
    // self(c): the enclosing trait + its apply.
    let got =
        narrow_application(vec![codec[0], trait_apply[0]], call(&f, "self"), Some(Language::Scala), &decls);
    assert_eq!(got, vec![trait_apply[0]]);
}

/// Case class application: the class and its companion object construct the class.
#[test]
fn rule_scala_case_class_application_constructs_the_class() {
    let f = facts();
    let decls = DeclTable::new([("p/A.scala", SRC.as_bytes(), &f)]);
    let wub = named(&decls, &f, "Wub");
    assert_eq!(wub.len(), 2, "{:?}", f.declarations);
    let got = narrow_application(vec![wub[1], wub[0]], call(&f, "Wub"), Some(Language::Scala), &decls);
    assert_eq!(got, vec![wub[0]], "the case class, never the object");
}

/// Negatives: an unrelated type next to an `apply` (not named by the callee, not its
/// enclosing type), a non-`apply` method, two unrelated classes, a class whose companion
/// declares or inherits an `apply` and other languages keep every target.
#[test]
fn rule_scala_application_leaves_unexplained_answers_ambiguous() {
    let f = facts();
    let decls = DeclTable::new([("p/A.scala", SRC.as_bytes(), &f)]);
    let other = named(&decls, &f, "Other");
    let failure = named(&decls, &f, "Failure");
    let failure_apply = named(&decls, &f, "Failure.apply");
    let zero = named(&decls, &f, "Wub.zero");
    let site = call(&f, "other");
    let unrelated = vec![other[0], failure_apply[0]];
    assert_eq!(narrow_application(unrelated.clone(), site, Some(Language::Scala), &decls), unrelated);
    let classes = vec![other[0], failure[0]];
    assert_eq!(narrow_application(classes.clone(), site, Some(Language::Scala), &decls), classes);
    let wub = named(&decls, &f, "Wub");
    let not_apply = vec![wub[1], zero[0]];
    assert_eq!(
        narrow_application(not_apply.clone(), call(&f, "Wub"), Some(Language::Scala), &decls),
        not_apply
    );
    let targets = vec![failure[1], failure_apply[0]];
    assert_eq!(
        narrow_application(targets.clone(), call(&f, "Failure"), Some(Language::Java), &decls),
        targets
    );
    // A companion declaring its own `apply` is not a plain construction.
    let pair = vec![failure[0], failure[1]];
    assert_eq!(narrow_application(pair.clone(), call(&f, "Failure"), Some(Language::Scala), &decls), pair);
    // Nor is a companion that inherits (it may inherit an `apply`).
    let ext = named(&decls, &f, "Ext");
    assert_eq!(ext.len(), 2, "the case class and its companion object");
    assert_eq!(narrow_application(ext.clone(), call(&f, "Failure"), Some(Language::Scala), &decls), ext);
}
