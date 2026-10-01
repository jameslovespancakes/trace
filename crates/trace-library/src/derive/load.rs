//! Import loading of a derivation (child of `derive`): which library files one derivation
//! reads within its unit budget.
//!
//! Rule "own distribution first": imports are loaded by kinship with the target file before
//! import distance - files of the target's own distribution (the same top-level package
//! below the same library root), then other files of the target's library root, then files
//! of other roots (the language's standard library, other toolchains) - and breadth-first
//! within one kinship. Rule "called imports first": within one kinship, files reached from the
//! target only through names their importers call (or derive classes from) load before files
//! imported for other uses (annotations, re-exports, checks). A package's dispatch structures (a route class, its handler wrapper,
//! the subclass overriding an abstract registration method) live in its own files and the
//! packages it builds on; a large standard-library package imported for one helper must not
//! use up the budget first.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use super::*;

/// Kinship of a file with the target file (lower loads first).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kinship {
    /// The target's distribution (same library root, same top-level package).
    Own,
    /// Another distribution below the target's library root.
    SameRoot,
    /// Below another root (or no root).
    Other,
}

impl<'c> Program<'c> {
    fn kinship(&self, target: &Path, path: &Path) -> Kinship {
        let t = languages::relative_to_roots(target, self.cx.roots);
        let p = languages::relative_to_roots(path, self.cx.roots);
        match (t, p) {
            (Some((trel, troot)), Some((prel, proot))) if troot == proot => {
                if trel.components().next().is_some() && trel.components().next() == prel.components().next()
                {
                    Kinship::Own
                } else {
                    Kinship::SameRoot
                }
            }
            _ => Kinship::Other,
        }
    }

    /// Root names a unit calls or derives classes from (`wrap(..)`,
    /// `routing.Route` as a base -> `routing`).
    fn called_roots(&self, u: u32) -> HashSet<String> {
        let facts = &self.units[u as usize].file.facts;
        let root = |text: &str| -> String {
            text.chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                .collect()
        };
        let mut out: HashSet<String> = facts.calls.iter().map(|c| root(&c.callee)).collect();
        for d in &facts.declarations {
            out.extend(d.bases.iter().map(|b| root(b)));
        }
        out.remove("");
        out
    }

    /// Import loading from the target file: pending imports are loaded by (kinship, import
    /// distance), bounded by the import depth and the unit budget (`derive` settings); every unit's imports are
    /// then linked to the loaded files.
    pub(super) fn load(&mut self, root: u32) {
        let target = self.units[root as usize].path.clone();
        // (kinship, not called, depth, order, path)
        let mut pending: BTreeSet<(Kinship, bool, usize, usize, PathBuf)> = BTreeSet::new();
        let mut order = 0usize;
        // (unit, depth, reached through called names)
        let mut expand: Vec<(u32, usize, bool)> = vec![(root, 0, true)];
        let mut queue = std::collections::VecDeque::new();
        self.load_group(root, &mut queue, 0);
        // The package's entry module runs when the repository imports the package.
        if let Some(entry) = (self.spec.package_entry)(&target) {
            self.load_path(&entry, &mut queue, 0);
        }
        expand.extend(queue.drain(..).map(|(u, d)| (u, d, true)));
        loop {
            for (u, depth, chain) in std::mem::take(&mut expand) {
                if depth >= self.cx.limits.import_depth {
                    continue;
                }
                let from = self.units[u as usize].path.clone();
                let called = self.called_roots(u);
                let targets: Vec<_> = self.units[u as usize]
                    .imports
                    .iter()
                    .map(|imp| (imp.target.clone(), imp.kind, chain && called.contains(&imp.local)))
                    .collect();
                for (t, kind, via_call) in targets {
                    let Some((path, _)) = self.resolve_import(&from, &t, kind) else {
                        continue;
                    };
                    if self.by_path.contains_key(&path) {
                        continue;
                    }
                    let k = self.kinship(&target, &path);
                    pending.insert((k, !via_call, depth + 1, order, path));
                    order += 1;
                }
            }
            let Some((_, not_called, depth, _, path)) = pending.pop_first() else { break };
            if self.by_path.contains_key(&path) {
                continue;
            }
            if self.units.len() >= self.cx.limits.max_units {
                break;
            }
            let mut loaded = std::collections::VecDeque::new();
            if self.load_path(&path, &mut loaded, depth).is_some() {
                expand.extend(loaded.drain(..).map(|(u, d)| (u, d, !not_called)));
            }
        }
        // Link every unit's imports to the loaded files.
        for u in 0..self.units.len() {
            for i in 0..self.units[u].imports.len() {
                let (t, kind, from) = {
                    let imp = &self.units[u].imports[i];
                    (imp.target.clone(), imp.kind, self.units[u].path.clone())
                };
                let Some((path, member)) = self.resolve_import(&from, &t, kind) else {
                    continue;
                };
                if let Some(&u2) = self.by_path.get(&path) {
                    self.units[u].imports[i].resolved = Some((u2, member));
                }
            }
        }
    }
}

impl<'c> Program<'c> {
    /// The file (and member) an import of `from` names; resolved once per batch.
    fn resolve_import(
        &self,
        from: &Path,
        target: &str,
        kind: ImportKind,
    ) -> Option<(PathBuf, Option<String>)> {
        let key = (self.language, from.to_path_buf(), target.to_string(), kind);
        self.cx
            .parsed
            .imports
            .get_or_init(key, || (self.spec.resolve_import)(from, target, kind, self.cx.roots))
    }

    pub(super) fn add_unit(&mut self, path: PathBuf, file: Arc<ParsedFile>) -> u32 {
        let facts = &file.facts;
        let low = &file.low;
        let u = self.units.len() as u32;
        let mut decl_func = HashMap::new();
        let mut decl_class = HashMap::new();
        for (d, decl) in facts.declarations.iter().enumerate() {
            let d = d as u32;
            if decl.kind.is_type() {
                let c = self.classes.len() as u32;
                self.classes.push(Class {
                    unit: u,
                    decl: d,
                    qualified: decl.qualified_name.clone(),
                    base_names: decl.bases.clone(),
                    bases: Vec::new(),
                    unresolved_bases: Vec::new(),
                    methods: HashMap::new(),
                    clauses: HashMap::new(),
                    nested_classes: HashMap::new(),
                    subclasses: Vec::new(),
                    is_interface: decl.kind == SymbolKind::Interface,
                });
                decl_class.insert(d, c);
            } else if decl.kind.is_callable() || decl.kind == SymbolKind::Module {
                let f = self.funcs.len() as u32;
                self.funcs.push(Func {
                    unit: u,
                    decl: d,
                    name: decl.name.clone(),
                    qualified: decl.qualified_name.clone(),
                    is_module: decl.kind == SymbolKind::Module,
                    is_ctor: decl.kind == SymbolKind::Constructor,
                    has_body: decl.body_start < decl.span.bytes.end,
                    params: decl
                        .parameters
                        .iter()
                        .map(|p| PInfo {
                            name: p.name.clone(),
                            kind: p.kind,
                        })
                        .collect(),
                    ..Func::default()
                });
                decl_func.insert(d, f);
            }
        }
        let module_func = facts.module_decl.and_then(|m| decl_func.get(&m).copied());
        let mut globals: HashMap<String, Global> = HashMap::new();
        // Relationships: nesting, methods, constructors, globals.
        for (d, decl) in facts.declarations.iter().enumerate() {
            let d = d as u32;
            let parent_func = decl.parent.and_then(|p| decl_func.get(&p).copied());
            let parent_class = decl.parent.and_then(|p| decl_class.get(&p).copied());
            if let Some(&f) = decl_func.get(&d) {
                if self.funcs[f as usize].is_module {
                    continue;
                }
                if let Some(pc) = parent_class {
                    self.funcs[f as usize].class = Some(pc);
                    self.classes[pc as usize]
                        .methods
                        .entry(decl.name.clone())
                        .or_insert(f);
                    self.classes[pc as usize]
                        .clauses
                        .entry(decl.name.clone())
                        .or_default()
                        .push(f);
                } else if let Some(pf) = parent_func {
                    self.funcs[f as usize].parent = Some(pf);
                    if !decl.name.starts_with('<') {
                        self.funcs[pf as usize].nested.insert(decl.name.clone(), f);
                    }
                } else if decl.parent.is_none() && decl.container.is_none() && !decl.name.starts_with('<') {
                    // Out-of-line methods (Go receivers: `container`) are no globals.
                    globals.entry(decl.name.clone()).or_insert(Global::Func(f));
                }
                if parent_class.is_some() && self.spec.constructors.contains(&decl.name.as_str()) {
                    self.funcs[f as usize].is_ctor = true;
                }
            }
            if let Some(&c) = decl_class.get(&d) {
                if let Some(pc) = parent_class {
                    self.classes[pc as usize].nested_classes.insert(decl.name.clone(), c);
                } else if let Some(pf) = parent_func {
                    self.funcs[pf as usize].local_classes.insert(decl.name.clone(), c);
                } else if decl.parent.is_none() {
                    globals.entry(decl.name.clone()).or_insert(Global::Class(c));
                }
                self.class_names.entry(decl.name.clone()).or_default().push(c);
            }
        }
        let scope_func = |scope: &Scope| -> Option<u32> {
            match scope {
                Scope::Module => module_func,
                Scope::Decl(d) => decl_func.get(d).copied().or(module_func),
            }
        };
        let exports = self.unit_exports(&low.flow, facts.module_decl);
        for fact in low.flow.iter().cloned() {
            match fact {
                FlowFact::Bind { target, value, scope } => match target {
                    BindTarget::Var {
                        scope: Scope::Decl(sd),
                        name,
                    } => {
                        let Some(&f) = decl_func.get(&sd) else { continue };
                        let func = &mut self.funcs[f as usize];
                        if func.is_module {
                            globals.entry(name.clone()).or_insert(Global::Var);
                            func.global_stores.push((name, value));
                        } else {
                            if is_container_literal(&value) {
                                func.container_locals.insert(name.clone());
                            }
                            func.locals.entry(name).or_default().push(value);
                        }
                    }
                    BindTarget::Var {
                        scope: Scope::Module,
                        name,
                    } => {
                        globals.entry(name.clone()).or_insert(Global::Var);
                        if let Some(m) = module_func {
                            self.funcs[m as usize].global_stores.push((name, value));
                        }
                    }
                    BindTarget::Member { class, name } => {
                        if let (Some(f), Some(&c)) = (scope_func(&scope), decl_class.get(&class)) {
                            self.funcs[f as usize].member_stores.push((c, name, value));
                        }
                    }
                    BindTarget::FieldOf { object, name } => {
                        if let Some(f) = scope_func(&scope) {
                            self.funcs[f as usize].field_stores.push((object, name, value));
                        }
                    }
                    BindTarget::Field { .. } => {}
                },
                FlowFact::Return { function, value } => {
                    if let Some(&f) = decl_func.get(&function) {
                        self.funcs[f as usize].returns.push(value);
                    }
                }
                FlowFact::Eval { scope, call } => {
                    if let Some(f) = scope_func(&scope) {
                        self.funcs[f as usize].evals.push(call);
                    }
                }
                FlowFact::Decorated {
                    scope,
                    function,
                    decorators,
                    ..
                } => {
                    if let (Some(f), Some(&g)) = (scope_func(&scope), decl_func.get(&function)) {
                        self.funcs[f as usize].decorated.push((g, decorators));
                    }
                }
                FlowFact::ImplicitSelf {
                    function,
                    param,
                    class,
                    is_class,
                } => {
                    if let Some(&f) = decl_func.get(&function) {
                        let func = &mut self.funcs[f as usize];
                        func.self_param = Some(param);
                        func.class_method = is_class;
                        if let Some(&c) = decl_class.get(&class) {
                            func.class = Some(c);
                        }
                    }
                }
            }
        }
        for op in low.ops.iter().cloned() {
            match op {
                LibraryOp::IndexStore {
                    scope,
                    object,
                    key,
                    value,
                } => {
                    if let Some(f) = scope_func(&scope) {
                        let func = &mut self.funcs[f as usize];
                        if let Expr::Name { name, .. } = &object {
                            if !func.is_module && func.locals.contains_key(name) {
                                func.container_locals.insert(name.clone());
                            }
                        }
                        func.index_stores.push((object, key, value));
                    }
                }
                LibraryOp::Iterate {
                    scope,
                    iterable,
                    targets,
                } => {
                    if let Some(f) = scope_func(&scope) {
                        let func = &mut self.funcs[f as usize];
                        for t in targets {
                            func.locals.entry(t).or_default().push(iterable.clone());
                        }
                        func.iterates.push(iterable);
                    }
                }
            }
        }
        // Declared types of parameters and fields (typed receivers, `CallsMethod`).
        for t in &facts.types {
            if t.source != TypeSource::Declared {
                continue;
            }
            match &t.subject {
                TypeSubject::Var {
                    scope: Scope::Decl(d),
                    name,
                } => {
                    let Some(&f) = decl_func.get(d) else { continue };
                    match self.funcs[f as usize].params.iter().position(|p| &p.name == name) {
                        Some(i) => {
                            self.param_types
                                .entry((f, i as u16))
                                .or_insert_with(|| t.type_name.clone());
                        }
                        None => {
                            self.local_types
                                .entry((f, name.clone()))
                                .or_insert_with(|| t.type_name.clone());
                        }
                    }
                }
                TypeSubject::Field { class, name } => {
                    if let Some(&c) = decl_class.get(class) {
                        self.field_types
                            .entry((c, name.clone()))
                            .or_insert_with(|| t.type_name.clone());
                    }
                }
                _ => {}
            }
        }
        // `handler = h;` inside a method of a class declaring the field `handler` stores the
        // field (languages with implicit `this` fields).
        if self.spec.implicit_fields {
            let mut ids: Vec<u32> = decl_func.values().copied().collect();
            ids.sort_unstable();
            for f in ids {
                let Some(c) = self.funcs[f as usize].class else { continue };
                let func = &self.funcs[f as usize];
                let mut names: Vec<String> = func
                    .locals
                    .keys()
                    .filter(|n| {
                        self.field_types.contains_key(&(c, (*n).clone()))
                            && !func.params.iter().any(|p| &p.name == *n)
                    })
                    .cloned()
                    .collect();
                names.sort();
                for name in names {
                    let func = &mut self.funcs[f as usize];
                    let values = func.locals.remove(&name).unwrap_or_default();
                    func.container_locals.remove(&name);
                    for v in values {
                        func.member_stores.push((c, name.clone(), v));
                    }
                }
            }
        }
        let imports: Vec<Imp> = facts
            .imports
            .iter()
            .map(|i| Imp {
                local: i.local.clone(),
                target: i.target.clone(),
                kind: i.kind,
                scope_func: match i.scope {
                    Scope::Module => None,
                    Scope::Decl(d) => decl_func.get(&d).copied(),
                },
                resolved: None,
            })
            .collect();
        let group = self
            .spec
            .namespace_group
            .then(|| path.parent().map(Path::to_path_buf))
            .flatten();
        if let Some(g) = &group {
            self.groups.entry(g.clone()).or_default().push(u);
        }
        let module = (self.spec.module_name)(&path, self.cx.roots);
        self.by_path.insert(path.clone(), u);
        self.units.push(Unit {
            path,
            module,
            file: file.clone(),
            decl_func,
            decl_class,
            globals,
            imports,
            group,
        });
        self.record_exports(u, exports);
        u
    }

    /// Load a file (and its namespace group) unless loaded or over the budget.
    pub(super) fn load_path(
        &mut self,
        path: &Path,
        queue: &mut VecDeque<(u32, usize)>,
        depth: usize,
    ) -> Option<u32> {
        if let Some(&u) = self.by_path.get(path) {
            return Some(u);
        }
        if self.units.len() >= self.cx.limits.max_units {
            return None;
        }
        let file = self
            .cx
            .parsed
            .get(self.language, path, || self.cx.loader.read(path))?;
        let u = self.add_unit(path.to_path_buf(), file);
        queue.push_back((u, depth));
        self.load_group(u, queue, depth);
        Some(u)
    }

    pub(super) fn load_group(&mut self, u: u32, queue: &mut VecDeque<(u32, usize)>, depth: usize) {
        if !self.spec.namespace_group {
            return;
        }
        let path = self.units[u as usize].path.clone();
        for sibling in languages::namespace_files(self.spec, &path, 64).into_iter().skip(1) {
            if self.by_path.contains_key(&sibling) || self.units.len() >= self.cx.limits.max_units {
                continue;
            }
            if let Some(file) = self
                .cx
                .parsed
                .get(self.language, &sibling, || self.cx.loader.read(&sibling))
            {
                let s = self.add_unit(sibling, file);
                queue.push_back((s, depth));
            }
        }
    }

    /// Out-of-line methods, class bases, families, subclasses.
    pub(super) fn link(&mut self) {
        // Methods declared outside their type (Go receivers, Rust impls, extensions).
        for f in 0..self.funcs.len() {
            if self.funcs[f].is_module {
                continue;
            }
            // A receiver typed by the implicit-self fact (Go `func (e *Engine) m()`) is a
            // method of that type too.
            if let Some(c) = self.funcs[f].class {
                let fname = self.funcs[f].name.clone();
                let class = &mut self.classes[c as usize];
                let clauses = class.clauses.entry(fname.clone()).or_default();
                if !clauses.contains(&(f as u32)) {
                    clauses.push(f as u32);
                    class.methods.entry(fname).or_insert(f as u32);
                }
                continue;
            }
            let (u, decl) = (self.funcs[f].unit, self.funcs[f].decl);
            let Some(container) = self.units[u as usize].file.facts.declarations[decl as usize]
                .container
                .clone()
            else {
                continue;
            };
            let name = container.rsplit(['.', ':']).next().unwrap_or(&container).to_string();
            if let Some(c) = self.class_in_scope(u, &name) {
                self.funcs[f].class = Some(c);
                let fname = self.funcs[f].name.clone();
                self.classes[c as usize]
                    .methods
                    .entry(fname.clone())
                    .or_insert(f as u32);
                self.classes[c as usize]
                    .clauses
                    .entry(fname)
                    .or_default()
                    .push(f as u32);
            }
        }
        self.link_objects();
        for c in 0..self.classes.len() {
            let u = self.classes[c].unit;
            let names = self.classes[c].base_names.clone();
            for spelling in names {
                match self.resolve_class_path(u, &spelling) {
                    Ok(b) if b as usize != c => self.classes[c].bases.push(b),
                    Ok(_) => {}
                    Err(Some(q)) => self.classes[c].unresolved_bases.push(q),
                    Err(None) => {}
                }
            }
        }
        for c in 0..self.classes.len() {
            for b in self.classes[c].bases.clone() {
                self.classes[b as usize].subclasses.push(c as u32);
            }
        }
        // Classes one object can be an instance of at the same time: every pair of classes
        // among the ancestors (self included) of one class.
        let mut related: Vec<BTreeSet<u32>> = vec![BTreeSet::new(); self.classes.len()];
        for c in 0..self.classes.len() {
            let lineage = self.lineage(c as u32);
            for &a in &lineage {
                for &b in &lineage {
                    if a != b {
                        related[a as usize].insert(b);
                    }
                }
            }
        }
        self.related = related.into_iter().map(|s| s.into_iter().collect()).collect();
    }
}
