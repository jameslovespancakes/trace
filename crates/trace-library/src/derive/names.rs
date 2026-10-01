//! Names and classes of a derivation (child of `derive`): class lineage, declared and open
//! types, module-level name resolution through imports and namespace groups, enclosing
//! scopes, methods, overrides and constructors.

use super::*;

impl<'c> Program<'c> {
    /// `c` and its (transitive) bases, bounded.
    pub(super) fn lineage(&self, c: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut todo = vec![c];
        let mut seen = HashSet::new();
        while let Some(x) = todo.pop() {
            if !seen.insert(x) || seen.len() > 32 {
                continue;
            }
            out.push(x);
            todo.extend(self.classes[x as usize].bases.iter().copied());
        }
        out.sort_unstable();
        out
    }

    /// The class a declared type spelling names in unit `u` (pointer / reference marks and
    /// generic arguments removed; the last segment in the unit's scope otherwise).
    pub(super) fn declared_class(&self, u: u32, type_name: &str) -> Option<u32> {
        let base = type_name.split(['<', '[', '(']).next().unwrap_or(type_name);
        let clean = base.trim().trim_start_matches(['*', '&']).trim();
        if clean.is_empty() {
            return None;
        }
        match self.resolve_class_path(u, clean) {
            Ok(c) => Some(c),
            Err(_) => self.class_in_scope(u, simple_type(clean)),
        }
    }

    /// Declared type of a class slot (the field of that class or of a related class) and the
    /// unit its spelling belongs to.
    pub(super) fn slot_type(&self, st: &State, s: u32) -> Option<(u32, String)> {
        let key = st.slot_keys.get(s as usize)?;
        let SlotOwner::Class(c) = key.owner else {
            return None;
        };
        std::iter::once(c)
            .chain(self.related.get(c as usize).into_iter().flatten().copied())
            .find_map(|x| {
                self.field_types
                    .get(&(x, key.name.clone()))
                    .map(|t| (self.classes[x as usize].unit, t.clone()))
            })
    }

    /// Whether a declared type spelling names a type whose objects the library cannot type
    /// further: an interface, or a type without library source. Methods called on such a
    /// receiver run on whatever object the caller passes (`CallsMethod`).
    pub(super) fn open_type(&self, u: u32, type_name: &str) -> bool {
        match self.declared_class(u, type_name) {
            Some(c) => self.classes[c as usize].is_interface,
            None => true,
        }
    }

    /// Whether calling `method` on a value of the declared type calls the value itself (a
    /// functional type of the language, or a library interface with exactly one abstract
    /// method where the adapter says those are function types).
    pub(super) fn calls_function(&self, u: u32, type_name: &str, method: &str) -> bool {
        let simple = simple_type(type_name);
        if self
            .spec
            .functional_types
            .iter()
            .any(|(t, m)| *t == simple && *m == method)
        {
            return true;
        }
        if !self.spec.single_method_interfaces_are_functions {
            return false;
        }
        let Some(c) = self.declared_class(u, type_name) else {
            return false;
        };
        let class = &self.classes[c as usize];
        if !class.is_interface {
            return false;
        }
        let mut abstract_methods = class
            .methods
            .iter()
            .filter(|(_, m)| !self.funcs[**m as usize].has_body)
            .map(|(name, _)| name.as_str());
        abstract_methods.next() == Some(method) && abstract_methods.next().is_none()
    }

    /// Every clause / overload of `g` (same name, same class) whose arity accepts
    /// `positional` arguments when the adapter has clauses (exact arity or variadic first,
    /// else the longer ones: default arguments); `g` alone otherwise.
    pub(super) fn clauses_of(&self, g: u32, positional: u32) -> Vec<u32> {
        if !self.spec.clauses {
            return vec![g];
        }
        let func = &self.funcs[g as usize];
        let same: Vec<u32> = func
            .class
            .and_then(|c| self.classes[c as usize].clauses.get(&func.name).cloned())
            .unwrap_or_else(|| vec![g]);
        let arity = |h: u32| {
            let fh = &self.funcs[h as usize];
            let receiver = usize::from(
                fh.params
                    .first()
                    .is_some_and(|p| fh.self_param.as_deref() == Some(p.name.as_str())),
            );
            fh.params.len().saturating_sub(receiver) as u32
        };
        let variadic = |h: u32| {
            self.funcs[h as usize]
                .params
                .iter()
                .any(|p| p.kind == ParamKind::VarPositional)
        };
        let mut out: Vec<u32> = same
            .iter()
            .copied()
            .filter(|&h| arity(h) == positional || variadic(h))
            .collect();
        if out.is_empty() {
            out = same.iter().copied().filter(|&h| arity(h) > positional).collect();
        }
        if out.is_empty() {
            out.push(g);
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// A class named `name` in unit `u` (or its namespace group).
    pub(super) fn class_in_scope(&self, u: u32, name: &str) -> Option<u32> {
        let mut units = vec![u];
        if let Some(g) = &self.units[u as usize].group {
            units.extend(self.groups.get(g).into_iter().flatten().copied().filter(|x| *x != u));
        }
        units.into_iter().find_map(|x| {
            self.units[x as usize]
                .decl_class
                .iter()
                .find(|(d, _)| self.units[x as usize].file.facts.declarations[**d as usize].name == name)
                .map(|(_, c)| *c)
        })
    }

    /// A base-class spelling (`base_events.BaseEventLoop`, `Base[T]`, `A::B`): the class, or
    /// `Err(Some(qualified))` for a base without readable source.
    pub(super) fn resolve_class_path(&self, u: u32, spelling: &str) -> Result<u32, Option<String>> {
        let clean = spelling
            .split(['(', '<', '['])
            .next()
            .unwrap_or(spelling)
            .trim()
            .replace("::", ".");
        let parts: Vec<&str> = clean.split('.').filter(|p| !p.is_empty()).collect();
        let Some(first) = parts.first() else {
            return Err(None);
        };
        let mut current: Vec<Resolved> = self.resolve_global(u, first, 0);
        for (i, part) in parts.iter().enumerate().skip(1) {
            current = match current.first() {
                Some(Resolved::Module(m)) => self.resolve_global(*m, part, 0),
                Some(Resolved::Class(c)) => self
                    .nested_class(*c, part)
                    .map(|n| vec![Resolved::Class(n)])
                    .unwrap_or_default(),
                Some(Resolved::Leaf(q)) => return Err(Some(format!("{q}.{}", parts[i..].join(".")))),
                _ => Vec::new(),
            };
        }
        match current.first() {
            Some(Resolved::Class(c)) => Ok(*c),
            Some(Resolved::Leaf(q)) => Err(Some(q.clone())),
            _ => Err(None),
        }
    }

    /// Module-level meaning of `name` in unit `u` (globals, namespace group, imports, wildcard
    /// imports, global constants).
    pub(super) fn resolve_global(&self, u: u32, name: &str, depth: usize) -> Vec<Resolved> {
        if depth > 4 {
            return Vec::new();
        }
        let unit = &self.units[u as usize];
        let global = |x: u32| -> Option<Resolved> {
            self.units[x as usize].globals.get(name).map(|g| match g {
                Global::Func(f) => Resolved::Func(*f),
                Global::Class(c) => Resolved::Class(*c),
                Global::Var => Resolved::Var(x, name.to_string()),
            })
        };
        if let Some(r) = global(u) {
            // `var x = require("m")`: the variable is the import (object-model languages).
            if matches!(r, Resolved::Var(..)) && self.spec.objects.is_some() {
                if let Some(imp) = unit
                    .imports
                    .iter()
                    .find(|i| i.scope_func.is_none() && i.local == name)
                {
                    return self.import_resolution(imp, depth);
                }
            }
            return vec![r];
        }
        if let Some(g) = &unit.group {
            for &o in self.groups.get(g).into_iter().flatten() {
                if o != u {
                    if let Some(r) = global(o) {
                        return vec![r];
                    }
                }
            }
        }
        if let Some(imp) = unit
            .imports
            .iter()
            .find(|i| i.scope_func.is_none() && i.local == name)
        {
            return self.import_resolution(imp, depth);
        }
        for imp in unit.imports.iter().filter(|i| i.kind == ImportKind::Wildcard) {
            if let Some((u2, _)) = imp.resolved {
                let r = self.resolve_global(u2, name, depth + 1);
                if !r.is_empty() {
                    return r;
                }
            }
        }
        Vec::new()
    }

    pub(super) fn import_resolution(&self, imp: &Imp, depth: usize) -> Vec<Resolved> {
        match &imp.resolved {
            Some((u2, None)) => vec![Resolved::Module(*u2)],
            Some((u2, Some(member))) => {
                let r = self.resolve_global(*u2, member, depth + 1);
                if r.is_empty() {
                    vec![Resolved::Leaf(imp.target.clone())]
                } else {
                    r
                }
            }
            None => vec![Resolved::Leaf(imp.target.clone())],
        }
    }

    pub(super) fn resolved_values(&self, st: &mut State, resolved: Vec<Resolved>) -> Vals {
        let literals = self.global_literals(st, &resolved);
        let mut out: Vals = resolved
            .into_iter()
            .filter_map(|r| match r {
                Resolved::Func(f) => Some(self.object_value(&Resolved::Func(f)).unwrap_or(V::Fn(f, false))),
                Resolved::Class(c) => Some(V::Class(c)),
                Resolved::Module(m) => Some(V::Module(m)),
                Resolved::Var(u, name) if self.object_value(&Resolved::Var(u, name.clone())).is_some() => {
                    self.object_value(&Resolved::Var(u, name))
                }
                Resolved::Var(u, name) => Some(V::Slot(
                    st.slot(SlotKey {
                        owner: SlotOwner::Unit(u),
                        name,
                    }),
                    How::Direct,
                )),
                Resolved::Leaf(_) => None,
            })
            .collect();
        out.extend(literals);
        out
    }

    /// Whether `name` is bound inside `f` or an enclosing function (never a leaf).
    pub(super) fn locally_bound(&self, f: u32, name: &str) -> bool {
        let mut cur = Some(f);
        for _ in 0..16 {
            let Some(g) = cur else { break };
            let func = &self.funcs[g as usize];
            if !func.is_module
                && (func.self_param.as_deref() == Some(name)
                    || func.params.iter().any(|p| p.name == name)
                    || func.locals.contains_key(name)
                    || func.nested.contains_key(name)
                    || func.local_classes.contains_key(name))
            {
                return true;
            }
            cur = func.parent;
        }
        false
    }

    pub(super) fn is_ancestor(&self, a: u32, f: u32) -> bool {
        let mut cur = self.funcs[f as usize].parent;
        for _ in 0..16 {
            match cur {
                Some(g) if g == a => return true,
                Some(g) => cur = self.funcs[g as usize].parent,
                None => return false,
            }
        }
        false
    }

    pub(super) fn enclosing_class(&self, f: u32) -> Option<u32> {
        let mut cur = Some(f);
        for _ in 0..16 {
            let g = cur?;
            if let Some(c) = self.funcs[g as usize].class {
                return Some(c);
            }
            cur = self.funcs[g as usize].parent;
        }
        None
    }

    pub(super) fn find_method(&self, st: &State, c: u32, name: &str) -> Option<u32> {
        let mut todo = vec![c];
        let mut seen = HashSet::new();
        while let Some(x) = todo.pop() {
            if !seen.insert(x) || seen.len() > 32 {
                continue;
            }
            if let Some(&m) = st.method_alias.get(&(x, name.to_string())) {
                return Some(m);
            }
            if let Some(&m) = self.classes[x as usize].methods.get(name) {
                return Some(m);
            }
            todo.extend(self.classes[x as usize].bases.iter().rev().copied());
        }
        None
    }

    /// Overrides of `name` in the (transitive) subclasses of `c`.
    pub(super) fn overrides(&self, c: u32, name: &str) -> Vec<u32> {
        let mut out = Vec::new();
        let mut todo: Vec<u32> = self.classes[c as usize].subclasses.clone();
        let mut seen = HashSet::new();
        while let Some(x) = todo.pop() {
            if !seen.insert(x) || seen.len() > 64 {
                continue;
            }
            if let Some(&m) = self.classes[x as usize].methods.get(name) {
                out.push(m);
            }
            todo.extend(self.classes[x as usize].subclasses.iter().copied());
        }
        out
    }

    pub(super) fn nested_class(&self, c: u32, name: &str) -> Option<u32> {
        let mut todo = vec![c];
        let mut seen = HashSet::new();
        while let Some(x) = todo.pop() {
            if !seen.insert(x) || seen.len() > 32 {
                continue;
            }
            if let Some(&n) = self.classes[x as usize].nested_classes.get(name) {
                return Some(n);
            }
            todo.extend(self.classes[x as usize].bases.iter().copied());
        }
        None
    }

    pub(super) fn unresolved_bases(&self, c: u32) -> Vec<String> {
        let mut out = Vec::new();
        let mut todo = vec![c];
        let mut seen = HashSet::new();
        while let Some(x) = todo.pop() {
            if !seen.insert(x) || seen.len() > 32 {
                continue;
            }
            out.extend(self.classes[x as usize].unresolved_bases.iter().cloned());
            todo.extend(self.classes[x as usize].bases.iter().copied());
        }
        out
    }

    pub(super) fn ctors(&self, st: &State, c: u32) -> Vec<u32> {
        let mut out: Vec<u32> = self
            .spec
            .constructors
            .iter()
            .filter_map(|n| self.find_method(st, c, n))
            .collect();
        let mut own: Vec<u32> = self.classes[c as usize]
            .methods
            .values()
            .copied()
            .filter(|m| self.funcs[*m as usize].is_ctor)
            .collect();
        own.sort_unstable();
        out.extend(own);
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The slot of attribute `name` of instances of class `c` (related classes share content
    /// through slot flows, see [`Program::propagate`]).
    pub(super) fn class_slot(&self, st: &mut State, c: u32, name: &str) -> u32 {
        st.slot(SlotKey {
            owner: SlotOwner::Class(c),
            name: name.to_string(),
        })
    }
}
