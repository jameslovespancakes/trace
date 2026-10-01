//! Declared and constructed types (SPEC §6.3, general fixes rule 10): `FileFacts::types`
//! with source `declared` / `constructed` (comment annotations: [`crate::annotations`]).
//!
//! * **Declared**: the type positions of the language — parameter, return, variable and field
//!   types (Scala, C#, Java, TypeScript, Go, Rust, C/C++, PHP, Python annotations), read with per-language tables (`SyntaxSpec::type_forms`, `SyntaxSpec::return_types`) of node kinds and
//!   picks.
//! * **Constructed**: a binding whose value constructs a type — `new Foo()` (every language
//!   with `new` syntax), Go `Foo{}` / `&Foo{}`, Rust `Foo { .. }`, `Foo::new()` (the
//!   convention of the constructor named `new`), and in the languages whose construction is
//!   call syntax (Python, Scala)
//!   `Foo(...)` when `Foo` is a type declared in the file or, for names declared elsewhere,
//!   follows the language's type naming convention (initial capital). Consumers resolve
//!   `type_name` against the index and ignore it when it does not name a type.
//!
//! Subjects: a name bound in a callable (parameters, locals) is `Var { Decl(callable) }`, in a
//! type body `Field { class }`, at file level `Var { Module }`; `self.x = ...` / `this.x = ...`
//! inside a method is `Field { enclosing type, x }`; return types are
//! `Return { decl }`. Type spellings: generic arguments removed, optional / nullable markers
//! stripped, unions split into one fact per alternative (`null` / `None` / `nil` /
//! `undefined` / `void` dropped), qualification kept (`pkg.Foo`, `a::B`, `A\B`), primitive,
//! array, tuple, map and function types skipped.

use std::collections::{HashMap, HashSet};

use trace_core::facts::{FileFacts, Scope, TypeFact, TypeSource, TypeSubject};
use trace_core::languages::{in_family, Family};
use trace_core::model::ByteSpan;
use trace_core::Language;
use tree_sitter::Node;

use crate::extract::{unwrap_node, DeclSyntax};
use crate::node::{has_direct_token, named_children, pick, span, text};
use crate::scopes::{binding_names, is_self, member_parts};
use crate::spec::SyntaxSpec;

/// A typed binding form: `(node kind, name pick, type pick, value pick)`. Picks follow the
/// spec selector syntax plus `^` (the parent; repeatable) and, for names, `*field` (every
/// child in that field).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Form {
    pub kind: &'static str,
    pub name: &'static str,
    pub ty: &'static str,
    pub value: &'static str,
}

pub(crate) const fn form(
    kind: &'static str,
    name: &'static str,
    ty: &'static str,
    value: &'static str,
) -> Form {
    Form {
        kind,
        name,
        ty,
        value,
    }
}

/// Type node kinds (substrings) that name no single nominal type: skipped.
const SKIPPED_TYPES: &[&str] = &[
    "array",
    "slice",
    "map_type",
    "tuple",
    "function_type",
    "dictionary",
    "channel",
    "object_type",
    "literal",
    "lambda",
    "intersection",
    "conditional_type",
    "index_type",
    "template_string",
    "struct_type",
    "interface_type",
    "type_query",
    "mapped_type",
    "primitive",
    "predefined",
    "integral",
    "floating_point",
    "boolean_type",
    "void_type",
    "builtin_type",
    "implicit_type",
    "placeholder_type",
    "sized_type",
    "parameter_list",
    "field_declaration_list",
    "unit_type",
];

/// Subtrees of a type spelling that are not part of its name (generic arguments, lifetimes,
/// qualifiers, annotations).
const NOT_NAME: &[&str] = &[
    "argument",
    "lifetime",
    "mutable_specifier",
    "type_parameters",
    "annotation",
    "modifier",
    "type_qualifier",
    "storage_class",
    "attribute",
];

/// Spellings that denote the absence of a value.
const NULLS: &[&str] = &["null", "undefined", "None", "NoneType", "nil", "void", "never", "Nothing"];

/// Implicitly typed declarations (`var x = ...`): no declared type.
const INFERRED: &[&str] = &["var", "val", "let", "auto", "dynamic"];

fn skipped_type(kind: &str) -> bool {
    SKIPPED_TYPES.iter().any(|s| kind.contains(s))
}

fn not_name(kind: &str) -> bool {
    NOT_NAME.iter().any(|s| kind.contains(s))
}

fn name_leaf(spec: &SyntaxSpec, kind: &str) -> bool {
    spec.is_name_like(kind) || kind.ends_with("identifier") || kind == "name" || kind == "constant"
}

/// The identifier naming a header base / trait / protocol spelling: its last name segment,
/// generic and call arguments skipped. `None` for a call (`extends mixin(B)`).
pub(crate) fn header_leaf<'t>(spec: &SyntaxSpec, node: Node<'t>) -> Option<Node<'t>> {
    let base = unwrap_node(spec, node);
    if spec.call_shape(base.kind()).is_some() {
        return None;
    }
    let mut last: Option<Node<'t>> = None;
    let mut stack = vec![base];
    let mut seen = 0usize;
    while let Some(n) = stack.pop() {
        seen += 1;
        if seen > 256 {
            break;
        }
        let kind = n.kind();
        if n.id() != base.id() && (not_name(kind) || kind.contains("argument")) {
            continue;
        }
        if n.named_child_count() == 0 {
            if n.is_named() && name_leaf(spec, kind) {
                last = Some(n);
            }
            continue;
        }
        let mut kids = named_children(n);
        kids.reverse();
        stack.extend(kids);
    }
    last
}

/// Apply a pick with `^` parent steps.
fn pick_rel<'t>(node: Node<'t>, selector: &str) -> Option<Node<'t>> {
    let mut current = node;
    let mut rest = selector;
    while let Some(r) = rest.strip_prefix('^') {
        current = current.parent()?;
        rest = r;
    }
    if rest.is_empty() {
        Some(current)
    } else {
        pick(current, rest)
    }
}

/// Where a typed name is bound.
enum Target {
    /// A bare binding name.
    Name(String),
    /// A field of the enclosing type (`self.x`, `this.x`).
    SelfField(String),
}

/// Shared context: declaration lookups, subjects and type spellings (also used by
/// [`crate::annotations`]).
pub(crate) struct Context<'a, 't> {
    pub spec: &'static SyntaxSpec,
    pub language: Language,
    pub source: &'a [u8],
    pub facts: &'a FileFacts,
    decls: &'a [DeclSyntax<'t>],
    /// `(start, end, decl)` of every declaration but `<module>`, sorted by start (outer
    /// first on ties).
    ranges: Vec<(u32, u32, u32)>,
    /// Names of the file's type declarations.
    file_types: HashSet<String>,
    /// Final declaration index -> index into `decls` (matched by name span start).
    syntax: HashMap<u32, usize>,
}

impl<'a, 't> Context<'a, 't> {
    pub(crate) fn new(
        spec: &'static SyntaxSpec,
        language: Language,
        source: &'a [u8],
        decls: &'a [DeclSyntax<'t>],
        facts: &'a FileFacts,
    ) -> Self {
        let mut ranges: Vec<(u32, u32, u32)> = Vec::with_capacity(facts.declarations.len());
        let mut file_types = HashSet::new();
        for (i, d) in facts.declarations.iter().enumerate() {
            if facts.module_decl == Some(i as u32) {
                continue;
            }
            ranges.push((d.span.bytes.start, d.span.bytes.end, i as u32));
            if d.kind.is_type() {
                file_types.insert(d.name.clone());
            }
        }
        ranges.sort_by_key(|&(s, e, i)| (s, std::cmp::Reverse(e), i));
        let by_name: HashMap<u32, usize> = decls
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name_node.start_byte() as u32, i))
            .collect();
        let mut syntax = HashMap::with_capacity(facts.declarations.len());
        for (i, d) in facts.declarations.iter().enumerate() {
            if let Some(&s) = by_name.get(&d.name_span.start) {
                syntax.insert(i as u32, s);
            }
        }
        Context {
            spec,
            language,
            source,
            facts,
            decls,
            ranges,
            file_types,
            syntax,
        }
    }

    /// Innermost declaration whose span contains byte `pos`.
    pub(crate) fn innermost(&self, pos: u32) -> Option<u32> {
        let hi = self.ranges.partition_point(|r| r.0 <= pos);
        self.ranges[..hi].iter().rev().find(|r| r.1 > pos).map(|r| r.2)
    }

    /// The nearest type declaration enclosing declaration `d` (itself included).
    pub(crate) fn enclosing_type(&self, d: u32) -> Option<u32> {
        let mut current = Some(d);
        for _ in 0..64 {
            let i = current?;
            let decl = self.facts.declarations.get(i as usize)?;
            if decl.kind.is_type() {
                return Some(i);
            }
            current = decl.parent;
        }
        None
    }

    /// The declaration syntax of final declaration `d`.
    pub(crate) fn syntax_of(&self, d: u32) -> Option<&DeclSyntax<'t>> {
        self.syntax.get(&d).and_then(|&s| self.decls.get(s))
    }

    /// The outermost non-synthetic declaration whose span starts at byte `start`.
    pub(crate) fn decl_starting_at(&self, start: u32) -> Option<u32> {
        let lo = self.ranges.partition_point(|r| r.0 < start);
        self.ranges[lo..]
            .iter()
            .take_while(|r| r.0 == start)
            .map(|r| r.2)
            .find(|&d| !self.facts.is_synthetic(d))
    }

    /// Subject of a name bound at byte `pos` (PHP field names drop their `$`: members are
    /// accessed as `$o->name`).
    pub(crate) fn subject(&self, pos: u32, name: String) -> TypeSubject {
        // A binding that names a declaration (`const f = () => ...`, a class field holding a
        // function) is bound in the declaration's enclosing scope.
        let innermost = self.innermost(pos).and_then(|d| {
            let decl = &self.facts.declarations[d as usize];
            if decl.name_span.start == pos {
                decl.parent
            } else {
                Some(d)
            }
        });
        match innermost {
            Some(d) => {
                let decl = &self.facts.declarations[d as usize];
                if decl.kind.is_callable() {
                    TypeSubject::Var {
                        scope: Scope::Decl(d),
                        name,
                    }
                } else if decl.kind.is_type() {
                    let name = if self.language == Language::Php {
                        name.trim_start_matches('$').to_string()
                    } else {
                        name
                    };
                    TypeSubject::Field { class: d, name }
                } else {
                    TypeSubject::Var {
                        scope: Scope::Module,
                        name,
                    }
                }
            }
            None => TypeSubject::Var {
                scope: Scope::Module,
                name,
            },
        }
    }

    /// Subject of a field written through the self reference at byte `pos`.
    fn self_field(&self, pos: u32, name: String) -> Option<TypeSubject> {
        let d = self.innermost(pos)?;
        let class = self.enclosing_type(d)?;
        Some(TypeSubject::Field { class, name })
    }

    fn target_subject(&self, pos: u32, target: Target) -> Option<TypeSubject> {
        match target {
            Target::Name(name) => Some(self.subject(pos, name)),
            Target::SelfField(name) => self.self_field(pos, name),
        }
    }

    /// `self.x` / `this.x`: the field name.
    fn self_member(&self, node: Node<'t>) -> Option<String> {
        let (object, property) = member_parts(self.spec, self.language, node)?;
        let object = unwrap_node(self.spec, object?);
        if !is_self(self.spec, object, self.source) {
            return None;
        }
        let name = text(property, self.source).trim().to_string();
        (!name.is_empty()).then_some(name)
    }

    /// Declared names of a C / C++ declarator (`*p`, `&r`, `x = v`, `a[3]`); `None` for a
    /// function declarator (a prototype, not a variable).
    fn c_declarator(&self, node: Node<'t>) -> Option<Node<'t>> {
        let mut current = node;
        for _ in 0..16 {
            let kind = current.kind();
            if kind == "function_declarator" || kind == "abstract_function_declarator" {
                return None;
            }
            if current.named_child_count() == 0 || self.spec.is_identifier(kind) {
                return name_leaf(self.spec, kind).then_some(current);
            }
            if !kind.ends_with("declarator") {
                return None;
            }
            current = current
                .child_by_field_name("declarator")
                .or_else(|| named_children(current).into_iter().last())?;
        }
        None
    }

    /// Names bound by a form node.
    fn names_of(&self, node: Node<'t>, form: &Form) -> Vec<(Node<'t>, Target)> {
        let nodes: Vec<Node<'t>> = if let Some(field) = form.name.strip_prefix('*') {
            let mut cursor = node.walk();
            let found: Vec<Node<'t>> = node.children_by_field_name(field, &mut cursor).collect();
            found
        } else {
            pick_rel(node, form.name).into_iter().collect()
        };
        // Lists of targets (`a, b = ...`) bind pairwise.
        let mut expanded: Vec<Node<'t>> = Vec::new();
        for n in nodes {
            if self.spec.lists.contains(&n.kind()) {
                expanded.extend(named_children(n));
            } else {
                expanded.push(n);
            }
        }
        let mut out = Vec::new();
        for n in expanded {
            let mut u = unwrap_node(self.spec, n);
            // Transparent single-child target wrappers (`directly_assignable_expression`).
            for _ in 0..3 {
                let kids = named_children(u);
                let wrapper = kids.len() == 1
                    && u.named_child_count() == 1
                    && !self.spec.is_identifier(u.kind())
                    && !name_leaf(self.spec, u.kind())
                    && member_parts(self.spec, self.language, u).is_none()
                    && self.spec.call_shape(u.kind()).is_none();
                if !wrapper {
                    break;
                }
                u = unwrap_node(self.spec, kids[0]);
            }
            if let Some(field) = self.self_member(u) {
                out.push((u, Target::SelfField(field)));
                continue;
            }
            if in_family(self.language, Family::C) {
                if let Some(leaf) = self.c_declarator(u) {
                    let name = text(leaf, self.source).trim().to_string();
                    out.push((leaf, Target::Name(name)));
                }
                continue;
            }
            if u.named_child_count() == 0 || self.spec.is_identifier(u.kind()) {
                let kind = u.kind();
                if name_leaf(self.spec, kind) && !is_self(self.spec, u, self.source) {
                    let name = text(u, self.source).trim().to_string();
                    if !name.is_empty() {
                        out.push((u, Target::Name(name)));
                    }
                }
                continue;
            }
            if member_parts(self.spec, self.language, u).is_some() {
                continue;
            }
            for leaf in binding_names(self.spec, self.language, u, self.source) {
                let name = text(leaf, self.source).trim().to_string();
                if !name.is_empty() {
                    out.push((leaf, Target::Name(name)));
                }
            }
        }
        out
    }

    /// Subjects bound by the first typed binding form in `node`'s subtree (breadth-first,
    /// three levels): the binding a comment annotation attaches to.
    pub(crate) fn binding_subjects(&self, node: Node<'t>) -> Vec<(TypeSubject, ByteSpan)> {
        let forms = self.spec.type_forms;
        let mut level = vec![node];
        for _ in 0..3 {
            let mut next = Vec::new();
            for n in &level {
                for f in forms.iter().filter(|f| f.kind == n.kind()) {
                    let names = self.names_of(*n, f);
                    let subjects: Vec<(TypeSubject, ByteSpan)> = names
                        .into_iter()
                        .filter_map(|(leaf, target)| {
                            let at = leaf.start_byte() as u32;
                            self.target_subject(at, target).map(|s| (s, span(leaf)))
                        })
                        .collect();
                    if !subjects.is_empty() {
                        return subjects;
                    }
                }
                next.extend(named_children(*n));
            }
            level = next;
        }
        Vec::new()
    }

    /// Type names spelled by a type node (unions split, see module docs).
    pub(crate) fn spell(&self, node: Node<'t>) -> Vec<(String, ByteSpan)> {
        let mut out = Vec::new();
        self.spell_into(node, 0, &mut out);
        out
    }

    fn spell_into(&self, node: Node<'t>, depth: usize, out: &mut Vec<(String, ByteSpan)>) {
        if depth > 8 {
            return;
        }
        let kind = node.kind();
        if skipped_type(kind) {
            return;
        }
        // Unions.
        let python_union =
            self.language == Language::Python && kind == "binary_operator" && has_direct_token(node, "|");
        if kind == "union_type" || python_union {
            for child in named_children(node) {
                self.spell_into(child, depth + 1, out);
            }
            return;
        }
        if self.language == Language::Python {
            if kind == "subscript" || kind == "generic_type" {
                self.python_subscript(node, depth, out);
                return;
            }
            if kind == "string" {
                self.python_string(node, out);
                return;
            }
        }
        // Single-child wrappers (annotations, optional / nullable markers, descriptors).
        let kids: Vec<Node<'t>> = named_children(node)
            .into_iter()
            .filter(|c| !not_name(c.kind()))
            .collect();
        if kids.len() == 1 && !name_leaf(self.spec, kind) {
            self.spell_into(kids[0], depth + 1, out);
            return;
        }
        if let Some(name) = self.path_spelling(node) {
            if !NULLS.contains(&name.as_str()) && !INFERRED.contains(&name.as_str()) {
                out.push((name, span(node)));
            }
        }
    }

    /// Python `Optional[X]` -> X, `Union[A, B]` -> A, B, `Annotated[X, ...]` -> X, other
    /// generics -> their base (`list[X]` -> `list`). Annotations parse as `generic_type`
    /// (base + `type_parameter` list); expressions as `subscript` (`value` + `subscript`).
    fn python_subscript(&self, node: Node<'t>, depth: usize, out: &mut Vec<(String, ByteSpan)>) {
        let (value, args) = if node.kind() == "generic_type" {
            let kids = named_children(node);
            let Some(value) = kids.first().copied() else {
                return;
            };
            let args: Vec<Node<'t>> = kids
                .iter()
                .filter(|k| k.kind() == "type_parameter")
                .flat_map(|k| named_children(*k))
                .collect();
            (value, args)
        } else {
            let Some(value) = node.child_by_field_name("value") else {
                return;
            };
            let mut cursor = node.walk();
            let args: Vec<Node<'t>> = node.children_by_field_name("subscript", &mut cursor).collect();
            (value, args)
        };
        let Some(base) = self.path_spelling(value) else {
            return;
        };
        let last = base.rsplit('.').next().unwrap_or(&base);
        match last {
            "Optional" | "Union" => {
                for a in args {
                    self.spell_into(a, depth + 1, out);
                }
            }
            "Annotated" => {
                if let Some(first) = args.first() {
                    self.spell_into(*first, depth + 1, out);
                }
            }
            _ => out.push((base, span(value))),
        }
    }

    /// Python forward references (`"Foo"`, `"pkg.Foo"`): the literal when it is a dotted name.
    fn python_string(&self, node: Node<'t>, out: &mut Vec<(String, ByteSpan)>) {
        let Some(content) = named_children(node)
            .into_iter()
            .find(|c| c.kind() == "string_content")
        else {
            return;
        };
        let value = text(content, self.source).trim().to_string();
        let dotted = !value.is_empty()
            && value
                .split('.')
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_alphanumeric() || c == '_'))
            && !value.starts_with(|c: char| c.is_ascii_digit());
        if dotted && !NULLS.contains(&value.as_str()) {
            out.push((value, span(content)));
        }
    }

    /// The qualified name spelled by a type node: its name leaves joined with the
    /// language's separator (generic arguments and qualifiers skipped).
    fn path_spelling(&self, node: Node<'t>) -> Option<String> {
        if skipped_type(node.kind()) {
            return None;
        }
        let mut parts: Vec<String> = Vec::new();
        let mut stack = vec![node];
        let mut seen = 0usize;
        while let Some(n) = stack.pop() {
            seen += 1;
            if seen > 128 {
                return None;
            }
            let kind = n.kind();
            if n.id() != node.id() && (skipped_type(kind) || not_name(kind)) {
                continue;
            }
            if n.named_child_count() == 0 || (self.spec.is_identifier(kind) && n.id() != node.id()) {
                if n.is_named() && name_leaf(self.spec, kind) {
                    let t = text(n, self.source).trim().trim_start_matches('\\').to_string();
                    if !t.is_empty() {
                        parts.push(t);
                    }
                }
                continue;
            }
            let mut kids = named_children(n);
            kids.reverse();
            stack.extend(kids);
        }
        if parts.is_empty() || parts.len() > 8 {
            return None;
        }
        Some(parts.join(self.spec.type_path_separator))
    }

    /// The type a value constructs (module docs), with the span of its type spelling.
    fn constructed(&self, value: Node<'t>, pos: u32) -> Option<(String, ByteSpan)> {
        let v = unwrap_node(self.spec, value);
        let kind = v.kind();
        if self.language == Language::Go && kind == "unary_expression" && has_direct_token(v, "&") {
            let operand = v.child_by_field_name("operand")?;
            return self.constructed(operand, pos);
        }
        if let Some(lit) = self.spec.literal_allocations.iter().find(|l| l.kind == kind) {
            return self.spell_first(pick(v, lit.first)?, pos);
        }
        if self.language == Language::Rust && kind == "struct_expression" {
            return self.spell_first(v.child_by_field_name("name")?, pos);
        }
        let shape = self.spec.call_shape(kind)?;
        if shape.is_new {
            return self.spell_first(pick(v, shape.function_field)?, pos);
        }
        if !shape.receiver_field.is_empty() {
            // `Foo::new(...)` / `Foo.new(...)` with a receiver.
            let method = pick(v, shape.function_field)?;
            let receiver = pick(v, shape.receiver_field)?;
            if text(method, self.source).trim() == "new" {
                return self.spell_first(receiver, pos);
            }
            return None;
        }
        let callee = unwrap_node(self.spec, pick(v, shape.function_field)?);
        // `Foo::new()`, `Foo.new()`, `Foo:new()`.
        if let Some(m) = self.spec.member(callee.kind()) {
            if let Some(property) = pick(callee, m.property_field) {
                if text(property, self.source).trim() == "new" {
                    return self.spell_first(pick(callee, m.object_field)?, pos);
                }
            }
        }
        if !self.spec.constructs_by_call {
            return None;
        }
        let chain = crate::names::chain(self.spec, callee, self.source)?;
        let last = chain.rest.last().unwrap_or(&chain.root).clone();
        let type_like =
            self.file_types.contains(&last) || last.chars().next().is_some_and(|c| c.is_uppercase());
        if !type_like {
            return None;
        }
        let mut path = chain.root;
        for part in chain.rest {
            path.push('.');
            path.push_str(&part);
        }
        Some((path, span(callee)))
    }

    /// The first spelling of a type node; `Self` names the enclosing container / type.
    fn spell_first(&self, node: Node<'t>, pos: u32) -> Option<(String, ByteSpan)> {
        let (name, at) = self.spell(node).into_iter().next()?;
        if name == "Self" {
            let d = self.innermost(pos)?;
            let decl = &self.facts.declarations[d as usize];
            let own = decl.container.clone().or_else(|| {
                self.enclosing_type(d)
                    .map(|t| self.facts.declarations[t as usize].name.clone())
            })?;
            return Some((own, at));
        }
        Some((name, at))
    }

    /// Return-type node of callable declaration `d`.
    fn return_node(&self, d: u32) -> Option<Node<'t>> {
        let syntax = self.syntax_of(d)?;
        if syntax.synthetic.is_some() {
            return None;
        }
        let table = self.spec.return_types;
        let mut candidates = vec![syntax.def];
        if let Some(callable) = syntax.body.and_then(|b| b.parent()) {
            if callable.id() != syntax.def.id() {
                candidates.push(callable);
            }
        }
        for node in candidates {
            for (_, sel) in table.iter().filter(|(k, _)| *k == node.kind()) {
                let found = pick(node, sel);
                if found.is_some() {
                    return found;
                }
            }
        }
        None
    }

    /// Declared and constructed type facts of the whole tree.
    pub(crate) fn extract(&self, root: Node<'t>) -> Vec<TypeFact> {
        let mut out: Vec<TypeFact> = Vec::new();
        let forms = self.spec.type_forms;
        if !forms.is_empty() {
            let mut stack = vec![root];
            while let Some(node) = stack.pop() {
                if self.spec.is_comment(node.kind()) {
                    continue;
                }
                for f in forms.iter().filter(|f| f.kind == node.kind()) {
                    self.form_facts(node, f, &mut out);
                }
                let mut kids = named_children(node);
                kids.reverse();
                stack.extend(kids);
            }
        }
        for (i, d) in self.facts.declarations.iter().enumerate() {
            let i = i as u32;
            if !d.kind.is_callable() || self.facts.is_synthetic(i) {
                continue;
            }
            let Some(node) = self.return_node(i) else {
                continue;
            };
            for (type_name, at) in self.spell(node) {
                // `-> Self` names the implementing / enclosing type.
                let type_name = if type_name == "Self" {
                    match d.container.clone().or_else(|| {
                        self.enclosing_type(i)
                            .map(|t| self.facts.declarations[t as usize].name.clone())
                    }) {
                        Some(own) => own,
                        None => continue,
                    }
                } else {
                    type_name
                };
                out.push(TypeFact {
                    subject: TypeSubject::Return { decl: i },
                    type_name,
                    span: at,
                    source: TypeSource::Declared,
                });
            }
        }
        out
    }

    fn form_facts(&self, node: Node<'t>, f: &Form, out: &mut Vec<TypeFact>) {
        let names = self.names_of(node, f);
        if names.is_empty() {
            return;
        }
        let types = if f.ty.is_empty() {
            Vec::new()
        } else {
            pick_rel(node, f.ty).map(|t| self.spell(t)).unwrap_or_default()
        };
        let name_end = names.iter().map(|(n, _)| n.end_byte()).max().unwrap_or(0);
        let values: Vec<Node<'t>> = if f.value.is_empty() {
            Vec::new()
        } else {
            match pick_rel(node, f.value) {
                Some(v) if v.start_byte() >= name_end => {
                    if self.spec.lists.contains(&v.kind()) {
                        named_children(v)
                    } else {
                        vec![v]
                    }
                }
                _ => Vec::new(),
            }
        };
        let count = names.len();
        for (i, (leaf, target)) in names.into_iter().enumerate() {
            let at = leaf.start_byte() as u32;
            let Some(subject) = self.target_subject(at, target) else {
                continue;
            };
            for (type_name, s) in &types {
                out.push(TypeFact {
                    subject: subject.clone(),
                    type_name: type_name.clone(),
                    span: *s,
                    source: TypeSource::Declared,
                });
            }
            let value = if values.len() == count {
                values.get(i).copied()
            } else {
                None
            };
            if let Some((type_name, s)) = value.and_then(|v| self.constructed(v, at)) {
                out.push(TypeFact {
                    subject,
                    type_name,
                    span: s,
                    source: TypeSource::Constructed,
                });
            }
        }
    }
}

/// Declared and constructed types of a file (module docs).
pub(crate) fn extract<'t>(ctx: &Context<'_, 't>, root: Node<'t>) -> Vec<TypeFact> {
    ctx.extract(root)
}

#[cfg(test)]
#[path = "../tests/unit/typefacts.rs"]
mod tests;
