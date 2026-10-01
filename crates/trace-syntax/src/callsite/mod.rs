//! Call-site views for display (query time, language-generic over syntax trees).
//!
//! For a byte position inside a call's callee (edge evidence) or inside any use, a
//! [`SiteView`] gives:
//!
//! * `call`: the whole call expression on one line: comments removed, line breaks and the
//!   indentation after them collapsed (`f(\n  a,\n  b,\n)` -> `f(a, b)`), never other text;
//!   for curried application (Haskell `f a b`) the whole application spine;
//! * `when`: the conditions under which the site runs inside its function, outermost first,
//!   read from the syntax tree between the site and the nearest function boundary
//!   (`SyntaxSpec::lazy_scopes`):
//!   - an enclosing `if` / `elif` / `else` branch (the condition, negated in an `else`
//!     branch; every earlier condition of an `elif` chain negated). The condition is the
//!     grammar's `condition` field (all its parts), Haskell `if`, or the unfielded children
//!     before `then` (Bash `elif`) / before the `consequence` field; the else branch is the
//!     `alternative` field, an `else` / `elif` clause child, or what follows an `else`
//!     keyword;
//!   - an enclosing ternary branch (`c ? a : b`, Python `a if c else b`, Haskell
//!     conditionals);
//!   - the right operand of a short-circuit `&&` / `and` (left operand true) or `||` / `or`
//!     (left operand false), Bash `a && b` / `a || b` lists included;
//!   - an earlier `if` in the same block whose branch always leaves the block (its last
//!     statement returns, raises, throws, breaks or continues) and has no `else`: the site
//!     runs only when that condition was false (Rust `let P = v else { return };`: when
//!     `v matches P`);
//!   - an enclosing case of a `switch` / `match` / `when` / `case`: `subject == label`
//!     (`subject matches label` for pattern-matching languages and pattern labels, Bash
//!     shell patterns keep `==`, the shell's own pattern comparison; several
//!     labels joined with `||`; labels of preceding empty cases fall through in the C
//!     family), then the case's guard (`when` / `where` / `if` clause). A default / wildcard
//!     case adds nothing. Without a subject the labels are conditions and the earlier entries
//!     did not hold;
//!   - Haskell guards (`f x | c = ...`: `c`, earlier single guards negated; `otherwise`
//!     adds nothing).
//!
//!   Negation flips comparison operators (`==` / `!=`, `<` / `>=`, ...), removes a leading
//!   `!` / `not`, and otherwise prefixes `!` (`not ` in Python and Haskell, `! ` in Bash).
//! * `guards`: option-like member names the conditions test directly (`engine.Debug`,
//!   `self.enabled`, the operands of `&&` / `||` / `!`), never compared values
//!   (`value.handlers != nil` names nothing): the settings that switch the site on or off;
//! * `arguments`: the call's argument expressions (source order, one line each) with their
//!   position or keyword (argument lists, Bash words, Haskell spines).
//!
//! Loops add no condition (a site in a loop body runs once per iteration). Everything is
//! read from the syntax tree; no source text is matched.

use serde::Serialize;
use trace_core::Language;
use tree_sitter::Node;

use crate::grammar::grammar;
use crate::node::{slice, text};
use crate::spec::SyntaxSpec;

mod cases;
mod conditions;
mod render;

use conditions::{collect_conditions, smallest_call, spine_top};
pub(crate) use render::one_line;
use render::{arguments, guards, joined, negate_text, render, unwrap_parens};

/// Display facts of one site.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SiteView {
    /// The call expression on one line; empty when no call contains the position.
    pub call: String,
    /// Conditions, outermost first (module docs).
    pub when: Vec<String>,
    /// Option-like member names tested by `when`.
    pub guards: Vec<Guard>,
    /// Call arguments in source order (empty for non-call sites).
    pub arguments: Vec<ArgumentView>,
}

/// A member name a condition tests directly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Guard {
    pub name: String,
    /// The site runs when the member is truthy (false: when it is falsy).
    pub truthy: bool,
}

/// One argument of a call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ArgumentView {
    /// 0-based position among positional arguments; `None` for keyword arguments.
    pub position: Option<u32>,
    /// Keyword of a named argument (`key=value`, `name: value`).
    pub keyword: Option<String>,
    /// Argument text on one line.
    pub text: String,
}

/// If-like node kinds (loops excluded): a condition guarding a consequence.
const BRANCHES: [&str; 4] = ["if_statement", "if_expression", "elif_clause", "else_if_clause"];
/// Alternative clauses of an if-like node that are its children without an `alternative`
/// field (Bash `elif` / `else`).
const ALT_CLAUSES: [&str; 3] = ["elif_clause", "else_clause", "else_if_clause"];
/// Ternary expressions.
const TERNARIES: [&str; 3] = ["ternary_expression", "conditional_expression", "conditional"];
/// Binary / boolean expression kinds whose operator may short-circuit (`list`: Bash only).
const BOOLEANS: [&str; 6] = [
    "binary_expression",
    "boolean_operator",
    "binary_operator",
    "infix_expression",
    "infix",
    "list",
];
const AND_OPS: [&str; 2] = ["&&", "and"];
const OR_OPS: [&str; 2] = ["||", "or"];
/// Statements after which the rest of a block does not run.
const EXITS: [&str; 14] = [
    "return_statement",
    "return_expression",
    "raise_statement",
    "throw_statement",
    "throw_expression",
    "continue_statement",
    "continue_expression",
    "break_statement",
    "break_expression",
    "break",
    "next",
    "goto_statement",
    "exit_statement",
    "co_return_statement",
];
/// Case clauses of a switch / match / when / case (`case_statement` is the switch itself in
/// Bash).
const CASES: [&str; 18] = [
    "expression_case",
    "type_case",
    "default_case",
    "switch_case",
    "switch_default",
    "case_statement",
    "default_statement",
    "case_clause",
    "match_arm",
    "switch_section",
    "switch_expression_arm",
    "switch_block_statement_group",
    "switch_rule",
    "match_conditional_expression",
    "match_default_expression",
    "case_item",
    "alternative",
    // Haskell guarded right-hand sides are handled by `haskell_guards`, listed here only
    // for the grammar-kind check.
    "match",
];
/// Switch-like nodes owning case clauses (Bash `case_statement` added per language).
const SWITCHES: [&str; 7] = [
    "expression_switch_statement",
    "type_switch_statement",
    "switch_statement",
    "switch_expression",
    "match_expression",
    "match_statement",
    "case",
];
/// Fields naming the subject of a switch.
const SUBJECTS: [&str; 4] = ["value", "condition", "subject", "expr"];
/// Wildcard labels (a default case).
const WILDCARDS: [&str; 3] = ["wildcard", "discard", "underscore_pattern"];
const FLIP: [(&str, &str); 10] = [
    ("==", "!="),
    ("!=", "=="),
    ("===", "!=="),
    ("!==", "==="),
    ("<", ">="),
    (">=", "<"),
    (">", "<="),
    ("<=", ">"),
    ("is", "is not"),
    ("is not", "is"),
];
/// Condition wrappers without meaning of their own.
const PARENS: [&str; 2] = ["parenthesized_expression", "condition_clause"];
/// Upper bound of sibling / ancestor scans (explicit limit, bounded work).
const SCAN_LIMIT: usize = 256;

/// Views of the sites at `points` (byte positions) in one file; `None` for a position
/// outside every node or a language without a grammar. The file is parsed once.
pub fn site_views(language: Language, source: &[u8], points: &[u32]) -> Vec<Option<SiteView>> {
    let Some(grammar) = grammar(language) else {
        return points.iter().map(|_| None).collect();
    };
    let Ok(tree) = crate::parse::parse(grammar, "", source) else {
        return points.iter().map(|_| None).collect();
    };
    let spec = grammar.spec;
    let root = tree.root_node();
    points
        .iter()
        .map(|&p| view_at(spec, language, root, source, p as usize))
        .collect()
}

fn view_at(
    spec: &SyntaxSpec,
    language: Language,
    root: Node<'_>,
    source: &[u8],
    point: usize,
) -> Option<SiteView> {
    let call = smallest_call(spec, root, point).map(|c| spine_top(spec, c));
    let node = match call {
        Some(c) => c,
        None => root.named_descendant_for_byte_range(point, point.saturating_add(1))?,
    };
    let cx = Cx {
        spec,
        language,
        source,
    };
    let mut conditions: Vec<Cond<'_>> = Vec::new();
    collect_conditions(&cx, node, &mut conditions);
    conditions.reverse();
    let mut view = SiteView::default();
    for cond in &conditions {
        let rendered = cond.render(language, source);
        if !rendered.is_empty() && !view.when.contains(&rendered) {
            view.when.push(rendered);
        }
        if cond.subject.is_none() {
            for part in &cond.parts {
                if let Some(single) = part.single() {
                    guards(&cx, single, cond.truthy, &mut view.guards);
                }
            }
        }
    }
    if let Some(call) = call {
        view.call = joined(call, source);
        view.arguments = arguments(spec, call, source);
    }
    Some(view)
}

/// Shared read-only context of one view.
struct Cx<'a> {
    spec: &'a SyntaxSpec,
    language: Language,
    source: &'a [u8],
}

/// A condition part: one node, or a run of sibling nodes read as one text (a condition field
/// with several parts).
#[derive(Clone, Copy)]
struct Part<'t> {
    first: Node<'t>,
    last: Node<'t>,
}

impl<'t> Part<'t> {
    fn one(node: Node<'t>) -> Self {
        Part {
            first: node,
            last: node,
        }
    }

    fn single(&self) -> Option<Node<'t>> {
        (self.first.id() == self.last.id()).then_some(self.first)
    }

    fn encloses(&self, node: Node<'_>) -> bool {
        self.first.start_byte() <= node.start_byte() && node.end_byte() <= self.last.end_byte()
    }

    fn text(&self, source: &[u8]) -> String {
        match self.single() {
            Some(n) => one_line(&text(unwrap_parens(n), source)),
            None => one_line(&slice(source, self.first.start_byte(), self.last.end_byte())),
        }
    }
}

/// One condition of a site.
struct Cond<'t> {
    /// Condition parts; several parts = any of them holds (`case a, b`).
    parts: Vec<Part<'t>>,
    /// The site runs when the condition holds (false: when it does not).
    truthy: bool,
    /// Case subject: the parts are labels compared with it.
    subject: Option<Node<'t>>,
    /// Labels are patterns (`subject matches label`).
    matches: bool,
}

impl<'t> Cond<'t> {
    fn test(part: Part<'t>, truthy: bool) -> Self {
        Cond {
            parts: vec![part],
            truthy,
            subject: None,
            matches: false,
        }
    }

    fn node(node: Node<'t>, truthy: bool) -> Self {
        Self::test(Part::one(node), truthy)
    }

    fn any(labels: &[Node<'t>], truthy: bool) -> Self {
        Cond {
            parts: labels.iter().map(|l| Part::one(*l)).collect(),
            truthy,
            subject: None,
            matches: false,
        }
    }

    fn label(subject: Node<'t>, labels: &[Node<'t>], matches: bool) -> Self {
        Cond {
            parts: labels.iter().map(|l| Part::one(*l)).collect(),
            truthy: true,
            subject: Some(subject),
            matches,
        }
    }

    fn render(&self, language: Language, source: &[u8]) -> String {
        if let Some(subject) = self.subject {
            let s = one_line(&text(unwrap_parens(subject), source));
            let alternatives: Vec<String> = self
                .parts
                .iter()
                .map(|p| {
                    let l = p.text(source);
                    if self.matches {
                        format!("{s} matches {l}")
                    } else {
                        format!("{s} == {l}")
                    }
                })
                .collect();
            return alternatives.join(" || ");
        }
        if let [part] = self.parts.as_slice() {
            return match part.single() {
                Some(node) => render(node, self.truthy, language, source),
                None => negate_text(part.text(source), self.truthy, language),
            };
        }
        let joined: Vec<String> = self
            .parts
            .iter()
            .map(|p| p.text(source))
            .filter(|t| !t.is_empty())
            .collect();
        negate_text(joined.join(" || "), self.truthy, language)
    }
}

#[cfg(test)]
#[path = "../../tests/unit/callsite/mod.rs"]
mod tests;
