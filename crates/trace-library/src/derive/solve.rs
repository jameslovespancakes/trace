//! Fixed point of a derivation (child of `derive`): channel composition from callees to
//! callers, propagation, class-slot relationships, dispatch from entry rows and the bounded
//! rounds of [`Program::solve`].

use super::*;

impl<'c> Program<'c> {
    /// Channel facts of callee `g` seen from its caller `f` (rules 1-3, 7 composition).
    pub(super) fn compose_channels(&self, st: &mut State, f: u32, g: u32, bound: &[(u16, &Expr)]) {
        let Some(facts) = st.chan.get(&g).cloned() else {
            return;
        };
        let arg_of = |j: u16| bound.iter().find(|(k, _)| *k == j).map(|(_, e)| *e);
        let own_params = |vals: &Vals| -> Vec<u16> {
            vals.iter()
                .filter_map(|v| match v {
                    V::Param(p, i, _) if *p == f => Some(*i),
                    _ => None,
                })
                .collect()
        };
        for fact in facts {
            match fact {
                Chan::Sends { channel, key, verb } => {
                    let Some(a) = arg_of(key) else { continue };
                    let vals = self.eval(st, f, a, 0);
                    let verb = self.map_verb(st, f, &verb, bound);
                    self.send_slots(st, &vals, channel, &verb);
                    for i in own_params(&vals) {
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
                Chan::Registers {
                    channel,
                    key,
                    handler,
                    verb,
                } => {
                    let (Some(ka), Some(ha)) = (arg_of(key), arg_of(handler)) else {
                        continue;
                    };
                    let raw_hands = self.eval(st, f, ha, 0);
                    let hands = self.through_own_stores(st, f, &raw_hands);
                    let raw_keys = self.eval(st, f, ka, 0);
                    self.own_prefix(st, f, &raw_keys);
                    let mut keys = self.through_own_stores(st, f, &raw_keys);
                    // A key that is no parameter of `f` (a value computed from one, stored in the
                    // entry object): the other parameter `f` stores into the handler's owner
                    // (the rule of `registers_at_store`).
                    if !keys
                        .iter()
                        .any(|v| matches!(v, V::Param(p, _, _) if *p == f || self.is_ancestor(*p, f)))
                    {
                        let handler_params: BTreeSet<u16> = hands
                            .iter()
                            .filter_map(|v| match v {
                                V::Param(p, h, _) if *p == f => Some(*h),
                                _ => None,
                            })
                            .collect();
                        let owners: BTreeSet<SlotOwner> = slots_in(&raw_hands)
                            .into_iter()
                            .map(|s| st.slot_keys[s as usize].owner)
                            .collect();
                        if let Some(k) = st
                            .owner_stores
                            .iter()
                            .filter(|(o, g, i)| owners.contains(o) && *g == f && !handler_params.contains(i))
                            .map(|(_, _, i)| *i)
                            .min()
                        {
                            keys.insert(V::Param(f, k, How::Direct));
                        }
                    }
                    let verbs = self.verbs_at(st, f, channel, &verb, bound);
                    for verb in &verbs {
                        for kv in &keys {
                            let V::Param(pk, k, _) = *kv else { continue };
                            if self.rest_key(pk, k) {
                                continue;
                            }
                            for hv in &hands {
                                match *hv {
                                    V::Param(ph, h, How::Direct | How::Wrapped)
                                        if ph == f && pk == f && h != k =>
                                    {
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
                                    // A nested function registering its enclosing function's
                                    // parameters (`fns.forEach(fn => router.use(path, fn))`).
                                    V::Param(ph, h, How::Direct | How::Wrapped)
                                        if ph != f
                                            && pk == ph
                                            && h != k
                                            && self.spec.objects.is_some()
                                            && self.is_ancestor(ph, f) =>
                                    {
                                        st.add_chan(
                                            ph,
                                            Chan::Registers {
                                                channel,
                                                key: k,
                                                handler: h,
                                                verb: verb.clone(),
                                            },
                                        );
                                    }
                                    V::Param(ph, h, How::Direct | How::Wrapped)
                                        if ph == f && pk != f && self.is_ancestor(pk, f) =>
                                    {
                                        if st.cap_reg.entry(f).or_default().insert((
                                            pk,
                                            k,
                                            h,
                                            channel,
                                            verb.clone(),
                                        )) {
                                            st.changed = true;
                                        }
                                    }
                                    V::ParamAttr(pt, t) if pt == f && pk == f && t != k => {
                                        st.add_chan(f, Chan::Mounts { key: k, target: t });
                                    }
                                    V::Fn(c, _) if pk == f => {
                                        let caps: Vec<_> =
                                            st.captured.get(&c).into_iter().flatten().copied().collect();
                                        for (p, i, how, ce) in caps {
                                            if p == f
                                                && how != How::Built
                                                && matches!(ce, CALLS | STORED)
                                                && i != k
                                            {
                                                st.add_chan(
                                                    f,
                                                    Chan::Registers {
                                                        channel,
                                                        key: k,
                                                        handler: i,
                                                        verb: verb.clone(),
                                                    },
                                                );
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                Chan::Mounts { key, target } => {
                    let (Some(ka), Some(ta)) = (arg_of(key), arg_of(target)) else {
                        continue;
                    };
                    let kv = self.eval(st, f, ka, 0);
                    let tv = self.eval(st, f, ta, 0);
                    for k in own_params(&kv) {
                        for t in own_params(&tv) {
                            if k != t {
                                st.add_chan(f, Chan::Mounts { key: k, target: t });
                            }
                        }
                    }
                }
                Chan::MountsSelf { key } => {
                    // A subclass constructor passing its own parameter up
                    // (`super().__init__(prefix=prefix)`): the same object's own prefix.
                    let same_object = match (self.funcs[f as usize].class, self.funcs[g as usize].class) {
                        (Some(fc), Some(gc)) => {
                            fc == gc || self.related.get(fc as usize).is_some_and(|r| r.contains(&gc))
                        }
                        _ => false,
                    };
                    if !same_object {
                        continue;
                    }
                    let Some(a) = arg_of(key) else { continue };
                    let vals = self.eval(st, f, a, 0);
                    for i in own_params(&vals) {
                        st.add_chan(f, Chan::MountsSelf { key: i });
                    }
                }
                Chan::DecoratesEffect { .. }
                | Chan::DecoratesRegisters { .. }
                | Chan::RegistersSelf { .. } => {}
            }
        }
    }

    /// `return factory(key)`: the decorator the callee returns becomes this function's.
    pub(super) fn returned_call_channels(&self, st: &mut State, f: u32, r: &Expr) {
        let Expr::Call {
            func, args, kwargs, ..
        } = r
        else {
            return;
        };
        for callee in self.callees(st, f, func, args.len() as u32) {
            let Callee::Lib(g, skip) = callee else { continue };
            let Some(facts) = st.chan.get(&g).cloned() else { continue };
            let bound = self.bind(g, skip, args, kwargs);
            for fact in facts {
                match fact {
                    Chan::DecoratesRegisters {
                        channel,
                        key,
                        handler,
                        verb,
                    } => {
                        let Some((_, a)) = bound.iter().find(|(k, _)| *k == key) else {
                            continue;
                        };
                        let vals = self.eval(st, f, a, 0);
                        let verbs = self.verbs_at(st, f, channel, &verb, &bound);
                        for v in vals {
                            if let V::Param(p, k, _) = v {
                                if p == f {
                                    for verb in &verbs {
                                        st.add_chan(
                                            f,
                                            Chan::DecoratesRegisters {
                                                channel,
                                                key: k,
                                                handler,
                                                verb: verb.clone(),
                                            },
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Chan::DecoratesEffect { .. } => st.add_chan(f, fact),
                    _ => {}
                }
            }
        }
    }

    /// Stored contents suffer their slot's effects; content flowing between slots shares
    /// effects and callers.
    pub(super) fn propagate(&self, st: &mut State) {
        let stored: Vec<(u32, u32, V)> = st.stored.iter().copied().collect();
        for (s, f, v) in stored {
            let m = st.slot_mask[s as usize];
            for e in [CALLS, WRAPS, PROPERTY] {
                if m & e != 0 {
                    let e2 = if e == CALLS { STORED } else { e };
                    self.apply(st, f, v, e2);
                }
            }
            if let V::Fn(g, _) = v {
                let callers: Vec<u32> = st.slot_callers[s as usize].iter().copied().collect();
                for c in callers {
                    st.edges.insert((c, g));
                }
            }
            // Methods called on the slot's content run on the stored parameter.
            if let V::Param(p, i, How::Direct | How::Wrapped) = v {
                let methods: Vec<String> = st.slot_methods.get(&s).into_iter().flatten().cloned().collect();
                for m in methods {
                    st.add_pmethod(p, i, &m);
                }
            }
        }
        self.relate_class_slots(st);
        let flows: Vec<(u32, u32)> = st.slot_flow.iter().copied().collect();
        for (from, to) in flows {
            let m = st.slot_mask[to as usize];
            st.add_slot_mask(from, m);
            let callers: Vec<u32> = st.slot_callers[to as usize].iter().copied().collect();
            for c in callers {
                st.add_caller(from, c);
            }
            // Content flows forward (typed instances), uses flow back (called methods).
            for c in st.insts_of(from) {
                if st.slot_insts.entry(to).or_default().insert(c) {
                    st.changed = true;
                }
            }
            for v in st.callables_of(from) {
                if st.slot_callables.entry(to).or_default().insert(v) {
                    st.changed = true;
                }
            }
            let methods: Vec<String> = st.slot_methods.get(&to).into_iter().flatten().cloned().collect();
            for m in methods {
                st.add_slot_method(from, &m);
            }
        }
    }

    /// Slots of one attribute name owned by related classes (one object can be an instance
    /// of both) share content: flows both ways. Unrelated classes (siblings without a common
    /// subclass) keep their attributes apart.
    pub(super) fn relate_class_slots(&self, st: &mut State) {
        let mut by_name: BTreeMap<&str, Vec<(u32, u32)>> = BTreeMap::new();
        for (s, key) in st.slot_keys.iter().enumerate() {
            if let SlotOwner::Class(c) = key.owner {
                by_name.entry(key.name.as_str()).or_default().push((c, s as u32));
            }
        }
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        for slots in by_name.values() {
            if slots.len() < 2 || slots.len() > self.cx.limits.max_related_slots {
                continue;
            }
            for &(c1, s1) in slots {
                let related = self.related.get(c1 as usize);
                for &(c2, s2) in slots {
                    if s1 != s2 && related.is_some_and(|r| r.binary_search(&c2).is_ok()) {
                        pairs.push((s1, s2));
                    }
                }
            }
        }
        for pair in pairs {
            if st.slot_flow.insert(pair) {
                st.changed = true;
            }
        }
    }

    /// Rule 2: entries (protocol patterns / symbols) -> reachable functions -> slots whose
    /// content they call are dispatch registries.
    pub(super) fn compute_dispatch(&self, st: &mut State) {
        let mut adjacency: HashMap<u32, Vec<u32>> = HashMap::new();
        for &(a, b) in &st.edges {
            adjacency.entry(a).or_default().push(b);
        }
        let mut reach: HashMap<u32, Channel> = HashMap::new();
        let mut queue: VecDeque<(u32, Channel)> = st.entries.iter().map(|(f, c)| (*f, *c)).collect();
        let mut visits = 0usize;
        while let Some((f, channel)) = queue.pop_front() {
            visits += 1;
            if visits > 100_000 || reach.contains_key(&f) {
                continue;
            }
            reach.insert(f, channel);
            for &g in adjacency.get(&f).into_iter().flatten() {
                queue.push_back((g, channel));
            }
        }
        for s in 0..st.slot_callers.len() {
            if let Some(ch) = st.slot_callers[s].iter().find_map(|c| reach.get(c).copied()) {
                st.dispatched.insert(s as u32, ch);
            }
        }
        let method_slots: Vec<(u32, Channel)> = st
            .slot_method_callers
            .iter()
            .filter_map(|(&s, callers)| callers.iter().find_map(|c| reach.get(c).copied()).map(|ch| (s, ch)))
            .collect();
        st.method_dispatched.extend(method_slots);
        for _ in 0..16 {
            let mut grew = false;
            for &(from, to) in &st.slot_flow {
                if let Some(&ch) = st.dispatched.get(&to) {
                    if let std::collections::hash_map::Entry::Vacant(e) = st.dispatched.entry(from) {
                        e.insert(ch);
                        grew = true;
                    }
                }
            }
            if !grew {
                break;
            }
        }
        let owners: Vec<u32> = st
            .dispatched
            .keys()
            .chain(st.method_dispatched.keys())
            .filter_map(|s| match st.slot_keys.get(*s as usize).map(|k| k.owner) {
                Some(SlotOwner::Class(c)) => Some(c),
                _ => None,
            })
            .collect();
        st.registry_classes.extend(owners);
        self.compute_table_owners(st);
        self.typed_dispatch(st, &reach);
    }

    pub(super) fn mark_pattern_entries(&self, st: &mut State) {
        for row in &self.io_entry {
            if row.handler.is_some() {
                continue;
            }
            if let Some((name, arity)) = row.entry_pattern() {
                for (f, func) in self.funcs.iter().enumerate() {
                    if func.name != name || func.class.is_none() {
                        continue;
                    }
                    let receiver = usize::from(
                        func.params
                            .first()
                            .is_some_and(|p| func.self_param.as_deref() == Some(p.name.as_str())),
                    );
                    let count = func.params.len().saturating_sub(receiver) as u32;
                    if arity.is_none_or(|a| a == count) {
                        st.entries.insert(f as u32, row.channel_or_default());
                    }
                }
            } else if let Some(symbol) = &row.symbol {
                for f in 0..self.funcs.len() {
                    if !self.funcs[f].is_module && &self.func_symbol(f as u32) == symbol {
                        st.entries.insert(f as u32, row.channel_or_default());
                    }
                }
            }
        }
    }

    pub(super) fn solve(&self, st: &mut State) {
        self.mark_pattern_entries(st);
        for _ in 0..self.cx.limits.max_rounds {
            st.changed = false;
            for f in 0..self.funcs.len() {
                self.process_function(st, f as u32);
            }
            self.propagate(st);
            if !st.changed {
                break;
            }
        }
        self.compute_dispatch(st);
        st.channels = true;
        for _ in 0..self.cx.limits.max_rounds {
            st.changed = false;
            for f in 0..self.funcs.len() {
                self.process_function(st, f as u32);
            }
            self.propagate(st);
            self.propagate_slot_sends(st);
            if !st.changed {
                break;
            }
        }
    }
}
