//! Type spellings resolved to declarations, and whether a receiver type relates to a
//! member or a class family (child of [`crate::types`]).

use super::*;

impl<'i> Types<'i> {
    /// Resolve several spellings and combine them.
    pub(super) fn resolve_all(&mut self, file: FileId, spellings: &[&str]) -> ReceiverType {
        let mut parts = Vec::new();
        for s in spellings {
            parts.push(self.resolve_spelling(file, s));
        }
        self.combine(parts)
    }

    /// A spelled type name used in `file` (module docs; family rule's base resolution).
    pub(crate) fn resolve_spelling(&mut self, file: FileId, spelling: &str) -> ReceiverType {
        let key = (file, spelling.to_string());
        if let Some(hit) = self.spellings.get(&key) {
            return hit.clone();
        }
        let found = self.resolve_spelling_uncached(file, spelling);
        self.spellings.insert(key, found.clone());
        found
    }

    fn resolve_spelling_uncached(&mut self, file: FileId, spelling: &str) -> ReceiverType {
        let index = self.index;
        let name = base_name(spelling);
        let generic = generic_look(name);
        if name.is_empty() || (uninformative(name) && !generic) {
            return ReceiverType::Unknown;
        }
        let unbound = || {
            if generic {
                ReceiverType::Unknown
            } else {
                ReceiverType::External(name.to_string())
            }
        };
        let language = index.file(file).language;
        let all: Vec<SymbolId> = self
            .types_named(name)
            .into_iter()
            .filter(|&t| name_interop(language, index.symbol(t).language))
            .collect();
        let head = spelling
            .trim()
            .split(['<', '[', '(', '{'])
            .next()
            .unwrap_or("")
            .trim();
        let head = head.rsplit(char::is_whitespace).next().unwrap_or(head);
        let parts: Vec<&str> = head
            .split(['.', ':', '\\'])
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        let qualifier: &[&str] = match parts.split_last() {
            Some((last, rest)) if *last == name => rest,
            _ => &[],
        };
        // 1. Same file (unqualified spellings).
        if qualifier.is_empty() {
            let local: Vec<SymbolId> = all
                .iter()
                .copied()
                .filter(|&t| index.symbol(t).file == file)
                .collect();
            match local.as_slice() {
                [only] => return ReceiverType::Index(vec![*only]),
                [] => {}
                _ => return ReceiverType::Unknown,
            }
        }
        // 2. An import binding the root (or the name itself) decides alone.
        let root = qualifier.first().copied().unwrap_or(name);
        let import = self.facts(file).and_then(|facts| {
            facts
                .imports
                .iter()
                .rev()
                .find(|i| i.local == root && i.kind != ImportKind::Wildcard)
        });
        if let Some(import) = import {
            let rest = if qualifier.is_empty() {
                &[][..]
            } else {
                &qualifier[1..]
            };
            let target = if rest.is_empty() {
                import.target.clone()
            } else {
                format!("{}.{}", import.target, rest.join("."))
            };
            let member = root == name && import.kind == ImportKind::Member;
            let path = index.file_path(file).to_string();
            let modules = self.modules();
            let resolved = modules
                .resolve(&path, language, &target, member)
                .or_else(|| modules.resolve(&path, language, &target, true));
            let Some((files, _)) = resolved else {
                return unbound();
            };
            let mut hits: Vec<SymbolId> = all
                .iter()
                .copied()
                .filter(|&t| files.contains(&index.symbol(t).file))
                .collect();
            if hits.is_empty() && root == name {
                let named =
                    crate::imports::declarations_at_path(index, modules, &path, language, &import.target);
                hits = named.into_iter().filter(|t| all.contains(t)).collect();
            }
            return match hits.as_slice() {
                [only] => ReceiverType::Index(vec![*only]),
                _ => ReceiverType::Unknown,
            };
        }
        // 3. Same package directory.
        if package_directories(language) {
            let dir = trace_core::relpath::parent(index.file_path(file));
            let same: Vec<SymbolId> = all
                .iter()
                .copied()
                .filter(|&t| trace_core::relpath::parent(index.file_path(index.symbol(t).file)) == dir)
                .collect();
            match same.as_slice() {
                [only] => return ReceiverType::Index(vec![*only]),
                [] => {}
                _ => return ReceiverType::Unknown,
            }
        }
        // 4. Globally unique in the language namespace (not for generic-looking spellings).
        match all.as_slice() {
            _ if generic => ReceiverType::Unknown,
            [] => ReceiverType::External(name.to_string()),
            [only] => ReceiverType::Index(vec![*only]),
            _ => ReceiverType::Unknown,
        }
    }

    /// Combine the types of several facts: any unknown or conflicting part -> `Unknown`.
    pub(super) fn combine(&self, parts: Vec<ReceiverType>) -> ReceiverType {
        if parts.is_empty() {
            return ReceiverType::Unknown;
        }
        let mut ids: BTreeSet<SymbolId> = BTreeSet::new();
        let mut external: Option<String> = None;
        for p in parts {
            match p {
                ReceiverType::Unknown => return ReceiverType::Unknown,
                ReceiverType::Index(v) => ids.extend(v),
                ReceiverType::External(n) => match &external {
                    None => external = Some(n),
                    Some(e) if *e == n => {}
                    Some(_) => return ReceiverType::Unknown,
                },
            }
        }
        match (ids.is_empty(), external) {
            (false, None) => {
                let v: Vec<SymbolId> = ids.into_iter().collect();
                // Two unrelated types: conflicting facts.
                for (i, &a) in v.iter().enumerate() {
                    for &b in &v[i + 1..] {
                        let related =
                            self.hierarchy.mro(a).contains(&b) || self.hierarchy.mro(b).contains(&a);
                        if !related {
                            return ReceiverType::Unknown;
                        }
                    }
                }
                ReceiverType::Index(v)
            }
            (true, Some(n)) => ReceiverType::External(n),
            _ => ReceiverType::Unknown,
        }
    }

    /// The type declaring member `m` (`m` itself for types).
    fn declaring_type(&self, m: SymbolId) -> Option<SymbolId> {
        let sym = self.index.symbol(m);
        if sym.kind.is_type() {
            return Some(m);
        }
        self.hierarchy.class_of(self.index, m)
    }

    /// Names identifying the container type of a member without a declaring type symbol:
    /// the container spelling and its class annotations.
    pub(super) fn member_type_names(&self, m: SymbolId) -> Vec<&'i str> {
        let index: &'i Index = self.index;
        let sym = index.symbol(m);
        let Some(container) = sym.container.as_deref() else {
            return Vec::new();
        };
        let mut names = vec![base_name(container)];
        for a in self.container_annotations(sym.file, container) {
            names.push(base_name(a));
        }
        names
    }

    /// Classes a receiver of these types may be at run time: the types, their bases and
    /// their subtypes.
    fn accept_set(&self, types: &[SymbolId]) -> HashSet<SymbolId> {
        let mut accept = HashSet::new();
        for &t in types {
            accept.extend(self.hierarchy.mro(t));
            accept.extend(self.hierarchy.family(t));
        }
        accept
    }

    /// Whether member `m` belongs to the family of a receiver of type `ty` (the positive
    /// counterpart of [`Types::unrelated_to_family`]): its declaring type is one of the
    /// receiver's types, bases or subtypes, or its container spells / is annotated as the
    /// receiver's type.
    pub fn related(&mut self, ty: &ReceiverType, m: SymbolId) -> bool {
        let index = self.index;
        match ty {
            ReceiverType::Unknown => false,
            ReceiverType::Index(types) => {
                let accept = self.accept_set(types);
                if let Some(dt) = self.declaring_type(m) {
                    return accept.contains(&dt);
                }
                let names = self.member_type_names(m);
                accept.iter().any(|&a| names.contains(&index.symbol(a).name.as_str()))
            }
            ReceiverType::External(n) => {
                if let Some(dt) = self.declaring_type(m) {
                    return self.hierarchy.mro(dt).iter().any(|&k| index.symbol(k).name == *n);
                }
                self.member_type_names(m).contains(&n.as_str())
            }
        }
    }

    /// Type names with an out-of-line impl / conformance to a trait naming no index type.
    fn has_unresolved_impl(&mut self, name: &str) -> bool {
        if self.unresolved_impls.is_none() {
            let index = self.index;
            let mut known: HashSet<&str> = HashSet::new();
            for s in &index.symbols {
                if s.kind.is_type() {
                    known.insert(s.name.as_str());
                }
            }
            let mut out = HashSet::new();
            for f in &index.files {
                let Some(facts) = &f.facts else { continue };
                for rel in &facts.impls {
                    let t = base_name(&rel.type_name);
                    let tr = base_name(&rel.trait_name);
                    if !t.is_empty() && !tr.is_empty() && !known.contains(tr) {
                        out.insert(t.to_string());
                    }
                }
            }
            self.unresolved_impls = Some(out);
        }
        self.unresolved_impls.as_ref().is_some_and(|s| s.contains(name))
    }

    /// Whether type `t` has a base spelling (or impl / conformance) that names no index type.
    fn has_unresolved_base(&mut self, t: SymbolId) -> bool {
        let index = self.index;
        let sym = index.symbol(t);
        for spelling in &sym.bases {
            let name = base_name(spelling);
            if !name.is_empty() && self.types_named(name).is_empty() {
                return true;
            }
        }
        let name = sym.name.clone();
        self.has_unresolved_impl(&name)
    }

    /// Declaring types that may be mixins syntax does not record (PHP traits).
    fn mixin_risk(&self, dt: SymbolId) -> bool {
        let sym = self.index.symbol(dt);
        rules(sym.language).interfaces_may_be_mixins && sym.kind == SymbolKind::Interface
    }

    /// [`unrelated_to_family`] (module function) with its guards. Narrowing passes
    /// `strict = false` when another candidate of the pool is related to the receiver (a
    /// member of the receiver's own family shadows mixed-in ones).
    pub fn unrelated_to_family(&mut self, ty: &ReceiverType, family: &[SymbolId]) -> bool {
        self.unrelated(ty, family, true)
    }

    pub(crate) fn unrelated(&mut self, ty: &ReceiverType, family: &[SymbolId], strict: bool) -> bool {
        if family.is_empty() {
            return false;
        }
        let index = self.index;
        match ty {
            ReceiverType::Unknown => false,
            ReceiverType::Index(types) => {
                if types.is_empty() {
                    return false;
                }
                for &t in types {
                    let sym = index.symbol(t);
                    let protocol = sym.bases.iter().any(|b| base_name(b).starts_with("Protocol"));
                    if (structural(sym.language) && sym.kind == SymbolKind::Interface) || protocol {
                        return false;
                    }
                    if forwards_by_deref(sym.language) {
                        for k in self.hierarchy.mro(t) {
                            if self.has_unresolved_base(k) {
                                return false;
                            }
                        }
                    }
                }
                let accept = self.accept_set(types);
                let accept_names: HashSet<&str> =
                    accept.iter().map(|&a| index.symbol(a).name.as_str()).collect();
                for &m in family {
                    match self.declaring_type(m) {
                        Some(dt) => {
                            if accept.contains(&dt) || (strict && self.mixin_risk(dt)) {
                                return false;
                            }
                        }
                        None => {
                            let sym = index.symbol(m);
                            let annotated = sym
                                .container
                                .as_deref()
                                .map(|c| self.container_annotations(sym.file, c))
                                .unwrap_or_default();
                            if annotated.is_empty() {
                                return false;
                            }
                            let names: Vec<&str> = annotated.iter().map(|a| base_name(a)).collect();
                            if names.iter().any(|n| accept_names.contains(n)) {
                                return false;
                            }
                        }
                    }
                }
                true
            }
            ReceiverType::External(name) => {
                if uninformative(name) {
                    return false;
                }
                for &m in family {
                    let sym = index.symbol(m);
                    if structural(sym.language) || forwards_by_deref(sym.language) {
                        return false;
                    }
                    match self.declaring_type(m) {
                        Some(dt) => {
                            for k in self.hierarchy.mro(dt) {
                                if index.symbol(k).name == *name || self.has_unresolved_base(k) {
                                    return false;
                                }
                            }
                            if strict && self.mixin_risk(dt) {
                                return false;
                            }
                        }
                        None => {
                            let annotated = sym
                                .container
                                .as_deref()
                                .map(|c| self.container_annotations(sym.file, c))
                                .unwrap_or_default();
                            if annotated.is_empty() || annotated.iter().any(|a| base_name(a) == name.as_str())
                            {
                                return false;
                            }
                        }
                    }
                }
                true
            }
        }
    }
}
