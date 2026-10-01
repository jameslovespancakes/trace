//! Value-flow lowering: syntax nodes -> `FlowFact` / `Expr` IR (port of the constraint
//! collection in codepath_v3/inference/flow.py `_stmt`, language-generic via `SyntaxSpec`).
//!
//! Emitted facts per statement kind (Python names; other languages map equivalents):
//!
//! | syntax                                   | facts                                                        |
//! |------------------------------------------|--------------------------------------------------------------|
//! | `def f(a=d)` in scope S                  | `Bind{Var(f,a), d, S}` per default (kw-only too)             |
//! | method `def m(self, ...)` in class C     | `ImplicitSelf{m, self, C, is_class=@classmethod}` unless `@staticmethod` |
//! | decorated `def f` in S / class C         | `Decorated{S, Var(S,f) | Member(C,f), f, decorators}`       |
//! | `x = v` at scope S (not class body)      | `Bind{Var(S,x), v, S}`                                        |
//! | `x = v` in class body C                  | `Bind{Member(C,x), v, S}` + `Bind{Field(x), v, S}`           |
//! | `o.a = v`                                | `Bind{Field(a), v, S}` + `Bind{FieldOf(o,a), v, S}`          |
//! | `o[k] = v`                               | `ImplicitOp{SubscriptStore, o}`                               |
//! | `return v` in function F                 | `Return{F, v}`                                                |
//! | every call evaluated by a statement      | `Eval{S, call}` (nested defs/lambdas excluded)               |
//! | `o[k]` load / `del o[k]`                 | `ImplicitOp{SubscriptLoad|SubscriptDelete, o}`               |
//! | attribute load `o.a`                     | `ImplicitOp{DescriptorGet, o.a}`                              |
//! | `with e:`                                | `ImplicitOp{WithEnter, e}`                                    |
//! | `for x in e:`                            | `ImplicitOp{Iterate, e}`                                      |
//! | `lambda ...: e` (synthetic decl L)       | `Return{L, e}`; body calls `Eval{Decl(L), call}`             |
//! | generator expression (synthetic decl G)  | calls of the element/conditions/later clauses `Eval{Decl(G)}`; the first iterable in the enclosing scope |
//!
//! Expressions: identifiers -> `Name`; member access -> `Attr` (attr_span = property
//! identifier); calls -> `Call` (positional args skip starred/spread; keyword args by name);
//! `a or b`, `x if c else y`, `c ? x : y`, `a ?? b`, `a || b` -> `Choice`; `await e` -> `Await`;
//! lambdas/arrows -> `Lambda { function }` (`Some(decl)` for synthetic `<lambda>`
//! declarations); a generator expression -> `Call { func: Lambda { function: <genexpr> },
//! args: [first iterable] }` (Python semantics: the generator function called with the
//! first iterable); everything else -> `Opaque`. Depth > MAX_EXPR_DEPTH -> `Opaque`.
//!
//! Scope rules (flow.py): a function body is walked with scope = the function and no class;
//! a class body with the enclosing scope and class = the class; decorators, default values
//! and class bases are not statements of any scope (defaults/decorators are captured by the
//! `Bind`/`Decorated` facts instead); synthetic declarations nested in decorators and
//! defaults are still lowered in their own scope. Since extractor 5 anonymous callables of
//! every language are synthetic `<lambda>` declarations (their bodies are active scopes;
//! a block body returns through its `return` statements, only Python lambdas and
//! expression bodies get a `Return` of the body value). Lazy scopes that are neither
//! declarations nor synthetic contribute no facts; declarations nested inside them are
//! still lowered (their own scope).
//! Other languages: receivers (`this`, Rust `self`, Go receivers) become `ImplicitSelf`;
//! `Decorated` and implicit data-model operations are Python-only. A synthetic `<lambda>`
//! gets `ImplicitSelf` only when it is bound directly in a class body (`f = lambda self: ...`).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use trace_core::facts::{FileFacts, Scope};
use trace_core::text::LineIndex;
use tree_sitter::Node;

use crate::extract::DeclSyntax;
use crate::spec::SyntaxSpec;

mod annotation_types;
mod expr;
mod library;
mod lowerer;
mod statements;

pub use annotation_types::{annotation_types, AnnotationElement, AnnotationType, AnnotationUse};
pub(crate) use expr::{lower_in, synthetic_map, Synthetic};
pub use expr::{OBJECT_CALLEE, YIELDED};
pub(crate) use library::holds_string;
pub use library::{
    lower_library, LibraryLowering, LibraryOp, CONCAT_CALLEE, CONTAINER_CALLEE, INDEX_READ, KEYS_CALLEE,
    KEYWORD_SPREAD, LITERAL_PREFIX, NUMBER_PREFIX, POSITIONAL_SPREAD,
};

/// Lower with declaration syntax already known (extraction path).
pub(crate) fn lower_with<'t>(
    spec: &SyntaxSpec,
    root: Node<'t>,
    source: &[u8],
    lines: &LineIndex,
    decls: &[DeclSyntax<'t>],
    facts: &mut FileFacts,
) {
    let mut lowerer = Lowerer::new(spec, source, lines, decls, facts);
    lowerer.run(root, facts);
}

#[derive(Clone, Copy)]
struct LCtx {
    scope: Scope,
    class: Option<u32>,
    /// False inside anonymous lazy scopes: no facts, but nested declarations still lower.
    active: bool,
    /// False in decorator and default-value subtrees: they are not statements of `scope`
    /// (captured by `Decorated` / `Bind` facts), but nested declarations still lower.
    statements: bool,
}

impl LCtx {
    fn body(scope: Scope, class: Option<u32>, active: bool) -> Self {
        LCtx {
            scope,
            class,
            active,
            statements: true,
        }
    }
}

struct Lowerer<'a, 't> {
    spec: &'a SyntaxSpec,
    source: &'a [u8],
    lines: &'a LineIndex,
    decls: &'a [DeclSyntax<'t>],
    /// Used by library lowering when only some declarations map to this tree.
    partial: Vec<Option<DeclSyntax<'t>>>,
    def_index: HashMap<usize, u32>,
    decorator_ids: HashSet<usize>,
    synth: Synthetic,
    /// Type declarations by name (receiver classes of out-of-line methods).
    types: HashMap<String, u32>,
    /// Library mode ([`lower_library`]): extra expression forms and [`LibraryOp`]s.
    lib: bool,
    ops: RefCell<Vec<LibraryOp>>,
}

#[cfg(test)]
#[path = "../../tests/unit/lower/mod.rs"]
mod tests;
