//! Typed dispatch (derivation rule of statically typed languages; child of `derive`).
//!
//! In a statically typed library the declared types say which values the server runs, even
//! when the value flow through the router's tree is too deep to follow:
//! * a named **function type** whose values are called by code reachable from an entry
//!   (`c.handlers[c.index](c)` with `handlers Chain` and `Chain []HandlerFunc`,
//!   `var h HandlerFunc; h(c)`) is a dispatched handler type;
//! * a type **implementing the entry protocol** (the interface of a pattern `io_entry` row, or
//!   a type declaring the protocol method with the row's arity: `http.HandlerFunc.ServeHTTP`,
//!   or an anonymous function type with the protocol method's parameter and result types:
//!   `func(http.ResponseWriter, *http.Request)`) is dispatched by the server itself.
//!
//! A function type whose parameters or results are handler types composes handlers
//! (middleware) and is no handler type itself.
//!
//! Every method with a parameter declared as a handler type (the type, a named sequence of
//! it, a variadic list of it) registers that argument: the key is the nearest string
//! parameter before it, the verb the method's name when it spells an HTTP method token.
//! Methods the repository cannot call (Go: unexported names) register only when that key is
//! their one string parameter before the handler; their summaries serve composition.
//! A method with a string parameter that builds a prefix field of a registry object it
//! constructs (a field the registry's methods build their registration keys from) makes a
//! group: routes registered on its result are under that parameter
//! (`Mounts { key, target: Receiver }`), and its handler-typed parameters are the group's
//! middleware, not routes.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use trace_core::facts::{Expr, ImportKind, ParamKind};
use trace_core::model::ByteSpan;
use trace_core::Language;
use trace_syntax::lower::{CONCAT_CALLEE, INDEX_READ};

use super::{Callee, Chan, Program, State, Verb};
use crate::languages;
use crate::model::{ArgSel, Channel, Effect, VerbSel};

/// Nesting of expressions / locals / same-class methods followed when typing or tracing.
const MAX_DEPTH: usize = 8;

/// Node kinds and fields of one language's type declarations (syntax trees only).
#[derive(Debug)]
pub(crate) struct TypeSyntax {
    /// Named type definitions (fields `name` and `type`).
    pub(crate) definitions: &'static [&'static str],
    /// Function type nodes (fields `parameters` and `result`).
    pub(crate) function_types: &'static [&'static str],
    /// Parameter declarations inside a parameter list (field `type`; field `name` repeated).
    pub(crate) parameters: &'static [&'static str],
    /// Sequence type nodes and the field holding their element type.
    pub(crate) sequences: &'static [(&'static str, &'static str)],
    /// Map type nodes and the field holding their value type.
    pub(crate) maps: &'static [(&'static str, &'static str)],
    /// Method declarations with a receiver (fields `receiver`, `name`, `parameters`).
    pub(crate) methods: &'static [&'static str],
    /// Function declarations without a receiver (fields `name`, `parameters`).
    pub(crate) functions: &'static [&'static str],
    /// Method declarations inside an interface type (fields `name`, `parameters`, `result`).
    pub(crate) interface_methods: &'static [&'static str],
    /// Import specifications (fields `name` (optional) and `path`).
    pub(crate) imports: &'static [&'static str],
    /// The language's string type spellings (registration keys).
    pub(crate) string_types: &'static [&'static str],
    /// Nodes the grammar cannot tell from a call of an element read (`c.handlers[i](c)` parses
    /// as a conversion to an instantiated generic type): (node kind, field of the generic
    /// type, generic type kind, its base field, its arguments field, qualified base kind
    /// with its qualifier and name fields). The base is typed as a value; a type names none.
    pub(crate) ambiguous_index_calls: &'static [AmbiguousIndexCall],
    /// Only names starting with an upper-case letter are visible outside their package
    /// (Go language specification, "Exported identifiers"): the repository can call only
    /// those.
    pub(crate) exported_capitalized: bool,
}

/// See [`TypeSyntax::ambiguous_index_calls`].
#[derive(Debug)]
pub(crate) struct AmbiguousIndexCall {
    pub(crate) kind: &'static str,
    pub(crate) type_field: &'static str,
    pub(crate) generic: &'static str,
    pub(crate) base: &'static str,
    pub(crate) qualified: &'static str,
    pub(crate) qualifier: &'static str,
    pub(crate) name: &'static str,
}

/// The type declaration syntax of `language` (its library adapter's), if typed dispatch applies.
fn type_syntax(language: Language) -> Option<&'static TypeSyntax> {
    languages::adapter(language).and_then(|a| a.typed)
}

/// A named type: the module (package import path) declaring it and its name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct TypeId {
    module: String,
    name: String,
}

/// A declared type: a named type inside `seq` sequence levels (`[]T`, `...T`).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Ty {
    id: TypeId,
    seq: u8,
}

/// What a named type is defined as.
#[derive(Clone, Debug)]
enum Shape {
    /// A function type and the named types among its parameters and results.
    Func(Vec<TypeId>),
    Seq(TypeId),
    Map(TypeId),
    Alias(TypeId),
    Other,
}

/// A parameter declared with an anonymous function type: its name, and that type's
/// parameter and result types.
type FunctionParam = (String, Vec<TypeId>);

/// Type definitions and methods of one package.
#[derive(Default)]
pub(super) struct Package {
    defs: HashMap<String, Shape>,
    /// (receiver type, method) -> arities.
    methods: HashMap<(String, String), Vec<u32>>,
    /// (interface, method) -> parameter and result types of the declared method.
    signatures: HashMap<(String, String), Vec<TypeId>>,
    /// (receiver type or empty, function) -> its parameters declared with an anonymous
    /// function type, with that type's parameter and result types.
    function_params: HashMap<(String, String), Vec<FunctionParam>>,
}

/// An entry protocol: the interface of a pattern `io_entry` row and its method.
#[derive(Clone, Debug)]
struct Protocol {
    interface: Option<TypeId>,
    method: String,
    arity: Option<u32>,
    channel: Channel,
}

/// A method registering typed handlers: (method, key parameter, handler parameters, string
/// parameters before the first handler).
type Registering = (u32, u16, Vec<(u16, Channel)>, usize);

/// A package read for typed dispatch: (language, module, first file, file limit).
pub(super) type PackageKey = (Language, String, PathBuf, usize);

/// Typed-dispatch facts of one derivation.
#[derive(Default)]
pub(super) struct TypedState {
    packages: HashMap<String, Arc<Package>>,
    /// Function types whose values entry-reachable code calls.
    dispatched: HashMap<TypeId, Channel>,
}

/// A type spelling read structurally: sequence marks (`[]`, `[N]`, `...`) and pointer /
/// reference marks around a (qualified) type name; anonymous and composite types (function,
/// struct, map, channel types) name nothing.
fn read_spelling(text: &str) -> Option<(Option<&str>, &str, u8)> {
    let mut s = text.trim();
    let mut seq = 0u8;
    loop {
        if let Some(r) = s.strip_prefix("...") {
            seq += 1;
            s = r.trim_start();
        } else if let Some(r) = s.strip_prefix(['*', '&']) {
            s = r.trim_start();
        } else if s.starts_with('[') {
            let close = s.find(']')?;
            seq += 1;
            s = s[close + 1..].trim_start();
        } else {
            break;
        }
    }
    let base = s.split(['[', '<']).next().unwrap_or(s).trim();
    if base.is_empty() || base.contains(|c: char| c == '(' || c == '{' || c.is_whitespace()) {
        return None;
    }
    Some(match base.rsplit_once('.') {
        Some((q, n)) if !q.is_empty() && !n.is_empty() => (Some(q), n, seq),
        _ => (None, base, seq),
    })
}

/// Resolve a spelling in a file of `module` with `imports` (local name -> import path).
fn resolve(module: &str, imports: &[(String, String)], text: &str) -> Option<Ty> {
    let (qualifier, name, seq) = read_spelling(text)?;
    let module = match qualifier {
        Some(q) => imports.iter().find(|(l, _)| l == q)?.1.clone(),
        None => module.to_string(),
    };
    Some(Ty {
        id: TypeId {
            module,
            name: name.to_string(),
        },
        seq,
    })
}

fn node_text<'s>(node: tree_sitter::Node<'_>, source: &'s [u8]) -> &'s str {
    node.utf8_text(source).unwrap_or("")
}

/// Named children of a node.
fn children(node: tree_sitter::Node<'_>) -> Vec<tree_sitter::Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// Read the type definitions and methods of one package file.
fn read_file(language: Language, syntax: &TypeSyntax, source: &[u8], module: &str, pkg: &mut Package) {
    let Ok(tree) = trace_syntax::parse_tree(language, source) else {
        return;
    };
    // Declarations sit at most three levels below the file (`type (...)` groups, import lists).
    let mut nodes = Vec::new();
    let mut todo = vec![(tree.root_node(), 0usize)];
    while let Some((n, depth)) = todo.pop() {
        nodes.push(n);
        if depth < 3 {
            todo.extend(children(n).into_iter().map(|c| (c, depth + 1)));
        }
    }
    let mut imports: Vec<(String, String)> = Vec::new();
    for n in nodes.iter().filter(|n| syntax.imports.contains(&n.kind())) {
        let Some(path) = n.child_by_field_name("path") else { continue };
        let target = node_text(path, source).trim_matches(['"', '`']).to_string();
        let local = match n.child_by_field_name("name") {
            Some(l) => node_text(l, source).to_string(),
            None => target.rsplit('/').next().unwrap_or(&target).to_string(),
        };
        imports.push((local, target));
    }
    let param_types = |list: tree_sitter::Node<'_>| -> Vec<(String, u32)> {
        children(list)
            .into_iter()
            .filter(|p| syntax.parameters.contains(&p.kind()))
            .filter_map(|p| {
                let t = p.child_by_field_name("type")?;
                let mut cursor = p.walk();
                let names = p.children_by_field_name("name", &mut cursor).count().max(1) as u32;
                Some((node_text(t, source).to_string(), names))
            })
            .collect()
    };
    // Named parameter and result types of a signature (function type, method).
    let signature = |t: tree_sitter::Node<'_>| -> Vec<TypeId> {
        let mut spellings: Vec<String> = Vec::new();
        if let Some(ps) = t.child_by_field_name("parameters") {
            for (s, names) in param_types(ps) {
                spellings.extend(std::iter::repeat_n(s, names as usize));
            }
        }
        if let Some(r) = t.child_by_field_name("result") {
            let listed = param_types(r);
            if listed.is_empty() {
                spellings.push(node_text(r, source).to_string());
            } else {
                spellings.extend(listed.into_iter().map(|(s, _)| s));
            }
        }
        spellings
            .iter()
            .filter_map(|s| resolve(module, &imports, s))
            .map(|t| t.id)
            .collect()
    };
    // Parameters of a declaration declared with an anonymous function type.
    let function_params = |decl: tree_sitter::Node<'_>| -> Vec<(String, Vec<TypeId>)> {
        let Some(ps) = decl.child_by_field_name("parameters") else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for p in children(ps)
            .into_iter()
            .filter(|p| syntax.parameters.contains(&p.kind()))
        {
            let Some(t) = p
                .child_by_field_name("type")
                .filter(|t| syntax.function_types.contains(&t.kind()))
            else {
                continue;
            };
            let sig = signature(t);
            let mut cursor = p.walk();
            for name in p.children_by_field_name("name", &mut cursor) {
                out.push((node_text(name, source).to_string(), sig.clone()));
            }
        }
        out
    };
    for n in &nodes {
        if syntax.functions.contains(&n.kind()) {
            if let Some(name) = n.child_by_field_name("name") {
                let params = function_params(*n);
                if !params.is_empty() {
                    pkg.function_params
                        .insert((String::new(), node_text(name, source).to_string()), params);
                }
            }
        } else if syntax.definitions.contains(&n.kind()) {
            let (Some(name), Some(t)) = (n.child_by_field_name("name"), n.child_by_field_name("type")) else {
                continue;
            };
            let kind = t.kind();
            let type_name = node_text(name, source).to_string();
            for m in children(t)
                .into_iter()
                .filter(|m| syntax.interface_methods.contains(&m.kind()))
            {
                if let Some(mname) = m.child_by_field_name("name") {
                    pkg.signatures
                        .insert((type_name.clone(), node_text(mname, source).to_string()), signature(m));
                }
            }
            let shape = if syntax.function_types.contains(&kind) {
                Shape::Func(signature(t))
            } else if let Some((_, field)) = syntax.sequences.iter().find(|(k, _)| *k == kind) {
                match t
                    .child_by_field_name(field)
                    .and_then(|e| resolve(module, &imports, node_text(e, source)))
                {
                    Some(e) => Shape::Seq(e.id),
                    None => Shape::Other,
                }
            } else if let Some((_, field)) = syntax.maps.iter().find(|(k, _)| *k == kind) {
                match t
                    .child_by_field_name(field)
                    .and_then(|e| resolve(module, &imports, node_text(e, source)))
                {
                    Some(e) => Shape::Map(e.id),
                    None => Shape::Other,
                }
            } else {
                match resolve(module, &imports, node_text(t, source)) {
                    Some(r) if r.seq == 0 => Shape::Alias(r.id),
                    Some(r) => Shape::Seq(r.id),
                    None => Shape::Other,
                }
            };
            pkg.defs.entry(node_text(name, source).to_string()).or_insert(shape);
        } else if syntax.methods.contains(&n.kind()) {
            let (Some(receiver), Some(name)) =
                (n.child_by_field_name("receiver"), n.child_by_field_name("name"))
            else {
                continue;
            };
            let Some((rtype, _)) = param_types(receiver).into_iter().next() else { continue };
            let Some((_, rname, _)) = read_spelling(&rtype) else { continue };
            let params = function_params(*n);
            if !params.is_empty() {
                pkg.function_params
                    .insert((rname.to_string(), node_text(name, source).to_string()), params);
            }
            let arity = n
                .child_by_field_name("parameters")
                .map(|ps| param_types(ps).iter().map(|(_, k)| *k).sum())
                .unwrap_or(0);
            pkg.methods
                .entry((rname.to_string(), node_text(name, source).to_string()))
                .or_default()
                .push(arity);
        }
    }
}

/// Callees (element reads) of the grammar-ambiguous element calls inside `[start, end)`.
fn ambiguous_callees(
    syntax: &TypeSyntax,
    root: tree_sitter::Node<'_>,
    source: &[u8],
    start: u32,
    end: u32,
) -> Vec<Expr> {
    let mut out = Vec::new();
    if syntax.ambiguous_index_calls.is_empty() {
        return out;
    }
    let span_of = |n: tree_sitter::Node<'_>| ByteSpan::new(n.start_byte() as u32, n.end_byte() as u32);
    let mut todo = vec![root];
    while let Some(n) = todo.pop() {
        if n.end_byte() as u32 <= start || n.start_byte() as u32 >= end {
            continue;
        }
        for a in syntax.ambiguous_index_calls.iter().filter(|a| a.kind == n.kind()) {
            let Some(g) = n.child_by_field_name(a.type_field).filter(|g| g.kind() == a.generic) else {
                continue;
            };
            let Some(b) = g.child_by_field_name(a.base) else { continue };
            let span = span_of(b);
            let base = if b.kind() == a.qualified {
                let (Some(q), Some(name)) =
                    (b.child_by_field_name(a.qualifier), b.child_by_field_name(a.name))
                else {
                    continue;
                };
                Expr::Attr {
                    object: Box::new(Expr::Name {
                        name: node_text(q, source).to_string(),
                        span: span_of(q),
                    }),
                    attr: node_text(name, source).to_string(),
                    attr_span: span_of(name),
                    span,
                }
            } else {
                Expr::Name {
                    name: node_text(b, source).to_string(),
                    span,
                }
            };
            out.push(Expr::Call {
                func: Box::new(Expr::Attr {
                    object: Box::new(base),
                    attr: INDEX_READ.to_string(),
                    attr_span: span_of(g),
                    span: span_of(g),
                }),
                func_span: span_of(g),
                args: Vec::new(),
                kwargs: Vec::new(),
                span: span_of(g),
                is_new: false,
            });
        }
        todo.extend(children(n));
    }
    out
}

/// Walk every call expression below `e`.
fn calls_in<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
    match e {
        Expr::Call {
            func, args, kwargs, ..
        } => {
            out.push(e);
            calls_in(func, out);
            for a in args {
                calls_in(a, out);
            }
            for (_, v) in kwargs {
                calls_in(v, out);
            }
        }
        Expr::Attr { object, .. } => calls_in(object, out),
        Expr::Choice(alts) => {
            for a in alts {
                calls_in(a, out);
            }
        }
        Expr::Await(inner) => calls_in(inner, out),
        _ => {}
    }
}

impl Program<'_> {
    /// Every expression of a function (its calls, bindings, stores, returns, loops).
    fn expressions(&self, f: u32) -> Vec<&Expr> {
        let func = &self.funcs[f as usize];
        let mut out: Vec<&Expr> = func.evals.iter().collect();
        out.extend(func.locals.values().flatten());
        for (o, _, v) in &func.field_stores {
            out.push(o);
            out.push(v);
        }
        out.extend(func.member_stores.iter().map(|(_, _, v)| v));
        out.extend(func.global_stores.iter().map(|(_, v)| v));
        for (o, k, v) in &func.index_stores {
            out.extend([o, k, v]);
        }
        out.extend(func.returns.iter());
        out.extend(func.iterates.iter());
        out
    }

    /// Module (package import path) and imports of a loaded unit.
    fn unit_scope(&self, u: u32) -> (String, Vec<(String, String)>) {
        let unit = &self.units[u as usize];
        let module = unit.module.clone().unwrap_or_default();
        let imports = unit
            .imports
            .iter()
            .map(|i| (i.local.clone(), i.target.clone()))
            .collect();
        (module, imports)
    }

    fn spelled(&self, u: u32, text: &str) -> Option<Ty> {
        let (module, imports) = self.unit_scope(u);
        resolve(&module, &imports, text)
    }

    fn class_id(&self, c: u32) -> TypeId {
        let class = &self.classes[c as usize];
        TypeId {
            module: self.units[class.unit as usize].module.clone().unwrap_or_default(),
            name: class.qualified.clone(),
        }
    }

    /// The loaded class a named type is.
    fn class_of(&self, id: &TypeId) -> Option<u32> {
        self.class_names
            .get(&id.name)
            .into_iter()
            .flatten()
            .copied()
            .find(|&c| self.class_id(c) == *id)
    }

    /// Read (once) the type definitions and methods of a package: the loaded files' folder,
    /// else the folder an import of the module resolves to. The package is a function of the
    /// module and its first file: the batch reads it once.
    fn ensure_package(&self, st: &mut State, module: &str) {
        if st.typed.packages.contains_key(module) {
            return;
        }
        let first: Option<PathBuf> = type_syntax(self.language).and_then(|_| {
            match self.units.iter().find(|u| u.module.as_deref() == Some(module)) {
                Some(u) => Some(u.path.clone()),
                None => self.units.first().and_then(|root| {
                    (self.spec.resolve_import)(&root.path, module, ImportKind::Module, self.cx.roots)
                        .map(|(p, _)| p)
                }),
            }
        });
        let pkg = match first {
            Some(first) => {
                let key =
                    (self.language, module.to_string(), first.clone(), self.cx.limits.max_package_files);
                self.cx
                    .parsed
                    .packages
                    .get_or_init(key, || Arc::new(self.read_package(module, &first)))
            }
            None => Arc::new(Package::default()),
        };
        st.typed.packages.insert(module.to_string(), pkg);
    }

    /// The type definitions and methods of the files of `first`'s folder (loaded files are
    /// read from memory).
    fn read_package(&self, module: &str, first: &Path) -> Package {
        let mut pkg = Package::default();
        let Some(syntax) = type_syntax(self.language) else { return pkg };
        for path in languages::namespace_files(self.spec, first, self.cx.limits.max_package_files) {
            match self.by_path.get(&path) {
                Some(&u) => {
                    read_file(self.language, syntax, &self.units[u as usize].file.source, module, &mut pkg)
                }
                None => {
                    if let Some(source) = self.cx.loader.read(&path) {
                        read_file(self.language, syntax, &source, module, &mut pkg);
                    }
                }
            }
        }
        pkg
    }

    fn shape(&self, st: &mut State, id: &TypeId) -> Option<Shape> {
        self.ensure_package(st, &id.module);
        st.typed.packages.get(&id.module)?.defs.get(&id.name).cloned()
    }

    /// Whether a named type is (an alias of) a function type; its named parameter / result
    /// types.
    fn function_type(&self, st: &mut State, id: &TypeId) -> Option<Vec<TypeId>> {
        let mut cur = id.clone();
        for _ in 0..4 {
            match self.shape(st, &cur)? {
                Shape::Func(refs) => return Some(refs),
                Shape::Alias(next) => cur = next,
                _ => return None,
            }
        }
        None
    }

    /// The element type of a sequence / map type.
    fn element(&self, st: &mut State, ty: &Ty) -> Option<Ty> {
        if ty.seq > 0 {
            return Some(Ty {
                id: ty.id.clone(),
                seq: ty.seq - 1,
            });
        }
        let mut cur = ty.id.clone();
        for _ in 0..4 {
            match self.shape(st, &cur)? {
                Shape::Seq(e) | Shape::Map(e) => return Some(Ty { id: e, seq: 0 }),
                Shape::Alias(next) => cur = next,
                _ => return None,
            }
        }
        None
    }

    /// Declared type of a name in `f` (receiver, local, parameter; enclosing functions for
    /// closures).
    fn name_type(&self, f: u32, name: &str) -> Option<Ty> {
        let mut cur = Some(f);
        for _ in 0..16 {
            let g = cur?;
            let func = &self.funcs[g as usize];
            if func.self_param.as_deref() == Some(name) {
                return func.class.map(|c| Ty {
                    id: self.class_id(c),
                    seq: 0,
                });
            }
            if let Some(t) = self.local_types.get(&(g, name.to_string())) {
                return self.spelled(func.unit, t);
            }
            if let Some(i) = func.params.iter().position(|p| p.name == name) {
                let t = self.param_types.get(&(g, i as u16))?;
                let mut ty = self.spelled(func.unit, t)?;
                if func.params[i].kind == ParamKind::VarPositional {
                    ty.seq += 1;
                }
                return Some(ty);
            }
            if func.locals.contains_key(name) {
                return None;
            }
            cur = func.parent;
        }
        None
    }

    /// Declared type of field `name` of class `c` (or a related class).
    fn field_type(&self, c: u32, name: &str) -> Option<Ty> {
        std::iter::once(c)
            .chain(self.related.get(c as usize).into_iter().flatten().copied())
            .find_map(|x| {
                let t = self.field_types.get(&(x, name.to_string()))?;
                self.spelled(self.classes[x as usize].unit, t)
            })
    }

    /// Static type of an expression from declarations only (names, fields, element reads).
    fn expr_type(&self, st: &mut State, f: u32, e: &Expr, depth: usize) -> Option<Ty> {
        if depth > MAX_DEPTH {
            return None;
        }
        match e {
            Expr::Name { name, .. } => self.name_type(f, name),
            Expr::Attr { object, attr, .. } => {
                let obj = self.expr_type(st, f, object, depth + 1)?;
                if obj.seq > 0 {
                    return None;
                }
                let c = self.class_of(&obj.id)?;
                self.field_type(c, attr)
            }
            Expr::Call { func, .. } => match func.as_ref() {
                Expr::Attr { object, attr, .. } if attr == INDEX_READ => {
                    let obj = self.expr_type(st, f, object, depth + 1)?;
                    self.element(st, &obj)
                }
                _ => None,
            },
            Expr::Await(inner) => self.expr_type(st, f, inner, depth + 1),
            _ => None,
        }
    }

    /// Entry protocols of the language (pattern `io_entry` rows).
    fn protocols(&self) -> Vec<Protocol> {
        self.io_entry
            .iter()
            .filter_map(|row| {
                let (method, arity) = row.entry_pattern()?;
                let interface = row.symbol.as_deref().and_then(|s| {
                    let (owner, m) = s.rsplit_once('.')?;
                    if m != method {
                        return None;
                    }
                    let (module, name) = owner.rsplit_once('.')?;
                    Some(TypeId {
                        module: module.to_string(),
                        name: name.to_string(),
                    })
                });
                Some(Protocol {
                    interface,
                    method: method.to_string(),
                    arity,
                    channel: row.channel_or_default(),
                })
            })
            .collect()
    }

    /// The entry protocol a named type implements (it is the protocol's interface, or its
    /// package declares the protocol method on it).
    fn protocol_of(&self, st: &mut State, id: &TypeId, protocols: &[Protocol]) -> Option<Channel> {
        for p in protocols {
            if p.interface.as_ref() == Some(id) {
                return Some(p.channel);
            }
        }
        if protocols.is_empty() {
            return None;
        }
        self.ensure_package(st, &id.module);
        let pkg = st.typed.packages.get(&id.module)?;
        protocols.iter().find_map(|p| {
            let arities = pkg.methods.get(&(id.name.clone(), p.method.clone()))?;
            arities
                .iter()
                .any(|a| p.arity.is_none_or(|x| x == *a))
                .then_some(p.channel)
        })
    }

    /// The entry protocol whose method signature parameter `name` of `g` is declared with
    /// (an anonymous function type of the same parameter and result types).
    fn protocol_signature_param(
        &self,
        st: &mut State,
        g: u32,
        name: &str,
        protocols: &[Protocol],
    ) -> Option<Channel> {
        let func = &self.funcs[g as usize];
        let module = self.units[func.unit as usize].module.clone().unwrap_or_default();
        let owner = func
            .class
            .map(|c| self.classes[c as usize].qualified.clone())
            .unwrap_or_default();
        self.ensure_package(st, &module);
        let sig = st
            .typed
            .packages
            .get(&module)?
            .function_params
            .get(&(owner, func.name.clone()))?
            .iter()
            .find(|(n, _)| n == name)?
            .1
            .clone();
        for p in protocols {
            let Some(iface) = &p.interface else { continue };
            self.ensure_package(st, &iface.module);
            let declared = st
                .typed
                .packages
                .get(&iface.module)
                .and_then(|k| k.signatures.get(&(iface.name.clone(), p.method.clone())));
            if declared == Some(&sig) {
                return Some(p.channel);
            }
        }
        None
    }

    /// A handler type before the middleware test: dispatched function type or protocol type.
    fn dispatched_type(&self, st: &mut State, id: &TypeId, protocols: &[Protocol]) -> Option<Channel> {
        if let Some(&ch) = st.typed.dispatched.get(id) {
            return Some(ch);
        }
        self.protocol_of(st, id, protocols)
    }

    /// The channel of a declared parameter type that receives handlers: a handler type, a
    /// named sequence of one, a variadic / sequence parameter of one. Function types over
    /// handler types (middleware) are none.
    fn handler_type(&self, st: &mut State, ty: &Ty, protocols: &[Protocol]) -> Option<Channel> {
        let id = match ty.seq {
            0 => match self.shape(st, &ty.id) {
                Some(Shape::Seq(e)) => e,
                _ => ty.id.clone(),
            },
            1 => ty.id.clone(),
            _ => return None,
        };
        let channel = self.dispatched_type(st, &id, protocols)?;
        if let Some(refs) = self.function_type(st, &id) {
            for r in refs {
                if r != id && self.dispatched_type(st, &r, protocols).is_some() {
                    return None;
                }
            }
        }
        Some(channel)
    }

    fn is_string_type(&self, text: &str) -> bool {
        type_syntax(self.language).is_some_and(
            |s| matches!(read_spelling(text), Some((None, name, 0)) if s.string_types.contains(&name)),
        )
    }

    /// Declared type of parameter `i` of `g` (variadic parameters are sequences).
    fn param_ty(&self, g: u32, i: u16) -> Option<Ty> {
        let func = &self.funcs[g as usize];
        let t = self.param_types.get(&(g, i))?;
        let mut ty = self.spelled(func.unit, t)?;
        if func.params.get(i as usize)?.kind == ParamKind::VarPositional {
            ty.seq += 1;
        }
        Some(ty)
    }

    /// (string parameters, handler parameters with their channel) of a method.
    fn typed_params(
        &self,
        st: &mut State,
        g: u32,
        protocols: &[Protocol],
    ) -> (Vec<u16>, Vec<(u16, Channel)>) {
        let func = &self.funcs[g as usize];
        let mut strings = Vec::new();
        let mut handlers = Vec::new();
        for (i, p) in func.params.iter().enumerate() {
            if func.self_param.as_deref() == Some(p.name.as_str()) {
                continue;
            }
            let i = i as u16;
            let Some(text) = self.param_types.get(&(g, i)) else {
                // An anonymous function type with the entry protocol method's signature
                // (`f func(http.ResponseWriter, *http.Request)`).
                if let Some(ch) = self.protocol_signature_param(st, g, &p.name, protocols) {
                    handlers.push((i, ch));
                }
                continue;
            };
            if p.kind != ParamKind::VarPositional && self.is_string_type(text) {
                strings.push(i);
                continue;
            }
            let Some(ty) = self.param_ty(g, i) else { continue };
            if let Some(ch) = self.handler_type(st, &ty, protocols) {
                handlers.push((i, ch));
            }
        }
        (strings, handlers)
    }

    /// Rule entry point (after the entry-reachable functions are known): dispatched function
    /// types, then typed registrations and groups of every loaded method.
    pub(super) fn typed_dispatch(&self, st: &mut State, reach: &HashMap<u32, Channel>) {
        if type_syntax(self.language).is_none() {
            return;
        }
        let Some(syntax) = type_syntax(self.language) else { return };
        let mut reached: Vec<(u32, Channel)> = reach.iter().map(|(f, c)| (*f, *c)).collect();
        reached.sort_unstable_by_key(|(f, _)| *f);
        let mut trees: HashMap<u32, Option<tree_sitter::Tree>> = HashMap::new();
        for (f, channel) in reached {
            let func = &self.funcs[f as usize];
            if func.is_module {
                continue;
            }
            let mut calls = Vec::new();
            for e in self.expressions(f) {
                calls_in(e, &mut calls);
            }
            let mut callees: Vec<Expr> = calls
                .into_iter()
                .filter_map(|c| match c {
                    Expr::Call { func, .. } => Some(func.as_ref().clone()),
                    _ => None,
                })
                .collect();
            if !syntax.ambiguous_index_calls.is_empty() {
                let unit = &self.units[func.unit as usize];
                let decl = &unit.file.facts.declarations[func.decl as usize];
                let tree = trees
                    .entry(func.unit)
                    .or_insert_with(|| trace_syntax::parse_tree(self.language, &unit.file.source).ok());
                if let Some(tree) = tree {
                    callees.extend(ambiguous_callees(
                        syntax,
                        tree.root_node(),
                        &unit.file.source,
                        decl.body_start,
                        decl.span.bytes.end,
                    ));
                }
            }
            for callee in callees {
                let Some(ty) = self.expr_type(st, f, &callee, 0) else { continue };
                if ty.seq == 0 && self.function_type(st, &ty.id).is_some() {
                    st.typed.dispatched.entry(ty.id).or_insert(channel);
                }
            }
        }
        let protocols = self.protocols();
        if st.typed.dispatched.is_empty() && protocols.is_empty() {
            return;
        }
        // Registrations per method.
        let mut registering: Vec<Registering> = Vec::new();
        for g in 0..self.funcs.len() as u32 {
            let func = &self.funcs[g as usize];
            if func.is_module || func.class.is_none() {
                continue;
            }
            let (strings, handlers) = self.typed_params(st, g, &protocols);
            let Some(first) = handlers.iter().map(|(h, _)| *h).min() else { continue };
            let before: Vec<u16> = strings.iter().copied().filter(|&s| s < first).collect();
            let Some(&key) = before.iter().max() else { continue };
            registering.push((g, key, handlers, before.len()));
        }
        let registries: BTreeSet<u32> = registering
            .iter()
            .filter_map(|(g, _, _, _)| self.funcs[*g as usize].class)
            .collect();
        let prefixes = self.prefix_fields(st, &registries, &registering);
        let groups = self.groups(st, &prefixes);
        let exported_only = type_syntax(self.language).is_some_and(|s| s.exported_capitalized);
        for (g, key, handlers, strings) in registering {
            let name = &self.funcs[g as usize].name;
            // An unexported method registers only with one string parameter before its
            // handlers (the key is unambiguous); its others are internal (names, hosts).
            let internal = exported_only && !name.starts_with(char::is_uppercase);
            if groups.contains(&g) || (internal && strings != 1) {
                continue;
            }
            for (h, channel) in handlers {
                let verb = match crate::channels::http_method_token(name) {
                    Some(t) if channel == Channel::Http => Verb::Const(st.lit(t)),
                    _ => Verb::Any,
                };
                st.add_chan(
                    g,
                    Chan::Registers {
                        channel,
                        key,
                        handler: h,
                        verb,
                    },
                );
            }
        }
    }

    /// Whether `e` (in `f`, whose receiver is `receiver`) is built from `target`: equal to it,
    /// an argument (at any depth) of a call or concatenation, a local bound to such a value,
    /// or the result of a method of the receiver's class whose returned value is.
    fn built_from(
        &self,
        f: u32,
        e: &Expr,
        target: &dyn Fn(u32, &Expr) -> bool,
        depth: usize,
        seen: &mut HashSet<(u32, String)>,
    ) -> bool {
        if depth > MAX_DEPTH {
            return false;
        }
        if target(f, e) {
            return true;
        }
        match e {
            Expr::Name { name, .. } => {
                let func = &self.funcs[f as usize];
                if !seen.insert((f, name.clone())) {
                    return false;
                }
                func.locals
                    .get(name)
                    .is_some_and(|vals| vals.iter().any(|v| self.built_from(f, v, target, depth + 1, seen)))
            }
            Expr::Call {
                func: callee,
                args,
                kwargs,
                ..
            } => {
                if args
                    .iter()
                    .chain(kwargs.iter().map(|(_, v)| v))
                    .any(|a| self.built_from(f, a, target, depth + 1, seen))
                {
                    return true;
                }
                // A method of the same class called on the receiver: its returned values.
                let Expr::Attr { object, attr, .. } = callee.as_ref() else { return false };
                let Expr::Name { name, .. } = object.as_ref() else { return false };
                let func = &self.funcs[f as usize];
                if func.self_param.as_deref() != Some(name.as_str()) {
                    return false;
                }
                let Some(m) = func
                    .class
                    .and_then(|c| self.classes[c as usize].methods.get(attr).copied())
                else {
                    return false;
                };
                if !seen.insert((m, String::new())) {
                    return false;
                }
                self.funcs[m as usize]
                    .returns
                    .iter()
                    .any(|r| self.built_from(m, r, target, depth + 1, seen))
            }
            Expr::Choice(alts) => alts.iter().any(|a| self.built_from(f, a, target, depth + 1, seen)),
            Expr::Await(inner) => self.built_from(f, inner, target, depth + 1, seen),
            _ => false,
        }
    }

    /// Prefix fields of registry classes: fields a method of the class builds the key of a
    /// typed registration from (`g.owner.add(.., g.prefix+path, h)`,
    /// `add(m, g.absolute(p), hs)` where the method `absolute` returns `join(g.base, p)`).
    fn prefix_fields(
        &self,
        st: &mut State,
        registries: &BTreeSet<u32>,
        registering: &[Registering],
    ) -> HashMap<u32, BTreeSet<String>> {
        let keys: HashMap<u32, u16> = registering.iter().map(|(g, k, _, _)| (*g, *k)).collect();
        let mut out: HashMap<u32, BTreeSet<String>> = HashMap::new();
        for &c in registries {
            let mut methods: Vec<u32> = self.classes[c as usize].methods.values().copied().collect();
            methods.sort_unstable();
            let fields: Vec<String> = std::iter::once(c)
                .chain(self.related.get(c as usize).into_iter().flatten().copied())
                .flat_map(|x| {
                    self.field_types
                        .keys()
                        .filter(move |(y, _)| *y == x)
                        .map(|(_, n)| n.clone())
                })
                .collect::<BTreeSet<String>>()
                .into_iter()
                .collect();
            for m in methods {
                if self.funcs[m as usize].self_param.is_none() {
                    continue;
                }
                let mut calls = Vec::new();
                for e in self.expressions(m) {
                    calls_in(e, &mut calls);
                }
                for call in calls {
                    let Expr::Call {
                        func, args, kwargs, ..
                    } = call
                    else {
                        continue;
                    };
                    if matches!(func.as_ref(), Expr::Name { name, .. } if name == CONCAT_CALLEE) {
                        continue;
                    }
                    for callee in self.callees(st, m, func, args.len() as u32) {
                        let Callee::Lib(h, skip) = callee else { continue };
                        let Some(&k) = keys.get(&h) else { continue };
                        let Some((_, arg)) =
                            self.bind(h, skip, args, kwargs).into_iter().find(|(j, _)| *j == k)
                        else {
                            continue;
                        };
                        for field in &fields {
                            // The field of the function's own receiver (the same object when
                            // followed into a method of the class called on it).
                            let is_field = |h: u32, x: &Expr| {
                                let receiver = self.funcs[h as usize].self_param.as_deref();
                                matches!(x, Expr::Attr { object, attr, .. } if attr == field
                                    && matches!(object.as_ref(), Expr::Name { name, .. } if Some(name.as_str()) == receiver))
                            };
                            if self.built_from(m, arg, &is_field, 0, &mut HashSet::new()) {
                                out.entry(c).or_default().insert(field.clone());
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// Group methods: a method with a string parameter that constructs an instance of a
    /// registry class whose prefix field is built from that parameter gets
    /// `MountsSelf { key }`. Returns the group methods.
    fn groups(&self, st: &mut State, prefixes: &HashMap<u32, BTreeSet<String>>) -> BTreeSet<u32> {
        let mut keys: HashMap<u32, u16> = HashMap::new();
        if prefixes.is_empty() {
            return BTreeSet::new();
        }
        // Rounds: a method passing a value built from its string parameter as the key of a
        // group method (`g.owner.Group(g.prefix+prefix)`) is a group too.
        for _ in 0..4 {
            let before = keys.len();
            for g in 0..self.funcs.len() as u32 {
                let func = &self.funcs[g as usize];
                if func.is_module || !func.has_body || keys.contains_key(&g) {
                    continue;
                }
                let strings: Vec<u16> = (0..func.params.len() as u16)
                    .filter(|&i| {
                        func.params[i as usize].kind != ParamKind::VarPositional
                            && self.param_types.get(&(g, i)).is_some_and(|t| self.is_string_type(t))
                    })
                    .collect();
                if strings.is_empty() {
                    continue;
                }
                let mut calls = Vec::new();
                for e in self.expressions(g) {
                    calls_in(e, &mut calls);
                }
                // (value built into the group's prefix) per construction / group call.
                let mut values: Vec<&Expr> = Vec::new();
                for call in calls {
                    let Expr::Call {
                        func: callee,
                        args,
                        kwargs,
                        ..
                    } = call
                    else {
                        continue;
                    };
                    for c in self.callees(st, g, callee, args.len() as u32) {
                        match c {
                            Callee::Ctor(c) => {
                                let Some(fields) = std::iter::once(c)
                                    .chain(self.related.get(c as usize).into_iter().flatten().copied())
                                    .find_map(|x| prefixes.get(&x))
                                else {
                                    continue;
                                };
                                values.extend(
                                    kwargs.iter().filter(|(n, _)| fields.contains(n)).map(|(_, v)| v),
                                );
                            }
                            Callee::Lib(h, skip) => {
                                let Some(&k) = keys.get(&h) else { continue };
                                values.extend(
                                    self.bind(h, skip, args, kwargs)
                                        .into_iter()
                                        .filter(|(j, _)| *j == k)
                                        .map(|(_, a)| a),
                                );
                            }
                            _ => {}
                        }
                    }
                }
                'strings: for &k in &strings {
                    let pname = self.funcs[g as usize].params[k as usize].name.clone();
                    let is_param =
                        |h: u32, x: &Expr| h == g && matches!(x, Expr::Name { name, .. } if *name == pname);
                    for value in &values {
                        if self.built_from(g, value, &is_param, 0, &mut HashSet::new()) {
                            keys.insert(g, k);
                            break 'strings;
                        }
                    }
                }
            }
            if keys.len() == before {
                break;
            }
        }
        for (&g, &k) in &keys {
            st.add_chan(g, Chan::MountsSelf { key: k });
        }
        keys.into_keys().collect()
    }

    /// Whether a declared type spelling names a type the library defines as a function,
    /// sequence or map type (`type Chain []HandlerFunc`): its methods are
    /// library code, not methods of whatever object a caller passes.
    /// The language's string type is closed the same way (a string has no methods of a
    /// caller's object).
    pub(super) fn closed_type(&self, st: &mut State, u: u32, text: &str) -> bool {
        if type_syntax(self.language).is_none() {
            return false;
        }
        if self.is_string_type(text) {
            return true;
        }
        match self.spelled(u, text) {
            Some(ty) if ty.seq == 0 => {
                matches!(self.shape(st, &ty.id), Some(Shape::Func(_) | Shape::Seq(_) | Shape::Map(_)))
            }
            _ => false,
        }
    }

    /// The selector of a registration's handler parameter: a variadic handler list registers
    /// its last element (the ones before it run first).
    pub(super) fn handler_selector(&self, f: u32, i: u16) -> Option<ArgSel> {
        let p = self.funcs[f as usize].params.get(i as usize)?;
        if p.kind == ParamKind::VarPositional {
            return Some(ArgSel::Last);
        }
        self.selector(f, i)
    }
}

/// A registration with a named verb makes the same registration with any verb redundant.
pub(super) fn prefer_named_verbs(effects: &mut Vec<Effect>) {
    let named: Vec<(ArgSel, ArgSel, Channel)> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::Registers {
                channel,
                key,
                handler,
                verb: VerbSel::Const(_),
            } => Some((key.clone(), handler.clone(), *channel)),
            _ => None,
        })
        .collect();
    if named.is_empty() {
        return;
    }
    effects.retain(|e| match e {
        Effect::Registers {
            channel,
            key,
            handler,
            verb: VerbSel::Any,
        } => !named.iter().any(|(k, h, c)| k == key && h == handler && c == channel),
        _ => true,
    });
}

#[cfg(test)]
#[path = "../../tests/unit/derive/typed.rs"]
mod tests;
