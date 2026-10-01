//! Decorator metadata (DESIGN-bridges §2 rule 5, "TS decorators are code").
//!
//! A decorator factory call (`@Name(args)`) returns a decorator the language applies to the
//! decorated class or member. What the decorator stores for a reflection scanner is read from
//! the installed JavaScript implementation by evaluating it symbolically over its syntax tree
//! (never executed): the factory's parameters are the call's arguments (`Arg(i)`), the
//! returned decorator is applied to a symbolic target, member name and property descriptor,
//! and every call of a `reflection_roots` row (a metadata store such as
//! `Reflect.defineMetadata(key, value, target)`: the row's key selector names the metadata
//! key argument) records a write: the metadata key (a constant, resolved through the
//! module's constants and `require`d modules), the stored value, and whether it is attached
//! to the decorated member (the descriptor's value, or the target with the member name) or to
//! the target itself.
//!
//! The language features followed are the ones decorator code is made of: closures
//! capturing factory parameters, default parameters, `const` / `let` / `var` bindings with
//! array and object destructuring, object literals with computed keys, member and subscript
//! reads, `?:` / `||` / `??` / `&&` alternatives, `(0, f)(...)` calls, CommonJS
//! `exports.X = ...` and `require(...)`. Anything else is opaque; evaluation is bounded.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use trace_core::Language;
use tree_sitter::{Node, Tree};

/// A stored metadata value in terms of the decorator factory call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetaValue {
    /// The factory call's `i`-th argument.
    Arg(u32),
    /// Property `name` of the `i`-th argument.
    ArgField(u32, String),
    Str(String),
    /// A member read the implementation does not resolve further, by member name
    /// (`RequestMethod.GET` -> `GET`: compiled enumerations keep their member names here).
    Member(String),
    /// Any of the alternatives (defaults, conditional values).
    Choice(Vec<MetaValue>),
    Other,
}

/// One metadata write of an applied decorator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetaWrite {
    pub key: String,
    pub value: MetaValue,
    /// Attached to the decorated member (else to the decorated target itself).
    pub on_member: bool,
}

/// Evaluation budget (nodes visited per decorator).
const MAX_STEPS: usize = 20_000;
/// Call depth.
const MAX_DEPTH: usize = 12;
/// Modules loaded per decorator.
const MAX_MODULES: usize = 24;

/// Byte range `(start, end)` of a syntax node in its module's source.
type Bytes = (usize, usize);

#[derive(Clone, Debug)]
enum Val {
    Arg(u32),
    ArgField(u32, String),
    Str(String),
    Member(String),
    Choice(Vec<Val>),
    Obj(Vec<(Val, Val)>),
    Arr(Vec<Val>),
    Func(Rc<Closure>),
    Module(usize),
    Exports(usize),
    /// The decorated target, the member name and the property descriptor a decorator is
    /// applied to; `Decorated` is the descriptor's value (the member).
    Target,
    MemberName,
    Descriptor,
    Decorated,
    Undefined,
    Opaque,
}

#[derive(Debug)]
struct Closure {
    module: usize,
    start: usize,
    end: usize,
    env: Rc<Env>,
}

#[derive(Debug, Default)]
struct Env {
    vars: RefCell<HashMap<String, Val>>,
    parent: Option<Rc<Env>>,
}

impl Env {
    fn child(parent: &Rc<Env>) -> Rc<Env> {
        Rc::new(Env {
            vars: RefCell::new(HashMap::new()),
            parent: Some(parent.clone()),
        })
    }

    fn get(&self, name: &str) -> Option<Val> {
        if let Some(v) = self.vars.borrow().get(name) {
            return Some(v.clone());
        }
        self.parent.as_ref().and_then(|p| p.get(name))
    }

    fn set(&self, name: &str, v: Val) {
        self.vars.borrow_mut().insert(name.to_string(), v);
    }
}

struct Module {
    path: PathBuf,
    source: Rc<[u8]>,
    tree: Tree,
    /// Module scope (top-level bindings evaluated lazily into it).
    env: Rc<Env>,
}

struct Eval {
    modules: Vec<Module>,
    root: String,
    key_pos: usize,
    writes: Vec<MetaWrite>,
    steps: usize,
    /// Top-level bindings under evaluation (cycles read as opaque).
    pending: Vec<(usize, String)>,
}

fn text<'s>(n: Node<'_>, source: &'s [u8]) -> &'s str {
    std::str::from_utf8(&source[n.start_byte()..n.end_byte()]).unwrap_or("")
}

fn named(n: Node<'_>) -> Vec<Node<'_>> {
    let mut c = n.walk();
    n.named_children(&mut c)
        .filter(|k| !k.kind().contains("comment"))
        .collect()
}

/// The node spanning exactly `start..end` (the outermost of equal spans).
fn node_at(tree: &Tree, start: usize, end: usize) -> Option<Node<'_>> {
    let root = tree.root_node();
    let mut n = root.descendant_for_byte_range(start, end)?;
    while n.start_byte() != start || n.end_byte() != end {
        n = n.parent()?;
    }
    while let Some(p) = n.parent().filter(|p| p.start_byte() == start && p.end_byte() == end) {
        n = p;
    }
    Some(n)
}

fn choice(mut vals: Vec<Val>) -> Val {
    let mut flat = Vec::new();
    for v in vals.drain(..) {
        match v {
            Val::Choice(inner) => flat.extend(inner),
            other => flat.push(other),
        }
    }
    if flat.len() == 1 {
        flat.pop().unwrap_or(Val::Opaque)
    } else {
        Val::Choice(flat)
    }
}

/// Whether a value is certainly present (`a || b` then reads `a`).
fn present(v: &Val) -> bool {
    match v {
        Val::Str(s) => !s.is_empty(),
        Val::Member(_) | Val::Obj(_) | Val::Arr(_) | Val::Func(_) => true,
        Val::Choice(c) => c.iter().all(present),
        _ => false,
    }
}

fn string_value(n: Node<'_>, source: &[u8]) -> String {
    let t = text(n, source);
    let inner = t.get(1..t.len().saturating_sub(1)).unwrap_or("");
    inner.to_string()
}

impl Eval {
    fn load(&mut self, path: &Path) -> Option<usize> {
        if let Some(i) = self.modules.iter().position(|m| m.path == path) {
            return Some(i);
        }
        if self.modules.len() >= MAX_MODULES {
            return None;
        }
        let source = std::fs::read(path).ok()?;
        let tree = trace_syntax::parse_tree(Language::JavaScript, &source).ok()?;
        self.modules.push(Module {
            path: path.to_path_buf(),
            source: source.into(),
            tree,
            env: Rc::new(Env::default()),
        });
        Some(self.modules.len() - 1)
    }

    /// `require("./x")` relative to module `m` (`x.js`, `x/index.js`); package specifiers
    /// are not followed.
    fn require(&mut self, m: usize, spec: &str) -> Val {
        if !spec.starts_with('.') {
            return Val::Opaque;
        }
        let Some(dir) = self.modules[m].path.parent().map(Path::to_path_buf) else {
            return Val::Opaque;
        };
        let base = dir.join(spec);
        let candidates = [
            PathBuf::from(format!("{}.js", base.display())),
            base.join("index.js"),
            base.clone(),
        ];
        for c in candidates {
            if c.is_file() {
                if let Some(i) = self.load(&c) {
                    return Val::Module(i);
                }
            }
        }
        Val::Opaque
    }

    /// The module's tree and source (cheap copies: nodes borrow the copy, not `self`).
    fn handle(&self, m: usize) -> (Tree, Rc<[u8]>) {
        (self.modules[m].tree.clone(), self.modules[m].source.clone())
    }

    /// Value of `exports.<name>` in module `m`: the last top-level assignment that is not
    /// the `void 0` initialisation.
    fn export(&mut self, m: usize, name: &str, depth: usize) -> Val {
        let key = (m, format!("exports.{name}"));
        if self.pending.contains(&key) {
            return Val::Opaque;
        }
        let mut found: Option<Bytes> = None;
        {
            let module = &self.modules[m];
            let source = &module.source;
            for stmt in named(module.tree.root_node()) {
                if stmt.kind() != "expression_statement" {
                    continue;
                }
                let Some(mut e) = named(stmt).into_iter().next() else { continue };
                while e.kind() == "assignment_expression" {
                    let (Some(left), Some(right)) =
                        (e.child_by_field_name("left"), e.child_by_field_name("right"))
                    else {
                        break;
                    };
                    let target = left.kind() == "member_expression"
                        && left
                            .child_by_field_name("object")
                            .is_some_and(|o| text(o, source) == "exports")
                        && left
                            .child_by_field_name("property")
                            .is_some_and(|p| text(p, source) == name);
                    let mut value = right;
                    while value.kind() == "assignment_expression" {
                        match value.child_by_field_name("right") {
                            Some(r) => value = r,
                            None => break,
                        }
                    }
                    let void = value.kind() == "unary_expression" && text(value, source).starts_with("void");
                    if target && !void {
                        found = Some((value.start_byte(), value.end_byte()));
                    }
                    e = right;
                }
            }
        }
        let Some((s, e)) = found else { return Val::Opaque };
        self.pending.push(key);
        let env = self.modules[m].env.clone();
        let v = self.expr_at(m, s, e, &env, depth);
        self.pending.pop();
        v
    }

    /// A top-level binding of module `m` (function declaration or variable declarator).
    fn top_level(&mut self, m: usize, name: &str, depth: usize) -> Option<Val> {
        let key = (m, name.to_string());
        if self.pending.contains(&key) {
            return Some(Val::Opaque);
        }
        let mut found: Option<(bool, usize, usize)> = None;
        {
            let module = &self.modules[m];
            let source = &module.source;
            for stmt in named(module.tree.root_node()) {
                match stmt.kind() {
                    "function_declaration"
                        if stmt
                            .child_by_field_name("name")
                            .is_some_and(|n| text(n, source) == name) =>
                    {
                        found = Some((true, stmt.start_byte(), stmt.end_byte()));
                    }
                    "lexical_declaration" | "variable_declaration" => {
                        for d in named(stmt) {
                            if d.kind() != "variable_declarator" {
                                continue;
                            }
                            let is_name = d
                                .child_by_field_name("name")
                                .is_some_and(|n| n.kind() == "identifier" && text(n, source) == name);
                            if let (true, Some(v)) = (is_name, d.child_by_field_name("value")) {
                                found = Some((false, v.start_byte(), v.end_byte()));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let (is_fn, s, e) = found?;
        let env = self.modules[m].env.clone();
        if is_fn {
            let v = Val::Func(Rc::new(Closure {
                module: m,
                start: s,
                end: e,
                env,
            }));
            self.modules[m].env.set(name, v.clone());
            return Some(v);
        }
        self.pending.push(key);
        let v = self.expr_at(m, s, e, &env, depth);
        self.pending.pop();
        self.modules[m].env.set(name, v.clone());
        Some(v)
    }

    fn lookup(&mut self, m: usize, name: &str, env: &Rc<Env>, depth: usize) -> Val {
        if let Some(v) = env.get(name) {
            return v;
        }
        match name {
            "exports" => return Val::Exports(m),
            "undefined" => return Val::Undefined,
            _ => {}
        }
        self.top_level(m, name, depth).unwrap_or(Val::Opaque)
    }

    fn expr_at(&mut self, m: usize, start: usize, end: usize, env: &Rc<Env>, depth: usize) -> Val {
        self.expr(m, (start, end), env, depth)
    }

    fn field(&mut self, obj: Val, name: &str, depth: usize) -> Val {
        match obj {
            Val::Obj(pairs) => pairs
                .into_iter()
                .rev()
                .find(|(k, _)| matches!(k, Val::Str(s) if s == name))
                .map(|(_, v)| v)
                .unwrap_or(Val::Undefined),
            Val::Arg(i) => Val::ArgField(i, name.to_string()),
            Val::Module(i) | Val::Exports(i) => self.export(i, name, depth + 1),
            Val::Descriptor if name == "value" => Val::Decorated,
            Val::Choice(c) => {
                let vals = c.into_iter().map(|v| self.field(v, name, depth)).collect();
                choice(vals)
            }
            Val::Str(_) if name == "length" => Val::Opaque,
            _ => Val::Member(name.to_string()),
        }
    }

    fn params(&self, m: usize, f: Node<'_>) -> Vec<(String, Option<Bytes>, bool)> {
        let source = &self.modules[m].source;
        let mut out = Vec::new();
        if let Some(p) = f.child_by_field_name("parameter") {
            out.push((text(p, source).to_string(), None, false));
            return out;
        }
        let Some(ps) = f.child_by_field_name("parameters") else { return out };
        for p in named(ps) {
            match p.kind() {
                "identifier" => out.push((text(p, source).to_string(), None, false)),
                "assignment_pattern" => {
                    let name = p
                        .child_by_field_name("left")
                        .map(|l| text(l, source).to_string())
                        .unwrap_or_default();
                    let default = p.child_by_field_name("right").map(|r| (r.start_byte(), r.end_byte()));
                    out.push((name, default, false));
                }
                "rest_pattern" => out.push((
                    named(p)
                        .first()
                        .map(|i| text(*i, source).to_string())
                        .unwrap_or_default(),
                    None,
                    true,
                )),
                _ => out.push((String::new(), None, false)),
            }
        }
        out
    }

    /// Call a closure with argument values (missing arguments: `None`).
    fn call(&mut self, c: &Rc<Closure>, args: &[Option<Val>], depth: usize) -> Val {
        if depth > MAX_DEPTH {
            return Val::Opaque;
        }
        let m = c.module;
        let (tree, _) = self.handle(m);
        let Some(f) = node_at(&tree, c.start, c.end) else { return Val::Opaque };
        let params = self.params(m, f);
        let body = f
            .child_by_field_name("body")
            .map(|b| (b.start_byte(), b.end_byte(), b.kind() == "statement_block"));
        let env = Env::child(&c.env);
        for (i, (name, default, rest)) in params.into_iter().enumerate() {
            if name.is_empty() {
                continue;
            }
            let v = match (args.get(i).cloned().flatten(), default) {
                _ if rest => Val::Opaque,
                (Some(v), _) => v,
                (None, Some((s, e))) => self.expr(m, (s, e), &env, depth),
                (None, None) => Val::Undefined,
            };
            env.set(&name, v);
        }
        let Some((s, e, block)) = body else { return Val::Opaque };
        if block {
            let mut returns = Vec::new();
            self.block(m, (s, e), &env, depth, &mut returns);
            if returns.is_empty() {
                Val::Undefined
            } else {
                choice(returns)
            }
        } else {
            self.expr(m, (s, e), &env, depth)
        }
    }

    fn bind_pattern(&mut self, m: usize, pattern: (usize, usize), v: Val, env: &Rc<Env>) {
        let (tree, source) = self.handle(m);
        let Some(p) = node_at(&tree, pattern.0, pattern.1) else { return };
        match p.kind() {
            "identifier" => env.set(text(p, &source), v),
            "array_pattern" => {
                let items: Vec<(usize, usize, String)> = named(p)
                    .into_iter()
                    .map(|i| (i.start_byte(), i.end_byte(), i.kind().to_string()))
                    .collect();
                for (idx, (s, e, kind)) in items.into_iter().enumerate() {
                    let element = element_of(&v, idx);
                    if kind == "identifier" || kind == "array_pattern" || kind == "object_pattern" {
                        self.bind_pattern(m, (s, e), element, env);
                    }
                }
            }
            "object_pattern" => {
                for item in named(p) {
                    match item.kind() {
                        "shorthand_property_identifier_pattern" => {
                            let name = text(item, &source).to_string();
                            let fv = self.field(v.clone(), &name, 0);
                            env.set(&name, fv);
                        }
                        "pair_pattern" => {
                            let (Some(k), Some(val)) =
                                (item.child_by_field_name("key"), item.child_by_field_name("value"))
                            else {
                                continue;
                            };
                            let fv = self.field(v.clone(), text(k, &source), 0);
                            self.bind_pattern(m, (val.start_byte(), val.end_byte()), fv, env);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn block(&mut self, m: usize, span: (usize, usize), env: &Rc<Env>, depth: usize, returns: &mut Vec<Val>) {
        let (tree, _) = self.handle(m);
        let Some(b) = node_at(&tree, span.0, span.1) else { return };
        let stmts: Vec<(usize, usize)> =
            named(b).into_iter().map(|s| (s.start_byte(), s.end_byte())).collect();
        for s in stmts {
            self.statement(m, s, env, depth, returns);
        }
    }

    fn statement(
        &mut self,
        m: usize,
        span: (usize, usize),
        env: &Rc<Env>,
        depth: usize,
        returns: &mut Vec<Val>,
    ) {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            return;
        }
        let (tree, source) = self.handle(m);
        let Some(s) = node_at(&tree, span.0, span.1) else { return };
        let kind = s.kind().to_string();
        let kids: Vec<(usize, usize)> =
            named(s).into_iter().map(|k| (k.start_byte(), k.end_byte())).collect();
        match kind.as_str() {
            "lexical_declaration" | "variable_declaration" => {
                let decls: Vec<(Option<Bytes>, Option<Bytes>)> = named(s)
                    .into_iter()
                    .filter(|d| d.kind() == "variable_declarator")
                    .map(|d| {
                        (
                            d.child_by_field_name("name").map(|n| (n.start_byte(), n.end_byte())),
                            d.child_by_field_name("value").map(|v| (v.start_byte(), v.end_byte())),
                        )
                    })
                    .collect();
                for (name, value) in decls {
                    let v = match value {
                        Some(v) => self.expr(m, v, env, depth),
                        None => Val::Undefined,
                    };
                    if let Some(n) = name {
                        self.bind_pattern(m, n, v, env);
                    }
                }
            }
            "return_statement" => {
                let v = match kids.first() {
                    Some(&k) => self.expr(m, k, env, depth),
                    None => Val::Undefined,
                };
                returns.push(v);
            }
            "expression_statement" => {
                if let Some(&k) = kids.first() {
                    self.expr(m, k, env, depth);
                }
            }
            "statement_block" => self.block(m, span, env, depth, returns),
            "if_statement" | "else_clause" => {
                let (cons, alt) = (
                    s.child_by_field_name("consequence")
                        .map(|c| (c.start_byte(), c.end_byte())),
                    s.child_by_field_name("alternative")
                        .map(|c| (c.start_byte(), c.end_byte())),
                );
                if kind == "else_clause" {
                    for k in kids {
                        self.statement(m, k, env, depth, returns);
                    }
                    return;
                }
                if let Some(c) = cons {
                    self.statement(m, c, env, depth, returns);
                }
                if let Some(a) = alt {
                    self.statement(m, a, env, depth, returns);
                }
            }
            "function_declaration" => {
                if let Some(n) = s.child_by_field_name("name") {
                    let name = text(n, &source).to_string();
                    env.set(
                        &name,
                        Val::Func(Rc::new(Closure {
                            module: m,
                            start: span.0,
                            end: span.1,
                            env: env.clone(),
                        })),
                    );
                }
            }
            // An expression statement spanning exactly its expression.
            _ => {
                self.expr(m, span, env, depth);
            }
        }
    }

    fn expr(&mut self, m: usize, span: (usize, usize), env: &Rc<Env>, depth: usize) -> Val {
        self.steps += 1;
        if self.steps > MAX_STEPS || depth > MAX_DEPTH {
            return Val::Opaque;
        }
        let (tree, source) = self.handle(m);
        let Some(n) = node_at(&tree, span.0, span.1) else { return Val::Opaque };
        let kind = n.kind().to_string();
        let field = |name: &str| n.child_by_field_name(name).map(|c| (c.start_byte(), c.end_byte()));
        let kids: Vec<(usize, usize)> =
            named(n).into_iter().map(|k| (k.start_byte(), k.end_byte())).collect();
        match kind.as_str() {
            "string" => Val::Str(string_value(n, &source)),
            "identifier" => {
                let name = text(n, &source).to_string();
                self.lookup(m, &name, env, depth)
            }
            "undefined" => Val::Undefined,
            "parenthesized_expression" => kids
                .last()
                .map(|&k| self.expr(m, k, env, depth))
                .unwrap_or(Val::Opaque),
            "sequence_expression" => {
                let mut last = Val::Opaque;
                for k in kids {
                    last = self.expr(m, k, env, depth);
                }
                last
            }
            "arrow_function" | "function_expression" | "function" => Val::Func(Rc::new(Closure {
                module: m,
                start: span.0,
                end: span.1,
                env: env.clone(),
            })),
            "member_expression" => {
                let (Some(o), Some(p)) = (field("object"), n.child_by_field_name("property")) else {
                    return Val::Opaque;
                };
                let name = text(p, &source).to_string();
                let obj = self.expr(m, o, env, depth);
                self.field(obj, &name, depth)
            }
            "subscript_expression" => {
                let (Some(o), Some(i)) = (field("object"), field("index")) else { return Val::Opaque };
                let obj = self.expr(m, o, env, depth);
                match self.expr(m, i, env, depth) {
                    Val::Str(k) => self.field(obj, &k, depth),
                    _ => Val::Opaque,
                }
            }
            "object" => {
                let mut pairs = Vec::new();
                for item in named(n) {
                    match item.kind() {
                        "pair" => {
                            let (Some(k), Some(v)) =
                                (item.child_by_field_name("key"), item.child_by_field_name("value"))
                            else {
                                continue;
                            };
                            let key = match k.kind() {
                                "property_identifier" | "number" => Val::Str(text(k, &source).to_string()),
                                "string" => Val::Str(string_value(k, &source)),
                                "computed_property_name" => match named(k).first() {
                                    Some(e) => self.expr(m, (e.start_byte(), e.end_byte()), env, depth),
                                    None => Val::Opaque,
                                },
                                _ => Val::Opaque,
                            };
                            let value = self.expr(m, (v.start_byte(), v.end_byte()), env, depth);
                            pairs.push((key, value));
                        }
                        "shorthand_property_identifier" => {
                            let name = text(item, &source).to_string();
                            let value = self.lookup(m, &name, env, depth);
                            pairs.push((Val::Str(name), value));
                        }
                        _ => {}
                    }
                }
                Val::Obj(pairs)
            }
            "array" => Val::Arr(kids.into_iter().map(|k| self.expr(m, k, env, depth)).collect()),
            "ternary_expression" => {
                let (Some(c), Some(a)) = (field("consequence"), field("alternative")) else {
                    return Val::Opaque;
                };
                let cv = self.expr(m, c, env, depth);
                let av = self.expr(m, a, env, depth);
                choice(vec![cv, av])
            }
            "binary_expression" => {
                let op = n
                    .child_by_field_name("operator")
                    .map(|o| text(o, &source).to_string())
                    .unwrap_or_default();
                let (Some(l), Some(r)) = (field("left"), field("right")) else { return Val::Opaque };
                match op.as_str() {
                    "||" | "??" => {
                        let lv = self.expr(m, l, env, depth);
                        if present(&lv) {
                            return lv;
                        }
                        let rv = self.expr(m, r, env, depth);
                        choice(vec![lv, rv])
                    }
                    "&&" => self.expr(m, r, env, depth),
                    _ => Val::Opaque,
                }
            }
            "assignment_expression" => field("right")
                .map(|r| self.expr(m, r, env, depth))
                .unwrap_or(Val::Opaque),
            "call_expression" => self.call_expr(m, n.start_byte(), n.end_byte(), env, depth),
            _ => Val::Opaque,
        }
    }

    fn call_expr(&mut self, m: usize, start: usize, end: usize, env: &Rc<Env>, depth: usize) -> Val {
        let (tree, source) = self.handle(m);
        let Some(n) = node_at(&tree, start, end) else { return Val::Opaque };
        let (Some(func), Some(args)) =
            (n.child_by_field_name("function"), n.child_by_field_name("arguments"))
        else {
            return Val::Opaque;
        };
        let func_span = (func.start_byte(), func.end_byte());
        let spelled = text(func, &source).to_string();
        let arg_spans: Vec<(usize, usize)> = named(args)
            .into_iter()
            .map(|a| (a.start_byte(), a.end_byte()))
            .collect();
        if func.kind() == "identifier" && spelled == "require" && env.get("require").is_none() {
            let spec = named(args)
                .first()
                .filter(|a| a.kind() == "string")
                .map(|a| string_value(*a, &source));
            return match spec {
                Some(s) => self.require(m, &s),
                None => Val::Opaque,
            };
        }
        let arg_vals: Vec<Val> = arg_spans.iter().map(|&a| self.expr(m, a, env, depth)).collect();
        if spelled == self.root {
            self.record(&arg_vals);
            return Val::Opaque;
        }
        match self.expr(m, func_span, env, depth) {
            Val::Func(c) => {
                let args: Vec<Option<Val>> = arg_vals.into_iter().map(Some).collect();
                self.call(&c, &args, depth + 1)
            }
            _ => Val::Opaque,
        }
    }

    /// A metadata store: key at the row's position, the decorated target among the other
    /// arguments (by value flow), the stored value the one remaining argument.
    fn record(&mut self, args: &[Val]) {
        let Some(Val::Str(key)) = args.get(self.key_pos).map(resolve_choice) else { return };
        let mut on_member = None;
        let mut rest = Vec::new();
        for (i, a) in args.iter().enumerate() {
            if i == self.key_pos {
                continue;
            }
            match a {
                Val::Decorated => on_member = Some(true),
                Val::Target => on_member = Some(on_member.unwrap_or(false)),
                Val::MemberName => on_member = Some(true),
                other => rest.push(other.clone()),
            }
        }
        let Some(on_member) = on_member else { return };
        let value = match rest.as_slice() {
            [v] => meta_value(v, 0),
            _ => MetaValue::Other,
        };
        self.writes.push(MetaWrite {
            key,
            value,
            on_member,
        });
    }
}

/// A choice of one constant string is that string.
fn resolve_choice(v: &Val) -> Val {
    match v {
        Val::Choice(c) => {
            let strings: Vec<&String> = c
                .iter()
                .filter_map(|x| if let Val::Str(s) = x { Some(s) } else { None })
                .collect();
            if strings.len() == c.len() && strings.windows(2).all(|w| w[0] == w[1]) {
                strings.first().map(|s| Val::Str((*s).clone())).unwrap_or(Val::Opaque)
            } else {
                v.clone()
            }
        }
        other => other.clone(),
    }
}

fn element_of(v: &Val, idx: usize) -> Val {
    match v {
        Val::Arr(items) => items.get(idx).cloned().unwrap_or(Val::Undefined),
        Val::Choice(c) => choice(c.iter().map(|x| element_of(x, idx)).collect()),
        _ => Val::Opaque,
    }
}

fn meta_value(v: &Val, depth: usize) -> MetaValue {
    if depth > 8 {
        return MetaValue::Other;
    }
    match v {
        Val::Arg(i) => MetaValue::Arg(*i),
        Val::ArgField(i, f) => MetaValue::ArgField(*i, f.clone()),
        Val::Str(s) => MetaValue::Str(s.clone()),
        Val::Member(n) => MetaValue::Member(n.clone()),
        Val::Choice(c) => MetaValue::Choice(c.iter().map(|x| meta_value(x, depth + 1)).collect()),
        _ => MetaValue::Other,
    }
}

/// The byte offset of a 0-based line / byte column.
fn offset_of(source: &[u8], line: u32, column: u32) -> Option<usize> {
    let mut start = 0usize;
    for _ in 0..line {
        start += source.get(start..)?.iter().position(|&b| b == b'\n')? + 1;
    }
    Some(start + column as usize)
}

/// The factory value declared at `at` in module `m`: a function declaration, a variable
/// declarator's value, or an `exports.X = value` assignment.
fn declared_at(ev: &mut Eval, m: usize, at: usize) -> Option<Val> {
    let found = {
        let (tree, source) = ev.handle(m);
        let mut n = tree.root_node().descendant_for_byte_range(at, at)?;
        // `exports.X` at the position: the module's value of the export (its initialising
        // `void 0` chains are skipped).
        if let Some(member) = std::iter::successors(Some(n), |x| x.parent())
            .take(3)
            .find(|x| x.kind() == "member_expression")
            .filter(|x| {
                x.child_by_field_name("object")
                    .is_some_and(|o| text(o, &source) == "exports")
            })
            .filter(|x| {
                x.parent().is_some_and(|p| {
                    p.kind() == "assignment_expression" && p.child_by_field_name("left") == Some(*x)
                })
            })
        {
            let name = member
                .child_by_field_name("property")
                .map(|p| text(p, &source).to_string())?;
            return Some(ev.export(m, &name, 0));
        }
        loop {
            match n.kind() {
                "function_declaration" => break Some((true, n.start_byte(), n.end_byte())),
                "variable_declarator" => {
                    let v = n.child_by_field_name("value")?;
                    break Some((false, v.start_byte(), v.end_byte()));
                }
                "lexical_declaration" | "variable_declaration" => {
                    let d = named(n).into_iter().find(|d| d.kind() == "variable_declarator")?;
                    let v = d.child_by_field_name("value")?;
                    break Some((false, v.start_byte(), v.end_byte()));
                }
                "assignment_expression" => {
                    let v = n.child_by_field_name("right")?;
                    break Some((false, v.start_byte(), v.end_byte()));
                }
                "expression_statement" => {
                    let e = named(n).into_iter().next()?;
                    if e.kind() != "assignment_expression" {
                        break None;
                    }
                    let v = e.child_by_field_name("right")?;
                    break Some((false, v.start_byte(), v.end_byte()));
                }
                "program" => break None,
                _ => n = n.parent()?,
            }
        }
    };
    let (is_fn, s, e) = found?;
    let env = ev.modules[m].env.clone();
    if is_fn {
        return Some(Val::Func(Rc::new(Closure {
            module: m,
            start: s,
            end: e,
            env,
        })));
    }
    Some(ev.expr(m, (s, e), &env, 0))
}

/// The identifier at `at` of a declaration file (`.d.ts`): the declared name.
fn declared_name(path: &Path, line: u32, column: u32) -> Option<String> {
    let source = std::fs::read(path).ok()?;
    let at = offset_of(&source, line, column)?;
    let tree = trace_syntax::parse_tree(Language::TypeScript, &source).ok()?;
    let n = tree.root_node().descendant_for_byte_range(at, at)?;
    matches!(n.kind(), "identifier" | "property_identifier" | "type_identifier")
        .then(|| text(n, &source).to_string())
}

/// Metadata writes of the decorator a factory call returns: the factory is the library
/// declaration at `line` / `column` of `file` (a JavaScript implementation, or a declaration
/// file whose implementation is found next to it: the declared name's export), called with
/// `arg_count` arguments and the returned decorator applied to a member. `root` is the
/// spelling of the metadata-store row (`symbol`), `key_pos` its key argument.
pub fn decorator_writes(
    file: &Path,
    line: u32,
    column: u32,
    arg_count: u32,
    root: &str,
    key_pos: u32,
) -> Vec<MetaWrite> {
    let text_path = file.to_string_lossy();
    let (implementation, name) =
        if text_path.ends_with(".d.ts") || text_path.ends_with(".d.mts") || text_path.ends_with(".d.cts") {
            let Some(js) =
                crate::languages::javascript::implementation_of_declaration(Path::new(&*text_path))
            else {
                return Vec::new();
            };
            (js, declared_name(file, line, column))
        } else {
            (file.to_path_buf(), None)
        };
    let mut ev = Eval {
        modules: Vec::new(),
        root: root.to_string(),
        key_pos: key_pos as usize,
        writes: Vec::new(),
        steps: 0,
        pending: Vec::new(),
    };
    let Some(m) = ev.load(&implementation) else { return Vec::new() };
    let source = ev.modules[m].source.clone();
    let factory = match name {
        Some(n) => {
            let v = ev.export(m, &n, 0);
            if matches!(v, Val::Opaque) {
                ev.top_level(m, &n, 0)
            } else {
                Some(v)
            }
        }
        None => offset_of(&source, line, column).and_then(|at| declared_at(&mut ev, m, at)),
    };
    let Some(Val::Func(factory)) = factory else { return Vec::new() };
    let args: Vec<Option<Val>> = (0..arg_count).map(|i| Some(Val::Arg(i))).collect();
    let decorators = match ev.call(&factory, &args, 1) {
        Val::Choice(c) => c,
        v => vec![v],
    };
    for d in decorators {
        if let Val::Func(d) = d {
            ev.call(&d, &[Some(Val::Target), Some(Val::MemberName), Some(Val::Descriptor)], 1);
        }
    }
    let mut writes = ev.writes;
    writes.dedup();
    writes
}

#[cfg(test)]
#[path = "../../tests/unit/reflect/decorator_meta.rs"]
mod tests;
