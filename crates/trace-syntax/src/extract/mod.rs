//! Generic query-driven extraction pipeline (one pass per file).
//!
//! 1. parse with a thread-local `Parser` (deadline: the `syntax.parse_timeout_ms` setting);
//! 2. run the grammar query; collect `@definition.*`, `@reference.*`, `@doc`, `@decorator`,
//!    `@base`, `@parameter`, `@impl.*`, `@test.*` captures (SPEC §6.2);
//! 3. build declarations in pre-order: qualified names from lexical nesting, spans widened to
//!    decorators/attributes/export wrappers, `body_start`, doc (Python docstring via
//!    [`crate::python`], else adjacent preceding comment nodes with only whitespace between);
//! 4. walk the tree once with an owner stack driven by [`crate::spec::SyntaxSpec`] to
//!    assign call/reference owners (lazy scopes -> `None` unless the scope is a symbol),
//!    create synthetic declarations (`<lambda>`, `<genexpr>`) with their consumers, and
//!    record imports and name bindings per scope ([`crate::names`]);
//! 5. lower flow facts ([`crate::lower`]) and implicit operations;
//! 6. build call details and qualified names ([`crate::detail`]);
//! 7. language post-processing (Python overload collapse, stub detection, execution model).
//!
//! Ownership rule (generic form of pyright.py `Ownership`): inside a declaration node, the
//! `@body` subtree executes when the declaration is called (owner = the declaration for
//! callables, `None` for class bodies); everything else of the declaration node (decorators,
//! default values, bases) executes in the enclosing scope. Anonymous functions listed in
//! `SyntaxSpec::anonymous_functions` (every language: arrows, closures, lambdas, blocks,
//! anonymous function expressions not bound by a named definition) are synthetic
//! `<lambda>` declarations owning their body (the whole non-header part when the node has
//! no `body` field); generator expressions are synthetic `<genexpr>` declarations owning
//! everything but their first iterable. `owner == None` therefore means module-level or
//! class-body code, executed by the synthetic `<module>` declaration appended last
//! (`FileFacts::module_decl`, SPEC §6.3).
//!
//! Non-call facts (extractor 5): every reference carries a [`trace_core::facts::RefKind`]
//! (`read`, `write` for store targets incl. attribute targets, `argument`, `decorator`,
//! `import` at the imported name of each binding, `export` for re-export entries, `type`
//! for type positions); `FileFacts::exports` lists re-exports.
//!
//! Synthetic declarations: name `<lambda>` / `<genexpr>` (never a valid identifier),
//! qualified name `<enclosing qualified name>.<lambda>`, kind `Function`, `span` = the
//! expression, `name_span` = its first token (`lambda` keyword / opening parenthesis),
//! `body_start` = the body expression, parameters from the lambda parameter list, no doc,
//! decorators or identifiers; one [`AnonymousScope`] each (sorted by declaration).

use std::collections::{HashMap, HashSet};

use trace_core::facts::{AnonymousKind, Declaration, FileFacts, ImplRelation, ParamKind, Scope, TestBlock};
use trace_core::model::{ByteSpan, ExecutionModel, Span, SymbolKind};
use trace_core::text::LineIndex;
use trace_core::Language;
use tree_sitter::Node;

use crate::grammar::grammar;
use crate::names::Names;
use crate::node::{named_children, slice, span, text, type_name};
use crate::spec::SyntaxSpec;
use crate::{SourceInput, SyntaxError};

mod calls;
mod collect;
mod data;
mod declare;
mod query;
mod walk;

pub(crate) use calls::unwrap_node;
pub(crate) use collect::{collect, decl_syntax_for, error_nodes};
use query::push_unique;
pub(crate) use query::{run_query, Captures, Pending};

/// Field names that belong to a declaration header (never its body).
const HEADER_FIELDS: &[&str] = &[
    "name",
    "parameters",
    "parameter",
    "return_type",
    "type",
    "type_parameters",
    "receiver",
    "result",
    "superclass",
    "superclasses",
    "interfaces",
    "decorator",
    "attributes",
    "object",
    "patterns",
];

// ---------------------------------------------------------------------------------------
// Declaration syntax kept for lowering
// ---------------------------------------------------------------------------------------

/// One parameter as read from syntax.
pub(crate) struct ParamSyntax<'t> {
    pub name: String,
    pub kind: ParamKind,
    pub name_node: Option<Node<'t>>,
    pub default: Option<Node<'t>>,
}

/// Syntax nodes of one declaration (same index as `FileFacts::declarations`).
pub(crate) struct DeclSyntax<'t> {
    pub def: Node<'t>,
    pub body: Option<Node<'t>>,
    /// Callable without a body node whose non-header children are its body.
    pub header_mode: bool,
    pub name_node: Node<'t>,
    pub decorators: Vec<Node<'t>>,
    pub bases: Vec<Node<'t>>,
    pub params: Vec<ParamSyntax<'t>>,
    /// Synthetic declaration (`def` is the lambda / generator expression node).
    pub synthetic: Option<AnonymousKind>,
}

impl<'t> DeclSyntax<'t> {
    /// Whether `child` (a direct child of the definition node) belongs to the header.
    pub fn is_header_child(&self, child: Node<'t>, field: Option<&str>) -> bool {
        if field.is_some_and(|f| HEADER_FIELDS.contains(&f)) {
            return true;
        }
        let id = child.id();
        id == self.name_node.id()
            || self.decorators.iter().any(|n| n.id() == id)
            || self.bases.iter().any(|n| n.id() == id)
            || self.params.iter().any(|p| p.name_node.is_some_and(|n| n.id() == id))
            || !child.is_named()
            || child.kind().ends_with("parameters")
            || child.kind().ends_with("parameter_list")
            || child.kind().contains("modifier")
            || child.kind() == "attribute_list"
    }
}

/// Span data computed for every pending definition before the walk (decorators and leading
/// attributes precede the definition node in pre-order).
struct Prepared<'t> {
    start: usize,
    end: usize,
    decorators: Vec<Node<'t>>,
}

fn prepare<'t>(spec: &SyntaxSpec, pending: &Pending<'t>) -> Prepared<'t> {
    let mut outer = pending.def;
    while let Some(parent) = outer.parent() {
        if !spec.wrappers.contains(&parent.kind()) {
            break;
        }
        let kind = outer.kind();
        let mut cursor = parent.walk();
        let same = parent
            .named_children(&mut cursor)
            .filter(|c| c.kind() == kind)
            .count();
        if same > 1 {
            break;
        }
        outer = parent;
    }
    let mut decorators = pending.decorators.clone();
    let mut sibling = outer.prev_named_sibling();
    while let Some(s) = sibling {
        if !spec.leading_attributes.contains(&s.kind()) {
            break;
        }
        push_unique(&mut decorators, s);
        sibling = s.prev_named_sibling();
    }
    decorators.sort_by_key(|n| n.start_byte());
    let mut start = outer.start_byte().min(pending.def.start_byte());
    for d in &decorators {
        start = start.min(d.start_byte());
    }
    Prepared {
        start,
        end: outer.end_byte().max(pending.def.end_byte()),
        decorators,
    }
}

// ---------------------------------------------------------------------------------------
// The walk
// ---------------------------------------------------------------------------------------

/// The enclosing context of a generator expression (its first iterable runs there).
#[derive(Clone, Copy, Default)]
struct Eager {
    owner: Option<u32>,
    lexical: Option<u32>,
    scope: Option<u32>,
}

/// What a binding position does to the names in it (SPEC §6.3 `RefKind::Write`).
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Store {
    /// Not a store position (parameters, imports, patterns of other constructs).
    #[default]
    None,
    /// Assignment / augmented assignment / `del` target: a `write` reference.
    Write,
    /// A position that declares a new local (`let x`, `const x`, loop variables, labels):
    /// bare names get no reference (attribute targets still are writes).
    Declare,
}

#[derive(Clone, Copy, Default)]
struct Ctx {
    /// Executing declaration.
    owner: Option<u32>,
    /// Innermost enclosing declaration.
    lexical: Option<u32>,
    /// Name-resolution scope: declaration whose body / parameter list this is (`None` =
    /// module). Decorators and default values stay in the enclosing scope.
    scope: Option<u32>,
    /// Inside a decorator / attribute expression.
    in_decorator: bool,
    /// Identifiers here are bound, not referenced.
    binding: bool,
    /// This node is the property of a member access.
    attr_property: bool,
    /// This node is a keyword-argument name (a label, not a binding).
    keyword_name: bool,
    /// Inside an import statement (bindings are read by `crate::names`).
    in_import: bool,
    /// First generator clause: the context of its iterable.
    eager: Option<Eager>,
    /// Innermost non-symbol namespace (index into `Walker::namespaces`).
    namespace: Option<u32>,
    /// Kind of store position this subtree is in.
    store: Store,
    /// Inside a type position (annotation, `extends`/`implements`, generic arguments).
    in_type: bool,
    /// Name of a re-export list entry (`export { a }`).
    export: bool,
    /// A label that names nothing in this file (`b` of `export { a as b }`).
    label: bool,
}

/// A call recorded by the walk (same index as `FileFacts::calls`).
pub(crate) struct WalkCall<'t> {
    pub callee: Node<'t>,
    /// Receiver child of split method calls (Java `obj.m()`).
    pub receiver: Option<Node<'t>>,
    pub arguments: Option<Node<'t>>,
    /// Name-resolution scope of the call.
    pub scope: Option<u32>,
}

/// Everything the walk produces besides `FileFacts`.
pub(crate) struct Collected<'t> {
    /// Syntax of every declaration (same order as `FileFacts::declarations`).
    pub decls: Vec<DeclSyntax<'t>>,
    /// Identifier-like occurrences `(start, end)`, sorted.
    pub idents: Vec<(u32, u32)>,
    pub calls: Vec<WalkCall<'t>>,
    /// Dotted-name candidates for qualified names (decorators, assigned values) with their
    /// resolution scope.
    pub chains: Vec<(Node<'t>, Option<u32>)>,
    pub names: Names,
}

struct Walker<'a, 't> {
    spec: &'static SyntaxSpec,
    language: Language,
    source: &'a [u8],
    lines: &'a LineIndex,
    caps: &'a Captures<'t>,
    prepared: HashMap<usize, Prepared<'t>>,
    decls: Vec<DeclSyntax<'t>>,
    /// Body node id -> declaration.
    body_owner: HashMap<usize, u32>,
    /// Nodes that are never value references (declared names, parameter names).
    skip_ids: HashSet<usize>,
    callee_ids: HashSet<usize>,
    decorator_ids: HashSet<usize>,
    comments: Vec<ByteSpan>,
    /// Identifier-like occurrences `(start, end)` in pre-order.
    idents: Vec<(u32, u32)>,
    /// Qualified namespace paths.
    namespaces: Vec<String>,
    /// Calls recorded so far (same index as `FileFacts::calls`).
    calls: Vec<WalkCall<'t>>,
    /// Call node id -> index into `FileFacts::calls`.
    call_index: HashMap<usize, u32>,
    chains: Vec<(Node<'t>, Option<u32>)>,
    names: Names,
    /// Name-resolution scope of every bare-name reference (index into
    /// `FileFacts::references`; attribute references are not listed).
    bare_refs: Vec<(usize, Option<u32>)>,
    /// Anonymous-function nodes that are the value of a named query definition
    /// (`const f = () => {}`, `f <- function(x) ...`): not synthetic `<lambda>` scopes.
    claimed: HashSet<usize>,
    /// Identifier nodes passed as call arguments without being called (`RefKind::Argument`).
    argument_ids: HashSet<usize>,
    /// Type names of conversions spelled as calls (`(*T)(x)`): `type` references.
    type_ref_ids: HashSet<usize>,
    /// JS/TS `export` statements and module-level JavaScript assignments (CommonJS
    /// `module.exports = require(..)`); re-exports are resolved after the walk.
    export_nodes: Vec<Node<'t>>,
    /// Re-exports found by import readers (Rust `pub use`).
    exports: Vec<trace_core::facts::Export>,
    /// Python binding identifiers (store targets and parameters) with their name-resolution
    /// scope: the binding half of `FileFacts::local_spans` (Python keeps its scoping rule,
    /// [`crate::names::Names::binding`]).
    py_bindings: Vec<(ByteSpan, String, Option<u32>)>,
}

pub(crate) fn extract_file(input: SourceInput<'_>) -> Result<FileFacts, SyntaxError> {
    let mut grammar = grammar(input.language).ok_or(SyntaxError::NoGrammar(input.language))?;
    let mut tree = crate::parse::parse(grammar, input.path, input.source)?;
    // `.h` headers are shared by C and C++ (`header::header_language`): a C-classified header
    // the C++ grammar parses with fewer errors is extracted with the C++ grammar and its facts
    // say C++ (the file's inventory language stays until flow uses the facts' language).
    let mut language = input.language;
    if input.language == Language::C {
        if let Some(alternative) = crate::header::cpp_alternative(input.path, input.source, &tree) {
            if let Some(cpp) = crate::grammar::grammar(Language::Cpp) {
                grammar = cpp;
                tree = alternative;
                language = Language::Cpp;
            }
        }
    }
    let root = tree.root_node();
    let source = input.source;
    let lines = LineIndex::new(source);
    let mut facts = FileFacts {
        language: Some(language),
        ..FileFacts::default()
    };

    let caps = run_query(grammar, root, source);
    let collected = collect(grammar, root, source, &lines, &caps, &mut facts);

    // Identifier counts per declaration span (search terms, test mentions); synthetic
    // declarations have none (their identifiers count for the enclosing declaration).
    for (decl, syntax) in facts.declarations.iter_mut().zip(&collected.decls) {
        if syntax.synthetic.is_none() {
            decl.identifiers = count_identifiers(source, &collected.idents, decl.span.bytes);
            if let Some(body) = syntax.body {
                facts
                    .body_identifiers
                    .insert(decl.body_start, count_identifiers(source, &collected.idents, span(body)));
            }
        }
    }

    // Out-of-line type relations.
    for (type_node, trait_node) in &caps.impls {
        let implementing = type_name(*type_node, source);
        let implemented = type_name(*trait_node, source);
        if implementing.is_empty() || implemented.is_empty() {
            continue;
        }
        // The relation block: the smallest node holding both names (`impl Tr for T { .. }`,
        // Haskell `instance C T where ..`), so the members
        // the relation declares lie inside its span.
        let mut block = type_node.parent().unwrap_or(*type_node);
        for _ in 0..64 {
            if block.start_byte() <= trait_node.start_byte() && trait_node.end_byte() <= block.end_byte() {
                break;
            }
            match block.parent() {
                Some(p) => block = p,
                None => break,
            }
        }
        facts.impls.push(ImplRelation {
            type_name: implementing,
            trait_name: implemented,
            span: span(block),
        });
    }

    // Generics dispatching by naming convention (R `UseMethod("g")`): the calling function
    // declares the generic (a stub); its methods `g.<class>` implement it.
    if !grammar.spec.generic_dispatch_calls.is_empty() {
        let generics: Vec<u32> = facts
            .calls
            .iter()
            .filter(|c| {
                c.member
                    .as_deref()
                    .is_some_and(|m| grammar.spec.generic_dispatch_calls.contains(&m))
            })
            .filter_map(|c| c.owner)
            .collect();
        for d in generics {
            if let Some(decl) = facts.declarations.get_mut(d as usize) {
                if decl.kind.is_callable() {
                    decl.is_stub = true;
                }
            }
        }
    }

    // Resolve value flow and import bindings before recognizing test APIs. Reuse the
    // scope-aware name resolver rather than guessing from a same-spelled import.
    crate::lower::lower_with(grammar.spec, root, source, &lines, &collected.decls, &mut facts);
    crate::detail::build(grammar.spec, source, &collected, &mut facts);

    // Test conventions.
    let test_file =
        crate::testing::is_test_path(input.path, input.language, &crate::testing::TestConfig::default());
    for decl in &mut facts.declarations {
        decl.is_test = crate::testing::is_test_declaration(test_file, decl);
    }
    {
        let paths: HashMap<_, _> = facts
            .call_details
            .iter()
            .filter_map(|d| {
                let call = facts.calls.get(d.call as usize)?;
                Some(((call.span.start, call.span.end), d.callee_path.as_deref()?))
            })
            .collect();
        for (block, name) in &caps.tests {
            let callee = caps
                .calls
                .get(&block.id())
                .and_then(|c| c.callee)
                .map(|n| crate::node::text(n, source).into_owned())
                .unwrap_or_default();
            let block_span = span(*block);
            let qualified = paths.get(&(block_span.start, block_span.end)).copied();
            if !crate::testing::is_test_call(input.language, &callee, qualified, test_file) {
                continue;
            }
            facts.tests.push(TestBlock {
                name: string_literal_text(*name, source),
                span: block_span,
                line: lines.line1(block_span.start),
                end_line: lines.line1(block_span.end.saturating_sub(1)),
                mentions: mentions(source, &collected.idents, block_span),
            });
        }
    }

    // Language post-processing.
    match input.language {
        Language::Python => crate::python::postprocess(input.path, root, source, &mut facts),
        // TypeScript overload signatures stay declarations of their own (`is_stub`, SPEC 6.3
        // general fixes rule 9): the family rule links each to its implementation.
        Language::TypeScript | Language::Tsx if is_declaration_file(input.path) => {
            for decl in &mut facts.declarations {
                decl.is_stub = true;
            }
        }
        _ => {}
    }
    if input.language == Language::Python && is_package_init(input.path) {
        python_all_exports(root, source, &lines, &mut facts);
    }

    // Declared, constructed and comment-annotated types (SPEC 6.3, general fixes rules 10
    // and 11), on the final declaration indices.
    let mut types = {
        let ctx =
            crate::typefacts::Context::new(grammar.spec, grammar.language, source, &collected.decls, &facts);
        let mut types = crate::typefacts::extract(&ctx, root);
        types.extend(crate::annotations::extract(&ctx, root));
        types
    };
    types.sort_by_key(|t| (t.span.start, t.span.end));
    facts.types = types;

    // The synthetic `<module>` declaration (SPEC §6.3): appended last, so no index moves.
    append_module(&mut facts, source, &lines, test_file);

    // Cross-language boundary facts (owned by the bridge package, `crate::boundary`).
    crate::boundary::extract(grammar.language, input.path, root, source, &lines, &mut facts);

    // Interface fingerprint (what dependents can see; PLAN decision 13).
    // The extractor's own tree (no second parse).
    facts.interface = crate::interface::fingerprint(language, &facts, source, root);
    Ok(facts)
}

/// Append the synthetic `<module>` declaration: kind `module`, whole-file span, empty name
/// span at 0, no parent (it executes module-level and class-body code; owners stay `None`).
fn append_module(facts: &mut FileFacts, source: &[u8], lines: &LineIndex, test_file: bool) {
    let len = source.len() as u32;
    let whole = ByteSpan::new(0, len);
    let index = facts.declarations.len() as u32;
    facts.declarations.push(Declaration {
        name: MODULE_NAME.to_string(),
        qualified_name: MODULE_NAME.to_string(),
        kind: SymbolKind::Module,
        span: Span {
            bytes: whole,
            start_line: 1,
            end_line: lines.line1(len.saturating_sub(1)).max(1),
        },
        name_span: ByteSpan::new(0, 0),
        body_start: 0,
        parent: None,
        container: None,
        doc: None,
        decorators: Vec::new(),
        bases: Vec::new(),
        parameters: Vec::new(),
        execution: ExecutionModel::Ordinary,
        is_stub: false,
        is_test: test_file,
        declaration_lines: Vec::new(),
        identifiers: Vec::new(),
    });
    facts.module_decl = Some(index);
}

/// Name and qualified name of the synthetic module declaration.
pub(crate) const MODULE_NAME: &str = "<module>";

/// `__init__.py` / `__init__.pyi` (a package's module).
fn is_package_init(path: &str) -> bool {
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    file == "__init__.py" || file == "__init__.pyi"
}

/// Python package `__all__ = [...]` / `__all__ += [...]` / tuples at module level: every
/// listed name bound by a module-level import is a re-export of the import target.
fn python_all_exports(root: Node<'_>, source: &[u8], lines: &LineIndex, facts: &mut FileFacts) {
    let mut found: Vec<trace_core::facts::Export> = Vec::new();
    for statement in named_children(root) {
        if statement.kind() != "expression_statement" {
            continue;
        }
        for assignment in named_children(statement) {
            if !matches!(assignment.kind(), "assignment" | "augmented_assignment") {
                continue;
            }
            let (Some(left), Some(right)) =
                (assignment.child_by_field_name("left"), assignment.child_by_field_name("right"))
            else {
                continue;
            };
            if left.kind() != "identifier" || text(left, source).trim() != "__all__" {
                continue;
            }
            if !matches!(right.kind(), "list" | "tuple") {
                continue;
            }
            for item in named_children(right) {
                if item.kind() != "string" {
                    continue;
                }
                let content: Vec<Node<'_>> = named_children(item)
                    .into_iter()
                    .filter(|c| c.kind() == "string_content")
                    .collect();
                let [part] = content.as_slice() else {
                    continue;
                };
                let name = text(*part, source).trim().to_string();
                let Some(import) = facts
                    .imports
                    .iter()
                    .find(|i| i.scope == Scope::Module && i.local == name)
                else {
                    continue;
                };
                let at = span(*part);
                found.push(trace_core::facts::Export {
                    exported: name,
                    target: import.target.clone(),
                    span: at,
                    line: lines.line1(at.start),
                });
            }
        }
    }
    facts.exports.extend(found);
}

/// `.d.ts` / `.d.mts` / `.d.cts` declaration files.
fn is_declaration_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".d.ts") || lower.ends_with(".d.mts") || lower.ends_with(".d.cts")
}

fn count_identifiers(source: &[u8], idents: &[(u32, u32)], within: ByteSpan) -> Vec<(String, u32)> {
    let lo = idents.partition_point(|&(s, _)| s < within.start);
    let hi = idents.partition_point(|&(s, _)| s < within.end);
    let mut counts: HashMap<&str, u32> = HashMap::new();
    for &(s, e) in &idents[lo..hi] {
        if e > within.end {
            continue;
        }
        if let Ok(t) = std::str::from_utf8(&source[s as usize..e as usize]) {
            if !t.is_empty() {
                *counts.entry(t).or_insert(0) += 1;
            }
        }
    }
    let mut out: Vec<(String, u32)> = counts.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    out.sort_unstable();
    out
}

fn mentions(source: &[u8], idents: &[(u32, u32)], within: ByteSpan) -> Vec<String> {
    let mut out: Vec<String> = count_identifiers(source, idents, within)
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    out.dedup();
    out
}

/// Text inside a string literal node (quotes removed structurally where possible).
fn string_literal_text(node: Node<'_>, source: &[u8]) -> String {
    let parts: Vec<Node<'_>> = named_children(node)
        .into_iter()
        .filter(|c| c.kind().contains("fragment") || c.kind().contains("content"))
        .collect();
    if !parts.is_empty() {
        let a = parts[0].start_byte();
        let b = parts[parts.len() - 1].end_byte();
        return slice(source, a, b).trim().to_string();
    }
    let raw = text(node, source);
    raw.trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .trim()
        .to_string()
}

#[cfg(test)]
#[path = "../../tests/unit/extract/mod.rs"]
mod tests;
