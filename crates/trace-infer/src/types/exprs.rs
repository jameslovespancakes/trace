//! Types of expressions: calls, static calls, method returns, fields and callables
//! named in scope (child of [`crate::types`]).

use super::*;

impl<'i> Types<'i> {
    /// Type of a value expression evaluated in the scopes of `chain`.
    pub(super) fn expr_type(
        &mut self,
        file: FileId,
        chain: &[SymbolId],
        expr: &Expr,
        depth: u32,
    ) -> ReceiverType {
        if depth > self.max_depth {
            return ReceiverType::Unknown;
        }
        match expr {
            Expr::Name { name, .. } => self.name_type(file, chain, name, depth + 1),
            Expr::Call { func, is_new, .. } => self.call_type(file, chain, func, *is_new, depth + 1),
            Expr::Attr { object, attr, .. } => {
                let owner = self.expr_type(file, chain, object, depth + 1);
                self.field_of(&owner, attr)
            }
            Expr::Choice(alts) => {
                let mut parts = Vec::new();
                for a in alts {
                    parts.push(self.expr_type(file, chain, a, depth + 1));
                }
                self.combine(parts)
            }
            _ => ReceiverType::Unknown,
        }
    }

    /// Type of the value a call returns (module docs, rule 3).
    fn call_type(
        &mut self,
        file: FileId,
        chain: &[SymbolId],
        func: &Expr,
        is_new: bool,
        depth: u32,
    ) -> ReceiverType {
        let index = self.index;
        let language = index.file(file).language;
        match func {
            Expr::Name { name, .. } => {
                if is_new {
                    return self.resolve_spelling(file, name);
                }
                let (qualifier, last) = split_path(name);
                if let Some(q) = qualifier {
                    // `Foo::new()` / `Foo.new` (constructors spelled as a static call).
                    if last == "new" {
                        if let t @ ReceiverType::Index(_) = self.resolve_spelling(file, q) {
                            return t;
                        }
                    }
                    return self.static_call(file, q, last);
                }
                if constructs_by_call(language) {
                    if let t @ ReceiverType::Index(_) = self.resolve_spelling(file, name) {
                        return t;
                    }
                }
                match self.callable_named(file, chain, name) {
                    Some(c) => self.return_type(c),
                    None => ReceiverType::Unknown,
                }
            }
            Expr::Attr { object, attr, .. } => {
                if is_new {
                    return self.resolve_spelling(file, attr);
                }
                if let Expr::Name { name: o, .. } = object.as_ref() {
                    if !is_self_name(language, o) && !self.local_binding(file, chain, o) {
                        if attr == "new" {
                            if let t @ ReceiverType::Index(_) = self.resolve_spelling(file, o) {
                                return t;
                            }
                        }
                        let t = self.static_call(file, o, attr);
                        if t != ReceiverType::Unknown {
                            return t;
                        }
                    }
                }
                let owner = self.expr_type(file, chain, object, depth + 1);
                self.method_return(&owner, attr)
            }
            _ => ReceiverType::Unknown,
        }
    }

    /// Whether `name` is bound by a scope of `chain` or the module (a value, not a type).
    fn local_binding(&mut self, file: FileId, chain: &[SymbolId], name: &str) -> bool {
        let Some(facts) = self.facts(file) else {
            return false;
        };
        let index = self.index;
        let mut scopes: Vec<Scope> = Vec::new();
        for &s in chain {
            let sym = index.symbol(s);
            if !sym.kind.is_callable() {
                continue;
            }
            let param = facts
                .declarations
                .get(sym.decl as usize)
                .is_some_and(|d| d.parameters.iter().any(|p| same_var(&p.name, name)));
            if param {
                return true;
            }
            scopes.push(Scope::Decl(sym.decl));
        }
        scopes.push(Scope::Module);
        for scope in scopes {
            let (types, binds) = self.var_facts(file, scope, name);
            if !types.is_empty()
                || !binds.is_empty()
                || facts.imports.iter().any(|i| i.scope == scope && i.local == name)
            {
                return true;
            }
        }
        false
    }

    /// `Type.m()` / `table.m()` where the qualifier names a type or a container table: the
    /// return type of that member.
    fn static_call(&mut self, file: FileId, qualifier: &str, member: &str) -> ReceiverType {
        let index = self.index;
        if let ReceiverType::Index(types) = self.resolve_spelling(file, qualifier) {
            let ret = self.method_return(&ReceiverType::Index(types), member);
            if ret != ReceiverType::Unknown {
                return ret;
            }
        }
        // Container objects (`obj.make = function` in JS).
        let owner = base_name(qualifier);
        let found: Vec<SymbolId> = self
            .hierarchy
            .functions
            .get(member)
            .map(|v| {
                v.iter()
                    .copied()
                    .filter(|&c| {
                        let s = index.symbol(c);
                        s.file == file && s.container.as_deref().map(base_name) == Some(owner)
                    })
                    .collect()
            })
            .unwrap_or_default();
        match found.as_slice() {
            [only] => self.return_type(*only),
            _ => ReceiverType::Unknown,
        }
    }

    /// Return type of the method `member` a receiver of type `owner` dispatches to.
    fn method_return(&mut self, owner: &ReceiverType, member: &str) -> ReceiverType {
        let index = self.index;
        match owner {
            ReceiverType::Index(types) => {
                let mut parts = Vec::new();
                for &t in types {
                    for k in self.hierarchy.mro(t) {
                        if let Some(m) = self.hierarchy.method(k, member) {
                            parts.push(self.return_type(m));
                            break;
                        }
                    }
                }
                if parts.is_empty() {
                    ReceiverType::Unknown
                } else {
                    self.combine(parts)
                }
            }
            ReceiverType::External(name) => {
                // Container-typed members (`function Picker:clone()` with `---@class Picker`).
                let name = name.clone();
                let pool: Vec<SymbolId> = self.hierarchy.functions.get(member).cloned().unwrap_or_default();
                let found: Vec<SymbolId> = pool
                    .into_iter()
                    .filter(|&c| self.hierarchy.class_of(index, c).is_none())
                    .filter(|&c| self.member_type_names(c).iter().any(|n| *n == name))
                    .collect();
                match found.as_slice() {
                    [only] => self.return_type(*only),
                    _ => ReceiverType::Unknown,
                }
            }
            ReceiverType::Unknown => ReceiverType::Unknown,
        }
    }

    /// Type of field `name` of a value of type `owner` (module docs, rule 4).
    fn field_of(&mut self, owner: &ReceiverType, name: &str) -> ReceiverType {
        let ReceiverType::Index(types) = owner else {
            return ReceiverType::Unknown;
        };
        let mut parts = Vec::new();
        for &t in types {
            for k in self.hierarchy.mro(t) {
                let found = self.field_spellings(k, name);
                if !found.is_empty() {
                    let file = self.index.symbol(k).file;
                    parts.push(self.resolve_all(file, &found));
                    break;
                }
            }
        }
        if parts.is_empty() {
            ReceiverType::Unknown
        } else {
            self.combine(parts)
        }
    }

    /// Return type of callable `c` (a type called as a constructor is that type).
    fn return_type(&mut self, c: SymbolId) -> ReceiverType {
        let index = self.index;
        let sym = index.symbol(c);
        if sym.kind.is_type() {
            return ReceiverType::Index(vec![c]);
        }
        if is_constructor(sym) {
            return match self.hierarchy.class_of(index, c) {
                Some(k) => ReceiverType::Index(vec![k]),
                None => ReceiverType::Unknown,
            };
        }
        let Some(facts) = self.facts(sym.file) else {
            return ReceiverType::Unknown;
        };
        let spellings: Vec<&'i str> = facts
            .types
            .iter()
            .filter(|t| t.subject == TypeSubject::Return { decl: sym.decl })
            .map(|t| t.type_name.as_str())
            .collect();
        if spellings.is_empty() {
            return ReceiverType::Unknown;
        }
        // `-> Self`, `---@return self`, `@return static`: the declaring type.
        if spellings
            .iter()
            .all(|s| matches!(base_name(s), "Self" | "self" | "static" | "$this" | "this"))
        {
            if let Some(k) = self.hierarchy.class_of(index, c) {
                return ReceiverType::Index(vec![k]);
            }
            if let Some(container) = sym.container.as_deref() {
                return self.container_type(sym.file, container);
            }
            return ReceiverType::Unknown;
        }
        self.resolve_all(sym.file, &spellings)
    }

    /// The callable a bare call `name(...)` in the scopes of `chain` names, when unique:
    /// nested functions of the chain, implicit-receiver methods, same file, an import, a
    /// globally unique free function of the language.
    fn callable_named(&mut self, file: FileId, chain: &[SymbolId], name: &str) -> Option<SymbolId> {
        let index = self.index;
        let language = index.file(file).language;
        let pool: Vec<SymbolId> = self
            .hierarchy
            .functions
            .get(name)?
            .iter()
            .copied()
            .filter(|&c| name_interop(language, index.symbol(c).language))
            .collect();
        if pool.is_empty() {
            return None;
        }
        for &s in chain {
            let nested: Vec<SymbolId> = pool
                .iter()
                .copied()
                .filter(|&c| index.symbol(c).parent == Some(s))
                .collect();
            if let [only] = nested.as_slice() {
                return Some(*only);
            }
            if index.symbol(s).kind.is_type() && implicit_receiver(language) {
                for k in self.hierarchy.mro(s) {
                    if let Some(m) = self.hierarchy.method(k, name) {
                        return Some(m);
                    }
                }
            }
        }
        let top: Vec<SymbolId> = pool
            .iter()
            .copied()
            .filter(|&c| index.symbol(c).parent.is_none())
            .collect();
        let same_file: Vec<SymbolId> = top
            .iter()
            .copied()
            .filter(|&c| index.symbol(c).file == file)
            .collect();
        match same_file.as_slice() {
            [only] => return Some(*only),
            [] => {}
            _ => return None,
        }
        let facts = self.facts(file)?;
        if let Some(import) = facts
            .imports
            .iter()
            .rev()
            .find(|i| i.local == name && i.kind == ImportKind::Member)
        {
            let path = index.file_path(file).to_string();
            let target = import.target.clone();
            let named = crate::imports::declarations_at_path(index, self.modules(), &path, language, &target);
            let hits: Vec<SymbolId> = named.into_iter().filter(|c| pool.contains(c)).collect();
            return match hits.as_slice() {
                [only] => Some(*only),
                _ => None,
            };
        }
        match top.as_slice() {
            [only] => Some(*only),
            _ => None,
        }
    }
}
