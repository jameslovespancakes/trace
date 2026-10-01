//! Effects of a derivation (child of `derive`): applying an effect to a value, stores into
//! slots and registries, the statements of one function, calls into library functions and
//! native leaves, keyword forwarding, sends and verbs.

use super::*;

impl<'c> Program<'c> {
    pub(super) fn apply(&self, st: &mut State, f: u32, v: V, e: u8) {
        match v {
            V::Param(p, i, how) => {
                let Some(e2) = eff_for(e, how) else { return };
                if e == CALLS && (p == f || self.is_ancestor(p, f)) {
                    self.note_param_caller(st, f, p, i, how);
                }
                if p == f {
                    st.add_mask(p, i, e2);
                } else if self.is_ancestor(p, f) && st.captured.entry(f).or_default().insert((p, i, how, e)) {
                    st.changed = true;
                }
            }
            V::Slot(s, how) => {
                if how == How::Built {
                    return;
                }
                let add = match e {
                    CALLS => {
                        st.add_caller(s, f);
                        let func = &self.funcs[f as usize];
                        let mut m = CALLS;
                        if func.class.is_some() {
                            if Some(func.name.as_str()) == self.spec.call_method {
                                m |= WRAPS;
                            }
                            if Some(func.name.as_str()) == self.spec.get_method {
                                m |= PROPERTY;
                            }
                        }
                        m
                    }
                    STORED => CALLS,
                    ITERATES => ITERATES,
                    PROPERTY => PROPERTY,
                    WRAPS => WRAPS,
                    _ => 0,
                };
                if add != 0 {
                    st.add_slot_mask(s, add);
                }
            }
            V::Fn(g, _) => {
                if e == CALLS {
                    st.edges.insert((f, g));
                }
                if let Some(caps) = st.captured.get(&g).cloned() {
                    for (p, i, how, ce) in caps {
                        if let Some(c) = compose(e, ce) {
                            self.apply(st, f, V::Param(p, i, how), c);
                        }
                    }
                }
            }
            V::Inst(c) if e == CALLS => {
                if let Some(m) = self.spec.call_method.and_then(|cm| self.find_method(st, c, cm)) {
                    st.edges.insert((f, m));
                }
            }
            _ => {}
        }
    }

    /// Store `vals` into slots `targets` (keyed by `key` when the store names one).
    pub(super) fn store(&self, st: &mut State, f: u32, targets: &[u32], vals: &Vals, key: Option<&Expr>) {
        let key_vals: Vals = match key {
            Some(k) => self.eval(st, f, k, 0),
            None => Vals::new(),
        };
        for &s in targets {
            let owner = st.slot_keys[s as usize].owner;
            for &v in vals {
                match v {
                    V::Param(p, i, how) => {
                        if p == f || self.is_ancestor(p, f) {
                            st.insert_stored(s, f, v);
                            self.note_param_store(st, p, i, how, s);
                        }
                        if p == f && st.owner_stores.insert((owner, f, i)) {
                            st.changed = true;
                        }
                    }
                    V::Slot(s2, How::Direct | How::Wrapped) => {
                        if s2 != s && st.slot_flow.insert((s2, s)) {
                            st.changed = true;
                        }
                    }
                    V::Fn(..) | V::Inst(_) | V::Class(_) | V::ParamAttr(..) => st.insert_stored(s, f, v),
                    _ => {}
                }
            }
            if st.channels {
                self.registers_at_store(st, f, s, vals, &key_vals);
            }
        }
    }

    /// Rule 2/3: a store into a dispatch registry registers handlers / mounts sub-registries
    /// under the key (the store's key, else the first other parameter stored into the same
    /// owner).
    pub(super) fn registers_at_store(&self, st: &mut State, f: u32, s: u32, vals: &Vals, key_vals: &Vals) {
        self.registers_entry_objects(st, f, s, vals, key_vals);
        // Rule 3 through entry objects: an instance holding a registry-object parameter stored
        // into a registry mounts that registry under the first other held parameter.
        if st.dispatched.contains_key(&s) || st.method_dispatched.contains_key(&s) {
            for &v in vals {
                let V::Inst(c) = v else { continue };
                let Some(inside) = st.held.get(&(f, c)).cloned() else { continue };
                let targets: Vec<u16> = inside
                    .iter()
                    .copied()
                    .filter(|&i| self.registry_param(st, f, i))
                    .collect();
                if targets.is_empty() {
                    continue;
                }
                if let Some(k) = inside.iter().copied().filter(|i| !targets.contains(i)).min() {
                    for t in targets {
                        st.add_chan(f, Chan::Mounts { key: k, target: t });
                    }
                }
            }
        }
        let Some(&channel) = st.dispatched.get(&s) else {
            return;
        };
        if key_vals.is_empty() && self.table_configuration(st, s) {
            return;
        }
        let mut handlers: BTreeSet<u16> = BTreeSet::new();
        let mut targets: BTreeSet<u16> = BTreeSet::new();
        for &v in vals {
            match v {
                V::Param(p, h, How::Direct | How::Wrapped) if p == f => {
                    handlers.insert(h);
                }
                V::Fn(c, _) => {
                    for &(p, i, how, ce) in st.captured.get(&c).into_iter().flatten() {
                        if p == f && how != How::Built && matches!(ce, CALLS | STORED) {
                            handlers.insert(i);
                        }
                    }
                }
                V::ParamAttr(p, t) if p == f && self.mount_target(st, f, t) => {
                    targets.insert(t);
                }
                _ => {}
            }
        }
        if handlers.is_empty() && targets.is_empty() {
            return;
        }
        let mut outer_keys: Vec<(u32, u16)> = Vec::new();
        let mut key: Option<u16> = None;
        for v in key_vals {
            if let V::Param(p, k, _) = *v {
                if p == f && key.is_none() {
                    key = Some(k);
                } else if p != f && self.is_ancestor(p, f) {
                    outer_keys.push((p, k));
                }
            }
        }
        if key.is_none() && outer_keys.is_empty() {
            let owner = st.slot_keys[s as usize].owner;
            key = st
                .owner_stores
                .iter()
                .filter(|(o, g, i)| *o == owner && *g == f && !handlers.contains(i) && !targets.contains(i))
                .map(|(_, _, i)| *i)
                .min();
        }
        if let Some(k) = key {
            for &h in &handlers {
                if h != k {
                    st.add_chan(
                        f,
                        Chan::Registers {
                            channel,
                            key: k,
                            handler: h,
                            verb: Verb::Any,
                        },
                    );
                }
            }
            for &t in &targets {
                if t != k {
                    st.add_chan(f, Chan::Mounts { key: k, target: t });
                }
            }
        }
        for (p, k) in outer_keys {
            for &h in &handlers {
                if st.cap_reg.entry(f).or_default().insert((p, k, h, channel, Verb::Any)) {
                    st.changed = true;
                }
            }
        }
    }

    pub(super) fn field_targets(&self, st: &mut State, f: u32, object: &Expr, name: &str) -> Vec<u32> {
        // Typed like reads ([`Program::attr_of`]): a store into an attribute of a parameter
        // (or a slot) of a declared library class writes that class's slot, where the reads
        // of the same attribute look.
        let raw = self.eval(st, f, object, 0);
        let objs = self.typed(st, &raw);
        let mut out = Vec::new();
        for v in objs {
            match v {
                V::Inst(c) | V::Class(c) => out.push(self.class_slot(st, c, name)),
                V::Module(u) => out.push(st.slot(SlotKey {
                    owner: SlotOwner::Unit(u),
                    name: name.to_string(),
                })),
                _ => {}
            }
        }
        out
    }

    pub(super) fn process_function(&self, st: &mut State, f: u32) {
        st.memo.clear();
        self.object_stores(st, f);
        self.returned_through(st, f);
        let func = &self.funcs[f as usize];
        for call in &func.evals {
            self.process_call(st, f, call);
        }
        for (object, name, value) in &func.field_stores {
            let targets = self.field_targets(st, f, object, name);
            if targets.is_empty() {
                continue;
            }
            let vals = self.eval(st, f, value, 0);
            self.store(st, f, &targets, &vals, None);
        }
        for (class, name, value) in &func.member_stores {
            let vals = self.eval(st, f, value, 0);
            if !self.classes[*class as usize].methods.contains_key(name) {
                // The last function value is the alias; a change is a different alias after
                // all values (not the intermediate ones).
                let key = (*class, name.clone());
                let before = st.method_alias.get(&key).copied();
                for v in &vals {
                    if let V::Fn(g, _) = v {
                        st.method_alias.insert(key.clone(), *g);
                    }
                }
                if st.method_alias.get(&key).copied() != before {
                    st.changed = true;
                }
            }
            let s = self.class_slot(st, *class, name);
            self.store(st, f, &[s], &vals, None);
        }
        for (name, value) in &func.global_stores {
            let vals = self.eval(st, f, value, 0);
            let s = st.slot(SlotKey {
                owner: SlotOwner::Unit(func.unit),
                name: name.clone(),
            });
            self.store(st, f, &[s], &vals, None);
        }
        for (object, key, value) in &func.index_stores {
            let objs = self.eval(st, f, object, 0);
            if objs.iter().any(|v| matches!(v, V::Param(..))) {
                let keys = self.eval(st, f, key, 0);
                let values = self.eval(st, f, value, 0);
                // `dest[k] = src[k]`: the stored value must come from the source object.
                if !values.is_empty() {
                    self.member_copy(st, f, &objs, &keys, Some(&values));
                }
            }
            let targets = slots_in(&objs);
            if targets.is_empty() {
                continue;
            }
            let vals = self.eval(st, f, value, 0);
            self.store(st, f, &targets, &vals, Some(key));
        }
        for iterable in &func.iterates {
            for v in self.eval(st, f, iterable, 0) {
                self.apply(st, f, v, ITERATES);
            }
        }
        for r in &func.returns {
            let vals = self.eval(st, f, r, 0);
            for &v in &vals {
                self.apply(st, f, v, RETURNS);
                if matches!(
                    v,
                    V::Fn(..) | V::Inst(_) | V::Class(_) | V::Lit(_) | V::Slot(_, How::Direct) | V::Rec(_)
                ) && st.ret.entry(f).or_default().insert(v)
                {
                    st.changed = true;
                }
                if let V::Fn(g, _) = v {
                    if self.funcs[g as usize].parent == Some(f) {
                        self.decorate(st, f, g);
                    }
                }
            }
            if st.channels {
                self.returned_call_channels(st, f, r);
            }
        }
        for (g, decorators) in &func.decorated {
            for dec in decorators.iter().rev() {
                for callee in self.callees(st, f, dec, 1) {
                    match callee {
                        Callee::Lib(h, skip) => {
                            let args = [Expr::Opaque];
                            if let Some((j, _)) = self.bind(h, skip, &args, &[]).first() {
                                let m = st.mask(h, *j);
                                for e in [CALLS, STORED, PROPERTY] {
                                    if m & e != 0 {
                                        self.apply(st, f, V::Fn(*g, false), e);
                                    }
                                }
                            }
                        }
                        Callee::Leaf { effects, .. } => {
                            for eff in &effects {
                                let e = match eff {
                                    Effect::Calls(ArgSel::Pos(0) | ArgSel::PosOrKw(0, _)) => CALLS,
                                    Effect::StoredThenCalled(ArgSel::Pos(0) | ArgSel::PosOrKw(0, _)) => {
                                        STORED
                                    }
                                    Effect::Property(ArgSel::Pos(0) | ArgSel::PosOrKw(0, _)) => PROPERTY,
                                    _ => continue,
                                };
                                self.apply(st, f, V::Fn(*g, false), e);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Rule 4: a returned closure's own effects and registrations become `Decorates`.
    pub(super) fn decorate(&self, st: &mut State, f: u32, g: u32) {
        for i in 0..self.funcs[g as usize].params.len() {
            let m = st.mask(g, i as u16);
            for e in [CALLS, STORED, WRAPS] {
                if m & e != 0 {
                    st.add_chan(
                        f,
                        Chan::DecoratesEffect {
                            effect: e,
                            param: i as u16,
                        },
                    );
                }
            }
        }
        let caps: Vec<_> = st.cap_reg.get(&g).into_iter().flatten().cloned().collect();
        for (p, k, h, channel, verb) in caps {
            if p == f {
                st.add_chan(
                    f,
                    Chan::DecoratesRegisters {
                        channel,
                        key: k,
                        handler: h,
                        verb,
                    },
                );
            }
        }
    }

    pub(super) fn process_call(&self, st: &mut State, f: u32, call: &Expr) {
        let Expr::Call {
            func, args, kwargs, ..
        } = call
        else {
            return;
        };
        let func: &Expr = func;
        if let Expr::Name { name, .. } = func {
            if name == CONCAT_CALLEE || name == CONTAINER_CALLEE {
                return;
            }
        }
        if self.object_call(st, f, func, args) {
            return;
        }
        self.command_sends(st, f, args);
        if let Expr::Attr { object, attr, .. } = func {
            if self.spec.store_methods.contains(&attr.as_str())
                && !args.is_empty()
                && !self.receiver_defines(st, f, object, attr)
            {
                let objs = self.eval(st, f, object, 0);
                let targets = slots_in(&objs);
                if objs.iter().any(|v| matches!(v, V::Param(..))) {
                    let mut vals = Vals::new();
                    for a in args.iter() {
                        vals.extend(self.eval(st, f, a, 0));
                    }
                    self.param_container_store(st, f, &objs, &vals);
                }
                if !targets.is_empty() {
                    let (key, values) = if args.len() >= 2 {
                        (Some(&args[0]), &args[1..])
                    } else {
                        (None, &args[..])
                    };
                    let mut vals = Vals::new();
                    for a in values {
                        vals.extend(self.eval(st, f, a, 0));
                    }
                    self.store(st, f, &targets, &vals, key);
                }
            }
            if self.spec.element_methods.contains(&attr.as_str()) {
                let recv = self.eval(st, f, object, 0);
                // The elements of a required module's value (`require("./names").forEach`).
                let recv = self.with_module_values(st, &recv, 0);
                for v in &recv {
                    if let V::Param(p, i, How::Direct) = *v {
                        self.apply(st, f, V::Param(p, i, How::Direct), ITERATES);
                    }
                }
                for a in args {
                    for v in self.eval(st, f, a, 0) {
                        if let V::Fn(g, _) = v {
                            let grew = {
                                let entry = st.elem_alias.entry((g, 0)).or_default();
                                let before = entry.len();
                                entry.extend(recv.iter().copied());
                                entry.len() != before
                            };
                            if grew {
                                st.changed = true;
                            }
                        }
                        if matches!(v, V::Fn(..) | V::Param(..) | V::Slot(..)) {
                            self.apply(st, f, v, CALLS);
                        }
                    }
                }
            }
        }
        if let Some(sp) = spelling(func) {
            if !self.locally_bound(f, sp.split('.').next().unwrap_or(&sp)) {
                if let Some(&(_, idx)) = self.spec.store_functions.iter().find(|(n, _)| *n == sp) {
                    if let Some(container) = args.get(idx as usize) {
                        let objs = self.eval(st, f, container, 0);
                        let targets = slots_in(&objs);
                        if !targets.is_empty() {
                            let mut vals = Vals::new();
                            for (j, a) in args.iter().enumerate() {
                                if j != idx as usize {
                                    vals.extend(self.eval(st, f, a, 0));
                                }
                            }
                            self.store(st, f, &targets, &vals, None);
                        }
                    }
                }
                if let Some(&(_, idx)) = self.spec.invoke_functions.iter().find(|(n, _)| *n == sp) {
                    if let Some(a) = args.get(idx as usize) {
                        for v in self.eval(st, f, a, 0) {
                            self.apply(st, f, v, CALLS);
                        }
                    }
                }
                // `Object.defineProperty(dest, name, descriptor)` under a member name of
                // another parameter: a member copy.
                if let Some(&(_, oi, ki)) = self.spec.define_functions.iter().find(|(n, _, _)| *n == sp) {
                    if let (Some(o), Some(k)) = (args.get(oi as usize), args.get(ki as usize)) {
                        let objs = self.eval(st, f, o, 0);
                        let keys = self.eval(st, f, k, 0);
                        let values = args.get(ki as usize + 1).map(|v| self.eval(st, f, v, 0));
                        self.member_copy(st, f, &objs, &keys, values.as_ref());
                    }
                }
            }
        }
        for callee in self.callees(st, f, func, args.len() as u32) {
            match callee {
                Callee::Lib(g, skip) => {
                    self.call_lib(st, f, g, skip, args, kwargs);
                    if st.channels {
                        if let Expr::Attr { object, .. } = func {
                            let bound = self.bind(g, skip, args, kwargs);
                            self.compose_receiver(st, f, g, Some(object), &bound);
                        }
                    }
                }
                Callee::Ctor(c) => {
                    for g in self.ctors(st, c) {
                        self.call_lib(st, f, g, true, args, kwargs);
                    }
                    self.construct_unresolved_bases(st, f, c, func, args, kwargs);
                }
                Callee::Value(v) => self.apply(st, f, v, CALLS),
                Callee::Leaf { effects, symbols } => {
                    self.call_leaf(st, f, func, &effects, &symbols, args, kwargs);
                }
            }
        }
    }

    pub(super) fn call_lib(
        &self,
        st: &mut State,
        f: u32,
        g: u32,
        skip: bool,
        args: &[Expr],
        kwargs: &[(String, Expr)],
    ) {
        st.edges.insert((f, g));
        if g == f {
            return;
        }
        let bound = self.bind(g, skip, args, kwargs);
        self.pass_stored(st, f, g, &bound);
        for (j, a) in &bound {
            self.bind_param_flow(st, f, g, *j, a);
            let m = st.mask(g, *j);
            if m & (CALLS | STORED | ITERATES | PROPERTY) == 0 {
                continue;
            }
            for v in self.eval(st, f, a, 0) {
                for e in [CALLS, STORED, ITERATES, PROPERTY] {
                    if m & e != 0 {
                        self.apply(st, f, v, e);
                    }
                }
            }
        }
        // Methods the callee runs on its parameters run on the arguments.
        for (j, a) in &bound {
            let methods = st.methods_of(g, *j);
            if methods.is_empty() {
                continue;
            }
            for v in self.eval(st, f, a, 0) {
                for m in &methods {
                    self.apply_method(st, f, v, m);
                }
            }
        }
        // Member copies of the callee between arguments that are this function's parameters.
        let copies: Vec<(u16, u16)> = st
            .copies
            .range((g, 0, 0)..)
            .take_while(|(h, _, _)| *h == g)
            .map(|(_, a, b)| (*a, *b))
            .collect();
        for (from, to) in copies {
            let arg = |i: u16| bound.iter().find(|(j, _)| *j == i).map(|(_, e)| *e);
            let (Some(fa), Some(ta)) = (arg(from), arg(to)) else {
                continue;
            };
            let fv = self.eval(st, f, fa, 0);
            let tv = self.eval(st, f, ta, 0);
            self.copy_between(st, f, &fv, &tv);
        }
        self.forward_keywords(st, f, g, skip, &bound, kwargs);
        if st.channels {
            let mut bound = bound;
            bound.extend(self.spread_binding(st, f, g, skip, args, kwargs));
            self.compose_channels(st, f, g, &bound);
        }
    }

    /// Member copies from the parameters among `from` to the parameters (of the same
    /// function) among `to`.
    pub(super) fn copy_between(&self, st: &mut State, f: u32, from: &Vals, to: &Vals) {
        self.link_instances(st, from, to);
        for a in from {
            let V::Param(p, i, How::Direct) = *a else { continue };
            if p != f && !self.is_ancestor(p, f) {
                continue;
            }
            for b in to {
                if let V::Param(q, j, How::Direct) = *b {
                    if q == p {
                        st.add_copy(p, i, j);
                    }
                }
            }
        }
    }

    /// `g(..., **kwargs)` where `kwargs` collects this function's keyword arguments
    /// (`def __init__(self, x, **attrs): super().__init__(x, **attrs)`): every
    /// keyword-capable parameter of `g` the call leaves unbound, and every keyword `g`
    /// forwards itself, may be passed through it - their effects become effects on those
    /// keywords of this function (unless it has a parameter of that name).
    pub(super) fn forward_keywords(
        &self,
        st: &mut State,
        f: u32,
        g: u32,
        skip: bool,
        bound: &[(u16, &Expr)],
        kwargs: &[(String, Expr)],
    ) {
        let Some((_, spread)) = kwargs.iter().find(|(k, _)| k == KEYWORD_SPREAD) else {
            return;
        };
        let own = &self.funcs[f as usize].params;
        let collects = self.eval(st, f, spread, 0).into_iter().any(|v| {
            matches!(v, V::Param(p, i, How::Direct)
                if p == f && own.get(i as usize).is_some_and(|q| q.kind == ParamKind::VarKeyword))
        });
        if !collects {
            return;
        }
        let callee = &self.funcs[g as usize];
        let receiver = skip
            && callee
                .params
                .first()
                .is_some_and(|p| callee.self_param.as_deref() == Some(p.name.as_str()));
        let explicit: HashSet<&str> = kwargs.iter().map(|(k, _)| k.as_str()).collect();
        let mut forwarded: Vec<(String, u8)> = Vec::new();
        for (q, param) in callee.params.iter().enumerate() {
            if (receiver && q == 0) || bound.iter().any(|(j, _)| usize::from(*j) == q) {
                continue;
            }
            let keyword_capable = match param.kind {
                ParamKind::KeywordOnly => true,
                ParamKind::Positional => self.spec.keyword_args,
                ParamKind::VarPositional | ParamKind::VarKeyword => false,
            };
            if !keyword_capable || callee.self_param.as_deref() == Some(param.name.as_str()) {
                continue;
            }
            let m = st.mask(g, q as u16) & (CALLS | STORED | ITERATES | PROPERTY);
            if m != 0 {
                forwarded.push((param.name.clone(), m));
            }
        }
        for ((_, name), m) in st
            .kw_masks
            .range((g, String::new())..)
            .take_while(|((h, _), _)| *h == g)
        {
            if !explicit.contains(name.as_str()) {
                forwarded.push((name.clone(), *m));
            }
        }
        for (name, m) in forwarded {
            if own.iter().any(|p| p.name == name) {
                continue;
            }
            st.add_kw_mask(f, name, m);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn call_leaf(
        &self,
        st: &mut State,
        f: u32,
        func: &Expr,
        effects: &[Effect],
        symbols: &[String],
        args: &[Expr],
        kwargs: &[(String, Expr)],
    ) {
        for eff in effects {
            let (sel, e) = match eff {
                Effect::Calls(s) => (s, CALLS),
                Effect::StoredThenCalled(s) => (s, STORED),
                Effect::Iterates(s) | Effect::Advances(s) => (s, ITERATES),
                Effect::Property(s) => (s, PROPERTY),
                Effect::CallsMethod { arg, method } => {
                    for a in select(arg, func, args, kwargs) {
                        for v in self.eval(st, f, a, 0) {
                            self.apply_method(st, f, v, method);
                        }
                    }
                    continue;
                }
                Effect::CopiesMembers { from, to } => {
                    let mut fv = Vals::new();
                    for a in select(from, func, args, kwargs) {
                        fv.extend(self.eval(st, f, a, 0));
                    }
                    let mut tv = Vals::new();
                    for a in select(to, func, args, kwargs) {
                        tv.extend(self.eval(st, f, a, 0));
                    }
                    self.copy_between(st, f, &fv, &tv);
                    continue;
                }
                _ => continue,
            };
            for a in select(sel, func, args, kwargs) {
                for v in self.eval(st, f, a, 0) {
                    self.apply(st, f, v, e);
                }
            }
        }
        let matches = |row: &ChannelRow| row.symbol.as_ref().is_some_and(|s| symbols.contains(s));
        for row in self.io_entry.iter().filter(|r| matches(r)) {
            let Some(handler) = &row.handler else { continue };
            for a in select(handler, func, args, kwargs) {
                for v in self.eval(st, f, a, 0) {
                    let entry = match v {
                        V::Fn(g, _) => Some(g),
                        V::Inst(c) => {
                            for g in self.instance_fns(st, c) {
                                if st.entries.insert(g, row.channel_or_default()).is_none() {
                                    st.changed = true;
                                }
                            }
                            self.spec.call_method.and_then(|cm| self.find_method(st, c, cm))
                        }
                        _ => None,
                    };
                    if let Some(g) = entry {
                        if st.entries.insert(g, row.channel_or_default()).is_none() {
                            st.changed = true;
                        }
                    }
                }
            }
        }
        if !st.channels {
            return;
        }
        for row in self.io_send.iter().filter(|r| matches(r)) {
            let Some(key) = &row.key else { continue };
            let verb = match &row.verb {
                Some(VerbSel::Const(s)) => Verb::Const(st.lit(s)),
                Some(VerbSel::Arg(sel)) => {
                    let mut verb = Verb::Any;
                    for a in select(sel, func, args, kwargs) {
                        for v in self.eval(st, f, a, 0) {
                            match v {
                                V::Lit(l) => verb = Verb::Const(l),
                                V::Param(p, i, _) if p == f => verb = Verb::Param(i),
                                _ => {}
                            }
                        }
                    }
                    verb
                }
                Some(VerbSel::Any) | None => Verb::Any,
            };
            self.leaf_sends(st, f, func, args, kwargs, key, row.channel_or_default(), verb);
        }
        // A dynamic symbol lookup (`dlsym`, a function pointer built from a name) uses the
        // name (as a direct call of the row does in the bridge stage).
        for row in self.ffi.iter().filter(|r| matches(r)) {
            let Some(key) = &row.key else { continue };
            self.leaf_sends(st, f, func, args, kwargs, key, Channel::Ffi, Verb::Any);
        }
    }

    /// The key argument of a primitive row at a leaf call: this function's parameters are
    /// sent under `channel`; slot contents are sent fields.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn leaf_sends(
        &self,
        st: &mut State,
        f: u32,
        func: &Expr,
        args: &[Expr],
        kwargs: &[(String, Expr)],
        key: &ArgSel,
        channel: Channel,
        verb: Verb,
    ) {
        for a in select(key, func, args, kwargs) {
            let vals = self.eval(st, f, a, 0);
            for &v in &vals {
                if let V::Param(p, i, _) = v {
                    if p == f {
                        st.add_chan(
                            f,
                            Chan::Sends {
                                channel,
                                key: i,
                                verb: verb.clone(),
                            },
                        );
                    }
                }
            }
            self.send_slots(st, &vals, channel, &verb);
        }
    }

    /// The verbs of a registration composed at a call: the callee's verb mapped to the call
    /// ([`Program::map_verb`]); an HTTP registration without one takes the HTTP method tokens
    /// among the call's literal arguments (`methods=["GET"]`, a verb constant), one each.
    pub(super) fn verbs_at(
        &self,
        st: &mut State,
        f: u32,
        channel: Channel,
        verb: &Verb,
        bound: &[(u16, &Expr)],
    ) -> Vec<Verb> {
        let mapped = self.map_verb(st, f, verb, bound);
        if mapped != Verb::Any || channel != Channel::Http {
            return vec![mapped];
        }
        let mut tokens: BTreeSet<&'static str> = BTreeSet::new();
        for (_, a) in bound {
            for v in self.eval(st, f, a, 0) {
                if let V::Lit(l) = v {
                    if let Some(t) = st
                        .lits
                        .get(l as usize)
                        .and_then(|text| crate::channels::http_method_token(text))
                    {
                        tokens.insert(t);
                    }
                }
            }
        }
        if tokens.is_empty() {
            return vec![Verb::Any];
        }
        tokens.into_iter().map(|t| Verb::Const(st.lit(t))).collect()
    }

    pub(super) fn map_verb(&self, st: &mut State, f: u32, verb: &Verb, bound: &[(u16, &Expr)]) -> Verb {
        match verb {
            Verb::Any => Verb::Any,
            Verb::Const(l) => Verb::Const(*l),
            Verb::Param(j) => {
                let Some((_, a)) = bound.iter().find(|(k, _)| k == j) else {
                    return Verb::Any;
                };
                let vals = self.eval(st, f, a, 0);
                if let Some(V::Lit(l)) = vals.iter().find(|v| matches!(v, V::Lit(_))) {
                    return Verb::Const(*l);
                }
                if let Some(V::Param(_, i, _)) =
                    vals.iter().find(|v| matches!(v, V::Param(p, _, _) if *p == f))
                {
                    return Verb::Param(*i);
                }
                Verb::Any
            }
        }
    }

    /// Own prefix (rule 3, receiver target): a registration key built from an attribute of the
    /// registering object (`self.prefix + path`) whose content is a parameter of a function
    /// of that object's class (the constructor's `prefix`): that function gets
    /// `Mounts { key, target: Receiver }`.
    pub(super) fn own_prefix(&self, st: &mut State, f: u32, raw_keys: &Vals) {
        let Some(class) = self.funcs[f as usize].class else {
            return;
        };
        // A prefix is concatenated with the registered path; an attribute that is the whole key
        // (`self.add_route(self.openapi_url, ...)`) is a registration, not a prefix.
        if !raw_keys
            .iter()
            .any(|v| matches!(v, V::Param(p, _, How::Built) if *p == f))
        {
            return;
        }
        let related: Vec<u32> = std::iter::once(class)
            .chain(self.related.get(class as usize).into_iter().flatten().copied())
            .collect();
        for s in slots_in_any(raw_keys) {
            let SlotOwner::Class(c) = st.slot_keys[s as usize].owner else { continue };
            if !related.contains(&c) {
                continue;
            }
            let stored: Vec<(u32, u16)> = st
                .stored
                .iter()
                .filter_map(|&(s2, g, v)| match v {
                    V::Param(p, i, How::Direct) if s2 == s && p == g => Some((g, i)),
                    _ => None,
                })
                .collect();
            for (g, i) in stored {
                if self.funcs[g as usize].class.is_some_and(|gc| related.contains(&gc)) {
                    st.add_chan(g, Chan::MountsSelf { key: i });
                }
            }
        }
    }

    /// `vals` plus, for every slot among them, the parameters `f` itself stored into that slot
    /// (a value `f` writes into an object and passes on read back: `route.endpoint =
    /// endpoint; build(call=route.endpoint)`).
    pub(super) fn through_own_stores(&self, st: &State, f: u32, vals: &Vals) -> Vals {
        let mut out = vals.clone();
        let slots = slots_in(vals);
        if slots.is_empty() {
            return out;
        }
        for &(s, g, v) in &st.stored {
            if g == f
                && slots.contains(&s)
                && matches!(v, V::Param(p, _, How::Direct | How::Wrapped) if p == f)
            {
                out.insert(v);
            }
        }
        out
    }
}
