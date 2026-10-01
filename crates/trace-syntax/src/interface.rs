//! Interface fingerprint of a file (PLAN decision 13, DESIGN §1.14.1; owner speed): changes
//! only when what other files can see of this file changes, so dependents are re-queried
//! only then (`trace_core::incremental::semantic_requery`, the interface rule).
//!
//! What counts (never positions, never comments, never bodies except inferred returns):
//! * every declaration's header: qualified name, kind, container, bases, decorators,
//!   parameters, stub / execution flags, and the syntax-tree tokens between the start of the
//!   declaration and the start of its body (modifiers, parameter list with types, declared
//!   return type, `extends` / `implements`, export wrappers), comments skipped;
//! * declared, constructed and annotated types that are visible from outside: return
//!   types, fields, declared type names and module-level variables (`TypeFact`s other than
//!   function locals);
//! * values bound where other files can read them: module-level variables and attribute /
//!   member stores (`self.x = Store()` decides the type of `obj.x` in a dependent); the
//!   value expressions are hashed structurally, without spans;
//! * re-exports and module-level imports (Python re-exports imported names);
//! * languages whose return types are inferred from bodies ([`ReturnRule`]): the return
//!   statement subtrees (and expression bodies of arrow functions / lambdas) whose innermost
//!   enclosing callable declares no return type (returns of module-level code and of
//!   callables that are not declarations count too), or, for implicit-return languages, the
//!   whole body of every callable without a declared return type. A `return` edit inside a
//!   callable with a declared return type keeps the interface: dependents see the declared
//!   type, never the returned expression.
//!
//! Tokens come from the syntax tree (leaf nodes), so whitespace and comment edits never
//! change the fingerprint (no string processing of source code).

use trace_core::facts::{BindTarget, Expr, FileFacts, FlowFact, Scope, TypeSubject};
use trace_core::fingerprint::{Hash32, PartsHasher};
use trace_core::Language;
use tree_sitter::Node;

/// How a language's return types are known to dependents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReturnRule {
    /// Return types are declared in the header (or not visible to other files).
    Declared,
    /// Return types may be inferred from `return` statements and expression bodies.
    ReturnStatements,
    /// The last expression of a body is its value: the whole body of a callable without a
    /// declared return type counts.
    WholeBody,
}

/// Node kinds whose subtree is a returned value (explicit returns and generator yields).
const RETURN_KINDS: &[&str] = &[
    "return_statement",
    "return_expression",
    "yield",
    "yield_expression",
    "yield_statement",
];

/// Anonymous function kinds whose `body` field may be an expression (its value is returned).
const EXPRESSION_BODY_KINDS: &[&str] = &["arrow_function", "lambda"];

/// Interface fingerprint of `facts`, extracted from `source` whose parsed tree is `root` (the
/// extractor's own tree: no second parse).
pub(crate) fn fingerprint(language: Language, facts: &FileFacts, source: &[u8], root: Node<'_>) -> Hash32 {
    let tokens = Tokens::collect(root, source);
    let mut h = PartsHasher::new();
    hash_facts(&mut h, language, facts);
    h.text("headers");
    for d in &facts.declarations {
        let end = d.body_start.max(d.span.bytes.start).min(d.span.bytes.end);
        h.text(&d.qualified_name);
        tokens.hash_range(&mut h, source, d.span.bytes.start, end);
    }
    match crate::languages::syntax(language).map_or(ReturnRule::Declared, |s| s.return_rule) {
        ReturnRule::Declared => {}
        ReturnRule::ReturnStatements => {
            h.text("returns");
            let callables = Callables::of(facts);
            for &(start, end) in &returned_ranges(root) {
                // A declared return type is the interface; the returned value is a body detail.
                if callables
                    .innermost(start, end)
                    .is_some_and(|d| has_declared_return(facts, d))
                {
                    continue;
                }
                tokens.hash_range(&mut h, source, start, end);
            }
        }
        ReturnRule::WholeBody => {
            h.text("bodies");
            for (i, d) in facts.declarations.iter().enumerate() {
                if !d.kind.is_callable() || has_declared_return(facts, i as u32) {
                    continue;
                }
                let start = d.body_start.max(d.span.bytes.start);
                h.text(&d.qualified_name);
                tokens.hash_range(&mut h, source, start, d.span.bytes.end);
            }
        }
    }
    h.finish()
}

/// Callable declarations of a file by span start (innermost-callable lookup of the return
/// rule: a returned value counts only when its callable declares no return type).
struct Callables {
    /// (start, end, declaration index), sorted by start then by end descending (outer
    /// first), so the last containing entry before a position is the innermost.
    spans: Vec<(u32, u32, u32)>,
}

impl Callables {
    fn of(facts: &FileFacts) -> Callables {
        let mut spans: Vec<(u32, u32, u32)> = facts
            .declarations
            .iter()
            .enumerate()
            .filter(|(_, d)| d.kind.is_callable())
            .map(|(i, d)| (d.span.bytes.start, d.span.bytes.end, i as u32))
            .collect();
        spans.sort_by_key(|&(s, e, i)| (s, std::cmp::Reverse(e), i));
        Callables { spans }
    }

    /// The innermost callable declaration whose span contains `[start, end)`.
    fn innermost(&self, start: u32, end: u32) -> Option<u32> {
        let upto = self.spans.partition_point(|&(s, _, _)| s <= start);
        self.spans[..upto]
            .iter()
            .rev()
            .find(|&&(_, e, _)| e >= end)
            .map(|&(_, _, i)| i)
    }
}

/// Declared return type of declaration `decl` present.
fn has_declared_return(facts: &FileFacts, decl: u32) -> bool {
    facts.types.iter().any(|t| {
        t.subject == TypeSubject::Return { decl } && t.source == trace_core::facts::TypeSource::Declared
    })
}

fn hash_facts(h: &mut PartsHasher, language: Language, facts: &FileFacts) {
    h.text(language.as_str());
    h.text("declarations");
    for d in &facts.declarations {
        h.text(&d.qualified_name)
            .text(&d.name)
            .text(d.kind.as_str())
            .text(d.container.as_deref().unwrap_or(""))
            .text(&format!("{:?}", d.execution))
            .int(u64::from(d.is_stub))
            .int(u64::from(d.is_test))
            .int(d.parent.map_or(0, |p| u64::from(p) + 1));
        h.int(d.bases.len() as u64);
        for b in &d.bases {
            h.text(b);
        }
        h.int(d.decorators.len() as u64);
        for dec in &d.decorators {
            h.text(dec);
        }
        h.int(d.parameters.len() as u64);
        for p in &d.parameters {
            h.text(&p.name)
                .text(&format!("{:?}", p.kind))
                .int(u64::from(p.has_default));
        }
    }
    h.text("types");
    for t in &facts.types {
        let visible = match &t.subject {
            TypeSubject::Var { scope, .. } => *scope == Scope::Module,
            TypeSubject::Return { .. } | TypeSubject::Field { .. } => true,
        };
        if visible {
            h.text(&format!("{:?}", t.subject))
                .text(&t.type_name)
                .text(&format!("{:?}", t.source));
        }
    }
    h.text("bindings");
    for f in &facts.flow {
        match f {
            FlowFact::Bind { target, value, scope } if visible_target(target, *scope) => {
                hash_target(h, target);
                hash_expr(h, value);
            }
            FlowFact::Decorated {
                target,
                function,
                decorators,
                ..
            } => {
                h.text("decorated");
                hash_target(h, target);
                h.int(u64::from(*function));
                for d in decorators {
                    hash_expr(h, d);
                }
            }
            _ => {}
        }
    }
    h.text("exports");
    for e in &facts.exports {
        h.text(&e.exported).text(&e.target);
    }
    h.text("imports");
    for i in &facts.imports {
        if i.scope == Scope::Module {
            h.text(&i.local).text(&i.target).text(&format!("{:?}", i.kind));
        }
    }
    h.text("impls");
    for r in &facts.impls {
        h.text(&r.type_name).text(&r.trait_name);
    }
    h.int(u64::from(facts.module_decl.is_some()));
}

/// A binding other files can observe: module-level variables and every member / attribute
/// store (instance attributes are read through objects anywhere).
fn visible_target(target: &BindTarget, scope: Scope) -> bool {
    match target {
        BindTarget::Var { scope: var_scope, .. } => *var_scope == Scope::Module && scope == Scope::Module,
        BindTarget::Member { .. } | BindTarget::Field { .. } | BindTarget::FieldOf { .. } => true,
    }
}

fn hash_target(h: &mut PartsHasher, target: &BindTarget) {
    match target {
        BindTarget::Var { name, .. } => {
            h.text("var").text(name);
        }
        BindTarget::Member { class, name } => {
            h.text("member").int(u64::from(*class)).text(name);
        }
        BindTarget::Field { name } => {
            h.text("field").text(name);
        }
        BindTarget::FieldOf { object, name } => {
            h.text("fieldof").text(name);
            hash_expr(h, object);
        }
    }
}

/// Structural hash of an expression without spans (bounded by the expression size).
fn hash_expr(h: &mut PartsHasher, e: &Expr) {
    match e {
        Expr::Name { name, .. } => {
            h.text("n").text(name);
        }
        Expr::Attr { object, attr, .. } => {
            h.text("a").text(attr);
            hash_expr(h, object);
        }
        Expr::Call {
            func,
            args,
            kwargs,
            is_new,
            ..
        } => {
            h.text("c")
                .int(u64::from(*is_new))
                .int(args.len() as u64)
                .int(kwargs.len() as u64);
            hash_expr(h, func);
            for a in args {
                hash_expr(h, a);
            }
            for (k, v) in kwargs {
                h.text(k);
                hash_expr(h, v);
            }
        }
        Expr::Choice(items) => {
            h.text("choice").int(items.len() as u64);
            for i in items {
                hash_expr(h, i);
            }
        }
        Expr::Await(inner) => {
            h.text("await");
            hash_expr(h, inner);
        }
        Expr::Lambda { function, .. } => {
            h.text("lambda").int(function.map_or(0, |f| u64::from(f) + 1));
        }
        Expr::Opaque => {
            h.text("opaque");
        }
    }
}

/// Byte ranges of returned values: explicit return / yield subtrees and expression bodies
/// of arrow functions and lambdas, in document order.
fn returned_ranges(root: Node<'_>) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        let kind = node.kind();
        let mut descend = true;
        if RETURN_KINDS.contains(&kind) {
            out.push((node.start_byte() as u32, node.end_byte() as u32));
            // Nested returns are inside this range already.
            descend = false;
        } else if EXPRESSION_BODY_KINDS.contains(&kind) {
            if let Some(body) = node.child_by_field_name("body") {
                if !is_block(body) {
                    out.push((body.start_byte() as u32, body.end_byte() as u32));
                }
            }
        }
        if descend && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return out;
            }
        }
    }
}

/// A statement block (its value is not the function's result; returns inside it are found
/// on their own).
fn is_block(node: Node<'_>) -> bool {
    matches!(node.kind(), "statement_block" | "block" | "compound_statement")
}

/// Tokens of a tree in document order: leaf nodes plus the non-blank text between the
/// children of a node that no child covers (e.g. literal text of template strings in
/// grammars without fragment nodes). Comments and other extras are skipped; blank gaps
/// (whitespace only) are never tokens.
struct Tokens {
    /// (start, end) byte ranges, sorted by start.
    leaves: Vec<(u32, u32)>,
}

impl Tokens {
    fn collect(root: Node<'_>, source: &[u8]) -> Tokens {
        let mut leaves: Vec<(u32, u32)> = Vec::new();
        let gap = |leaves: &mut Vec<(u32, u32)>, start: usize, end: usize| {
            if end > start
                && source
                    .get(start..end)
                    .is_some_and(|b| b.iter().any(|c| !c.is_ascii_whitespace()))
            {
                leaves.push((start as u32, end as u32));
            }
        };
        let mut cursor = root.walk();
        // Per open ancestor: the end of its last visited child.
        let mut pos: Vec<usize> = Vec::new();
        'outer: loop {
            let node = cursor.node();
            if let Some(&p) = pos.last() {
                gap(&mut leaves, p, node.start_byte());
            }
            let comment = node.is_extra() || node.kind().contains("comment");
            if !comment {
                if node.child_count() == 0 {
                    if node.end_byte() > node.start_byte() {
                        leaves.push((node.start_byte() as u32, node.end_byte() as u32));
                    }
                } else if cursor.goto_first_child() {
                    pos.push(node.start_byte());
                    continue;
                }
            }
            if let Some(p) = pos.last_mut() {
                *p = (*p).max(node.end_byte());
            }
            loop {
                if cursor.goto_next_sibling() {
                    continue 'outer;
                }
                if !cursor.goto_parent() {
                    break 'outer;
                }
                // Every child of the parent was visited: its trailing text.
                let parent = cursor.node();
                let p = pos.pop().unwrap_or(parent.start_byte());
                gap(&mut leaves, p, parent.end_byte());
                if let Some(pp) = pos.last_mut() {
                    *pp = (*pp).max(parent.end_byte());
                }
            }
        }
        Tokens { leaves }
    }

    /// Hash the texts of the tokens inside `[start, end)` (one part per token).
    fn hash_range(&self, h: &mut PartsHasher, source: &[u8], start: u32, end: u32) {
        let first = self.leaves.partition_point(|&(s, _)| s < start);
        let mut count = 0u64;
        for &(s, e) in &self.leaves[first..] {
            if s >= end {
                break;
            }
            let e = e.min(end);
            if let Some(bytes) = source.get(s as usize..e as usize) {
                h.part(bytes);
                count += 1;
            }
        }
        h.int(count);
    }
}

#[cfg(test)]
#[path = "../tests/unit/interface.rs"]
mod tests;
