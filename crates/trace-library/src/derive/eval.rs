//! Expression evaluation of a derivation (child of `derive`): abstract values of names,
//! attributes and calls, callee resolution (library functions, constructors, slot contents,
//! native leaves), held constructor arguments, member copies and argument binding.

use super::*;

impl<'c> Program<'c> {
    pub(super) fn eval(&self, st: &mut State, f: u32, e: &Expr, depth: usize) -> Vals {
        if depth > self.cx.limits.max_eval_depth {
            return Vals::new();
        }
        match e {
            Expr::Name { name, .. } => self.lookup(st, f, name, depth),
            Expr::Attr { object, attr, .. } => {
                let objs = self.eval(st, f, object, depth + 1);
                self.attr_of(st, &objs, attr)
            }
            Expr::Call {
                func,
                args,
                kwargs,
                span,
                ..
            } => {
                let mut out = self.call_value(st, f, func, args, kwargs, depth);
                out.extend(self.record_value(st, f, func, kwargs, *span, depth));
                out
            }
            Expr::Choice(alts) => {
                let mut out = Vals::new();
                for a in alts {
                    out.extend(self.eval(st, f, a, depth + 1));
                }
                out
            }
            Expr::Await(inner) => self.eval(st, f, inner, depth + 1),
            Expr::Lambda {
                function: Some(d), ..
            } => {
                let u = self.funcs[f as usize].unit;
                self.units[u as usize]
                    .decl_func
                    .get(d)
                    .map(|g| V::Fn(*g, false))
                    .into_iter()
                    .collect()
            }
            _ => Vals::new(),
        }
    }

    pub(super) fn lookup(&self, st: &mut State, f: u32, name: &str, depth: usize) -> Vals {
        if let Some(lit) = name.strip_prefix(LITERAL_PREFIX) {
            let id = st.lit(lit);
            return [V::Lit(id)].into_iter().collect();
        }
        if let Some(v) = self.arguments_value(f, name) {
            return v;
        }
        let key = (f, name.to_string());
        if let Some(v) = st.memo.get(&key) {
            return v.clone();
        }
        if !st.visiting.insert(key.clone()) {
            return Vals::new();
        }
        let out = self.lookup_uncached(st, f, name, depth);
        st.visiting.remove(&key);
        st.memo.insert(key, out.clone());
        out
    }

    pub(super) fn lookup_uncached(&self, st: &mut State, f: u32, name: &str, depth: usize) -> Vals {
        let mut cur = Some(f);
        for _ in 0..16 {
            let Some(g) = cur else { break };
            let func = &self.funcs[g as usize];
            if func.self_param.as_deref() == Some(name) {
                if let Some(c) = func.class {
                    let v = if func.class_method {
                        V::Class(c)
                    } else {
                        V::Inst(c)
                    };
                    return [v].into_iter().collect();
                }
            }
            if !func.is_module {
                if let Some(&n) = func.nested.get(name) {
                    return [V::Fn(n, false)].into_iter().collect();
                }
                if let Some(&c) = func.local_classes.get(name) {
                    return [V::Class(c)].into_iter().collect();
                }
                let mut out = Vals::new();
                let mut found = false;
                if let Some(i) = func.params.iter().position(|p| p.name == name) {
                    found = true;
                    let i = i as u16;
                    out.insert(V::Param(g, i, How::Direct));
                    if let Some(extra) = st.elem_alias.get(&(g, i)) {
                        out.extend(extra.iter().copied());
                    }
                }
                if let Some(values) = func.locals.get(name) {
                    found = true;
                    for v in values {
                        out.extend(self.eval(st, g, v, depth + 1));
                    }
                    if func.container_locals.contains(name) {
                        let s = st.slot(SlotKey {
                            owner: SlotOwner::Local(g),
                            name: name.to_string(),
                        });
                        out.insert(V::Slot(s, How::Direct));
                    }
                } else if func.container_locals.contains(name) {
                    found = true;
                    let s = st.slot(SlotKey {
                        owner: SlotOwner::Local(g),
                        name: name.to_string(),
                    });
                    out.insert(V::Slot(s, How::Direct));
                }
                if !found {
                    let unit = &self.units[func.unit as usize];
                    if let Some(imp) = unit
                        .imports
                        .iter()
                        .find(|i| i.scope_func == Some(g) && i.local == name)
                    {
                        let resolved = self.import_resolution(imp, 0);
                        return self.resolved_values(st, resolved);
                    }
                }
                if found {
                    return out;
                }
            }
            cur = func.parent;
        }
        // A bare name that is a declared field of the enclosing class (implicit `this`).
        if self.spec.implicit_fields {
            if let Some(c) = self.enclosing_class(f) {
                let declared = std::iter::once(c)
                    .chain(self.related.get(c as usize).into_iter().flatten().copied())
                    .any(|x| self.field_types.contains_key(&(x, name.to_string())));
                if declared {
                    let s = self.class_slot(st, c, name);
                    return [V::Slot(s, How::Direct)].into_iter().collect();
                }
            }
        }
        let u = self.funcs[f as usize].unit;
        let resolved = self.resolve_global(u, name, 0);
        self.resolved_values(st, resolved)
    }

    pub(super) fn attr_of(&self, st: &mut State, objs: &Vals, attr: &str) -> Vals {
        let mut out = Vals::new();
        let objs = self.typed(st, objs);
        let prototype = self.spec.objects.as_ref().map(|m| m.prototype);
        for v in &objs {
            match *v {
                V::Class(c) if Some(attr) == prototype => {
                    out.insert(V::Inst(c));
                }
                V::Inst(c) | V::Class(c) => {
                    if let Some(n) = self.nested_class(c, attr) {
                        out.insert(V::Class(n));
                    } else if let Some(m) = self.find_method(st, c, attr) {
                        let bound = matches!(v, V::Inst(_)) || self.funcs[m as usize].class_method;
                        out.insert(V::Fn(m, bound));
                    } else {
                        let s = self.class_slot(st, c, attr);
                        out.insert(V::Slot(s, How::Direct));
                    }
                }
                V::Module(u) => {
                    let resolved = self.resolve_global(u, attr, 0);
                    if resolved.is_empty() {
                        let values = self.module_value(st, u, 0);
                        out.extend(self.attr_of(st, &values, attr));
                    }
                    out.extend(self.resolved_values(st, resolved));
                }
                V::Rec(r) => {
                    let s = st.slot(SlotKey {
                        owner: SlotOwner::Record(r),
                        name: attr.to_string(),
                    });
                    out.insert(V::Slot(s, How::Direct));
                }
                // Attributes of a parameter object (and theirs) stay inside that object.
                V::Param(f, i, How::Direct | How::Wrapped) | V::ParamAttr(f, i) => {
                    out.insert(V::ParamAttr(f, i));
                }
                _ => {}
            }
        }
        out
    }

    /// The parameters of `f` (and what instances `f` built hold) among `held_args`: held by
    /// the instance of `c` built from them.
    pub(super) fn hold(&self, st: &mut State, f: u32, c: u32, held_args: &[&Expr], depth: usize) {
        let mut add: BTreeSet<u16> = BTreeSet::new();
        for a in held_args {
            for v in self.eval(st, f, a, depth + 1) {
                match v {
                    // Built values too: a key held as `parent.prefix + prefix`.
                    V::Param(p, i, _) if p == f => {
                        add.insert(i);
                    }
                    V::Inst(c2) if c2 != c => {
                        if let Some(h) = st.held.get(&(f, c2)) {
                            add.extend(h.iter().copied());
                        }
                    }
                    _ => {}
                }
            }
        }
        if add.is_empty() {
            return;
        }
        let entry = st.held.entry((f, c)).or_default();
        let before = entry.len();
        entry.extend(add);
        if entry.len() != before {
            st.changed = true;
        }
    }

    /// Whether parameter `i` of `f` is declared as a registry object (an instance of a class
    /// owning a dispatched slot, or of a class related to one).
    pub(super) fn registry_param(&self, st: &State, f: u32, i: u16) -> bool {
        let Some(t) = self.param_types.get(&(f, i)) else {
            return false;
        };
        let Some(c) = self.declared_class(self.funcs[f as usize].unit, t) else {
            return false;
        };
        std::iter::once(c)
            .chain(self.related.get(c as usize).into_iter().flatten().copied())
            .any(|x| st.registry_classes.contains(&x))
    }

    pub(super) fn call_value(
        &self,
        st: &mut State,
        f: u32,
        func: &Expr,
        args: &[Expr],
        kwargs: &[(String, Expr)],
        depth: usize,
    ) -> Vals {
        let mut out = Vals::new();
        if let Expr::Name { name, .. } = func {
            if name == CONCAT_CALLEE {
                for a in args {
                    let v = self.eval(st, f, a, depth + 1);
                    out.extend(built(v));
                }
                return out;
            }
            if name == CONTAINER_CALLEE {
                for a in args.iter().chain(kwargs.iter().map(|(_, v)| v)) {
                    out.extend(self.eval(st, f, a, depth + 1));
                }
                return out;
            }
            if name == KEYS_CALLEE {
                if let Some(a) = args.first() {
                    out.extend(self.keys_of(st, f, a, depth));
                }
                return out;
            }
            if Some(name.as_str()) == self.spec.super_call {
                if let Some(c) = self.enclosing_class(f) {
                    let base = self.classes[c as usize].bases.first().copied().unwrap_or(c);
                    out.insert(V::Inst(base));
                    return out;
                }
            }
        }
        if let Some(v) = self.object_call_value(st, f, func, args, depth) {
            return v;
        }
        if let Some(sp) = spelling(func) {
            if !self.locally_bound(f, sp.split('.').next().unwrap_or(&sp)) {
                if let Some(&(_, i)) = self.spec.identity_functions.iter().find(|(n, _)| *n == sp) {
                    if let Some(a) = args.get(i as usize) {
                        return self.eval(st, f, a, depth + 1);
                    }
                    return out;
                }
                if self.spec.container_functions.contains(&sp.as_str()) {
                    for a in args {
                        out.extend(self.eval(st, f, a, depth + 1));
                    }
                    return out;
                }
                // The member names of a parameter object (`Object.keys(p)`).
                if let Some(&(_, i)) = self.spec.key_functions.iter().find(|(n, _)| *n == sp) {
                    if let Some(a) = args.get(i as usize) {
                        out.extend(self.keys_of(st, f, a, depth));
                    }
                    return out;
                }
            }
        }
        if let Expr::Attr { object, attr, .. } = func {
            if attr == INDEX_READ || self.spec.read_methods.contains(&attr.as_str()) {
                let objs = self.eval(st, f, object, depth + 1);
                let content: Vals = objs
                    .iter()
                    .filter(|v| matches!(v, V::Slot(..) | V::Param(..)))
                    .copied()
                    .collect();
                if attr == INDEX_READ {
                    let mut content = content;
                    content.extend(self.computed_members(st, f, &objs, args.first(), depth));
                    return content;
                }
                out.extend(content);
            }
        }
        let callees = self.callees(st, f, func, args.len() as u32);
        if callees.is_empty() {
            if out.is_empty() {
                for a in args {
                    let v = self.eval(st, f, a, depth + 1);
                    out.extend(built(v));
                }
            }
            return out;
        }
        for callee in callees {
            match callee {
                Callee::Lib(g, skip) => {
                    if let Some(r) = st.ret.get(&g) {
                        out.extend(r.iter().copied());
                    }
                    // A factory returning an instance holding some of its parameters: the
                    // arguments bound to them are held by the instance here too.
                    let returned: Vec<u32> = st
                        .ret
                        .get(&g)
                        .into_iter()
                        .flatten()
                        .filter_map(|v| match v {
                            V::Inst(c) => Some(*c),
                            _ => None,
                        })
                        .collect();
                    for c in returned {
                        self.factory_roles(st, f, g, skip, c, args, kwargs, depth);
                        let Some(inside) = st.held.get(&(g, c)).cloned() else { continue };
                        let bound = self.bind(g, skip, args, kwargs);
                        let held_args: Vec<&Expr> = bound
                            .iter()
                            .filter(|(j, _)| inside.contains(j))
                            .map(|(_, a)| *a)
                            .collect();
                        self.hold(st, f, c, &held_args, depth);
                    }
                    // A constructor called directly (`super().__new__(cls)`) makes an instance.
                    let callee = &self.funcs[g as usize];
                    if callee.is_ctor {
                        if let Some(c) = callee.class {
                            out.insert(V::Inst(c));
                        }
                    }
                    for (j, a) in self.bind(g, skip, args, kwargs) {
                        let m = st.mask(g, j);
                        if m & RETURNS != 0 {
                            out.extend(self.eval(st, f, a, depth + 1));
                        }
                        if m & WRAPS != 0 {
                            let v = self.eval(st, f, a, depth + 1);
                            out.extend(wrapped(v));
                        }
                    }
                }
                Callee::Ctor(c) => {
                    out.insert(V::Inst(c));
                    let all_args: Vec<&Expr> = args.iter().chain(kwargs.iter().map(|(_, v)| v)).collect();
                    self.hold(st, f, c, &all_args, depth);
                    self.hold_roles(st, f, c, args, kwargs, depth);
                    let ctors = self.ctors(st, c);
                    // An allocation of a type without constructor code initialises its
                    // fields by name (Go `&Server{Handler: h}`, generated record / data
                    // class initialisers).
                    if ctors.is_empty() && self.unresolved_bases(c).is_empty() {
                        self.store_fields(st, f, c, kwargs, depth);
                    }
                    for g in ctors {
                        for (j, a) in self.bind(g, true, args, kwargs) {
                            if st.mask(g, j) & WRAPS != 0 {
                                let v = self.eval(st, f, a, depth + 1);
                                out.extend(wrapped(v));
                            }
                        }
                    }
                }
                Callee::Value(_) => {}
                Callee::Leaf { effects, .. } => {
                    for eff in &effects {
                        match eff {
                            Effect::Returns(sel) => {
                                for a in select(sel, func, args, kwargs) {
                                    out.extend(self.eval(st, f, a, depth + 1));
                                }
                            }
                            Effect::Wraps(sel) | Effect::Partial(sel) => {
                                for a in select(sel, func, args, kwargs) {
                                    let v = self.eval(st, f, a, depth + 1);
                                    out.extend(wrapped(v));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        out
    }

    /// The library classes (constructed) and functions (called) a called slot holds.
    pub(super) fn slot_callees(&self, st: &State, s: u32, positional: u32) -> Vec<Callee> {
        let mut out = Vec::new();
        for v in st.callables_of(s) {
            match v {
                V::Class(c) => out.push(Callee::Ctor(c)),
                V::Fn(g, bound) => {
                    out.extend(
                        self.clauses_of(g, positional)
                            .into_iter()
                            .map(|h| Callee::Lib(h, bound)),
                    );
                }
                _ => {}
            }
        }
        out
    }

    /// What a callee expression calls (typed receivers only; an unknown receiver is nothing).
    /// `objs` plus the instances held by its slots (a value read back out of a slot is typed
    /// by what was stored).
    pub(super) fn with_slot_instances(&self, st: &State, objs: &Vals) -> Vals {
        let mut out = objs.clone();
        for v in objs {
            if let V::Slot(s, How::Direct | How::Wrapped) = *v {
                out.extend(st.insts_of(s).into_iter().map(V::Inst));
            }
        }
        out
    }

    /// [`Program::with_slot_instances`] plus the instances of declared types: a parameter or
    /// field whose declared type is a class of the loaded library code is an instance of it.
    pub(super) fn typed(&self, st: &State, objs: &Vals) -> Vals {
        let mut out = self.with_slot_instances(st, objs);
        out.extend(self.fn_instances(st, objs));
        for v in objs {
            let declared = match *v {
                V::Param(p, i, How::Direct | How::Wrapped) => self
                    .param_types
                    .get(&(p, i))
                    .map(|t| (self.funcs[p as usize].unit, t.clone())),
                V::Slot(s, How::Direct | How::Wrapped) => self.slot_type(st, s),
                _ => None,
            };
            if let Some(c) = declared.and_then(|(u, t)| self.declared_class(u, &t)) {
                out.insert(V::Inst(c));
            }
        }
        out
    }

    /// The member names of the parameter objects `object` evaluates to.
    pub(super) fn keys_of(&self, st: &mut State, f: u32, object: &Expr, depth: usize) -> Vals {
        self.eval(st, f, object, depth + 1)
            .into_iter()
            .filter_map(|v| match v {
                V::Param(p, j, How::Direct) => Some(V::Keys(p, j)),
                V::Keys(..) => Some(v),
                _ => None,
            })
            .collect()
    }

    /// Keyword arguments of an allocation stored into the fields of that name.
    pub(super) fn store_fields(
        &self,
        st: &mut State,
        f: u32,
        c: u32,
        kwargs: &[(String, Expr)],
        depth: usize,
    ) {
        for (name, value) in kwargs {
            if name == KEYWORD_SPREAD || name == POSITIONAL_SPREAD {
                continue;
            }
            let s = self.class_slot(st, c, name);
            let vals = self.eval(st, f, value, depth + 1);
            self.store(st, f, &[s], &vals, None);
        }
    }

    /// Record a method called on a receiver value whose declared type the library cannot
    /// type further (an interface or a type without source): the method runs on whatever
    /// object the caller passed (`CallsMethod`), now or after the value was stored.
    pub(super) fn record_method(&self, st: &mut State, f: u32, v: V, method: &str) {
        match v {
            V::Param(p, i, How::Direct | How::Wrapped) if p == f || self.is_ancestor(p, f) => {
                let u = self.funcs[p as usize].unit;
                let open = self
                    .param_types
                    .get(&(p, i))
                    .is_some_and(|t| self.open_type(u, t) && !self.closed_type(st, u, t));
                if open {
                    st.add_pmethod(p, i, method);
                }
            }
            V::Slot(s, How::Direct | How::Wrapped) => {
                if st.slot_method_callers.entry(s).or_default().insert(f) {
                    st.changed = true;
                }
                if self
                    .slot_type(st, s)
                    .is_some_and(|(u, t)| self.open_type(u, &t) && !self.closed_type(st, u, &t))
                {
                    st.add_slot_method(s, method);
                }
            }
            _ => {}
        }
    }

    /// A method the callee runs on one of its parameters, applied to the argument value.
    pub(super) fn apply_method(&self, st: &mut State, f: u32, v: V, method: &str) {
        match v {
            V::Param(p, i, How::Direct | How::Wrapped) if p == f || self.is_ancestor(p, f) => {
                let u = self.funcs[p as usize].unit;
                if !self
                    .param_types
                    .get(&(p, i))
                    .is_some_and(|t| self.closed_type(st, u, t))
                {
                    st.add_pmethod(p, i, method);
                }
            }
            V::Slot(s, How::Direct | How::Wrapped) => st.add_slot_method(s, method),
            V::Inst(c) => {
                if let Some(m) = self.find_method(st, c, method) {
                    st.edges.insert((f, m));
                }
            }
            _ => {}
        }
    }

    /// Member-copy rule: `objs` (a parameter `to`) gets members under `keys` that are the
    /// member names of another parameter `from` of the same function (its key list, or the
    /// object iterated by `for (k in from)`), with values taken from `from` (a definition
    /// whose value is unknown - a descriptor built by an engine built-in - also counts).
    pub(super) fn member_copy(
        &self,
        st: &mut State,
        f: u32,
        objs: &Vals,
        keys: &Vals,
        values: Option<&Vals>,
    ) {
        for o in objs {
            let V::Param(p, to, How::Direct) = *o else { continue };
            if p != f && !self.is_ancestor(p, f) {
                continue;
            }
            for k in keys {
                let from = match *k {
                    V::Keys(q, i) | V::Param(q, i, How::Direct) if q == p => i,
                    _ => continue,
                };
                if from == to {
                    continue;
                }
                let from_source = values.is_none_or(|vals| {
                    vals.is_empty()
                        || vals
                            .iter()
                            .any(|v| matches!(*v, V::Param(q, i, _) | V::Keys(q, i) if q == p && i == from))
                });
                if from_source {
                    st.add_copy(p, from, to);
                }
            }
        }
    }

    pub(super) fn callees(&self, st: &mut State, f: u32, func: &Expr, positional: u32) -> Vec<Callee> {
        let mut out: Vec<Callee> = Vec::new();
        let mut typed_receiver = false;
        if let Expr::Attr { object, attr, .. } = func {
            let raw = self.eval(st, f, object, 1);
            let raw = if raw.iter().any(|v| matches!(v, V::Module(_))) {
                self.with_module_values(st, &raw, 1)
            } else {
                raw
            };
            // Container methods act on the container, not on the instances it holds.
            let container_method = self.spec.store_methods.contains(&attr.as_str())
                || self.spec.read_methods.contains(&attr.as_str());
            let invoke_method = self.spec.invoke_methods.contains(&attr.as_str());
            let element_method = self.spec.element_methods.contains(&attr.as_str());
            // A local variable's declared type (`for (Runnable t : tasks) t.run();`).
            let local_type = match object.as_ref() {
                Expr::Name { name, .. } => self
                    .local_types
                    .get(&(f, name.clone()))
                    .map(|t| (self.funcs[f as usize].unit, t.clone())),
                _ => None,
            };
            for &o in &raw {
                // The single method of a functional type calls the value itself.
                let declared = match o {
                    V::Param(p, i, How::Direct | How::Wrapped) => local_type.clone().or_else(|| {
                        self.param_types
                            .get(&(p, i))
                            .map(|t| (self.funcs[p as usize].unit, t.clone()))
                    }),
                    V::Slot(s, How::Direct | How::Wrapped) => {
                        local_type.clone().or_else(|| self.slot_type(st, s))
                    }
                    _ => None,
                };
                if let Some((u, t)) = declared {
                    if self.calls_function(u, &t, attr) {
                        typed_receiver = true;
                        out.push(Callee::Value(o));
                        continue;
                    }
                }
                if !container_method && !invoke_method && !element_method {
                    self.record_method(st, f, o, attr);
                }
            }
            let objs = if container_method {
                let own = self.method_receivers(st, &raw, attr);
                raw.into_iter().chain(own).collect()
            } else {
                self.typed(st, &raw)
            };
            for o in &objs {
                match *o {
                    V::Inst(c) | V::Class(c) => {
                        typed_receiver = true;
                        if let Some(m) = self.find_method(st, c, attr) {
                            let bound = matches!(o, V::Inst(_)) || self.funcs[m as usize].class_method;
                            for h in self.clauses_of(m, positional) {
                                out.push(Callee::Lib(h, bound));
                            }
                            if matches!(o, V::Inst(_)) {
                                out.extend(self.overrides(c, attr).into_iter().map(|m| Callee::Lib(m, true)));
                            }
                        } else if let Some(n) = self.nested_class(c, attr) {
                            out.push(Callee::Ctor(n));
                        } else if !self.classes[c as usize].is_interface {
                            let bases = self.unresolved_bases(c);
                            if bases.is_empty() {
                                let s = self.class_slot(st, c, attr);
                                out.push(Callee::Value(V::Slot(s, How::Direct)));
                                out.extend(self.slot_callees(st, s, positional));
                            } else {
                                let mut symbols = vec![format!("{}.{attr}", self.class_symbol(c))];
                                symbols.extend(bases.iter().map(|b| format!("{b}.{attr}")));
                                let mut effects = Vec::new();
                                for s in &symbols {
                                    effects.extend(self.cx.leaves.by_symbol(self.language, s, positional));
                                }
                                out.push(Callee::Leaf { effects, symbols });
                            }
                        }
                    }
                    V::Module(u) => {
                        // A module attribute without source falls through to the leaf path.
                        let resolved = self.resolve_global(u, attr, 0);
                        if resolved.iter().any(|r| !matches!(r, Resolved::Leaf(_))) {
                            typed_receiver = true;
                        }
                        for v in self.resolved_values(st, resolved) {
                            match v {
                                V::Fn(g, _) => {
                                    for h in self.clauses_of(g, positional) {
                                        out.push(Callee::Lib(h, false));
                                    }
                                }
                                V::Class(c) => out.push(Callee::Ctor(c)),
                                V::Slot(..) => out.push(Callee::Value(v)),
                                _ => {}
                            }
                        }
                    }
                    V::Param(_, _, How::Direct | How::Wrapped)
                    | V::Slot(_, How::Direct | How::Wrapped)
                    | V::Fn(..)
                        if self.spec.invoke_methods.contains(&attr.as_str()) =>
                    {
                        typed_receiver = true;
                        out.push(Callee::Value(*o));
                    }
                    V::Param(..)
                    | V::Slot(..)
                    | V::ParamAttr(..)
                    | V::Fn(..)
                    | V::Lit(_)
                    | V::Keys(..)
                    | V::Rec(_) => {
                        // Unknown receiver type: nothing (never a method-name union).
                        typed_receiver = true;
                    }
                }
            }
        } else {
            let values = self.eval(st, f, func, 1);
            let values = self.with_module_values(st, &values, 1);
            for v in values {
                match v {
                    V::Fn(g, bound) => {
                        for h in self.clauses_of(g, positional) {
                            out.push(Callee::Lib(h, bound));
                        }
                    }
                    V::Class(c) => out.push(Callee::Ctor(c)),
                    V::Inst(c) => {
                        if let Some(m) = self.spec.call_method.and_then(|cm| self.find_method(st, c, cm)) {
                            out.push(Callee::Lib(m, true));
                        }
                        out.extend(self.instance_fns(st, c).into_iter().map(|g| Callee::Lib(g, false)));
                    }
                    V::Param(_, _, How::Direct | How::Wrapped) => out.push(Callee::Value(v)),
                    V::Slot(s, How::Direct | How::Wrapped) => {
                        out.push(Callee::Value(v));
                        out.extend(self.slot_callees(st, s, positional));
                    }
                    _ => {}
                }
            }
        }
        if out.is_empty() && !typed_receiver {
            if let Some((symbols, qualifier, name)) = self.leaf_path(f, func) {
                let mut effects = Vec::new();
                for s in &symbols {
                    effects.extend(self.cx.leaves.by_symbol(self.language, s, positional));
                }
                if effects.is_empty() {
                    effects =
                        self.cx
                            .leaves
                            .by_spelling(self.language, qualifier.as_deref(), &name, positional);
                }
                out.push(Callee::Leaf { effects, symbols });
            }
        }
        let mut seen = HashSet::new();
        out.retain(|c| match c {
            Callee::Lib(g, b) => seen.insert((0u8, *g, *b)),
            Callee::Ctor(c) => seen.insert((1u8, *c, false)),
            _ => true,
        });
        out
    }

    /// Qualified symbol candidates, qualifier and name of a callee that resolves to no library
    /// source: an import without source (`_signal.signal`), a language builtin
    /// (`builtins.map`, `globalThis.setTimeout`) or an unbound dotted global (`table.insert`).
    pub(super) fn leaf_path(&self, f: u32, func: &Expr) -> Option<(Vec<String>, Option<String>, String)> {
        let spelled = spelling(func)?;
        let parts: Vec<&str> = spelled.split('.').collect();
        let root = *parts.first()?;
        if self.locally_bound(f, root) || root.starts_with('<') {
            return None;
        }
        let u = self.funcs[f as usize].unit;
        let base = match self.resolve_global(u, root, 0).first() {
            None if parts.len() == 1 && !self.builtins_module.is_empty() => {
                format!("{}.{root}", self.builtins_module)
            }
            None => root.to_string(),
            Some(Resolved::Leaf(t)) => t.clone(),
            Some(Resolved::Module(m)) => self.units[*m as usize].module.clone()?,
            Some(_) => return None,
        };
        let symbol = if parts.len() > 1 {
            format!("{base}.{}", parts[1..].join("."))
        } else {
            base
        };
        let name = (*parts.last()?).to_string();
        let qualifier = (parts.len() > 1).then(|| parts[parts.len() - 2].to_string());
        Some((vec![symbol], qualifier, name))
    }

    /// (parameter index, argument) pairs of a call of `g`.
    pub(super) fn bind<'e>(
        &self,
        g: u32,
        skip: bool,
        args: &'e [Expr],
        kwargs: &'e [(String, Expr)],
    ) -> Vec<(u16, &'e Expr)> {
        let func = &self.funcs[g as usize];
        let params = &func.params;
        let receiver = skip
            && params
                .first()
                .is_some_and(|p| func.self_param.as_deref() == Some(p.name.as_str()));
        let start = usize::from(receiver);
        let positional: Vec<usize> = (start..params.len())
            .filter(|&i| params[i].kind == ParamKind::Positional)
            .collect();
        let rest = params.iter().position(|p| p.kind == ParamKind::VarPositional);
        let mut out = Vec::new();
        for (k, a) in args.iter().enumerate() {
            if let Some(&i) = positional.get(k) {
                out.push((i as u16, a));
            } else if let Some(r) = rest {
                out.push((r as u16, a));
            }
        }
        for (name, v) in kwargs {
            if let Some(i) = params.iter().position(|p| &p.name == name) {
                out.push((i as u16, v));
            }
        }
        out
    }
}
