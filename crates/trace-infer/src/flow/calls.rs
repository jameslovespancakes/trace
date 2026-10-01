//! Calls in the evaluator: callees, results, library behaviour, decorators, argument
//! binding and implicit operations (child of [`crate::flow`]).

use super::*;

impl<'s> Ev<'s, '_> {
    pub(super) fn callees(&self, call: &CallNode, sc: Sc, eff: &mut Eff) -> Vals {
        // The receiver of `obj.m(..)` is evaluated once and shared with the proven-target
        // binding below: evaluating it twice per call doubles the work at every step of a
        // builder chain (`sb.append(a).append(b)...`), i.e. exponential in its length.
        let mut receiver: Option<Vals> = None;
        let super_of = match &call.func {
            Node::Attr {
                object, target: None, ..
            } => self.super_class(object, sc),
            _ => None,
        };
        let mut vals = match &call.func {
            // `super().m(..)` / `super.m(..)`: the member after the declaring class in the
            // receiver's MRO, bound to the current receiver (never the override itself).
            Node::Attr { attr, .. } if super_of.is_some() => {
                let (f, after) = super_of.expect("super receiver");
                let objs = self.recv_values(f, sc.ctx);
                let v = self.super_attr(&objs, after, *attr, eff);
                receiver = Some(objs);
                v
            }
            Node::Attr {
                object, attr, target, ..
            } if !target.is_some_and(|t| !self.f.selfs.contains_key(&t)) => {
                let objs = self.eval(object, sc, eff);
                let v = match target {
                    Some(t) => self.bind_method(*t, &objs),
                    None => self.attr_or_field(&objs, *attr, eff),
                };
                receiver = Some(objs);
                v
            }
            other => self.eval(other, sc, eff),
        };
        if let ScopeKey::Symbol(owner) = sc.scope {
            let proven = self.f.proven(owner, sc.file, call.func_span);
            if !proven.is_empty() {
                let objs = match &call.func {
                    Node::Attr { object, .. } => {
                        Some(receiver.take().unwrap_or_else(|| self.eval(object, sc, eff)))
                    }
                    _ => None,
                };
                for t in proven {
                    let symbol = self.f.index.symbol(t);
                    let constructed = match &objs {
                        None if is_constructor(symbol) => self.f.hierarchy.class_of(self.f.index, t),
                        _ => None,
                    };
                    if symbol.kind.is_type() {
                        vals.insert(Value::Class(t));
                    } else if let Some(class) = constructed {
                        // `C(...)` proven to `C.__init__`: the class is what is called.
                        vals.insert(Value::Class(class));
                    } else if let Some(objs) = &objs {
                        vals.extend(self.bind_method(t, objs));
                    } else {
                        vals.insert(Value::Function(t));
                    }
                }
            }
        }
        // Identity guards: an object the callee is proven not to be is not called here.
        // Only a guard evaluating to exactly one function or class names one object.
        for guard in &call.not_identical {
            let excluded = self.eval(guard, sc, &mut Eff::strict());
            if let [only @ (Value::Function(_) | Value::Class(_))] = excluded.as_slice() {
                vals.remove(only);
            }
        }
        vals
    }

    pub(super) fn call_result(&self, call: &CallNode, sc: Sc, eff: &mut Eff) -> Vals {
        if let Node::Lambda(g) = call.func {
            if self.f.is_generator(g) {
                // A generator expression is lowered as its synthetic function applied to
                // the first iterable: that creates the generator object; the body runs
                // only when the object is consumed.
                let args: Vec<Vals> = call.args.iter().map(|a| self.eval(a, sc, eff)).collect();
                self.bind_args(g, Recv::Unknown, &args, &[], eff);
                return Vals::one(Value::Generator(g));
            }
        }
        let callees = self.callees(call, sc, eff);
        let repository = has_repository_callee(&callees);
        // Library objects this call may create: the library the server resolved the callee
        // into, and the library objects whose member is called.
        let libraries: Vec<u32> = callees
            .iter()
            .filter_map(|v| match v {
                Value::Library(l) => Some(*l),
                _ => None,
            })
            .chain(call.library)
            .collect();
        if !repository && call.effects.is_empty() && libraries.is_empty() {
            // Arguments may still contain calls whose bindings matter; they are separate
            // `Eval` facts, so nothing else to do here.
            return Vals::new();
        }
        let args: Vec<Vals> = call.args.iter().map(|a| self.eval(a, sc, eff)).collect();
        let kwargs: Vec<(Name, Vals)> =
            call.kwargs.iter().map(|(k, v)| (*k, self.eval(v, sc, eff))).collect();
        let alloc = Alloc::At(sc.file, call.func_span.start);
        if repository {
            return self.apply(&callees, &args, &kwargs, alloc, eff);
        }
        let mut result = self.behave(&call.effects, &args, &kwargs, alloc, sc, eff);
        // The library made the result, unless its knowledge says what the result is
        // (`returns`, `wraps`, ...) or an argument may carry a repository object back.
        if !libraries.is_empty()
            && !produces_result(&call.effects)
            && self.inert_arguments(call, &args, &kwargs)
        {
            result.extend(libraries.into_iter().map(Value::Library));
        }
        result
    }

    /// Whether no argument of `call` can carry a repository object into the result of a
    /// library call: every argument is a literal, a function (called, not returned) or a
    /// library object. Unknown values (parameters nothing binds, containers, opaque
    /// expressions) and repository objects are not inert.
    fn inert_arguments(&self, call: &CallNode, args: &[Vals], kwargs: &[(Name, Vals)]) -> bool {
        let inert = |node: &Node, vals: &Vals| {
            matches!(node, Node::Literal | Node::Lambda(_))
                || (!vals.is_empty()
                    && vals
                        .iter()
                        .all(|v| matches!(v, Value::Function(_) | Value::Bound(..) | Value::Library(_))))
        };
        call.args.iter().zip(args).all(|(n, v)| inert(n, v))
            && call.kwargs.iter().zip(kwargs).all(|((_, n), (_, v))| inert(n, v))
    }

    /// Argument values selected by a library effect's argument selector; `{"field": [i, k]}`
    /// selects member `k` of argument `i`.
    fn effect_args(&self, sel: &ArgSel, args: &[Vals], kwargs: &[(Name, Vals)], eff: &mut Eff) -> Vec<Vals> {
        if let ArgSel::Field { arg, field } = sel {
            let (Some(object), Some(name)) = (args.get(*arg as usize), self.f.names.get(field)) else {
                return Vec::new();
            };
            let found = self.attr_values(object, name, eff);
            return if found.is_empty() { Vec::new() } else { vec![found] };
        }
        let positional = args.len();
        let mut out: Vec<Vals> = args
            .iter()
            .enumerate()
            .filter(|(i, _)| selects(sel, Some(*i), None, positional))
            .map(|(_, v)| v.clone())
            .collect();
        out.extend(
            kwargs
                .iter()
                .filter(|(k, _)| selects(sel, None, Some(self.f.names.text(*k)), positional))
                .map(|(_, v)| v.clone()),
        );
        out
    }

    /// Library member copying / delegation: the objects `receiver` selects delegate member
    /// lookups to `delegates` (copied members are looked up the same way). `ArgSel::Result`
    /// is a new plain object allocated by this call, which becomes the call's result.
    #[allow(clippy::too_many_arguments)]
    fn add_delegates(
        &self,
        receiver: &ArgSel,
        delegates: Vec<Vals>,
        args: &[Vals],
        kwargs: &[(Name, Vals)],
        alloc: Alloc,
        result: &mut Vals,
        eff: &mut Eff,
    ) {
        let mut values = Vals::new();
        for d in delegates {
            values.extend(d);
        }
        let holders: Vec<Holder> = if matches!(receiver, ArgSel::Result) {
            result.insert(Value::Object(alloc));
            vec![Holder::Obj(alloc)]
        } else {
            self.effect_args(receiver, args, kwargs, eff)
                .iter()
                .flat_map(|vals| vals.iter().filter_map(|v| Holder::of(*v)))
                .collect()
        };
        for h in holders {
            eff.write(Slot::Delegates(h), values.clone());
        }
    }

    /// Methods named `method` of the objects in `vals`, with the receiver each runs under
    /// (library `calls_method` effects).
    pub(super) fn method_callees(&self, vals: &Vals, method: &str) -> Vec<(SymbolId, Recv)> {
        let mut out = Vec::new();
        for v in vals {
            let (class, recv) = match *v {
                Value::Instance(o) => (o.class, Recv::Instance(o)),
                Value::Class(c) => (c, Recv::Class(c)),
                Value::Object(a) => {
                    // A plain object's method: its property (or a delegate's member).
                    if let Some(name) = self.f.names.get(method) {
                        let mut seen = Vec::new();
                        let found = self.holder_attr(Holder::Obj(a), name, &mut Eff::strict(), &mut seen);
                        for f in function_ids(&found) {
                            out.push((f, Recv::Unknown));
                        }
                    }
                    continue;
                }
                _ => continue,
            };
            for m in self.member_functions(class, method) {
                out.push((m, recv));
            }
        }
        out
    }

    /// Solve-time behaviour of a library call (its knowledge effects); returns the call's
    /// result.
    fn behave(
        &self,
        effects: &[Effect],
        args: &[Vals],
        kwargs: &[(Name, Vals)],
        alloc: Alloc,
        sc: Sc,
        eff: &mut Eff,
    ) -> Vals {
        let mut result = Vals::new();
        for e in effects {
            match e {
                Effect::Calls(a) | Effect::StoredThenCalled(a) | Effect::Registers { handler: a, .. } => {
                    for vals in self.effect_args(a, args, kwargs, eff) {
                        self.apply(&vals, &[], &[], alloc, eff);
                    }
                }
                Effect::CallsMethod { arg, method } => {
                    for vals in self.effect_args(arg, args, kwargs, eff) {
                        for (m, recv) in self.method_callees(&vals, method) {
                            self.invoke(m, recv, &[], &[], eff);
                        }
                    }
                }
                Effect::Iterates(a) | Effect::Advances(a) => {
                    let advance = matches!(e, Effect::Advances(_));
                    for vals in self.effect_args(a, args, kwargs, eff) {
                        self.iterate_targets(&vals, advance, eff);
                    }
                }
                Effect::Returns(a) | Effect::Wraps(a) => {
                    for vals in self.effect_args(a, args, kwargs, eff) {
                        result.extend(vals.iter().copied());
                    }
                }
                Effect::Partial(a) => {
                    let first = match a {
                        ArgSel::Pos(i) | ArgSel::PosOrKw(i, _) => Some(*i as usize),
                        _ => None,
                    };
                    if let Some(wrapped) = first.and_then(|i| args.get(i)) {
                        result.extend(wrapped.iter().copied());
                        // The remaining arguments are bound to the wrapped callable.
                        let rest = first.and_then(|i| args.get(i + 1..)).unwrap_or(&[]);
                        for &v in wrapped {
                            if let Value::Function(f) | Value::Bound(f, _) = v {
                                let recv = match v {
                                    Value::Bound(_, r) => r,
                                    _ => Recv::Unknown,
                                };
                                self.bind_args(f, recv, rest, kwargs, eff);
                            }
                        }
                    }
                }
                Effect::Property(a) => {
                    for vals in self.effect_args(a, args, kwargs, eff) {
                        result.extend(function_ids(&vals).into_iter().map(Value::Property));
                    }
                }
                Effect::Receiver => {
                    if let ScopeKey::Symbol(f) = sc.scope {
                        result.extend(self.recv_values(f, sc.ctx));
                    }
                }
                Effect::CopiesMembers { from, to } => {
                    let sources = self.effect_args(from, args, kwargs, eff);
                    self.add_delegates(to, sources, args, kwargs, alloc, &mut result, eff);
                }
                Effect::DelegatesMembers { object, to } => {
                    let delegates = self.effect_args(to, args, kwargs, eff);
                    self.add_delegates(object, delegates, args, kwargs, alloc, &mut result, eff);
                }
                Effect::NeverCalls(_)
                | Effect::Sends { .. }
                | Effect::Mounts { .. }
                | Effect::Decorates { .. }
                | Effect::Exports { .. } => {}
            }
        }
        result
    }

    /// Library behaviour of a decorator applied to a function value (`value` is argument 0):
    /// its effects run / store the function, and a wrapping effect (`returns`, `wraps`,
    /// `property`, `partial`) makes the result; otherwise the value passes through. `None`
    /// when the decorator has no library effects.
    pub(super) fn decorator_behaviour(
        &self,
        effects: &[Effect],
        value: &Vals,
        sc: Sc,
        alloc: Alloc,
        eff: &mut Eff,
    ) -> Option<Vals> {
        if effects.is_empty() {
            return None;
        }
        let args = [value.clone()];
        let result = self.behave(effects, &args, &[], alloc, sc, eff);
        let wraps = effects.iter().any(|e| {
            matches!(e, Effect::Returns(_) | Effect::Wraps(_) | Effect::Property(_) | Effect::Partial(_))
        });
        Some(if wraps { result } else { value.clone() })
    }

    pub(super) fn apply(
        &self,
        callees: &Vals,
        args: &[Vals],
        kwargs: &[(Name, Vals)],
        alloc: Alloc,
        eff: &mut Eff,
    ) -> Vals {
        let mut result = Vals::new();
        for &v in callees {
            match v {
                Value::Function(f) => match self.f.selfs.get(&f).copied() {
                    Some(sp) => {
                        // Unbound method call: the first argument is the receiver.
                        let (first, rest) = match args.split_first() {
                            Some((first, rest)) => (Some(first), rest),
                            None => (None, args),
                        };
                        let mut recvs: Vec<Recv> = first
                            .into_iter()
                            .flat_map(|vals| vals.iter())
                            .filter_map(|v| match *v {
                                Value::Instance(o) if sp.is_class => Some(Recv::Class(o.class)),
                                Value::Instance(o) => Some(Recv::Instance(o)),
                                Value::Class(k) if sp.is_class => Some(Recv::Class(k)),
                                _ => None,
                            })
                            .collect();
                        if recvs.is_empty() {
                            recvs.push(Recv::Unknown);
                        }
                        for r in recvs {
                            result.extend(self.invoke(f, r, rest, kwargs, eff));
                        }
                    }
                    None => result.extend(self.invoke(f, Recv::Unknown, args, kwargs, eff)),
                },
                Value::Bound(f, r) => result.extend(self.invoke(f, r, args, kwargs, eff)),
                Value::Class(c) => {
                    let o = Obj { class: c, alloc };
                    result.insert(Value::Instance(o));
                    for k in self.constructors(c) {
                        self.invoke(k, Recv::Instance(o), args, kwargs, eff);
                    }
                }
                Value::Instance(o) => {
                    for m in self.call_methods(o.class) {
                        result.extend(self.invoke(m, Recv::Instance(o), args, kwargs, eff));
                    }
                }
                // Calling a plain object does nothing; calls of library objects are library
                // calls (their result is decided by `call_result`).
                Value::Generator(_) | Value::Property(_) | Value::Object(_) | Value::Library(_) => {}
            }
        }
        result
    }

    /// Bind arguments to `f`'s parameters under receiver `r`; returns the context used.
    fn bind_args(&self, f: SymbolId, r: Recv, args: &[Vals], kwargs: &[(Name, Vals)], eff: &mut Eff) -> Recv {
        let ctx = if self.f.selfs.contains_key(&f) {
            let ctx = self.ctx_for(f, r);
            eff.contexts.push((f, ctx));
            ctx
        } else {
            Recv::Unknown
        };
        let params = self.f.call_params(f);
        let scope = ScopeKey::Symbol(f);
        for (&p, v) in params.iter().zip(args) {
            eff.write(Slot::Var(scope, ctx, p), v.clone());
        }
        for (k, v) in kwargs {
            if params.contains(k) {
                eff.write(Slot::Var(scope, ctx, *k), v.clone());
            }
        }
        ctx
    }

    /// Call `f` with receiver `r`: bind arguments, register the context, return its result.
    pub(super) fn invoke(
        &self,
        f: SymbolId,
        r: Recv,
        args: &[Vals],
        kwargs: &[(Name, Vals)],
        eff: &mut Eff,
    ) -> Vals {
        let ctx = self.bind_args(f, r, args, kwargs, eff);
        self.slot(Slot::Return(f, ctx))
            .map(SlotRef::to_vals)
            .unwrap_or_default()
    }

    /// Methods / generator bodies run by consuming `vals` (iteration protocol), with the
    /// receiver each runs under. `advance`: `next()` only (no `__iter__`).
    pub(super) fn iterate_targets(&self, vals: &Vals, advance: bool, eff: &mut Eff) -> Vec<(SymbolId, Recv)> {
        let mut out = Vec::new();
        for v in vals {
            match *v {
                Value::Instance(o) => {
                    let recv = Recv::Instance(o);
                    let iters = if advance {
                        Vec::new()
                    } else {
                        self.member_functions(o.class, "__iter__")
                    };
                    let mut next_objs: Vec<Obj> = Vec::new();
                    if iters.is_empty() {
                        next_objs.push(o);
                    }
                    for i in iters {
                        out.push((i, recv));
                        for r in self.invoke(i, recv, &[], &[], eff) {
                            match r {
                                Value::Instance(o2) => next_objs.push(o2),
                                Value::Generator(g) => out.push((g, Recv::Unknown)),
                                _ => {}
                            }
                        }
                    }
                    for o2 in next_objs {
                        for n in self.member_functions(o2.class, "__next__") {
                            self.invoke(n, Recv::Instance(o2), &[], &[], eff);
                            out.push((n, Recv::Instance(o2)));
                        }
                    }
                }
                Value::Generator(g) => out.push((g, Recv::Unknown)),
                _ => {}
            }
        }
        out
    }

    /// Methods an implicit data-model operation runs, with their receivers.
    pub(super) fn implicit_targets(&self, op: &Implicit, sc: Sc, eff: &mut Eff) -> Vec<(SymbolId, Recv)> {
        let mut out = Vec::new();
        match op.kind {
            ImplicitKind::DescriptorGet => {
                let Node::Attr { object, attr, .. } = &op.subject else {
                    return out;
                };
                for v in self.eval(object, sc, eff) {
                    let class = match v {
                        Value::Class(c) => c,
                        Value::Instance(o) => o.class,
                        _ => continue,
                    };
                    let Some(members) = self.raw_member(class, *attr) else {
                        continue;
                    };
                    for &m in members.iter() {
                        match m {
                            Value::Instance(d) => {
                                let args = [Vals::one(v), Vals::one(Value::Class(class))];
                                for g in self.member_functions(d.class, "__get__") {
                                    self.invoke(g, Recv::Instance(d), &args, &[], eff);
                                    out.push((g, Recv::Instance(d)));
                                }
                            }
                            Value::Property(p) => {
                                if let Value::Instance(o) = v {
                                    self.invoke(p, Recv::Instance(o), &[], &[], eff);
                                    out.push((p, Recv::Instance(o)));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            ImplicitKind::Iterate => {
                let vals = self.eval(&op.subject, sc, eff);
                out = self.iterate_targets(&vals, false, eff);
            }
            kind => {
                for v in self.eval(&op.subject, sc, eff) {
                    if let Value::Instance(o) = v {
                        for name in data_model_methods(kind) {
                            for m in self.member_functions(o.class, name) {
                                self.invoke(m, Recv::Instance(o), &[], &[], eff);
                                out.push((m, Recv::Instance(o)));
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// Targets a call can execute: functions, constructors of classes, `__call__` of instances.
    pub(super) fn call_targets(&self, callees: &Vals) -> BTreeSet<SymbolId> {
        let mut targets = BTreeSet::new();
        for &v in callees {
            match v {
                Value::Function(f) | Value::Bound(f, _) => {
                    targets.insert(f);
                }
                Value::Class(c) => {
                    // Calling a class constructs it: its constructors run; a class without a
                    // constructor in the index (inherited from library code) is itself what
                    // the call reaches (the `constructor` edge a proven call would have).
                    let constructors = self.constructors(c);
                    if constructors.is_empty() {
                        targets.insert(c);
                    } else {
                        targets.extend(constructors);
                    }
                }
                Value::Instance(o) => targets.extend(self.call_methods(o.class)),
                Value::Generator(_) | Value::Property(_) | Value::Object(_) | Value::Library(_) => {}
            }
        }
        targets
    }
}

/// Whether a callee value set holds a repository value (anything but library objects): then
/// the repository code runs, not the library knowledge of the call.
pub(super) fn has_repository_callee(callees: &Vals) -> bool {
    callees.iter().any(|v| !matches!(v, Value::Library(_)))
}

/// Whether library knowledge says what a call returns (`returns`, `wraps`, `partial`,
/// `property`, `receiver`, a new delegating object): then the library made no object of its
/// own for the result.
fn produces_result(effects: &[Effect]) -> bool {
    effects.iter().any(|e| match e {
        Effect::Returns(_)
        | Effect::Wraps(_)
        | Effect::Partial(_)
        | Effect::Property(_)
        | Effect::Receiver => true,
        Effect::CopiesMembers { to, .. } => matches!(to, ArgSel::Result),
        Effect::DelegatesMembers { object, .. } => matches!(object, ArgSel::Result),
        _ => false,
    })
}

/// Whether an effect copies members or makes an object delegate (also inside a decorator
/// factory's `decorates`).
pub(super) fn links_members(effect: &Effect) -> bool {
    match effect {
        Effect::CopiesMembers { .. } | Effect::DelegatesMembers { .. } => true,
        Effect::Decorates { inner } => links_members(inner),
        _ => false,
    }
}
