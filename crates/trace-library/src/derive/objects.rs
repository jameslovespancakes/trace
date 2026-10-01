//! Prototype object model rules of the derivation (languages whose adapter has an
//! [`ObjectModel`](crate::languages::objects::ObjectModel): JavaScript / TypeScript). Written
//! once over the IR; the adapter supplies the spellings.
//!
//! * **Object classes**: functions assigned as members of one module-level object
//!   (`app.use = function ..`, `Route.prototype.dispatch = function ..`) are the methods of a
//!   class of that object; a module-level function of the same name (`function Route(path)`)
//!   is its constructor; `this` inside them is an instance.
//! * **Computed member definitions**: `obj[k] = function ..` / `X.prototype[k] = ..` /
//!   `this[k] = (..) => ..` define methods of that class under the values of `k`; an index
//!   read `obj[k]` of an instance is those methods (the ones named by `k`'s literal values when
//!   known, else every computed member of the class - never a method of another class).
//! * **The arguments object**: a non-arrow function reading `arguments` gets a rest parameter
//!   for the arguments beyond its named ones; `arguments` is every parameter, and
//!   `arguments.slice(n)` / `slice.call(arguments, n)` the parameters from `n`.
//! * **Receiver-binding invocations**: `f.call(t, a, b)` calls `f(a, b)`, `f.apply(t, arr)`
//!   calls `f` with the elements of `arr`, `f.bind(t)` is `f`; `P.prototype.m.call(r, ..)` of a
//!   built-in prototype is `r.m(..)`.
//! * **Prototype links and mixins**: `Object.setPrototypeOf(o, p)`, `o.__proto__ = p` and member
//!   copies (`CopiesMembers`) make the function object `o` an instance of `p`'s class: its
//!   members are `p`'s methods, and calling an instance of the class calls that function.
//! * **Module values**: `module.exports = v` / `exports = v` is the value of the module
//!   (calling / constructing a required module calls / constructs `v`).
//! * **Registration through entry objects**: an instance whose constructor stores one argument
//!   where dispatch code calls it (the handler) and keeps another (the key), stored into a
//!   dispatched container, registers the handler under the key; without a key argument the
//!   receiver's own key (held by the receiver object) is the key (`RegistersSelf`).

use std::collections::{BTreeSet, HashMap, HashSet};

use trace_core::facts::{Expr, FlowFact, ParamKind, Scope};
use trace_core::model::SymbolKind;
use trace_syntax::lower::NUMBER_PREFIX;

use super::{Callee, Chan, Class, How, PInfo, Program, State, Vals, CALLS, STORED, V};
use crate::languages::objects::ObjectModel;

/// Link-time facts of the object model.
#[derive(Default)]
pub(super) struct Objects {
    /// Classes made from object members (no summaries of their own).
    pub synthetic: HashSet<u32>,
    /// (unit, module-level name) -> the class of the object the name holds.
    pub object_globals: HashMap<(u32, String), u32>,
    /// Per unit: expressions of the module's exported value.
    pub exports: HashMap<u32, Vec<Expr>>,
    /// Per unit: module-level names bound to the exported value (`app = exports = ..`).
    pub export_names: HashMap<u32, Vec<String>>,
    /// Class -> computed-member functions: (function, defining function, key).
    pub computed: HashMap<u32, Vec<(u32, u32, Expr)>>,
    /// Function -> index of its arguments-object rest parameter.
    pub arguments: HashMap<u32, u16>,
}

/// (constructor, constructor parameter, parameter of the building function).
pub(super) type HeldArg = (u32, u16, u16);

/// Solve-time facts of the object model.
#[derive(Default)]
pub(super) struct ObjState {
    /// Function objects that are instances of classes (prototype links, mixins).
    pub fn_classes: HashMap<u32, BTreeSet<u32>>,
    /// Class -> function objects that are its instances.
    pub class_fns: HashMap<u32, BTreeSet<u32>>,
    /// (function, class) -> (constructor, constructor parameter, parameter of the function):
    /// the function's parameter flows into that constructor parameter of an instance of the
    /// class it builds (directly or through factories). A constructor parameter stored to be
    /// called is a handler role, any other a key role (decided when used: masks grow).
    pub held_args: HashMap<(u32, u32), BTreeSet<HeldArg>>,
    /// Parameter values stored into another parameter container: (function, from, into).
    pub pflow: BTreeSet<(u32, u16, u16)>,
}

/// Collected at unit load: module-level export expressions and binds.
pub(super) struct UnitExports {
    pub exports: Vec<Expr>,
    pub vars: Vec<(String, Expr)>,
}

/// Whether `e` reads the name `name`.
fn mentions(e: &Expr, name: &str) -> bool {
    match e {
        Expr::Name { name: n, .. } => n == name,
        Expr::Attr { object, .. } => mentions(object, name),
        Expr::Call {
            func, args, kwargs, ..
        } => {
            mentions(func, name)
                || args.iter().any(|a| mentions(a, name))
                || kwargs.iter().any(|(_, v)| mentions(v, name))
        }
        Expr::Choice(alts) => alts.iter().any(|a| mentions(a, name)),
        Expr::Await(inner) => mentions(inner, name),
        Expr::Lambda { .. } | Expr::Opaque => false,
    }
}

fn expr_span_start(e: &Expr) -> Option<u32> {
    e.span().map(|s| s.start)
}

impl Program<'_> {
    fn model(&self) -> Option<&ObjectModel> {
        self.spec.objects.as_ref()
    }

    /// Module-level export expressions and variable binds of one lowered file.
    pub(super) fn unit_exports(&self, flow: &[FlowFact], module_decl: Option<u32>) -> Option<UnitExports> {
        let model = self.model()?;
        let mut out = UnitExports {
            exports: Vec::new(),
            vars: Vec::new(),
        };
        for fact in flow {
            let FlowFact::Bind { target, value, scope } = fact else { continue };
            let module_scope = match scope {
                Scope::Module => true,
                Scope::Decl(d) => Some(*d) == module_decl,
            };
            if !module_scope {
                continue;
            }
            if model.is_export_target(target) {
                out.exports.push(value.clone());
            } else if let trace_core::facts::BindTarget::Var { name, .. } = target {
                out.vars.push((name.clone(), value.clone()));
            }
        }
        Some(out)
    }

    /// Record a unit's exports (after it was added).
    pub(super) fn record_exports(&mut self, u: u32, found: Option<UnitExports>) {
        let Some(found) = found else { return };
        let starts: HashSet<u32> = found.exports.iter().filter_map(expr_span_start).collect();
        let names: Vec<String> = found
            .vars
            .iter()
            .filter(|(_, v)| expr_span_start(v).is_some_and(|s| starts.contains(&s)))
            .map(|(n, _)| n.clone())
            .collect();
        if !names.is_empty() {
            self.objects.export_names.insert(u, names);
        }
        // `module.exports = require("./x")`: the loaded module is an import of this one.
        let Some(model) = self.model().cloned() else { return };
        for e in &found.exports {
            let Expr::Call { func, args, .. } = e else { continue };
            if let Some(target) = model.required_module(func, args).map(str::to_string) {
                self.units[u as usize].imports.push(super::Imp {
                    local: format!("<{}>{target}", model.require),
                    target,
                    kind: trace_core::facts::ImportKind::Module,
                    scope_func: None,
                    resolved: None,
                });
            }
        }
        if !found.exports.is_empty() {
            self.objects.exports.insert(u, found.exports);
        }
    }

    /// The function declared by a function expression (`Lambda`), also when the lowering
    /// named the expression after its assignment target (`module.exports = function ..`).
    fn lambda_func(&self, u: u32, value: &Expr) -> Option<u32> {
        let Expr::Lambda { span, function } = value else { return None };
        let unit = &self.units[u as usize];
        if let Some(d) = function {
            return unit.decl_func.get(d).copied();
        }
        unit.file
            .facts
            .declarations
            .iter()
            .enumerate()
            .find(|(_, d)| {
                d.kind.is_callable() && d.span.bytes.start == span.start && d.span.bytes.end == span.end
            })
            .and_then(|(d, _)| unit.decl_func.get(&(d as u32)).copied())
    }

    /// The synthetic class of module-level name `name` of unit `u` (created on first use).
    fn object_class(&mut self, u: u32, name: &str) -> Option<u32> {
        if let Some(&c) = self.objects.object_globals.get(&(u, name.to_string())) {
            return Some(c);
        }
        let global = self.units[u as usize].globals.get(name).copied()?;
        let ctor = match global {
            super::Global::Func(f) => Some(f),
            super::Global::Var => {
                let m = self.funcs.iter().position(|f| f.unit == u && f.is_module)? as u32;
                let values = self.funcs[m as usize]
                    .global_stores
                    .iter()
                    .filter(|(n, _)| n == name)
                    .map(|(_, v)| v.clone())
                    .collect::<Vec<_>>();
                values.iter().find_map(|v| self.lambda_func(u, v))
            }
            super::Global::Class(_) => return None,
        };
        let decl = ctor.map(|f| self.funcs[f as usize].decl).unwrap_or(0);
        let c = self.classes.len() as u32;
        self.classes.push(Class {
            unit: u,
            decl,
            qualified: name.to_string(),
            base_names: Vec::new(),
            bases: Vec::new(),
            unresolved_bases: Vec::new(),
            methods: HashMap::new(),
            clauses: HashMap::new(),
            nested_classes: HashMap::new(),
            subclasses: Vec::new(),
            is_interface: false,
        });
        self.objects.synthetic.insert(c);
        self.objects.object_globals.insert((u, name.to_string()), c);
        if let Some(f) = ctor {
            let receiver = self.model().map(|m| m.receiver.to_string());
            let func = &mut self.funcs[f as usize];
            if func.class.is_none() {
                func.class = Some(c);
                func.is_ctor = true;
                func.self_param = receiver;
                // The constructor under the language's constructor name (`ctors`).
                if let Some(name) = self.spec.constructors.first() {
                    self.classes[c as usize].methods.insert((*name).to_string(), f);
                }
            }
        }
        Some(c)
    }

    fn make_method(&mut self, f: u32, c: u32, name: Option<&str>) {
        let receiver = self.model().map(|m| m.receiver.to_string());
        let func = &mut self.funcs[f as usize];
        func.class = Some(c);
        if func.self_param.is_none() {
            func.self_param = receiver;
        }
        if let Some(n) = name {
            self.classes[c as usize].methods.entry(n.to_string()).or_insert(f);
            self.classes[c as usize]
                .clauses
                .entry(n.to_string())
                .or_default()
                .push(f);
        }
    }

    /// The module-level object an object expression names (`X`, `X.prototype`).
    fn object_root<'e>(&self, e: &'e Expr) -> Option<&'e str> {
        let model = self.model()?;
        match e {
            Expr::Name { name, .. } if name != model.receiver => Some(name),
            Expr::Attr { object, attr, .. } if attr == model.prototype => match object.as_ref() {
                Expr::Name { name, .. } => Some(name),
                _ => None,
            },
            _ => None,
        }
    }

    /// Link-time object model: object classes, computed members, arguments parameters.
    pub(super) fn link_objects(&mut self) {
        let Some(model) = self.model().cloned() else { return };
        // Member functions of module-level objects.
        for f in 0..self.funcs.len() {
            let (u, d) = (self.funcs[f].unit, self.funcs[f].decl);
            if self.funcs[f].class.is_some() || self.funcs[f].is_module {
                continue;
            }
            let decl = &self.units[u as usize].file.facts.declarations[d as usize];
            if decl.kind != SymbolKind::Method {
                continue;
            }
            let Some(container) = decl.container.clone() else { continue };
            if container.contains('.') || decl.parent.is_some() {
                continue;
            }
            let name = decl.name.clone();
            if let Some(c) = self.object_class(u, &container) {
                self.make_method(f as u32, c, Some(&name));
            }
        }
        // Computed member definitions.
        for f in 0..self.funcs.len() {
            let u = self.funcs[f].unit;
            let stores = self.funcs[f].index_stores.clone();
            for (object, key, value) in stores {
                let Some(g) = self.lambda_func(u, &value) else { continue };
                let class = match &object {
                    Expr::Name { name, .. } if *name == model.receiver => self.enclosing_class(f as u32),
                    other => match self.object_root(other) {
                        Some(root) => {
                            let root = root.to_string();
                            self.object_class(u, &root)
                        }
                        None => None,
                    },
                };
                let Some(c) = class else { continue };
                self.make_method(g, c, None);
                self.objects.computed.entry(c).or_default().push((g, f as u32, key));
            }
        }
        // The arguments object of non-arrow functions.
        for f in 0..self.funcs.len() {
            let func = &self.funcs[f];
            if func.is_module
                || func
                    .params
                    .iter()
                    .any(|p| p.name == model.arguments || p.kind == ParamKind::VarPositional)
            {
                continue;
            }
            let unit = &self.units[func.unit as usize];
            let decl = &unit.file.facts.declarations[func.decl as usize];
            let mut own: Vec<&Expr> = Vec::new();
            own.extend(func.evals.iter());
            own.extend(func.returns.iter());
            own.extend(func.iterates.iter());
            own.extend(func.locals.values().flatten());
            own.extend(func.global_stores.iter().map(|(_, v)| v));
            own.extend(func.member_stores.iter().map(|(_, _, v)| v));
            for (o, _, v) in &func.field_stores {
                own.push(o);
                own.push(v);
            }
            for (o, k, v) in &func.index_stores {
                own.extend([o, k, v]);
            }
            if !own.iter().any(|e| mentions(e, model.arguments)) {
                continue;
            }
            let header = unit
                .file
                .source
                .get(decl.span.bytes.start as usize..decl.body_start as usize)
                .unwrap_or_default();
            if header.windows(2).any(|w| w == b"=>") {
                continue;
            }
            let i = func.params.len() as u16;
            self.funcs[f].params.push(PInfo {
                name: model.arguments.to_string(),
                kind: ParamKind::VarPositional,
            });
            self.objects.arguments.insert(f as u32, i);
        }
    }

    /// A rest parameter as a registration key (object-model languages): a key is one
    /// argument, never the rest of the arguments (a flow-insensitive merge of a variable
    /// reassigned from the arguments, `path = fn`, would otherwise make one).
    pub(super) fn rest_key(&self, f: u32, k: u16) -> bool {
        self.model().is_some()
            && self.funcs[f as usize]
                .params
                .get(k as usize)
                .is_some_and(|p| p.kind == ParamKind::VarPositional)
    }

    /// Whether class `c` was made from object members.
    pub(super) fn is_synthetic(&self, c: u32) -> bool {
        self.objects.synthetic.contains(&c)
    }

    /// Value of a module-level function / variable seen through the object model: a
    /// constructor function is its class, an object variable its instance.
    pub(super) fn object_value(&self, resolved: &super::Resolved) -> Option<V> {
        match resolved {
            super::Resolved::Func(f) => {
                let func = &self.funcs[*f as usize];
                match func.class {
                    Some(c) if func.is_ctor && self.is_synthetic(c) => Some(V::Class(c)),
                    _ => None,
                }
            }
            super::Resolved::Var(u, name) => {
                let c = *self.objects.object_globals.get(&(*u, name.clone()))?;
                let has_ctor = self.funcs.iter().any(|f| f.is_ctor && f.class == Some(c));
                Some(if has_ctor { V::Class(c) } else { V::Inst(c) })
            }
            _ => None,
        }
    }

    /// String constants a module-level variable holds (`var names = ["get", "post"]`,
    /// mapped arrays of them): variables are slots, whose content keeps no literals.
    pub(super) fn global_literals(&self, st: &mut State, resolved: &[super::Resolved]) -> Vals {
        let mut out = Vals::new();
        if self.model().is_none() {
            return out;
        }
        for r in resolved {
            let super::Resolved::Var(u, name) = r else { continue };
            let Some(m) = self.funcs.iter().position(|x| x.unit == *u && x.is_module) else { continue };
            let values: Vec<Expr> = self.funcs[m]
                .global_stores
                .iter()
                .filter(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
                .collect();
            for v in values {
                out.extend(
                    self.eval(st, m as u32, &v, 1)
                        .into_iter()
                        .filter(|x| matches!(x, V::Lit(_))),
                );
            }
        }
        out
    }

    /// The value of module `u` (`module.exports = v`).
    pub(super) fn module_value(&self, st: &mut State, u: u32, depth: usize) -> Vals {
        let mut out = Vals::new();
        if self.model().is_none() || depth > self.cx.limits.max_eval_depth {
            return out;
        }
        let Some(m) = self.funcs.iter().position(|f| f.unit == u && f.is_module) else {
            return out;
        };
        let m = m as u32;
        for e in self.objects.exports.get(&u).cloned().unwrap_or_default() {
            for v in self.eval(st, m, &e, depth + 1) {
                if !matches!(v, V::Module(x) if x == u) {
                    out.insert(v);
                }
            }
        }
        for name in self.objects.export_names.get(&u).cloned().unwrap_or_default() {
            if let Some(v) = self.object_value(&super::Resolved::Var(u, name)) {
                out.insert(v);
            }
        }
        let model = self.model().expect("object model");
        if let Some(super::Global::Func(f)) = self.units[u as usize].globals.get(model.exports) {
            let r = super::Resolved::Func(*f);
            out.insert(self.object_value(&r).unwrap_or(V::Fn(*f, false)));
        }
        out
    }

    /// Values of `vals` with modules replaced by their values (for calls and members).
    pub(super) fn with_module_values(&self, st: &mut State, vals: &Vals, depth: usize) -> Vals {
        let mut out = vals.clone();
        for v in vals {
            if let V::Module(u) = *v {
                out.extend(self.module_value(st, u, depth + 1));
            }
        }
        out
    }

    /// `arguments` inside `f` (or an arrow function nested in the function it belongs to):
    /// every parameter of that function.
    pub(super) fn arguments_value(&self, f: u32, name: &str) -> Option<Vals> {
        let model = self.model()?;
        if name != model.arguments {
            return None;
        }
        let mut cur = Some(f);
        for _ in 0..16 {
            let g = cur?;
            if self.objects.arguments.contains_key(&g) {
                let n = self.funcs[g as usize].params.len() as u16;
                return Some((0..n).map(|i| V::Param(g, i, How::Direct)).collect());
            }
            cur = self.funcs[g as usize].parent;
        }
        None
    }

    /// Non-negative integer literal value of an expression.
    fn number(e: &Expr) -> Option<u16> {
        match e {
            Expr::Name { name, .. } => name.strip_prefix(NUMBER_PREFIX)?.parse().ok(),
            _ => None,
        }
    }

    /// `recv.slice(n)`: the elements of `recv` from `n` (the parameters from `n` when `recv` is
    /// the arguments object).
    fn sliced(&self, st: &mut State, f: u32, recv: &Expr, offset: Option<&Expr>, depth: usize) -> Vals {
        if let Expr::Name { name, .. } = recv {
            if let Some(all) = self.arguments_value(f, name) {
                let n = match offset {
                    None => Some(0),
                    Some(e) => Self::number(e),
                };
                return all
                    .into_iter()
                    .filter(|v| match (*v, n) {
                        (V::Param(g, i, _), Some(n)) => i >= n || self.objects.arguments.get(&g) == Some(&i),
                        _ => true,
                    })
                    .collect();
            }
        }
        self.eval(st, f, recv, depth + 1)
            .into_iter()
            .filter(|v| matches!(v, V::Slot(..) | V::Param(..)))
            .collect()
    }

    /// Spelling of a callee through module-level aliases (`var slice = Array.prototype.slice`).
    fn alias_spelling(&self, f: u32, e: &Expr) -> Option<String> {
        let sp = super::spelling(e)?;
        let root = sp.split('.').next().unwrap_or(&sp);
        if self.locally_bound(f, root) {
            return None;
        }
        if !sp.contains('.') {
            let u = self.funcs[f as usize].unit;
            if let Some(super::Global::Var) = self.units[u as usize].globals.get(&sp) {
                let m = self.funcs.iter().position(|x| x.unit == u && x.is_module)?;
                let values: Vec<&Expr> = self.funcs[m]
                    .global_stores
                    .iter()
                    .filter(|(n, _)| *n == sp)
                    .map(|(_, v)| v)
                    .collect();
                if let [only] = values.as_slice() {
                    return super::spelling(only);
                }
            }
        }
        Some(sp)
    }

    /// A built-in prototype method invoked with an explicit receiver
    /// (`Array.prototype.slice.call(r, a)`): the equivalent method call `r.slice(a)`.
    fn builtin_receiver_call(&self, f: u32, func: &Expr, args: &[Expr]) -> Option<Expr> {
        let model = self.model()?;
        let Expr::Attr {
            object,
            attr,
            span,
            attr_span,
        } = func
        else {
            return None;
        };
        if attr != model.call_with_receiver {
            return None;
        }
        let sp = self.alias_spelling(f, object)?;
        let parts: Vec<&str> = sp.split('.').collect();
        let [owner, proto, method] = parts.as_slice() else { return None };
        if *proto != model.prototype || self.locally_bound(f, owner) {
            return None;
        }
        let u = self.funcs[f as usize].unit;
        if !self.resolve_global(u, owner, 0).is_empty() {
            return None;
        }
        let recv = args.first()?.clone();
        Some(Expr::Call {
            func: Box::new(Expr::Attr {
                object: Box::new(recv),
                attr: (*method).to_string(),
                attr_span: *attr_span,
                span: *span,
            }),
            func_span: *span,
            args: args.get(1..).unwrap_or_default().to_vec(),
            kwargs: Vec::new(),
            span: *span,
            is_new: false,
        })
    }

    /// Values of object-model call forms (`None`: not one of them).
    pub(super) fn object_call_value(
        &self,
        st: &mut State,
        f: u32,
        func: &Expr,
        args: &[Expr],
        depth: usize,
    ) -> Option<Vals> {
        let model = self.model()?.clone();
        if let Some(rewritten) = self.builtin_receiver_call(f, func, args) {
            return Some(self.eval(st, f, &rewritten, depth + 1));
        }
        if let Some(target) = model.required_module(func, args).map(str::to_string) {
            if !self.locally_bound(f, model.require) {
                let u = self.funcs[f as usize].unit;
                let local = format!("<{}>{target}", model.require);
                return Some(
                    self.units[u as usize]
                        .imports
                        .iter()
                        .filter(|i| i.local == local)
                        .filter_map(|i| i.resolved.as_ref().map(|(m, _)| V::Module(*m)))
                        .collect(),
                );
            }
        }
        if let Some(sp) = super::spelling(func) {
            if !self.locally_bound(f, sp.split('.').next().unwrap_or(&sp)) {
                if let Some(&(_, i)) = model.prototype_creators.iter().find(|(n, _)| *n == sp) {
                    let mut out = Vals::new();
                    if let Some(a) = args.get(i as usize) {
                        let protos = self.eval(st, f, a, depth + 1);
                        for c in self.classes_of(st, &protos, depth) {
                            out.insert(V::Inst(c));
                        }
                    }
                    return Some(out);
                }
            }
        }
        let Expr::Attr { object, attr, .. } = func else { return None };
        if model.slice_methods.contains(&attr.as_str()) {
            return Some(self.sliced(st, f, object, args.first(), depth));
        }
        let lower = model.lower_case_methods.contains(&attr.as_str());
        if lower || model.upper_case_methods.contains(&attr.as_str()) {
            let mut out = Vals::new();
            for v in self.eval(st, f, object, depth + 1) {
                match v {
                    V::Lit(l) => {
                        let text = st.lits.get(l as usize).cloned().unwrap_or_default();
                        let cased = if lower {
                            text.to_lowercase()
                        } else {
                            text.to_uppercase()
                        };
                        out.insert(V::Lit(st.lit(&cased)));
                    }
                    other => {
                        out.extend(super::built([other].into_iter().collect()));
                    }
                }
            }
            return Some(out);
        }
        if model.mapping_methods.contains(&attr.as_str()) {
            // What the callbacks return (their parameter holds the receiver's elements).
            let mut out = Vals::new();
            for a in args {
                for v in self.eval(st, f, a, depth + 1) {
                    if let V::Fn(g, _) = v {
                        out.extend(st.ret.get(&g).into_iter().flatten().copied());
                    }
                }
            }
            return Some(out);
        }
        if *attr == model.bind_receiver {
            let vals = self.eval(st, f, object, depth + 1);
            let mut out = Vals::new();
            for v in vals {
                match v {
                    V::Fn(..) => {
                        out.insert(v);
                    }
                    V::Param(p, i, How::Direct | How::Wrapped) => {
                        out.insert(V::Param(p, i, How::Wrapped));
                    }
                    V::Slot(s, How::Direct | How::Wrapped) => {
                        out.insert(V::Slot(s, How::Wrapped));
                    }
                    _ => {}
                }
            }
            return Some(out);
        }
        if *attr == model.call_with_receiver || *attr == model.apply_with_receiver {
            let (call_args, _) = self.receiver_args(&model, attr, args);
            let mut out = Vals::new();
            for callee in self.callees(st, f, object, call_args.len() as u32) {
                if let Callee::Lib(g, _) = callee {
                    if let Some(r) = st.ret.get(&g) {
                        out.extend(r.iter().copied());
                    }
                }
            }
            return Some(out);
        }
        None
    }

    /// Arguments of `f.call(t, a, b)` (`[a, b]`) / `f.apply(t, arr)` (`arr` for every
    /// parameter: unknown positions) and the receiver `t`.
    fn receiver_args(&self, model: &ObjectModel, attr: &str, args: &[Expr]) -> (Vec<Expr>, Option<Expr>) {
        let recv = args.first().cloned();
        if attr == model.apply_with_receiver {
            return match args.get(1) {
                Some(arr) => (vec![arr.clone(); 8], recv),
                None => (Vec::new(), recv),
            };
        }
        (args.get(1..).unwrap_or_default().to_vec(), recv)
    }

    /// Object-model call forms of an evaluated call statement; `true` when handled.
    pub(super) fn object_call(&self, st: &mut State, f: u32, func: &Expr, args: &[Expr]) -> bool {
        let Some(model) = self.model().cloned() else { return false };
        if let Some(rewritten) = self.builtin_receiver_call(f, func, args) {
            if let Expr::Call { func, args, .. } = &rewritten {
                self.process_call(
                    st,
                    f,
                    &Expr::Call {
                        func: func.clone(),
                        func_span: rewritten.span().unwrap_or_default(),
                        args: args.clone(),
                        kwargs: Vec::new(),
                        span: rewritten.span().unwrap_or_default(),
                        is_new: false,
                    },
                );
            }
            return true;
        }
        if let Some(sp) = super::spelling(func) {
            if !self.locally_bound(f, sp.split('.').next().unwrap_or(&sp)) {
                if let Some(&(_, oi, pi)) = model.prototype_setters.iter().find(|(n, _, _)| *n == sp) {
                    if let (Some(o), Some(p)) = (args.get(oi as usize), args.get(pi as usize)) {
                        let from = self.eval(st, f, p, 0);
                        let to = self.eval(st, f, o, 0);
                        self.link_instances(st, &from, &to);
                    }
                    return true;
                }
            }
        }
        let Expr::Attr { object, attr, .. } = func else { return false };
        if *attr != model.call_with_receiver && *attr != model.apply_with_receiver {
            return false;
        }
        let (call_args, recv) = self.receiver_args(&model, attr, args);
        let callees = self.callees(st, f, object, call_args.len() as u32);
        let mut handled = false;
        for callee in callees {
            match callee {
                Callee::Lib(g, skip) => {
                    handled = true;
                    self.call_lib(st, f, g, skip, &call_args, &[]);
                    if st.channels {
                        let bound = self.bind(g, skip, &call_args, &[]);
                        self.compose_receiver(st, f, g, recv.as_ref(), &bound);
                    }
                }
                Callee::Ctor(c) => {
                    handled = true;
                    for g in self.ctors(st, c) {
                        self.call_lib(st, f, g, true, &call_args, &[]);
                    }
                }
                Callee::Value(v) => {
                    handled = true;
                    self.apply(st, f, v, CALLS);
                }
                Callee::Leaf { .. } => {}
            }
        }
        handled
    }

    /// Classes of prototype / mixin source values (instances, classes, module values, function
    /// objects that are instances).
    fn classes_of(&self, st: &mut State, vals: &Vals, depth: usize) -> BTreeSet<u32> {
        let vals = self.with_module_values(st, vals, depth);
        let mut out = BTreeSet::new();
        for v in &vals {
            match *v {
                V::Inst(c) | V::Class(c) => {
                    out.insert(c);
                }
                V::Fn(g, _) => {
                    out.extend(st.obj.fn_classes.get(&g).into_iter().flatten().copied());
                }
                _ => {}
            }
        }
        out
    }

    /// Prototype link / member copy from `from` onto the function objects among `to`.
    pub(super) fn link_instances(&self, st: &mut State, from: &Vals, to: &Vals) {
        if self.model().is_none() {
            return;
        }
        let classes = self.classes_of(st, from, 0);
        if classes.is_empty() {
            return;
        }
        for v in to {
            let V::Fn(g, _) = *v else { continue };
            for &c in &classes {
                if st.obj.fn_classes.entry(g).or_default().insert(c) {
                    st.changed = true;
                }
                st.obj.class_fns.entry(c).or_default().insert(g);
            }
        }
    }

    /// Function / instance / class values passed to parameters the callee stores: they are in
    /// those slots (the slot's callers call the functions).
    pub(super) fn pass_stored(&self, st: &mut State, f: u32, g: u32, bound: &[(u16, &Expr)]) {
        if self.model().is_none() {
            return;
        }
        let slots: Vec<(u32, u16)> = st
            .stored
            .iter()
            .filter_map(|&(s, h, v)| match v {
                V::Param(p, j, How::Direct | How::Wrapped) if h == g && p == g => Some((s, j)),
                _ => None,
            })
            .collect();
        for (s, j) in slots {
            for (k, a) in bound {
                if *k != j {
                    continue;
                }
                for v in self.eval(st, f, a, 0) {
                    if matches!(v, V::Fn(..) | V::Inst(_) | V::Class(_)) {
                        st.insert_stored(s, f, v);
                    }
                }
            }
        }
    }

    /// Instances of classes among function-object values (`typed`).
    pub(super) fn fn_instances(&self, st: &State, objs: &Vals) -> Vals {
        let mut out = Vals::new();
        for v in objs {
            if let V::Fn(g, _) = *v {
                out.extend(st.obj.fn_classes.get(&g).into_iter().flatten().map(|c| V::Inst(*c)));
            }
        }
        out
    }

    /// Function objects that are instances of `c` (calling an instance calls them).
    pub(super) fn instance_fns(&self, st: &State, c: u32) -> Vec<u32> {
        st.obj.class_fns.get(&c).into_iter().flatten().copied().collect()
    }

    /// `obj[k]` of an instance / class: its computed members (those named by `k`'s literal
    /// values when some are, else all of them).
    pub(super) fn computed_members(
        &self,
        st: &mut State,
        f: u32,
        objs: &Vals,
        key: Option<&Expr>,
        depth: usize,
    ) -> Vals {
        let mut out = Vals::new();
        if self.model().is_none() {
            return out;
        }
        let mut classes: BTreeSet<u32> = BTreeSet::new();
        for v in self.typed(st, objs) {
            if let V::Inst(c) | V::Class(c) = v {
                classes.extend(self.lineage(c));
            }
        }
        if classes.is_empty() {
            return out;
        }
        let names: BTreeSet<String> = match key {
            Some(k) => self
                .eval(st, f, k, depth + 1)
                .into_iter()
                .filter_map(|v| match v {
                    V::Lit(l) => st.lits.get(l as usize).cloned(),
                    _ => None,
                })
                .collect(),
            None => BTreeSet::new(),
        };
        for c in classes {
            let mut named = false;
            for n in &names {
                if let Some(m) = self.find_method(st, c, n) {
                    out.insert(V::Fn(m, true));
                    named = true;
                }
            }
            if !named {
                for (g, _, _) in self.objects.computed.get(&c).into_iter().flatten() {
                    out.insert(V::Fn(*g, true));
                }
            }
        }
        out
    }

    /// Object-model stores of one function: prototype links (`o.__proto__ = p`), function
    /// values assigned to members of an object class (`X.prototype.get = factory('GET')`),
    /// parameter values pushed into a parameter container.
    pub(super) fn object_stores(&self, st: &mut State, f: u32) {
        let Some(model) = self.model().cloned() else { return };
        // Computed members defined here are methods under their key's literal values.
        let defined: Vec<(u32, u32, Expr)> = self
            .objects
            .computed
            .iter()
            .flat_map(|(c, list)| {
                list.iter()
                    .filter(|(_, d, _)| *d == f)
                    .map(|(g, _, k)| (*c, *g, k.clone()))
            })
            .collect();
        for (c, g, key) in defined {
            for v in self.eval(st, f, &key, 0) {
                let V::Lit(l) = v else { continue };
                let Some(name) = st.lits.get(l as usize).cloned() else { continue };
                if self.classes[c as usize].methods.contains_key(&name) {
                    continue;
                }
                if let std::collections::hash_map::Entry::Vacant(e) = st.method_alias.entry((c, name)) {
                    e.insert(g);
                    st.changed = true;
                }
            }
        }
        let stores = self.funcs[f as usize].field_stores.clone();
        for (object, name, value) in &stores {
            if *name == model.prototype_link {
                let to = self.eval(st, f, object, 0);
                let from = self.eval(st, f, value, 0);
                self.link_instances(st, &from, &to);
                for o in &to {
                    let V::Param(p, i, How::Direct) = *o else { continue };
                    for v in &from {
                        if let V::Param(q, j, How::Direct) = *v {
                            if p == f && q == f && i != j && st.copies.insert((f, j, i)) {
                                st.changed = true;
                            }
                        }
                    }
                }
                continue;
            }
            let objs = self.eval(st, f, object, 0);
            let vals = self.eval(st, f, value, 0);
            for o in &objs {
                let (V::Inst(c) | V::Class(c)) = *o else { continue };
                if !self.is_synthetic(c) || self.classes[c as usize].methods.contains_key(name) {
                    continue;
                }
                for v in &vals {
                    if let V::Fn(g, _) = v {
                        if st.method_alias.insert((c, name.clone()), *g) != Some(*g) {
                            st.changed = true;
                        }
                    }
                }
            }
        }
    }

    /// `p2.push(v)` of parameter values into a parameter container: returning the container
    /// returns them.
    pub(super) fn param_container_store(&self, st: &mut State, f: u32, objs: &Vals, vals: &Vals) {
        if self.model().is_none() {
            return;
        }
        for o in objs {
            let V::Param(p, into, How::Direct) = *o else { continue };
            if p != f {
                continue;
            }
            for v in vals {
                if let V::Param(q, from, How::Direct | How::Wrapped) = *v {
                    if q == f && from != into && st.obj.pflow.insert((f, from, into)) {
                        st.changed = true;
                    }
                }
            }
        }
    }

    /// Parameters returned inside a returned parameter container.
    pub(super) fn returned_through(&self, st: &mut State, f: u32) {
        let flows: Vec<(u16, u16)> = st
            .obj
            .pflow
            .range((f, 0, 0)..)
            .take_while(|(g, _, _)| *g == f)
            .map(|(_, a, b)| (*a, *b))
            .collect();
        for (from, into) in flows {
            if st.mask(f, into) & super::RETURNS != 0 {
                st.add_mask(f, from, super::RETURNS);
            }
        }
    }

    /// Constructor arguments an instance of `c` built in `f` keeps ([`ObjState::held_args`]).
    pub(super) fn hold_roles(
        &self,
        st: &mut State,
        f: u32,
        c: u32,
        args: &[Expr],
        kwargs: &[(String, Expr)],
        depth: usize,
    ) {
        if self.model().is_none() {
            return;
        }
        let mut add = BTreeSet::new();
        for g in self.ctors(st, c) {
            for (j, a) in self.bind(g, true, args, kwargs) {
                for v in self.eval(st, f, a, depth + 1) {
                    if let V::Param(p, i, _) = v {
                        if p == f {
                            add.insert((g, j, i));
                        }
                    }
                }
            }
        }
        self.add_held(st, f, c, add);
    }

    fn add_held(&self, st: &mut State, f: u32, c: u32, add: BTreeSet<HeldArg>) {
        if add.is_empty() {
            return;
        }
        let e = st.obj.held_args.entry((f, c)).or_default();
        let n = e.len();
        e.extend(add);
        if e.len() != n {
            st.changed = true;
        }
    }

    /// A factory `g` returning an instance of `c`: the arguments bound to the parameters it
    /// passes into the instance flow there from `f`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn factory_roles(
        &self,
        st: &mut State,
        f: u32,
        g: u32,
        skip: bool,
        c: u32,
        args: &[Expr],
        kwargs: &[(String, Expr)],
        depth: usize,
    ) {
        let inner = st.obj.held_args.get(&(g, c)).cloned().unwrap_or_default();
        if inner.is_empty() {
            return;
        }
        let mut add = BTreeSet::new();
        for (j, a) in self.bind(g, skip, args, kwargs) {
            let targets: Vec<(u32, u16)> = inner
                .iter()
                .filter(|(_, _, i)| *i == j)
                .map(|(h, k, _)| (*h, *k))
                .collect();
            if targets.is_empty() {
                continue;
            }
            for v in self.eval(st, f, a, depth + 1) {
                if let V::Param(p, i, _) = v {
                    if p == f {
                        add.extend(targets.iter().map(|(h, k)| (*h, *k, i)));
                    }
                }
            }
        }
        self.add_held(st, f, c, add);
    }

    /// (keys, handlers) an instance of `c` built in `f` holds: parameters of `f` flowing into
    /// constructor parameters that are stored to be called (handlers) or not (keys).
    fn held_roles(&self, st: &State, f: u32, c: u32) -> (BTreeSet<u16>, BTreeSet<u16>) {
        let mut keys = BTreeSet::new();
        let mut handlers = BTreeSet::new();
        let params = &self.funcs[f as usize].params;
        for &(g, j, i) in st.obj.held_args.get(&(f, c)).into_iter().flatten() {
            if st.mask(g, j) & (STORED | CALLS) != 0 {
                handlers.insert(i);
            } else if params
                .get(i as usize)
                .is_some_and(|p| p.kind != ParamKind::VarPositional)
            {
                // A key is one argument: never the rest of the arguments.
                keys.insert(i);
            }
        }
        (keys, handlers)
    }

    /// Rule 2 through entry objects: instances holding a handler stored into a dispatched
    /// container register it under a key they hold, else under the receiver's own key.
    pub(super) fn registers_entry_objects(
        &self,
        st: &mut State,
        f: u32,
        s: u32,
        vals: &Vals,
        key_vals: &Vals,
    ) {
        if self.model().is_none() {
            return;
        }
        let Some(channel) = st
            .dispatched
            .get(&s)
            .or_else(|| st.method_dispatched.get(&s))
            .copied()
        else {
            return;
        };
        for v in vals {
            let V::Inst(c) = *v else { continue };
            let (mut keys, handlers) = self.held_roles(st, f, c);
            if handlers.is_empty() {
                continue;
            }

            for k in key_vals {
                if let V::Param(p, i, _) = *k {
                    if p == f {
                        keys.insert(i);
                    }
                }
            }
            if keys.is_empty() {
                // The receiver's own container: its own key.
                let owner = st.slot_keys[s as usize].owner;
                let own = match (owner, self.funcs[f as usize].class) {
                    (super::SlotOwner::Class(o), Some(fc)) => {
                        o == fc || self.related.get(fc as usize).is_some_and(|r| r.contains(&o))
                    }
                    _ => false,
                };
                if own {
                    for h in handlers {
                        st.add_chan(
                            f,
                            Chan::RegistersSelf {
                                channel,
                                handler: h,
                                verb: super::Verb::Any,
                            },
                        );
                    }
                }
                continue;
            }
            for &k in &keys {
                for &h in &handlers {
                    if h != k {
                        st.add_chan(
                            f,
                            Chan::Registers {
                                channel,
                                key: k,
                                handler: h,
                                verb: super::Verb::Any,
                            },
                        );
                    }
                }
            }
        }
    }

    /// `RegistersSelf` of callee `g` called on receiver `recv` from `f`: the receiver's held
    /// key (an entry object `f` built from its parameter) registers the handler argument; a
    /// call on `f`'s own receiver passes the fact up.
    pub(super) fn compose_receiver(
        &self,
        st: &mut State,
        f: u32,
        g: u32,
        recv: Option<&Expr>,
        bound: &[(u16, &Expr)],
    ) {
        let facts: Vec<Chan> = st
            .chan
            .get(&g)
            .into_iter()
            .flatten()
            .filter(|c| matches!(c, Chan::RegistersSelf { .. }))
            .cloned()
            .collect();
        if facts.is_empty() {
            return;
        }
        let Some(recv) = recv else { return };
        let recv_vals = self.eval(st, f, recv, 0);
        for fact in facts {
            let Chan::RegistersSelf {
                channel,
                handler,
                verb,
            } = fact
            else {
                continue;
            };
            let Some((_, a)) = bound.iter().find(|(j, _)| *j == handler) else { continue };
            let hands: BTreeSet<(u32, u16)> = self
                .eval(st, f, a, 0)
                .into_iter()
                .filter_map(|v| match v {
                    V::Param(p, h, How::Direct | How::Wrapped) if p == f || self.is_ancestor(p, f) => {
                        Some((p, h))
                    }
                    _ => None,
                })
                .collect();
            if hands.is_empty() {
                continue;
            }
            for rv in &recv_vals {
                let V::Inst(c) = *rv else { continue };
                let own = self.funcs[f as usize].self_param.is_some()
                    && self.funcs[f as usize].class.is_some_and(|fc| {
                        fc == c || self.related.get(fc as usize).is_some_and(|r| r.contains(&c))
                    })
                    && matches!(recv, Expr::Name { name, .. } if Some(name) == self.funcs[f as usize].self_param.as_ref());
                if own {
                    for &(p, h) in &hands {
                        if p == f {
                            st.add_chan(
                                f,
                                Chan::RegistersSelf {
                                    channel,
                                    handler: h,
                                    verb: verb.clone(),
                                },
                            );
                        }
                    }
                    continue;
                }
                let (keys, _) = self.held_roles(st, f, c);
                for &k in &keys {
                    for &(p, h) in &hands {
                        if p == f && h != k {
                            st.add_chan(
                                f,
                                Chan::Registers {
                                    channel,
                                    key: k,
                                    handler: h,
                                    verb: verb.clone(),
                                },
                            );
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/derive/objects.rs"]
mod tests;
