//! Member lookup of the evaluator: cells, class members along the MRO, per-allocation
//! attributes, delegates and `super` (child of [`crate::flow`]).

use super::*;

impl<'s> Ev<'s, '_> {
    /// A cell's values in this view; `None` when empty.
    fn cell_ref(&self, id: u32) -> Option<SlotRef<'s>> {
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

    /// Clock of the last change of an attribute cell of `kind` named `attr`
    /// ([`Dep::AttrClock`]).
    fn attr_clock(&self, kind: u8, attr: Name) -> u32 {
        self.st
            .ids
            .get(&Dep::AttrClock(kind, attr))
            .map_or(0, |&i| self.st.cells[i as usize].changed_at)
    }

    /// A memo hit: record the coarse read (and a saturated result cell).
    fn memo_read(&self, attr: Name, id: Option<u32>) {
        self.note(Dep::Attr(attr));
        if let (Some(rec), Some(i)) = (self.rec, id) {
            if self.st.cells[i as usize].saturated {
                rec.borrow_mut().saturated = true;
            }
        }
    }

    /// Non-empty class-wide attribute cells (`ObjAttr(Any(k), attr)`) along the MRO.
    pub(super) fn any_slots(&self, class: SymbolId, attr: Name) -> Box<[u32]> {
        let key = (class, attr, self.view == View::Full);
        if let Some(memo) = self.memo {
            let memo = memo.borrow();
            if let Some((at, ids)) = memo.any_slots.get(&key) {
                if self.attr_clock(2, attr) < *at {
                    self.note(Dep::Attr(attr));
                    if let Some(rec) = self.rec {
                        if ids.iter().any(|&i| self.st.cells[i as usize].saturated) {
                            rec.borrow_mut().saturated = true;
                        }
                    }
                    return ids.clone();
                }
            }
        }
        self.note(Dep::Attr(attr));
        let ids: Box<[u32]> = match self.st.attr_classes.get(&(2, attr)) {
            None => Box::new([]),
            Some(classes) => self
                .f
                .mro(class)
                .iter()
                .filter(|k| classes.contains(k))
                .filter_map(|&k| self.note(Dep::Slot(Slot::ObjAttr(Obj::any(k), attr))))
                .filter(|&id| self.cell_ref(id).is_some())
                .collect(),
        };
        if let Some(memo) = self.memo {
            memo.borrow_mut().any_slots.insert(key, (self.st.clock, ids.clone()));
        }
        ids
    }

    /// First member slot or method named `attr` along the MRO of `class`.
    pub(super) fn first_member(&self, class: SymbolId, attr: Name) -> Option<Result<SlotRef<'s>, SymbolId>> {
        let key = (class, attr, self.view == View::Full);
        if let Some(memo) = self.memo {
            let hit = memo.borrow().member.get(&key).copied();
            if let Some((at, found)) = hit {
                if self.attr_clock(0, attr) < at {
                    self.memo_read(attr, found.and_then(Result::ok));
                    return found.map(|r| r.map(|id| self.cell_ref(id).expect("non-empty memo cell")));
                }
            }
        }
        // No class can ever define `attr` as a member: nothing to read or record.
        let defs = self.f.definers.get(&attr)?;
        let text = self.f.names.text(attr);
        let mut found = None;
        for &k in self.f.mro(class) {
            if !defs.contains(&k) {
                continue;
            }
            if let Some(id) = self.note(Dep::Slot(Slot::Member(k, attr))) {
                if self.cell_ref(id).is_some() {
                    found = Some(Ok(id));
                    break;
                }
            }
            if let Some(m) = self.f.hierarchy.method(k, text) {
                found = Some(Err(m));
                break;
            }
        }
        if let Some(memo) = self.memo {
            memo.borrow_mut().member.insert(key, (self.st.clock, found));
        }
        found.map(|r| r.map(|id| self.cell_ref(id).expect("non-empty cell")))
    }

    fn class_attr_slot(&self, class: SymbolId, attr: Name) -> Option<SlotRef<'s>> {
        let key = (class, attr, self.view == View::Full);
        if let Some(memo) = self.memo {
            let hit = memo.borrow().class_slot.get(&key).copied();
            if let Some((at, found)) = hit {
                if self.attr_clock(1, attr) < at {
                    self.memo_read(attr, found);
                    return found.and_then(|id| self.cell_ref(id));
                }
            }
        }
        self.note(Dep::Attr(attr));
        let found = self.st.attr_classes.get(&(1, attr)).and_then(|classes| {
            self.f
                .mro(class)
                .iter()
                .filter(|k| classes.contains(k))
                .find_map(|&k| {
                    self.note(Dep::Slot(Slot::ClassAttr(k, attr)))
                        .filter(|&id| self.cell_ref(id).is_some())
                })
        });
        if let Some(memo) = self.memo {
            memo.borrow_mut().class_slot.insert(key, (self.st.clock, found));
        }
        found.and_then(|id| self.cell_ref(id))
    }

    /// Raw member values (descriptor objects, properties) of `class`.
    pub(super) fn raw_member(&self, class: SymbolId, attr: Name) -> Option<SlotRef<'s>> {
        match self.first_member(class, attr) {
            Some(Ok(v)) => Some(v),
            Some(Err(_)) => None,
            None => self.class_attr_slot(class, attr),
        }
    }

    pub(super) fn attr_values(&self, objs: &Vals, attr: Name, eff: &mut Eff) -> Vals {
        let mut seen: Vec<Holder> = Vec::new();
        self.attr_values_in(objs, attr, eff, &mut seen)
    }

    /// [`Ev::attr_values`] with the holders whose delegates this lookup already walked
    /// (results are unioned, so a holder's delegates are walked once).
    fn attr_values_in(&self, objs: &Vals, attr: Name, eff: &mut Eff, seen: &mut Vec<Holder>) -> Vals {
        let mut out = Vals::new();
        // Class-level lookups (MRO walks) are shared by every allocation of one class.
        let mut classes: Vec<(SymbolId, ClassLookup<'s>)> = Vec::new();
        for v in objs {
            match *v {
                Value::Class(c) if self.is_prototype(c, attr) => {
                    // `C.prototype` (JavaScript): the instance side of the class, whose members
                    // every instance sees.
                    out.insert(Value::Instance(Obj::any(c)));
                }
                Value::Class(c) => out.extend(self.class_attr(c, attr)),
                Value::Instance(o) => {
                    let i = match classes.iter().position(|(k, _)| *k == o.class) {
                        Some(i) => i,
                        None => {
                            classes.push((o.class, self.class_lookup(o.class, attr)));
                            classes.len() - 1
                        }
                    };
                    let found = self.instance_attr_with(o, attr, &mut classes[i].1, eff);
                    if found.is_empty() {
                        out.extend(self.delegated(Holder::Inst(o), attr, eff, seen));
                    } else {
                        out.extend(found);
                    }
                }
                Value::Object(a) => out.extend(self.holder_attr(Holder::Obj(a), attr, eff, seen)),
                Value::Function(f) => out.extend(self.holder_attr(Holder::Fn(f), attr, eff, seen)),
                Value::Library(l) => {
                    // A member repository code never stored is the library's own: reading it
                    // gives the same library object.
                    let found = self.holder_attr(Holder::Lib(l), attr, eff, seen);
                    if found.is_empty() {
                        out.insert(Value::Library(l));
                    } else {
                        out.extend(found);
                    }
                }
                Value::Bound(..) | Value::Generator(_) | Value::Property(_) => {}
            }
        }
        out
    }

    /// Own property `attr` of a holder, else the member found through its delegates.
    pub(super) fn holder_attr(&self, h: Holder, attr: Name, eff: &mut Eff, seen: &mut Vec<Holder>) -> Vals {
        if let Some(v) = self.slot(Slot::Prop(h, attr)) {
            return v.to_vals();
        }
        self.delegated(h, attr, eff, seen)
    }

    /// Member `attr` found through the delegates of `h` (each holder once per lookup, at most
    /// `flow.max_delegate_visits`: beyond that the lookup is marked bounded).
    fn delegated(&self, h: Holder, attr: Name, eff: &mut Eff, seen: &mut Vec<Holder>) -> Vals {
        if !self.f.delegation || seen.contains(&h) {
            return Vals::new();
        }
        let Some(delegates) = self.slot(Slot::Delegates(h)) else {
            return Vals::new();
        };
        if seen.len() >= self.f.settings.max_delegate_visits {
            self.mark_bounded();
            return Vals::new();
        }
        seen.push(h);
        let delegates = delegates.to_vals();
        self.attr_values_in(&delegates, attr, eff, seen)
    }

    /// Candidates computed by this evaluation may be incomplete (a bound was reached).
    fn mark_bounded(&self) {
        if let Some(rec) = self.rec {
            rec.borrow_mut().saturated = true;
        }
    }

    /// Whether `attr` of class `c` is the `prototype` member of the class's language
    /// (language rule).
    fn is_prototype(&self, c: SymbolId, attr: Name) -> bool {
        self.f.prototypes[self.f.index.symbol(c).language as usize].prototype == Some(attr)
    }

    fn class_attr(&self, c: SymbolId, attr: Name) -> Vals {
        match self.first_member(c, attr) {
            Some(Ok(vals)) => vals
                .iter()
                .map(|&v| match v {
                    Value::Function(f) => self.method_value(f, Recv::Class(c), true),
                    other => other,
                })
                .collect(),
            Some(Err(m)) => Vals::one(self.method_value(m, Recv::Class(c), true)),
            None => self
                .class_attr_slot(c, attr)
                .map(SlotRef::to_vals)
                .unwrap_or_default(),
        }
    }

    /// The per-class part of an instance attribute lookup: the first member along the MRO
    /// and the class-wide (`Any` allocation) attribute slots of the MRO.
    fn class_lookup(&self, class: SymbolId, attr: Name) -> ClassLookup<'s> {
        let member = self.first_member(class, attr);
        let mut any = Vals::new();
        if member.is_none() {
            for id in self.any_slots(class, attr).iter() {
                if let Some(v) = self.cell_ref(*id) {
                    any.extend(v.iter());
                }
            }
        }
        ClassLookup {
            member,
            any,
            class_slot: None,
        }
    }

    fn instance_attr_with(&self, o: Obj, attr: Name, lookup: &mut ClassLookup<'s>, eff: &mut Eff) -> Vals {
        let recv = Recv::Instance(o);
        match lookup.member {
            Some(Ok(vals)) => {
                let mut out = Vals::new();
                for &v in vals.iter() {
                    match v {
                        Value::Function(f) => {
                            out.insert(self.method_value(f, recv, false));
                        }
                        Value::Property(g) => out.extend(self.invoke(g, recv, &[], &[], eff)),
                        other => {
                            out.insert(other);
                        }
                    }
                }
                return out;
            }
            Some(Err(m)) => return Vals::one(self.method_value(m, recv, false)),
            None => {}
        }
        let mut out = Vals::new();
        if let Some(v) = self.slot(Slot::ObjAttr(o, attr)) {
            out.extend(v.iter());
        }
        out.extend(lookup.any.iter());
        if o.alloc == Alloc::Any {
            for other in self.attr_objs(o.class, attr) {
                if let Some(v) = self.slot(Slot::ObjAttr(other, attr)) {
                    out.extend(v.iter());
                }
            }
        }
        if out.is_empty() {
            let slot = lookup
                .class_slot
                .get_or_insert_with(|| self.class_attr_slot(o.class, attr));
            if let Some(v) = slot {
                out.extend(v.iter());
            }
        }
        out
    }

    /// Function values of a member looked up by name (first hit in the MRO).
    pub(super) fn member_functions(&self, class: SymbolId, text: &str) -> Vec<SymbolId> {
        let name = self.f.names.get(text);
        let defs = name.and_then(|n| self.f.definers.get(&n));
        for &k in self.f.mro(class) {
            if let Some(n) = name.filter(|_| defs.is_some_and(|d| d.contains(&k))) {
                if let Some(v) = self.slot(Slot::Member(k, n)) {
                    return function_ids(v.iter()).into_iter().collect();
                }
            }
            if let Some(m) = self.f.hierarchy.method(k, text) {
                return vec![m];
            }
        }
        Vec::new()
    }

    /// The method and its declaring class when `object` denotes that method's `super`:
    /// Python `super()` / `super(C, self)` (language rule) or the language's super
    /// keyword used as a receiver ([`super_keyword`]: `super.m()`, C# `base.m()`, PHP
    /// `parent::m()`), inside a method with a receiver parameter.
    pub(super) fn super_class(&self, object: &Node, sc: Sc) -> Option<(SymbolId, SymbolId)> {
        let ScopeKey::Symbol(f) = sc.scope else {
            return None;
        };
        let sp = self.f.selfs.get(&f)?;
        let is_super = match object {
            Node::Call(c) => c.is_super(),
            Node::Name {
                name, target: None, ..
            } => super_keyword(self.f.index.symbol(f).language) == Some(self.f.names.text(*name)),
            _ => false,
        };
        is_super.then_some((f, sp.class))
    }

    /// First member slot or method named `attr` along the MRO of `class` after `after`
    /// (the declaring class of the method calling `super`); when `after` is not in that MRO,
    /// along `after`'s own bases.
    fn member_after(
        &self,
        class: SymbolId,
        after: SymbolId,
        attr: Name,
    ) -> Option<Result<SlotRef<'s>, SymbolId>> {
        let mro = self.f.mro(class);
        let rest: &[SymbolId] = match mro.iter().position(|&k| k == after) {
            Some(i) => &mro[i + 1..],
            None => self.f.mro(after).get(1..).unwrap_or(&[]),
        };
        let text = self.f.names.text(attr);
        let defs = self.f.definers.get(&attr);
        for &k in rest {
            if defs.is_some_and(|d| d.contains(&k)) {
                if let Some(id) = self.note(Dep::Slot(Slot::Member(k, attr))) {
                    if let Some(r) = self.cell_ref(id) {
                        return Some(Ok(r));
                    }
                }
            }
            if let Some(m) = self.f.hierarchy.method(k, text) {
                return Some(Err(m));
            }
        }
        None
    }

    /// Member `attr` of `super` for the receiver values `objs` (instances: bound methods,
    /// property getters run; classes: class-level lookup as in [`Ev::class_attr`]).
    pub(super) fn super_attr(&self, objs: &Vals, after: SymbolId, attr: Name, eff: &mut Eff) -> Vals {
        let mut out = Vals::new();
        for v in objs {
            let (class, recv, via_class) = match *v {
                Value::Instance(o) => (o.class, Recv::Instance(o), false),
                Value::Class(c) => (c, Recv::Class(c), true),
                _ => continue,
            };
            match self.member_after(class, after, attr) {
                Some(Ok(vals)) => {
                    for &x in vals.iter() {
                        match x {
                            Value::Function(f) => {
                                out.insert(self.method_value(f, recv, via_class));
                            }
                            Value::Property(g) if !via_class => {
                                out.extend(self.invoke(g, recv, &[], &[], eff));
                            }
                            other => {
                                out.insert(other);
                            }
                        }
                    }
                }
                Some(Err(m)) => {
                    out.insert(self.method_value(m, recv, via_class));
                }
                None => {}
            }
        }
        out
    }

    /// Constructors run when `class` is called.
    pub(super) fn constructors(&self, class: SymbolId) -> Vec<SymbolId> {
        match constructor_name(self.f.index.symbol(class).language) {
            Some(name) => self.member_functions(class, name),
            None => self
                .f
                .mro(class)
                .iter()
                .map(|&k| self.f.hierarchy.declared_constructors(k))
                .find(|c| !c.is_empty())
                .map(<[SymbolId]>::to_vec)
                .unwrap_or_default(),
        }
    }

    /// `__call__` implementations of instances of `class` (Python data model only).
    pub(super) fn call_methods(&self, class: SymbolId) -> Vec<SymbolId> {
        match rules(self.f.index.symbol(class).language).instance_call_method {
            Some(method) => self.member_functions(class, method),
            None => Vec::new(),
        }
    }
}
