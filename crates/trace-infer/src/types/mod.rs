//! Receiver types from syntax (general fixes, rules 10 and 11; SPEC §7.11).
//!
//! Shared by syntax-only narrowing (`narrow`) and the `uses` name scan (`unrelated_type`,
//! SPEC §10.1). Input: `FileFacts::types` (declared types, constructed values, comment
//! annotations: JSDoc, PHPDoc), `FileFacts::member_accesses`, `FileFacts::call_details`
//! receivers, flow `Bind` facts for simple aliases (`const p = getPicker()` + `@returns
//! {Picker}` on `getPicker`), and the hierarchy.
//!
//! Resolution of a receiver `r` of the member access at `member_span`:
//! 0. `r` is an identifier the language server resolved to type declarations (a proven
//!    `references` edge at exactly its span, every target a type: `Widget.refresh()`) ->
//!    those types plus the types declared inside them (companion objects) and same-named
//!    types of their files (Scala companions); never for a type whose bases include a
//!    keyword spelling (`metaclass=`: class-level lookups may reach the metaclass);
//! 1. `r` is the self reference -> the enclosing type (and, for dispatch, its subtypes);
//!    inside an anonymous function that declares a parameter spelled like the self
//!    reference (Python `lambda self: ...` passed as a callback), the name is that parameter, bound by the caller -> unknown;
//! 2. `r` is a plain name -> the `TypeFact`s of `Var { scope, name }` in the nearest scope
//!    binding it (parameter / local / module variable), else the return-type facts of the
//!    callable whose call result was bound to it;
//! 3. `r` is a call `f(...)` / `new T()` -> the return-type facts of `f` / `T`;
//! 4. `r` is `x.field` -> the `Field` facts of `x`'s type.
//!
//! Spelled type names resolve to index types with the family rule's base resolution (same
//! file, then imports, then a globally unique name in the language namespace); a spelling
//! that names no index type is [`ReceiverType::External`]. Conflicting facts (two unrelated
//! types) give [`ReceiverType::Unknown`]: when in doubt, nothing is excluded.
//!
//! Details (normative for this implementation):
//! * The receiver expression is taken from the call whose callee ends at the member
//!   (`CallDetail::receiver`), else from the flow expression whose attribute identifier is
//!   the member (`Expr::Attr::attr_span`), else from the member access fact (the self
//!   reference, or a receiver root that is not itself the end of a longer member chain).
//! * The nearest binding scope wins: a scope that binds the name (a `Var` type fact, a flow
//!   `Bind`, a parameter, an import) decides alone, even when it proves nothing (an
//!   unannotated parameter, an import, an opaque value -> `Unknown`). Declared and comment
//!   annotations bound every value of the variable; otherwise every bound value must have a
//!   known type (constructed facts, `new T()`, a type called as a constructor where the
//!   language constructs by call, `T.new` / `T::new()`, calls of callables with return-type
//!   facts, aliases), else `Unknown`. Implicit-receiver languages also read `Field` facts
//!   of the enclosing types and their bases (the nearest definer in base order) for bare
//!   names.
//! * Top types, `self` types and generic parameters (`any`, `Object`, `table`, `T`) prove
//!   nothing (`Unknown`); a `self` / `Self` return type is the declaring type. A spelling
//!   shaped like a generic parameter (one or two capitals / digits: `T`, `DB`) names a type
//!   only when the file, an import or the package binds it (never by the unique-name rule;
//!   unbound it is `Unknown`, not external).
//! * [`unrelated_to_family`] never excludes through structural typing (Go, TypeScript,
//!   JavaScript, Python protocols and interfaces), dereference forwarding (Rust `Deref`, C++
//!   smart pointers: receivers whose types have unresolved bases, external receivers), or
//!   mixins that syntax does not record (PHP traits).

use std::collections::{BTreeSet, HashMap, HashSet};

use trace_core::facts::{
    BindTarget, Expr, FileFacts, FlowFact, ImportKind, Scope, TypeFact, TypeSource, TypeSubject,
};
use trace_core::{ByteSpan, EdgeKind, FileId, Index, Language, SymbolId, SymbolKind, Tier};
use trace_syntax::language_rules::rules;

use crate::hierarchy::{base_name, is_anonymous, is_constructor, Hierarchy};
use crate::narrow::{name_interop, ModuleMap};

mod exprs;
mod relations;

/// What syntax proves about a receiver's type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiverType {
    /// Nothing is known (no fact, or conflicting facts).
    Unknown,
    /// In-index type symbols (sorted, unique).
    Index(Vec<SymbolId>),
    /// A spelled type that names no index type (library / builtin type), as written.
    External(String),
}

/// The receiver type of the member access whose member identifier is `member_span` in
/// `file` (module docs).
pub fn receiver_type(
    index: &Index,
    hierarchy: &Hierarchy,
    file: FileId,
    member_span: ByteSpan,
) -> ReceiverType {
    Types::new(index, hierarchy).receiver_type(file, member_span)
}

/// Whether a receiver of type `ty` can never dispatch to a member of `family`:
/// * `Index(types)`: no type is the declaring type of a family member, one of its bases,
///   or one of its subtypes (hierarchy, transitively);
/// * `External(_)`: no family member's declaring type (or any of its bases, transitively)
///   has a base spelling that did not resolve to an index type (such a type may extend the
///   external type);
/// * `Unknown`: never.
///
/// Guards (module docs): structural typing, dereference forwarding and unrecorded mixins
/// never count as unrelated; a family member without a declaring type is unrelated only
/// when its container is annotated with a class name (a comment annotation) that differs.
pub fn unrelated_to_family(
    index: &Index,
    hierarchy: &Hierarchy,
    ty: &ReceiverType,
    family: &[SymbolId],
) -> bool {
    Types::new(index, hierarchy).unrelated_to_family(ty, family)
}

/// Languages whose type compatibility is structural (an object of an unrelated declared
/// type may still provide the member): Go interfaces, TypeScript / JavaScript, Python
/// protocols and duck typing.
fn structural(language: Language) -> bool {
    rules(language).structural_typing
}

/// Languages whose receivers forward member calls through dereference (`Deref`, `->`).
fn forwards_by_deref(language: Language) -> bool {
    rules(language).deref_forwarding
}

/// Languages where a bare name inside a type body reads a field of the implicit receiver.
fn implicit_receiver(language: Language) -> bool {
    rules(language).implicit_field_receiver
}

/// Languages where calling a type's name constructs an instance (`Foo()`).
fn constructs_by_call(language: Language) -> bool {
    rules(language).calls_construct
}

/// Languages whose files are grouped in packages by directory (package-private names are
/// visible without an import).
fn package_directories(language: Language) -> bool {
    rules(language).package_private_by_directory
}

/// Spellings that never prove a type: top types, `self` types, generic parameters.
fn uninformative(name: &str) -> bool {
    matches!(
        name,
        "any"
            | "Any"
            | "unknown"
            | "mixed"
            | "object"
            | "Object"
            | "table"
            | "dynamic"
            | "AnyObject"
            | "AnyRef"
            | "AnyVal"
            | "interface"
            | "var"
            | "auto"
            | "self"
            | "Self"
            | "static"
            | "this"
            | "$this"
            | "void"
            | "nil"
            | "null"
            | "undefined"
            | "never"
            | "Nothing"
            | "Unit"
            | "function"
            | "Function"
            | "userdata"
            | "lightuserdata"
            | "thread"
    ) || generic_look(name)
}

/// A spelling shaped like a generic parameter (`T`, `K2`, `DB`): it proves a type only when
/// it names a type bound in scope (same file, an import, the same package), never through the
/// repository-wide unique-name rule.
fn generic_look(name: &str) -> bool {
    name.len() <= 2 && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// The self reference of a language (`self`, `this`, `$this`; `cls` where it is a self name
/// of the language: Python).
fn is_self_name(language: Language, name: &str) -> bool {
    match name {
        "self" | "this" | "$this" => true,
        "cls" => trace_syntax::syntax(language).is_some_and(|s| s.self_names.contains(&"cls")),
        _ => false,
    }
}

/// A variable name without its sigil (`$p` -> `p`).
fn bare(name: &str) -> &str {
    name.strip_prefix('$').unwrap_or(name)
}

fn same_var(a: &str, b: &str) -> bool {
    bare(a) == bare(b)
}

/// `Foo::new` -> (`Foo`, `new`); `a.b` -> (`a`, `b`); `f` -> (None, `f`).
fn split_path(name: &str) -> (Option<&str>, &str) {
    let name = name.trim();
    for sep in ["::", "\\", "."] {
        if let Some((q, last)) = name.rsplit_once(sep) {
            let q = q.trim();
            if !q.is_empty() && !last.is_empty() {
                return (Some(q), last.trim());
            }
        }
    }
    (None, name)
}

/// Where the receiver of a member access comes from.
enum Receiver<'f> {
    SelfRef,
    Name(String),
    Expr(&'f Expr),
    Unknown,
}

/// Every attribute expression inside `expr`: attribute identifier span -> object.
fn collect_attrs<'e>(expr: &'e Expr, out: &mut HashMap<(u32, u32), &'e Expr>, depth: u32) {
    if depth > 64 {
        return;
    }
    match expr {
        Expr::Attr {
            object, attr_span, ..
        } => {
            out.entry((attr_span.start, attr_span.end)).or_insert(object.as_ref());
            collect_attrs(object, out, depth + 1);
        }
        Expr::Call {
            func, args, kwargs, ..
        } => {
            collect_attrs(func, out, depth + 1);
            for a in args {
                collect_attrs(a, out, depth + 1);
            }
            for (_, v) in kwargs {
                collect_attrs(v, out, depth + 1);
            }
        }
        Expr::Choice(alts) => {
            for a in alts {
                collect_attrs(a, out, depth + 1);
            }
        }
        Expr::Await(inner) => collect_attrs(inner, out, depth + 1),
        _ => {}
    }
}

/// Type facts and bound values of one variable.
type VarFacts<'i> = (Vec<&'i TypeFact>, Vec<&'i Expr>);

/// Per-file lookup tables (built once per file).
struct FileTables<'i> {
    /// Callee end -> (callee start, receiver expression) of calls with a receiver.
    calls: HashMap<u32, (u32, &'i Expr)>,
    /// Attribute identifier span -> object expression (flow expressions).
    attrs: HashMap<(u32, u32), &'i Expr>,
    /// (scope, variable name without sigil) -> (type facts, bound values).
    vars: HashMap<(Scope, String), VarFacts<'i>>,
}

impl<'i> FileTables<'i> {
    fn build(facts: &'i FileFacts) -> FileTables<'i> {
        let mut calls = HashMap::new();
        for (ci, c) in facts.calls.iter().enumerate() {
            if let Some(r) = facts.call_detail(ci).and_then(|d| d.receiver.as_ref()) {
                calls.entry(c.callee_span.end).or_insert((c.callee_span.start, r));
            }
        }
        let mut attrs = HashMap::new();
        let mut vars: HashMap<(Scope, String), VarFacts<'i>> = HashMap::new();
        for fact in &facts.flow {
            match fact {
                FlowFact::Bind { target, value, .. } => {
                    match target {
                        BindTarget::FieldOf { object, .. } => collect_attrs(object, &mut attrs, 0),
                        BindTarget::Var { scope, name } => vars
                            .entry((*scope, bare(name).to_string()))
                            .or_default()
                            .1
                            .push(value),
                        _ => {}
                    }
                    collect_attrs(value, &mut attrs, 0);
                }
                FlowFact::Return { value, .. } => collect_attrs(value, &mut attrs, 0),
                FlowFact::Eval { call, .. } => collect_attrs(call, &mut attrs, 0),
                FlowFact::Decorated { decorators, .. } => {
                    for d in decorators {
                        collect_attrs(d, &mut attrs, 0);
                    }
                }
                FlowFact::ImplicitSelf { .. } => {}
            }
        }
        for t in &facts.types {
            if let TypeSubject::Var { scope, name } = &t.subject {
                vars.entry((*scope, bare(name).to_string())).or_default().0.push(t);
            }
        }
        FileTables { calls, attrs, vars }
    }
}

/// Syntax type resolution over one index (caches spellings, the module map and type names).
pub struct Types<'i> {
    index: &'i Index,
    hierarchy: &'i Hierarchy,
    modules: Option<ModuleMap>,
    /// Type name -> type symbols (non-synthetic) and declarations annotated as that type.
    by_name: Option<HashMap<String, Vec<SymbolId>>>,
    /// Type names with an out-of-line impl / conformance to a trait that names no index type.
    unresolved_impls: Option<HashSet<String>>,
    /// (file, spelling) -> resolution.
    spellings: HashMap<(FileId, String), ReceiverType>,
    /// Per-file lookup tables.
    tables: HashMap<FileId, FileTables<'i>>,
    /// (file, start, end) -> targets of the proven `references` edges at exactly that span
    /// (server answers for identifiers), on first use.
    server_refs: Option<HashMap<(FileId, u32, u32), Vec<SymbolId>>>,
    /// Type -> type declarations directly inside it (companion objects, nested types), on
    /// first use.
    nested_types: Option<HashMap<SymbolId, Vec<SymbolId>>>,
    /// Recursion bound of receiver resolution (aliases, call chains, fields;
    /// `flow.max_type_depth`).
    max_depth: u32,
}

impl<'i> Types<'i> {
    pub fn new(index: &'i Index, hierarchy: &'i Hierarchy) -> Types<'i> {
        Types {
            index,
            hierarchy,
            modules: None,
            by_name: None,
            unresolved_impls: None,
            spellings: HashMap::new(),
            tables: HashMap::new(),
            server_refs: None,
            nested_types: None,
            max_depth: trace_core::config::current().flow.max_type_depth,
        }
    }

    /// Rule 0 (module docs): the types a receiver identifier at `span` denotes by the
    /// server's answer, when every proven reference at exactly that span names a type.
    fn server_type(&mut self, file: FileId, span: ByteSpan) -> Option<ReceiverType> {
        let index = self.index;
        let refs = self.server_refs.get_or_insert_with(|| {
            let mut map: HashMap<(FileId, u32, u32), Vec<SymbolId>> = HashMap::new();
            for e in &index.edges {
                if e.tier == Tier::Proven && e.kind == EdgeKind::References {
                    map.entry((e.at.file, e.at.bytes.start, e.at.bytes.end))
                        .or_default()
                        .push(e.to);
                }
            }
            for targets in map.values_mut() {
                targets.sort_unstable();
                targets.dedup();
            }
            map
        });
        let targets = refs.get(&(file, span.start, span.end))?.clone();
        if targets.is_empty() || !targets.iter().all(|&t| index.symbol(t).kind.is_type()) {
            return None;
        }
        for &t in &targets {
            let keyword_base = self
                .hierarchy
                .mro(t)
                .into_iter()
                .any(|k| index.symbol(k).bases.iter().any(|b| b.contains('=')));
            if keyword_base {
                return None;
            }
        }
        let mut types: BTreeSet<SymbolId> = targets.iter().copied().collect();
        let nested = self.nested_types.get_or_insert_with(|| {
            let mut map: HashMap<SymbolId, Vec<SymbolId>> = HashMap::new();
            for s in &index.symbols {
                if let Some(p) = s.parent.filter(|_| s.kind.is_type() && !s.is_synthetic()) {
                    map.entry(p).or_default().push(s.id);
                }
            }
            map
        });
        for t in &targets {
            types.extend(nested.get(t).into_iter().flatten().copied());
        }
        for &t in &targets {
            let t = index.symbol(t);
            let companions: Vec<SymbolId> = self
                .types_named(&t.name)
                .into_iter()
                .filter(|&c| {
                    let c = index.symbol(c);
                    c.file == t.file && c.kind.is_type() && !c.is_synthetic()
                })
                .collect();
            types.extend(companions);
        }
        Some(ReceiverType::Index(types.into_iter().collect()))
    }

    /// Lookup tables of `file` (`None` without syntax facts).
    fn tables(&mut self, file: FileId) -> Option<&FileTables<'i>> {
        let facts = self.facts(file)?;
        Some(self.tables.entry(file).or_insert_with(|| FileTables::build(facts)))
    }

    /// Type facts and bound values of variable `name` in `scope` of `file`.
    fn var_facts(&mut self, file: FileId, scope: Scope, name: &str) -> VarFacts<'i> {
        self.tables(file)
            .and_then(|t| t.vars.get(&(scope, bare(name).to_string())))
            .cloned()
            .unwrap_or_default()
    }

    /// The receiver of the member access at `member_span` (module docs).
    fn receiver_of(&mut self, file: FileId, member_span: ByteSpan) -> Receiver<'i> {
        let Some(facts) = self.facts(file) else {
            return Receiver::Unknown;
        };
        if let Some(tables) = self.tables(file) {
            // 1. A call whose callee ends at the member.
            if let Some(&(start, r)) = tables.calls.get(&member_span.end) {
                if start < member_span.start {
                    return Receiver::Expr(r);
                }
            }
            // 2. A flow expression with that attribute identifier.
            if let Some(&r) = tables.attrs.get(&(member_span.start, member_span.end)) {
                return Receiver::Expr(r);
            }
        }
        // 3. The member access fact.
        let Some(access) = facts.member_access(member_span) else {
            return Receiver::Unknown;
        };
        if access.self_receiver {
            return Receiver::SelfRef;
        }
        let Some(root) = access.receiver_root.as_deref().filter(|r| !r.is_empty()) else {
            return Receiver::Unknown;
        };
        // `a.b.x`: the receiver is `a.b`, not `a` (another member access of the same root
        // ends right before this member, across the separator only).
        let pos = facts
            .member_accesses
            .partition_point(|m| (m.span.start, m.span.end) < (member_span.start, member_span.end));
        let chained = facts.member_accesses[..pos].iter().rev().take(4).any(|m| {
            m.receiver_root.as_deref() == Some(root)
                && m.span.end <= member_span.start
                && member_span.start - m.span.end <= 3
        });
        if chained {
            return Receiver::Unknown;
        }
        Receiver::Name(root.to_string())
    }

    fn modules(&mut self) -> &ModuleMap {
        let index = self.index;
        self.modules.get_or_insert_with(|| ModuleMap::new(index))
    }

    fn facts(&self, file: FileId) -> Option<&'i FileFacts> {
        let index: &'i Index = self.index;
        index.files.get(file.idx()).and_then(|f| f.facts.as_ref())
    }

    /// Type symbols named `name`.
    fn types_named(&mut self, name: &str) -> Vec<SymbolId> {
        let index = self.index;
        let map = self.by_name.get_or_insert_with(|| {
            let mut map: HashMap<String, Vec<SymbolId>> = HashMap::new();
            for s in &index.symbols {
                if s.kind.is_type() && !s.is_synthetic() {
                    map.entry(s.name.clone()).or_default().push(s.id);
                }
            }
            for list in map.values_mut() {
                list.sort_unstable();
                list.dedup();
            }
            map
        });
        map.get(name).cloned().unwrap_or_default()
    }

    /// Ancestors-or-self of the symbols enclosing `start` (innermost first, `<module>`
    /// excluded).
    fn chain_from(&self, start: Option<SymbolId>) -> Vec<SymbolId> {
        let index = self.index;
        let mut out = Vec::new();
        let mut cur = start;
        let mut steps = 0;
        while let Some(s) = cur {
            let sym = index.symbol(s);
            if sym.kind != SymbolKind::Module {
                out.push(s);
            }
            cur = sym.parent;
            steps += 1;
            if steps > 64 {
                break;
            }
        }
        out
    }

    fn chain_at(&self, file: FileId, byte: u32) -> Vec<SymbolId> {
        self.chain_from(self.index.symbol_at(file, byte))
    }

    fn chain_of_scope(&self, file: FileId, scope: Scope) -> Vec<SymbolId> {
        match scope {
            Scope::Module => Vec::new(),
            Scope::Decl(d) => self.chain_from(self.index.file(file).symbol_of_decl(d)),
        }
    }

    /// The receiver type of the member access at `member_span` in `file` (module docs).
    pub fn receiver_type(&mut self, file: FileId, member_span: ByteSpan) -> ReceiverType {
        let chain = self.chain_at(file, member_span.start);
        match self.receiver_of(file, member_span) {
            Receiver::SelfRef => self.self_type(&chain),
            Receiver::Name(name) => self.name_type(file, &chain, &name, 0),
            Receiver::Expr(e) => {
                if let Expr::Name { span, .. } = e {
                    if let Some(t) = self.server_type(file, *span) {
                        return t;
                    }
                }
                self.expr_type(file, &chain, e, 0)
            }
            Receiver::Unknown => ReceiverType::Unknown,
        }
    }

    /// The type `self` / `this` denotes in the innermost named scope of `chain`.
    fn self_type(&mut self, chain: &[SymbolId]) -> ReceiverType {
        let index = self.index;
        for &s in chain {
            let sym = index.symbol(s);
            if sym.kind.is_type() {
                return ReceiverType::Index(vec![s]);
            }
            if sym.kind.is_callable() {
                if is_anonymous(sym) {
                    // A parameter spelled like the self reference is bound by the caller.
                    let language = index.file(sym.file).language;
                    let own_self = self
                        .facts(sym.file)
                        .and_then(|f| f.declarations.get(sym.decl as usize))
                        .is_some_and(|d| d.parameters.iter().any(|p| is_self_name(language, bare(&p.name))));
                    if own_self {
                        return ReceiverType::Unknown;
                    }
                    continue;
                }
                if let Some(c) = self.hierarchy.class_of(index, s) {
                    return ReceiverType::Index(vec![c]);
                }
                if let Some(container) = sym.container.as_deref() {
                    return self.container_type(sym.file, container);
                }
                return ReceiverType::Unknown;
            }
        }
        ReceiverType::Unknown
    }

    /// Annotated class names of a container variable (`/** @type {Picker} */` above
    /// `const Picker = {}` -> `Var { Module, "Picker" }`), declared / comment facts only.
    fn container_annotations(&self, file: FileId, container: &str) -> Vec<&'i str> {
        let Some(facts) = self.facts(file) else {
            return Vec::new();
        };
        let name = base_name(container);
        facts
            .types
            .iter()
            .filter(|t| t.source != TypeSource::Constructed)
            .filter(|t| {
                matches!(&t.subject, TypeSubject::Var { scope: Scope::Module, name: n } if same_var(n, name))
            })
            .map(|t| t.type_name.as_str())
            .collect()
    }

    /// Type of a container value (`Picker` of `Picker.find = function () {}`): its annotation,
    /// else the spelling itself.
    fn container_type(&mut self, file: FileId, container: &str) -> ReceiverType {
        let annotated = self.container_annotations(file, container);
        if !annotated.is_empty() {
            return self.resolve_all(file, &annotated);
        }
        self.resolve_spelling(file, base_name(container))
    }

    /// Type of a bare name read in the scopes of `chain` (module docs, rule 2).
    fn name_type(&mut self, file: FileId, chain: &[SymbolId], name: &str, depth: u32) -> ReceiverType {
        let index = self.index;
        let language = index.file(file).language;
        if is_self_name(language, name) {
            return self.self_type(chain);
        }
        if depth > self.max_depth {
            return ReceiverType::Unknown;
        }
        let Some(facts) = self.facts(file) else {
            return ReceiverType::Unknown;
        };
        for &s in chain {
            let sym = index.symbol(s);
            if sym.kind.is_type() {
                if implicit_receiver(language) {
                    // Inherited fields too: the nearest definer in the base order.
                    for k in self.hierarchy.mro(s) {
                        let found = self.field_spellings(k, name);
                        if !found.is_empty() {
                            return self.resolve_all(index.symbol(k).file, &found);
                        }
                    }
                }
                continue;
            }
            if !sym.kind.is_callable() {
                continue;
            }
            if let Some(t) = self.scope_binding(file, facts, Scope::Decl(sym.decl), name, depth) {
                return t;
            }
        }
        self.scope_binding(file, facts, Scope::Module, name, depth)
            .unwrap_or(ReceiverType::Unknown)
    }

    /// Declared / comment `Field` facts of `name` on type `class`.
    fn field_spellings(&self, class: SymbolId, name: &str) -> Vec<&'i str> {
        let sym = self.index.symbol(class);
        let Some(facts) = self.facts(sym.file) else {
            return Vec::new();
        };
        facts
            .types
            .iter()
            .filter(|t| {
                matches!(&t.subject, TypeSubject::Field { class: c, name: n } if *c == sym.decl && same_var(n, name))
            })
            .map(|t| t.type_name.as_str())
            .collect()
    }

    /// The type of `name` if `scope` binds it (`None`: not bound there; look further out).
    fn scope_binding(
        &mut self,
        file: FileId,
        facts: &'i FileFacts,
        scope: Scope,
        name: &str,
        depth: u32,
    ) -> Option<ReceiverType> {
        let (types, binds) = self.var_facts(file, scope, name);
        let mut annotated: Vec<&'i str> = Vec::new();
        let mut constructed: Vec<&'i str> = Vec::new();
        for t in types {
            if t.source == TypeSource::Constructed {
                constructed.push(t.type_name.as_str());
            } else {
                annotated.push(t.type_name.as_str());
            }
        }
        let param = match scope {
            Scope::Decl(d) => facts
                .declarations
                .get(d as usize)
                .is_some_and(|decl| decl.parameters.iter().any(|p| same_var(&p.name, name))),
            Scope::Module => false,
        };
        let imported = facts.imports.iter().any(|i| i.scope == scope && i.local == name);
        if annotated.is_empty() && constructed.is_empty() && binds.is_empty() && !param && !imported {
            return None;
        }
        if !annotated.is_empty() {
            return Some(self.resolve_all(file, &annotated));
        }
        if imported || param {
            return Some(ReceiverType::Unknown);
        }
        let mut parts = Vec::new();
        for c in constructed {
            parts.push(self.resolve_spelling(file, c));
        }
        for v in binds {
            let chain = self.chain_of_scope(file, scope);
            parts.push(self.expr_type(file, &chain, v, depth + 1));
        }
        Some(self.combine(parts))
    }
}

#[cfg(test)]
#[path = "../../tests/unit/types/mod.rs"]
mod tests;
