//! Collection entry point (the walk over one tree) and the declaration syntax lookups
//! shared with lowering (header type references, error counts).

use std::collections::{HashMap, HashSet};

use trace_core::facts::{FileFacts, Reference, Scope};
use trace_core::model::ByteSpan;
use trace_core::text::LineIndex;
use trace_core::Language;
use tree_sitter::Node;

use super::{calls::unwrap_node, query::run_query, query::Captures, Collected, DeclSyntax, Walker};
use crate::grammar::Grammar;
use crate::names::Binding;
use crate::node::{is_pick, span, text, type_name};
use crate::spec::SyntaxSpec;

/// Walk the tree and build declarations, calls, references, callbacks, assignments.
/// Returns the syntax of every declaration (same order) for lowering.
pub(crate) fn collect<'t>(
    grammar: &Grammar,
    root: Node<'t>,
    source: &[u8],
    lines: &LineIndex,
    caps: &Captures<'t>,
    facts: &mut FileFacts,
) -> Collected<'t> {
    let mut walker = Walker::new(grammar.spec, grammar.language, source, lines, caps);
    walker.run(root, facts);
    let Walker {
        decls,
        mut idents,
        calls,
        chains,
        names,
        bare_refs,
        export_nodes,
        exports,
        py_bindings,
        ..
    } = walker;
    idents.sort_unstable();
    // Re-exports: import readers (Rust `pub use`), then JS/TS `export`
    // statements (`export {a as b} from`, `export * from`, `export {x}` of an imported x).
    facts.exports.extend(exports);
    for node in export_nodes {
        for e in crate::names::es_exports(node, source) {
            let target = match &e.specifier {
                Some(spec) if e.name.is_empty() => spec.clone(),
                Some(spec) => format!("{spec}.{}", e.name),
                None => match facts
                    .imports
                    .iter()
                    .find(|i| i.scope == Scope::Module && i.local == e.name)
                {
                    Some(import) => import.target.clone(),
                    None => continue,
                },
            };
            let at = span(e.node);
            facts.exports.push(trace_core::facts::Export {
                exported: e.exported,
                target,
                span: at,
                line: lines.line1(at.start),
            });
        }
    }
    // `Names` follows the Python scoping rule; other languages keep `local = false`.
    if grammar.language == Language::Python {
        for (i, scope) in bare_refs {
            let r = &facts.references[i];
            let binding = names.binding(&r.name, scope, &facts.declarations);
            facts.references[i].local = binding != Binding::Other;
        }
    }
    // Local bindings (`FileFacts::local_spans`) and member accesses (SPEC 6.3, general
    // fixes). Python keeps its scoping rule (`Names::binding`, `Reference::local` above):
    // its local spans are the local references, the binding identifiers (store targets,
    // parameters) and the bare callees that resolve to a local. Every other grammar is
    // resolved by `crate::scopes` (block scopes, `let/val/var`, patterns, parameters).
    let python_locals = (grammar.language == Language::Python).then(|| {
        let mut spans: Vec<ByteSpan> = facts.references.iter().filter(|r| r.local).map(|r| r.span).collect();
        for (at, name, scope) in &py_bindings {
            if names.binding(name, *scope, &facts.declarations) != Binding::Other {
                spans.push(*at);
            }
        }
        for call in &calls {
            let callee = unwrap_node(grammar.spec, call.callee);
            if !grammar.spec.is_identifier(callee.kind()) {
                continue;
            }
            let name = text(callee, source);
            if names.binding(name.trim(), call.scope, &facts.declarations) != Binding::Other {
                spans.push(span(callee));
            }
        }
        spans
    });
    crate::scopes::run(grammar.spec, grammar.language, root, source, &decls, facts, python_locals);
    header_type_references(grammar.spec, grammar.language, caps, &decls, source, facts);
    Collected {
        decls,
        idents,
        calls,
        chains,
        names,
    }
}

/// Header type references (SPEC 6.3, general fixes rule 6): every base / trait / protocol
/// spelling of a declaration header (`extends`, `implements`, `:` conformance, base lists,
/// `impl Trait for Type`, `extension T: P`, `instance C T`) has a `type` reference at its name
/// identifier (the last name segment, generic arguments skipped), so the engine asks
/// `definition` for it: a compiler fact for base resolution across files, crates and
/// packages. An existing `read` reference there becomes `type`. Python keeps its bases as
/// `read` references (runtime value expressions that feed the class hierarchy).
fn header_type_references<'t>(
    spec: &SyntaxSpec,
    language: Language,
    caps: &Captures<'t>,
    decls: &[DeclSyntax<'t>],
    source: &[u8],
    facts: &mut FileFacts,
) {
    use trace_core::facts::RefKind;
    if language == Language::Python {
        return;
    }
    let mut sites: Vec<(Node<'t>, Option<u32>)> = Vec::new();
    for (i, syntax) in decls.iter().enumerate() {
        if syntax.bases.is_empty() || syntax.synthetic.is_some() {
            continue;
        }
        // The header executes where the declaration is evaluated: an enclosing callable,
        // else module / class-body code (`None`).
        let owner = facts.declarations.get(i).and_then(|d| d.parent).filter(|&p| {
            facts
                .declarations
                .get(p as usize)
                .is_some_and(|d| d.kind.is_callable())
        });
        for base in &syntax.bases {
            sites.push((*base, owner));
        }
    }
    for (type_node, trait_node) in &caps.impls {
        sites.push((*trait_node, None));
        sites.push((*type_node, None));
    }
    if sites.is_empty() {
        return;
    }
    let existing: HashMap<(u32, u32), usize> = facts
        .references
        .iter()
        .enumerate()
        .map(|(i, r)| ((r.span.start, r.span.end), i))
        .collect();
    let mut added: Vec<Reference> = Vec::new();
    let mut seen: HashSet<(u32, u32)> = HashSet::new();
    for (node, owner) in sites {
        let Some(leaf) = crate::typefacts::header_leaf(spec, node) else {
            continue;
        };
        let at = span(leaf);
        let name = text(leaf, source).trim().to_string();
        if name.is_empty() || !seen.insert((at.start, at.end)) {
            continue;
        }
        if let Some(&i) = existing.get(&(at.start, at.end)) {
            if facts.references[i].kind == RefKind::Read {
                facts.references[i].kind = RefKind::Type;
            }
            continue;
        }
        added.push(Reference {
            span: at,
            name,
            owner,
            in_decorator: false,
            local: false,
            kind: RefKind::Type,
        });
    }
    if added.is_empty() {
        return;
    }
    // Merge by span start, keeping the existing (pre-order) order of equal starts.
    added.sort_by_key(|r| (r.span.start, r.span.end));
    let old = std::mem::take(&mut facts.references);
    let mut merged = Vec::with_capacity(old.len() + added.len());
    let mut new = added.into_iter().peekable();
    for r in old {
        while new.peek().is_some_and(|n| n.span.start < r.span.start) {
            merged.extend(new.next());
        }
        merged.push(r);
    }
    merged.extend(new);
    facts.references = merged;
}

/// C++ out-of-line declarators (`int a::B::f()`): the scope segments before the innermost
/// one (the container), outermost first (`[a]`), so the qualified name is the namespace /
/// class path of the in-class declaration (`a.B.f`).
pub(super) fn cpp_scope_prefix(name: Node<'_>, source: &[u8]) -> Vec<String> {
    let mut segments: Vec<String> = Vec::new();
    let mut child = name;
    while let Some(parent) = child.parent() {
        if parent.kind() != "qualified_identifier" || !is_pick(parent, "name", child) {
            break;
        }
        if let Some(scope) = parent.child_by_field_name("scope") {
            let segment = type_name(scope, source);
            if !segment.is_empty() {
                segments.push(segment);
            }
        }
        child = parent;
        if segments.len() > 16 {
            break;
        }
    }
    segments.reverse();
    segments.pop();
    segments
}

/// Number of ERROR / MISSING nodes of a tree (bounded walk).
pub(crate) fn error_nodes(root: Node<'_>) -> usize {
    let mut count = 0usize;
    let mut cursor = root.walk();
    let mut visited = 0usize;
    loop {
        let node = cursor.node();
        visited += 1;
        if node.is_error() || node.is_missing() {
            count += 1;
        }
        if visited < 2_000_000 && node.has_error() && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return count;
            }
        }
    }
}

/// Declaration syntax for already-extracted facts (used by the public `lower_file`): the
/// query is re-run and definitions are matched to declarations by their name span.
pub(crate) fn decl_syntax_for<'t>(
    grammar: &Grammar,
    root: Node<'t>,
    source: &[u8],
    facts: &FileFacts,
) -> Vec<Option<DeclSyntax<'t>>> {
    let caps = run_query(grammar, root, source);
    let lines = LineIndex::new(source);
    let mut scratch = FileFacts::default();
    let decls = collect(grammar, root, source, &lines, &caps, &mut scratch).decls;
    let by_name: HashMap<u32, usize> = scratch
        .declarations
        .iter()
        .enumerate()
        .map(|(i, d)| (d.name_span.start, i))
        .collect();
    let mut slots: Vec<Option<DeclSyntax<'t>>> = decls.into_iter().map(Some).collect();
    facts
        .declarations
        .iter()
        .enumerate()
        .map(|(i, d)| {
            if facts.module_decl == Some(i as u32) {
                // The synthetic `<module>` declaration has no syntax node of its own.
                return None;
            }
            by_name.get(&d.name_span.start).and_then(|&s| slots[s].take())
        })
        .collect()
}
