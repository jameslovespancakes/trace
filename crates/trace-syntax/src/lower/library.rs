//! Library-mode lowering (the facts trace-library derives library behaviour from; never
//! part of the index facts).

use trace_core::facts::{Expr, FileFacts, FlowFact, Scope};
use trace_core::languages::{in_family, Family};
use trace_core::text::LineIndex;
use trace_core::Language;
use tree_sitter::Node;

use super::{expr::lower_expr_in, expr::Mode, expr::Synthetic, expr::MAX_OBJECT_FIELDS, LCtx, Lowerer};
use crate::extract::decl_syntax_for;
use crate::node::{has_direct_token, named_children, nth_named, pick, span, text};
use crate::spec::SyntaxSpec;

// ---------------------------------------------------------------------------------------------
// Library mode (owner derive): the lowering trace-library derives library behaviour from.
//
// Same facts as the index lowering plus what the generic derivation rules need and the index
// never uses (so `FileFacts` and `EXTRACTOR_VERSION` are unaffected): subscript reads are
// container reads, string literals / concatenations / container literals are values,
// assignments by operator (R `<-`), destructuring of one value, and [`LibraryOp`]s (subscript
// stores with their key, iteration of every language).

/// Attribute of a subscript read lowered as a call (`o[k]` -> `Call { Attr { o, "[]" }, [k] }`):
/// a container read.
pub const INDEX_READ: &str = "[]";
/// Callee name of a concatenation / interpolation (`a + b`, f-strings, template literals): the
/// value is built from the operands.
pub const CONCAT_CALLEE: &str = "<concat>";
/// Callee name of a container literal (list, tuple, dict, array, object): the value
/// holds the elements.
pub const CONTAINER_CALLEE: &str = "<container>";
/// Prefix of a string literal lowered as a name (`"GET"` -> `Name { "<lit>GET" }`).
pub const LITERAL_PREFIX: &str = "<lit>";
/// Prefix of a non-negative integer literal lowered as a name in the library lowering
/// (`1` -> `Name { "<num>1" }`): element offsets (`args.slice(1)`).
pub const NUMBER_PREFIX: &str = "<num>";
/// Integer literal kinds of the grammars (library lowering, [`NUMBER_PREFIX`]).
const INTEGER_KINDS: &[&str] =
    &["number", "integer", "int_literal", "integer_literal", "decimal_integer_literal"];
/// Keyword of a `**mapping` argument in the library lowering (`f(**kwargs)` ->
/// `kwargs: [("**", kwargs)]`): every keyword the mapping holds.
pub const KEYWORD_SPREAD: &str = "**";
/// Keyword of a trailing positional spread in the library lowering (`f(a, *rest)` ->
/// `args: [a], kwargs: [("*", rest)]`): the spread elements fill the positions from
/// `args.len()` on.
pub const POSITIONAL_SPREAD: &str = "*";
/// Callee name of the member names of an object in the library lowering (`for (k in o)`
/// iterates `Call { Name("<keys>"), [o] }`).
pub const KEYS_CALLEE: &str = "<keys>";

/// Library-mode lowering of one file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LibraryLowering {
    /// Flow facts in library mode (same shapes as `FileFacts::flow`).
    pub flow: Vec<FlowFact>,
    pub ops: Vec<LibraryOp>,
}

/// Library-mode facts that have no `FlowFact` shape.
#[derive(Clone, Debug, PartialEq)]
pub enum LibraryOp {
    /// `object[key] = value`.
    IndexStore {
        scope: Scope,
        object: Expr,
        key: Expr,
        value: Expr,
    },
    /// `for targets in iterable`, comprehension clauses, `yield from` / `yield*`, spreads:
    /// `iterable` is iterated, its elements are bound to `targets`.
    Iterate {
        scope: Scope,
        iterable: Expr,
        targets: Vec<String>,
    },
}

/// String literal kinds (library mode lowers them to `<lit>` names or `<concat>` calls).
pub(super) const STRING_KINDS: &[&str] = &[
    "string",
    "string_literal",
    "interpreted_string_literal",
    "raw_string_literal",
    "template_string",
    "encapsed_string",
];

/// Interpolation hole kinds inside string literals.
pub(super) const HOLE_KINDS: &[&str] = &["interpolation", "template_substitution", "string_interpolation"];

/// Container literal kinds (library mode lowers them to `<container>` calls).
pub(super) const CONTAINER_KINDS: &[&str] = &[
    "list",
    "tuple",
    "set",
    "dictionary",
    "expression_list",
    "array",
    "object",
    "array_creation_expression",
    "literal_value",
    "array_expression",
    "tuple_expression",
    "list_literal",
];

/// Key/value element kinds inside container literals (the value is the last named child).
const PAIR_KINDS: &[&str] = &["pair", "keyed_element", "field", "array_element_initializer"];

/// Library-mode lowering of `source` (whose index facts are `facts`, from
/// [`crate::extract`] of the same bytes).
pub fn lower_library(
    language: Language,
    source: &[u8],
    facts: &FileFacts,
) -> Result<LibraryLowering, crate::SyntaxError> {
    let grammar = crate::grammar::grammar(language).ok_or(crate::SyntaxError::NoGrammar(language))?;
    let tree = crate::parse_tree(language, source)?;
    let root = tree.root_node();
    let lines = LineIndex::new(source);
    let found = decl_syntax_for(grammar, root, source, facts);
    let mut scratch = facts.clone();
    scratch.flow.clear();
    scratch.implicit.clear();
    let mut lowerer = Lowerer::new(grammar.spec, source, &lines, &[], &scratch);
    lowerer.partial = found;
    lowerer.lib = true;
    lowerer.run(root, &mut scratch);
    let ops = lowerer.ops.into_inner();
    Ok(LibraryLowering {
        flow: scratch.flow,
        ops,
    })
}

/// Content of a string literal without interpolation holes (`None` for other nodes).
/// Whether `node` is a string (literal or template) or an object / array / list literal with a
/// string among its entries (two levels: `{url: "/api"}`, `["GET"]`).
pub(crate) fn holds_string(node: Node<'_>) -> bool {
    pub(super) fn walk(node: Node<'_>, depth: u32) -> bool {
        if STRING_KINDS.contains(&node.kind()) {
            return true;
        }
        depth < 2 && named_children(node).into_iter().any(|c| walk(c, depth + 1))
    }
    walk(node, 0)
}

pub(super) fn string_literal(spec: &SyntaxSpec, node: Node<'_>, source: &[u8]) -> Option<String> {
    if !STRING_KINDS.contains(&node.kind()) {
        return None;
    }
    let holes = named_children(node)
        .iter()
        .any(|c| HOLE_KINDS.contains(&c.kind()) || spec.interpolations.contains(&c.kind()));
    (!holes).then(|| string_content(node, source))
}

/// Text of a string literal without quotes (content / fragment children, else the token text).
pub(super) fn string_content(node: Node<'_>, source: &[u8]) -> String {
    let parts: Vec<String> = named_children(node)
        .into_iter()
        .filter(|c| {
            let k = c.kind();
            k.contains("content") || k.contains("fragment")
        })
        .map(|c| text(c, source).into_owned())
        .collect();
    if !parts.is_empty() {
        return parts.concat();
    }
    if let Some(content) = node.child_by_field_name("content") {
        return text(content, source).into_owned();
    }
    text(node, source)
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '`')
        .to_string()
}

/// Library-mode expression forms; `None` = the index lowering applies.
pub(super) fn lower_library_expr(
    spec: &SyntaxSpec,
    node: Node<'_>,
    source: &[u8],
    depth: usize,
    synth: &Synthetic,
) -> Option<Expr> {
    let kind = node.kind();
    let whole = span(node);
    let sub = |n: Node<'_>| lower_expr_in(spec, n, source, depth + 1, synth, Mode::Library);
    let call = |func: Expr, args: Vec<Expr>| Expr::Call {
        func: Box::new(func),
        func_span: whole,
        args,
        kwargs: Vec::new(),
        span: whole,
        is_new: false,
    };
    let named = |name: &str| Expr::Name {
        name: name.to_string(),
        span: whole,
    };
    if let Some(s) = spec.subscript(kind) {
        let object = pick(node, s.first)?;
        let key = named_children(node).into_iter().rev().find(|c| c.id() != object.id());
        let func = Expr::Attr {
            object: Box::new(sub(object)),
            attr: INDEX_READ.to_string(),
            attr_span: whole,
            span: whole,
        };
        return Some(call(func, key.map(|k| vec![sub(k)]).unwrap_or_default()));
    }
    match (spec.language, kind) {
        // Function names (PHP `name`, `qualified_name`) are names of callees in library mode.
        (Language::Php, "name" | "qualified_name") => {
            return Some(Expr::Name {
                name: text(node, source).trim().to_string(),
                span: whole,
            });
        }
        // Go `&T{..}` / `&x`: the address of a value is that value (allocations included).
        (Language::Go, "unary_expression") if has_direct_token(node, "&") => {
            let operand = node.child_by_field_name("operand")?;
            return Some(sub(operand));
        }
        _ => {}
    }
    if STRING_KINDS.contains(&kind) {
        let holes: Vec<Expr> = named_children(node)
            .into_iter()
            .filter(|c| HOLE_KINDS.contains(&c.kind()) || spec.interpolations.contains(&c.kind()))
            .filter_map(|c| nth_named(c, 0))
            .map(sub)
            .collect();
        if !holes.is_empty() {
            return Some(call(named(CONCAT_CALLEE), holes));
        }
        return Some(Expr::Name {
            name: format!("{LITERAL_PREFIX}{}", string_content(node, source)),
            span: whole,
        });
    }
    if INTEGER_KINDS.contains(&kind) {
        let digits = text(node, source);
        if let Ok(n) = digits.trim().parse::<u32>() {
            return Some(Expr::Name {
                name: format!("{NUMBER_PREFIX}{n}"),
                span: whole,
            });
        }
    }
    if spec.arithmetic.contains(&kind) || spec.binary_ops.contains(&kind) {
        let is_choice = spec.choices.iter().any(|c| {
            c.kind == kind
                && (c.operators.is_empty() || c.operators.iter().any(|op| has_direct_token(node, op)))
        });
        if is_choice {
            return None;
        }
        let operands = named_children(node).into_iter().map(sub).collect();
        return Some(call(named(CONCAT_CALLEE), operands));
    }
    if CONTAINER_KINDS.contains(&kind) {
        let elements = named_children(node)
            .into_iter()
            .filter(|c| !spec.is_comment(c.kind()))
            .map(|c| {
                let value = if PAIR_KINDS.contains(&c.kind()) {
                    nth_named(c, -1)
                } else {
                    Some(c)
                };
                let value = value.map(|v| {
                    if v.kind() == "literal_element" {
                        nth_named(v, 0).unwrap_or(v)
                    } else {
                        v
                    }
                });
                value.map(sub).unwrap_or(Expr::Opaque)
            })
            .collect();
        // Object literals also name their entries (`{file, args: a}` -> kwargs `file`,
        // `args`): a record whose fields are read back by name.
        let kwargs = if spec.object_literals.contains(&kind) {
            record_fields(node, source, &sub)
        } else {
            Vec::new()
        };
        return Some(Expr::Call {
            func: Box::new(named(CONTAINER_CALLEE)),
            func_span: whole,
            args: elements,
            kwargs,
            span: whole,
            is_new: false,
        });
    }
    None
}

/// Named entries of an object literal (library lowering): `key: value` with an identifier,
/// plain string or number key, and shorthand `name` entries (at most [`MAX_OBJECT_FIELDS`]).
fn record_fields(node: Node<'_>, source: &[u8], sub: &dyn Fn(Node<'_>) -> Expr) -> Vec<(String, Expr)> {
    let mut out = Vec::new();
    for entry in named_children(node) {
        if out.len() >= MAX_OBJECT_FIELDS {
            break;
        }
        match entry.kind() {
            "shorthand_property_identifier" => {
                let name = text(entry, source).trim().to_string();
                out.push((
                    name.clone(),
                    Expr::Name {
                        name,
                        span: span(entry),
                    },
                ));
            }
            "pair" => {
                let (Some(key), Some(value)) =
                    (entry.child_by_field_name("key"), entry.child_by_field_name("value"))
                else {
                    continue;
                };
                let name = match key.kind() {
                    "property_identifier" | "number" => text(key, source).trim().to_string(),
                    k if STRING_KINDS.contains(&k)
                        && named_children(key).iter().all(|c| !HOLE_KINDS.contains(&c.kind())) =>
                    {
                        string_content(key, source)
                    }
                    _ => continue,
                };
                if !name.is_empty() {
                    out.push((name, sub(value)));
                }
            }
            _ => {}
        }
    }
    out
}

impl<'t> Lowerer<'_, 't> {
    /// Library-mode facts of one node (see [`lower_library`]).
    pub(super) fn library_statement(&self, node: Node<'t>, ctx: LCtx, facts: &mut FileFacts) {
        let kind = node.kind();
        let spec = self.spec;
        let language = spec.language;
        // Assignments by operator: R `x <- v` / `v -> x`.
        for store in spec.operator_stores {
            if store.kind != kind || !store.operators.iter().any(|op| has_direct_token(node, op)) {
                continue;
            }
            let other = match store.pick {
                "lhs" => "rhs",
                "rhs" => "lhs",
                "left" => "right",
                "right" => "left",
                _ => continue,
            };
            if let (Some(target), Some(value)) = (pick(node, store.pick), pick(node, other)) {
                self.bind_target(target, value, ctx, facts);
            }
        }
        // Iteration: loops, comprehension clauses, delegating yields, spreads.
        let mut loops: Vec<(Node<'t>, Option<Node<'t>>)> = Vec::new();
        for f in spec.for_loops {
            if f.kind == kind && (f.token.is_empty() || has_direct_token(node, f.token)) {
                if let Some(iterable) = pick(node, f.iterable) {
                    loops.push((iterable, pick(node, f.target)));
                }
            }
        }
        for f in spec.library_loops.iter().filter(|f| f.kind == kind) {
            if let Some(iterable) = pick(node, f.iterable) {
                loops.push((iterable, pick(node, f.target)));
            }
        }
        let clause = spec.generator_clause;
        if !clause.kind.is_empty() && clause.kind == kind {
            if let Some(iterable) = pick(node, clause.second) {
                loops.push((iterable, pick(node, clause.first)));
            }
        }
        for y in spec.delegating_yields {
            if y.kind == kind && has_direct_token(node, y.token) {
                if let Some(iterable) = nth_named(node, 0) {
                    loops.push((iterable, None));
                }
            }
        }
        if spec.iterate_parents.contains(&kind) {
            if let Some(iterable) = nth_named(node, 0) {
                loops.push((iterable, None));
            }
        }
        // JavaScript `for (k in o)`: the loop variable holds the member names of `o`
        // (`Call { Name("<keys>"), [o] }` is iterated; `for..of` is an ordinary loop above).
        if in_family(language, Family::JavaScript)
            && kind == "for_in_statement"
            && has_direct_token(node, "in")
        {
            if let Some(object) = node.child_by_field_name("right") {
                let mut targets = Vec::new();
                if let Some(t) = node.child_by_field_name("left") {
                    self.collect_names(t, &mut targets);
                }
                let whole = span(node);
                let keys = Expr::Call {
                    func: Box::new(Expr::Name {
                        name: KEYS_CALLEE.to_string(),
                        span: whole,
                    }),
                    func_span: whole,
                    args: vec![self.lower(object)],
                    kwargs: Vec::new(),
                    span: whole,
                    is_new: false,
                };
                self.ops.borrow_mut().push(LibraryOp::Iterate {
                    scope: ctx.scope,
                    iterable: keys,
                    targets,
                });
            }
        }
        for (iterable, target) in loops {
            let mut targets = Vec::new();
            if let Some(t) = target {
                self.collect_names(t, &mut targets);
            }
            // `for k, v in a, b`: the iterable is the list's first expression.
            let iterable = if spec.lists.contains(&iterable.kind()) {
                nth_named(iterable, 0).unwrap_or(iterable)
            } else {
                iterable
            };
            self.ops.borrow_mut().push(LibraryOp::Iterate {
                scope: ctx.scope,
                iterable: self.lower(iterable),
                targets,
            });
        }
    }

    /// Identifier names bound by a loop target (at most 8).
    fn collect_names(&self, node: Node<'t>, out: &mut Vec<String>) {
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if out.len() >= 8 {
                return;
            }
            if self.spec.is_identifier(n.kind()) {
                out.push(text(n, self.source).trim().to_string());
                continue;
            }
            let mut children = named_children(n);
            children.reverse();
            stack.extend(children);
        }
    }
}
