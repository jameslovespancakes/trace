//! The read-only evaluator [`Ev`]: transfer functions, names, receivers and expressions
//! (child of [`crate::flow`]).

use super::*;

/// Read-only evaluator over one view of the solution; records what it reads when `rec` is
/// set (solver dependency tracking, bounded-candidate detection).
pub(super) struct Ev<'s, 'a> {
    pub(super) f: &'s Flow<'a>,
    pub(super) st: &'s State,
    pub(super) view: View,
    pub(super) rec: Option<&'s RefCell<Rec>>,
    /// MRO lookup memo of the (single-threaded) solver loop; `None` elsewhere.
    pub(super) memo: Option<&'s RefCell<LookupMemo>>,
}

/// Memoized MRO walks keyed by (class, attribute, full view): the first member slot or
/// method and the first class-attribute slot. An entry computed at clock `at` stays valid
/// while no attribute cell of that name changed since ([`Dep::Attr`] clock `< at`); every
/// slot the walk reads is such a cell, and the hierarchy is fixed during a solve.
#[derive(Default)]
pub(super) struct LookupMemo {
    pub(super) member: HashMap<MemoKey, (u32, MemberHit)>,
    pub(super) class_slot: HashMap<MemoKey, (u32, Option<u32>)>,
    /// Non-empty `ObjAttr(Any(k), attr)` cells along the MRO.
    pub(super) any_slots: HashMap<MemoKey, (u32, Box<[u32]>)>,
}

/// (class, attribute, full view).
type MemoKey = (SymbolId, Name, bool);
/// First member along the MRO: its slot cell or the method.
type MemberHit = Option<Result<u32, SymbolId>>;

impl<'s> Ev<'s, '_> {
    /// Record a read of `dep`; returns its cell id if the cell exists.
    pub(super) fn note(&self, dep: Dep) -> Option<u32> {
        let id = self.st.ids.get(&dep).copied();
        if let Some(rec) = self.rec {
            let mut r = rec.borrow_mut();
            if id.is_some_and(|i| self.st.cells[i as usize].saturated) {
                r.saturated = true;
            }
            let recorded = recorded_dep(dep);
            let recorded_id = if recorded == dep {
                id
            } else {
                self.st.ids.get(&recorded).copied()
            };
            match recorded_id {
                Some(i) => r.ids.push(i),
                None => r.missing.push(recorded),
            }
        }
        id
    }

    pub(super) fn slot(&self, slot: Slot) -> Option<SlotRef<'s>> {
        let id = self.note(Dep::Slot(slot))?;
        let cell = &self.st.cells[id as usize];
        let test: &'s [Value] = if self.view == View::Full {
            cell.test.as_slice()
        } else {
            &[]
        };
        let r = SlotRef {
            prod: cell.prod.as_slice(),
            test,
        };
        (!r.is_empty()).then_some(r)
    }

    /// Receiver contexts of `f` (recorded read), with their test-origin bits.
    fn ctx_entries(&self, f: SymbolId) -> Option<&'s [(Recv, bool)]> {
        self.note(Dep::Contexts(f));
        self.st.contexts.get(&f).map(Vec::as_slice)
    }

    pub(super) fn visible(&self, test: bool) -> bool {
        self.view == View::Full || !test
    }

    /// Visible receiver contexts of a scope (recorded read).
    pub(super) fn contexts_of(&self, scope: ScopeKey) -> Vec<Recv> {
        if let ScopeKey::Symbol(f) = scope {
            if self.f.selfs.contains_key(&f) {
                self.note(Dep::Contexts(f));
            }
        }
        self.f.contexts_of(self.st, scope, self.view)
    }

    /// Visible allocations with an attribute slot for (class, attribute) (recorded read).
    pub(super) fn attr_objs(&self, class: SymbolId, attr: Name) -> Vec<Obj> {
        self.note(Dep::AttrObjs(class, attr));
        self.st
            .attr_objs
            .get(&(class, attr))
            .map(|l| l.iter().filter(|(_, t)| self.visible(*t)).map(|(o, _)| *o).collect())
            .unwrap_or_default()
    }

    pub(super) fn transfer(&self, c: &Constraint, sc: Sc) -> Eff {
        let mut eff = Eff::default();
        match &c.rule {
            Rule::Eval { call } => {
                self.call_result(call, sc, &mut eff);
            }
            Rule::Return { function, value } => {
                let vals = self.eval(value, sc, &mut eff);
                eff.write(Slot::Return(*function, sc.ctx), vals);
            }
            Rule::Bind { target, value } => {
                let vals = self.eval(value, sc, &mut eff);
                self.write(target, vals, sc, &mut eff);
            }
            Rule::Decorated {
                target,
                function,
                decorators,
            } => {
                let mut value = Vals::one(Value::Function(*function));
                for (d, alloc, effects) in decorators.iter().rev() {
                    let decorator = self.eval(d, sc, &mut eff);
                    let at = Alloc::At(sc.file, *alloc);
                    if !decorator.is_empty() {
                        value = self.apply(&decorator, &[value], &[], at, &mut eff);
                    } else if let Some(wrapped) = self.decorator_behaviour(effects, &value, sc, at, &mut eff)
                    {
                        value = wrapped;
                    }
                }
                self.write(target, value, sc, &mut eff);
            }
            Rule::Implicit(i) => {
                self.implicit_targets(&self.f.implicit[*i], sc, &mut eff);
            }
        }
        eff
    }

    pub(super) fn write(&self, target: &Target, vals: Vals, sc: Sc, eff: &mut Eff) {
        match target {
            Target::Var(s, n) => {
                let slot = if *s == sc.scope {
                    Slot::Var(*s, sc.ctx, *n)
                } else if let ScopeKey::Symbol(f) = s {
                    // Parameter defaults: evaluated in the enclosing scope, every context.
                    Slot::Default(*f, *n)
                } else {
                    Slot::Var(*s, Recv::Unknown, *n)
                };
                eff.write(slot, vals);
            }
            Target::Member(c, n) => eff.write(Slot::Member(*c, *n), vals),
            Target::Field(n) => eff.write(Slot::Field(*n), vals),
            Target::FieldOf(object, n) => {
                let proto = self.is_proto_key(*n, sc.file);
                for v in self.eval(object, sc, eff) {
                    if proto {
                        // `o.__proto__ = p` (JavaScript language rule): `o` delegates to `p`.
                        if let Some(h) = Holder::of(v) {
                            eff.write(Slot::Delegates(h), vals.clone());
                        }
                        continue;
                    }
                    match v {
                        Value::Instance(o) => eff.write(Slot::ObjAttr(o, *n), vals.clone()),
                        Value::Class(c) => eff.write(Slot::ClassAttr(c, *n), vals.clone()),
                        Value::Object(_) | Value::Function(_) | Value::Library(_) => {
                            if let Some(h) = Holder::of(v) {
                                eff.write(Slot::Prop(h, *n), vals.clone());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Whether member `n` in `file` is the prototype link of the file's language
    /// (`__proto__`, language rule).
    fn is_proto_key(&self, n: Name, file: FileId) -> bool {
        self.f.prototypes[self.f.index.file(file).language as usize].link == Some(n)
    }

    /// Effective context for a new receiver of `f` (widened beyond `flow.max_contexts`).
    pub(super) fn ctx_for(&self, f: SymbolId, r: Recv) -> Recv {
        let Some(list) = self.ctx_entries(f) else {
            return r;
        };
        let visible = |x: Recv| {
            list.binary_search_by(|(y, _)| y.cmp(&x))
                .ok()
                .is_some_and(|i| self.visible(list[i].1))
        };
        let len = list.iter().filter(|(_, t)| self.visible(*t)).count();
        if len == 0 {
            return r;
        }
        let max = self.f.settings.max_contexts;
        if !visible(r) && len >= max {
            let widened = r.widen();
            if visible(widened) || len < 2 * max {
                widened
            } else {
                Recv::Unknown
            }
        } else {
            r
        }
    }

    /// Values of the receiver parameter of method `f` under context `ctx`.
    pub(super) fn recv_values(&self, f: SymbolId, ctx: Recv) -> Vals {
        let Some(sp) = self.f.selfs.get(&f) else {
            return Vals::new();
        };
        let make = |k: SymbolId, o: Option<Obj>| {
            if sp.is_class {
                Value::Class(k)
            } else {
                Value::Instance(o.unwrap_or(Obj::any(k)))
            }
        };
        match ctx {
            Recv::Unknown => self
                .f
                .families
                .get(&sp.class)
                .map(Vec::as_slice)
                .unwrap_or(std::slice::from_ref(&sp.class))
                .iter()
                .map(|&k| make(k, None))
                .collect(),
            Recv::Class(k) => Vals::one(make(k, None)),
            Recv::Instance(o) => Vals::one(make(o.class, Some(o))),
        }
    }

    fn name_values(&self, name: Name, sc: Sc) -> Vals {
        let mut out = Vals::new();
        if let ScopeKey::Symbol(f) = sc.scope {
            if self.f.selfs.get(&f).is_some_and(|sp| sp.name == name) {
                return self.recv_values(f, sc.ctx);
            }
            for slot in [Slot::Var(sc.scope, sc.ctx, name), Slot::Default(f, name)] {
                if let Some(v) = self.slot(slot) {
                    out.extend(v.iter());
                }
            }
            // Closures: anonymous scopes read their enclosing scopes' variables.
            let mut scope = f;
            let mut steps = 0usize;
            while self.f.anonymous.contains(&scope) && steps <= self.f.index.symbols.len() {
                steps += 1;
                let Some(parent) = self.f.index.symbol(scope).parent else {
                    break;
                };
                if !self.f.index.symbol(parent).kind.is_callable() {
                    break;
                }
                if self.f.selfs.get(&parent).is_some_and(|sp| sp.name == name) {
                    for ctx in self.contexts_of(ScopeKey::Symbol(parent)) {
                        out.extend(self.recv_values(parent, ctx));
                    }
                }
                for slot in [Slot::VarAll(ScopeKey::Symbol(parent), name), Slot::Default(parent, name)] {
                    if let Some(v) = self.slot(slot) {
                        out.extend(v.iter());
                    }
                }
                scope = parent;
            }
        }
        if let Some(v) = self.slot(Slot::Var(ScopeKey::Module(sc.file), Recv::Unknown, name)) {
            out.extend(v.iter());
        }
        out
    }

    pub(super) fn eval(&self, node: &Node, sc: Sc, eff: &mut Eff) -> Vals {
        match node {
            Node::Name { target: Some(t), .. } => Vals::one(value_of(self.f.index, *t)),
            Node::Attr {
                object,
                target: Some(t),
                ..
            } => {
                if self.f.selfs.contains_key(t) {
                    let objs = self.eval(object, sc, eff);
                    self.bind_method(*t, &objs)
                } else {
                    Vals::one(value_of(self.f.index, *t))
                }
            }
            Node::Name {
                name,
                target: None,
                span,
            } => {
                if self.f.local_reads.contains(&(sc.file, *span)) {
                    self.local_values(*name, sc)
                } else {
                    self.name_values(*name, sc)
                }
            }
            Node::Attr {
                object,
                attr,
                target: None,
                ..
            } => {
                if let Some((f, after)) = self.super_class(object, sc) {
                    let objs = self.recv_values(f, sc.ctx);
                    return self.super_attr(&objs, after, *attr, eff);
                }
                let objs = self.eval(object, sc, eff);
                self.attr_or_field(&objs, *attr, eff)
            }
            Node::Call(call) => self.call_result(call, sc, eff),
            Node::Choice(options) => {
                let mut out = Vals::new();
                for o in options {
                    out.extend(self.eval(o, sc, eff));
                }
                out
            }
            Node::Lambda(s) => Vals::one(if self.f.is_generator(*s) {
                Value::Generator(*s)
            } else {
                Value::Function(*s)
            }),
            Node::Object { alloc, fields, .. } => {
                let a = Alloc::At(sc.file, *alloc);
                let holder = Holder::Obj(a);
                for (k, v) in fields {
                    let vals = self.eval(v, sc, eff);
                    if self.is_proto_key(*k, sc.file) {
                        eff.write(Slot::Delegates(holder), vals);
                    } else {
                        eff.write(Slot::Prop(holder, *k), vals);
                    }
                }
                Vals::one(Value::Object(a))
            }
            Node::Yielded { function, name } => self
                .slot(Slot::VarAll(ScopeKey::Symbol(*function), *name))
                .map(SlotRef::to_vals)
                .unwrap_or_default(),
            Node::Module(file) => self
                .f
                .exports_name
                .and_then(|n| self.slot(Slot::Var(ScopeKey::Module(*file), Recv::Unknown, n)))
                .map(SlotRef::to_vals)
                .unwrap_or_default(),
            Node::Literal | Node::Opaque => Vals::new(),
        }
    }

    /// Attribute values of `objs`, with the field-name slot as the fallback for receivers
    /// whose attribute is unknown (non-strict evaluation only).
    pub(super) fn attr_or_field(&self, objs: &Vals, attr: Name, eff: &mut Eff) -> Vals {
        let mut out = self.attr_values(objs, attr, eff);
        // Members of library objects are library members: they keep the field-name fallback
        // of unknown receivers (strict evaluation sees only the library object).
        if !eff.strict && out.iter().all(|v| matches!(v, Value::Library(_))) {
            if let Some(v) = self.slot(Slot::Field(attr)) {
                out.extend(v.iter());
            }
        }
        out
    }

    /// Value of method `f` found by member lookup on `recv` (`via_class`: looked up on the
    /// class itself, so instance methods stay unbound).
    pub(super) fn method_value(&self, f: SymbolId, recv: Recv, via_class: bool) -> Value {
        match self.f.selfs.get(&f) {
            None => Value::Function(f),
            Some(sp) if sp.is_class => Value::Bound(f, Recv::Class(recv.class().unwrap_or(sp.class))),
            Some(_) if via_class => Value::Function(f),
            Some(_) => Value::Bound(f, recv),
        }
    }

    /// Bind a semantically resolved method target to the evaluated receiver objects.
    pub(super) fn bind_method(&self, t: SymbolId, objs: &Vals) -> Vals {
        let Some(sp) = self.f.selfs.get(&t).copied() else {
            return Vals::one(value_of(self.f.index, t));
        };
        let mut out = Vals::new();
        for v in objs {
            match *v {
                Value::Instance(o) if self.f.is_subclass(o.class, sp.class) => {
                    out.insert(if sp.is_class {
                        Value::Bound(t, Recv::Class(o.class))
                    } else {
                        Value::Bound(t, Recv::Instance(o))
                    });
                }
                Value::Class(k) if self.f.is_subclass(k, sp.class) => {
                    out.insert(if sp.is_class {
                        Value::Bound(t, Recv::Class(k))
                    } else {
                        Value::Function(t)
                    });
                }
                _ => {}
            }
        }
        if out.is_empty() {
            out.insert(Value::Bound(t, Recv::Unknown));
        }
        out
    }
}
