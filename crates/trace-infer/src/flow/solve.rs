//! The solver: building the constraint system, the two-phase fixpoint (product, then
//! test constraints), diagnostics and the shared queries over the solution (child of
//! [`crate::flow`]).

use super::*;

/// Evaluation budget of one solve (module docs).
pub(super) fn eval_budget(flow: &FlowSettings, constraints: usize) -> u64 {
    flow.eval_budget
        .unwrap_or(flow.eval_budget_base + flow.eval_budget_per_constraint * constraints as u64)
}

impl<'a> Flow<'a> {
    /// [`Flow::solve_rules`] without library knowledge or injection rules.
    pub fn solve(index: &'a Index, hierarchy: &'a Hierarchy) -> Flow<'a> {
        Self::solve_rules(index, hierarchy, &LibraryKnowledge::default(), &[])
    }

    /// Collect constraints from every file's `FlowFact`s (library effects from `knowledge`,
    /// by-name injection bindings from `rules`) and solve to a fixpoint (product code first,
    /// then with test code; module docs).
    pub(crate) fn solve_rules(
        index: &'a Index,
        hierarchy: &'a Hierarchy,
        knowledge: &LibraryKnowledge,
        rules: &[Injection],
    ) -> Flow<'a> {
        let mut names = Interner::default();
        // Stub declarations (`.pyi`) have no body: at run time their implementation runs
        // (the proven stub -> implementation language rule).
        let mut implementations: HashMap<SymbolId, Vec<SymbolId>> = HashMap::new();
        for e in index.edges.iter().filter(|e| e.kind == EdgeKind::StubImplementation) {
            implementations.entry(e.from).or_default().push(e.to);
        }
        let runtime = |t: SymbolId| -> &[SymbolId] { implementations.get(&t).map_or(&[], Vec::as_slice) };
        let refs: HashMap<(FileId, ByteSpan), SymbolId> = index
            .value_refs
            .iter()
            .map(|r| match runtime(r.target) {
                [only] => ((r.at.file, r.at.bytes), *only),
                _ => ((r.at.file, r.at.bytes), r.target),
            })
            .collect();
        let mut lambdas: HashMap<(FileId, ByteSpan), SymbolId> = HashMap::new();
        for s in &index.symbols {
            if s.kind.is_callable() {
                lambdas.entry((s.file, s.span.bytes)).or_insert(s.id);
            }
        }

        let mut calls: HashMap<(SymbolId, FileId), Vec<(u32, SymbolId)>> = HashMap::new();
        let mut executed: HashMap<(SymbolId, FileId), Vec<(u32, SymbolId)>> = HashMap::new();
        for e in &index.edges {
            let call = CALL_KINDS.contains(&e.kind);
            if !call && e.kind != EdgeKind::PropertyGet {
                continue;
            }
            let key = (e.from, e.at.file);
            let point = e.at.bytes.start;
            let targets = std::iter::once(e.to).chain(runtime(e.to).iter().copied());
            if call {
                calls
                    .entry(key)
                    .or_default()
                    .extend(targets.clone().map(|t| (point, t)));
            }
            executed.entry(key).or_default().extend(targets.map(|t| (point, t)));
        }
        for list in calls.values_mut().chain(executed.values_mut()) {
            list.sort_unstable();
            list.dedup();
        }

        let params: Vec<Vec<Name>> = index
            .symbols
            .iter()
            .map(|s| {
                if s.kind.is_callable() {
                    s.parameters.iter().map(|p| names.intern(p)).collect()
                } else {
                    Vec::new()
                }
            })
            .collect();

        let test_files = test_files(index);
        let tests = test_symbols(index);

        let mut constraints = Vec::new();
        let mut implicit = Vec::new();
        let mut call_sites = HashMap::new();
        let mut selfs = HashMap::new();
        let mut anonymous = HashSet::new();
        let mut libraries: Vec<String> = Vec::new();
        let mut library_ids: HashMap<String, u32> = HashMap::new();
        let mut module_owner = Vec::with_capacity(index.files.len());
        let js_objects = JsObjects::new(index);
        let exports_name = js_objects.as_ref().map(|m| names.intern(m.exports_name()));
        for (fi, record) in index.files.iter().enumerate() {
            let Some(facts) = &record.facts else {
                module_owner.push(None);
                continue;
            };
            module_owner.push(facts.module_decl.and_then(|d| record.symbol_of_decl(d)));
            let file = FileId(fi as u32);
            for (ci, c) in facts.calls.iter().enumerate() {
                call_sites.entry((file, c.callee_span)).or_insert(ci as u32);
            }
            let data_model = trace_syntax::syntax(record.language).is_some_and(|s| s.implicit_ops);
            let guards: HashMap<ByteSpan, &[Expr]> = facts
                .calls
                .iter()
                .zip(&facts.call_details)
                .filter(|(_, d)| !d.not_identical.is_empty())
                .map(|(c, d)| (c.span, d.not_identical.as_slice()))
                .collect();
            let library_at: HashMap<ByteSpan, String> = record
                .semantic
                .as_ref()
                .map(|s| {
                    s.library_calls
                        .iter()
                        .filter_map(|c| {
                            let text = c.symbol.clone().or_else(|| {
                                s.library_files.get(c.file as usize).map(|f| f.package.clone())
                            })?;
                            Some((c.at, text))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut compiler = Compiler {
                index,
                names: &mut names,
                refs: &refs,
                lambdas: &lambdas,
                anonymous: &mut anonymous,
                guards,
                knowledge: file_knowledge(knowledge, &record.path),
                file,
                super_call: super_call(record.language),
                library_at,
                loaders: facts.imports.iter().map(|i| i.span).collect(),
                libraries: &mut libraries,
                library_ids: &mut library_ids,
                js_objects: js_objects.as_ref().filter(|_| JsObjects::applies(index, file)),
            };
            for fact in &facts.flow {
                if let FlowFact::ImplicitSelf {
                    function,
                    param,
                    class,
                    is_class,
                } = fact
                {
                    if let Some((f, sp)) = compiler.self_param(*function, param, *class, *is_class) {
                        selfs.insert(f, sp);
                    }
                } else if let Some(c) = compiler.constraint(fact) {
                    constraints.push(c);
                }
            }
            // A JavaScript file binding no module value exports its implicit `exports` object.
            if let (Some(m), Some(n)) = (compiler.js_objects, exports_name) {
                if m.implicit_exports(file) {
                    constraints.push(Constraint {
                        rule: Rule::Bind {
                            target: Target::Var(ScopeKey::Module(file), n),
                            value: Node::Object {
                                alloc: IMPLICIT_EXPORTS_ALLOC,
                                span: NOWHERE,
                                fields: Vec::new(),
                            },
                        },
                        scope: ScopeKey::Module(file),
                        file,
                        test: false,
                    });
                }
            }
            // Data-model operations: languages with implicit operations only (Python).
            if !data_model {
                continue;
            }
            for op in &facts.implicit {
                let Scope::Decl(d) = op.scope else { continue };
                let Some(owner) = record.symbol_of_decl(d) else {
                    continue;
                };
                implicit.push(Implicit {
                    owner,
                    file,
                    kind: op.kind,
                    subject: compiler.node(&op.subject),
                    span: op.span,
                    line: op.line,
                });
                constraints.push(Constraint {
                    rule: Rule::Implicit(implicit.len() - 1),
                    scope: ScopeKey::Symbol(owner),
                    file,
                    test: false,
                });
            }
        }
        constraints.extend(injection_binds(
            index,
            &mut names,
            &selfs,
            rules,
            &mut libraries,
            &mut library_ids,
        ));
        drop(refs);
        drop(lambdas);

        let mut locals = HashSet::new();
        let mut evals_by_scope: HashMap<SymbolId, Vec<usize>> = HashMap::new();
        for (i, c) in constraints.iter_mut().enumerate() {
            c.test = match c.scope {
                ScopeKey::Module(f) => test_files[f.idx()],
                ScopeKey::Symbol(s) => tests[s.idx()],
            };
            match (&c.rule, c.scope) {
                (
                    Rule::Bind {
                        target: Target::Var(ScopeKey::Symbol(s), n),
                        ..
                    },
                    ScopeKey::Symbol(scope),
                ) if *s == scope => {
                    locals.insert((scope, *n));
                }
                (Rule::Eval { .. }, ScopeKey::Symbol(s)) => {
                    evals_by_scope.entry(s).or_default().push(i);
                }
                _ => {}
            }
        }

        let mros: HashMap<SymbolId, Vec<SymbolId>> = index
            .symbols
            .iter()
            .filter(|s| s.kind.is_type())
            .map(|s| (s.id, hierarchy.mro(s.id)))
            .collect();
        let mut families: HashMap<SymbolId, Vec<SymbolId>> = HashMap::new();
        for sp in selfs.values() {
            families.entry(sp.class).or_insert_with(|| {
                let mut f: Vec<SymbolId> = hierarchy.family(sp.class).into_iter().collect();
                f.sort_unstable();
                f
            });
        }
        let has_tests = constraints.iter().any(|c| c.test);
        let mut definers: HashMap<Name, HashSet<SymbolId>> = HashMap::new();
        for (&class, methods) in &hierarchy.methods {
            for name in methods.keys() {
                definers.entry(names.intern(name)).or_default().insert(class);
            }
        }
        for c in &constraints {
            let target = match &c.rule {
                Rule::Bind { target, .. } | Rule::Decorated { target, .. } => target,
                _ => continue,
            };
            if let Target::Member(class, n) = target {
                definers.entry(*n).or_default().insert(*class);
            }
        }

        let mut flow = Flow {
            index,
            hierarchy,
            iterations: 0,
            names,
            constraints,
            implicit,
            calls,
            executed,
            params,
            selfs,
            anonymous,
            locals,
            local_reads: scoping::local_reads(index),
            call_sites,
            mros,
            families,
            evals_by_scope,
            module_owner,
            state: State::default(),
            has_tests,
            consumed: HashMap::new(),
            definers,
            libraries,
            settings: trace_core::config::current().flow.clone(),
            prototypes: Vec::new(),
            delegation: false,
            library_bases: library_bases::library_bases(index, knowledge),
            exports_name,
        };
        flow.prototypes = Language::ALL
            .iter()
            .map(|&l| match trace_library::languages::adapter(l).and_then(|a| a.objects.as_ref()) {
                Some(model) => PrototypeNames {
                    link: flow.names.get(model.prototype_link),
                    prototype: flow.names.get(model.prototype),
                },
                None => PrototypeNames::default(),
            })
            .collect();
        flow.delegation = flow.prototypes.iter().any(|p| p.link.is_some())
            || js_objects.as_ref().is_some_and(JsObjects::linked)
            || knowledge
                .by_call
                .values()
                .any(|b| b.effects.iter().any(links_members));
        flow.state = flow.solve_state();
        flow.iterations = flow.state.stats.rounds_product.max(flow.state.stats.rounds_full) as usize;
        flow.consumed = flow.compute_consumed();
        if trace_core::config::current().debug.fixtures {
            flow.debug_fixture_slots();
        }
        flow
    }

    /// Setting `debug.fixtures`: the values by-name injection bindings delivered (stderr).
    fn debug_fixture_slots(&self) {
        for c in &self.constraints {
            let Rule::Bind {
                target: Target::Var(ScopeKey::Symbol(f), n),
                value,
            } = &c.rule
            else {
                continue;
            };
            let ev = self.ev(View::Full);
            let (provider, ret) = match value {
                Node::Call(call) if call.func_span == NOWHERE => match &call.func {
                    Node::Name { target: Some(t), .. } => (
                        *t,
                        ev.slot(Slot::Return(*t, Recv::Unknown))
                            .map_or(0, |v| v.iter().count()),
                    ),
                    _ => continue,
                },
                Node::Yielded { function, name } => (
                    *function,
                    ev.slot(Slot::VarAll(ScopeKey::Symbol(*function), *name))
                        .map_or(0, |v| v.iter().count()),
                ),
                _ => continue,
            };
            let vals = ev
                .slot(Slot::Default(*f, *n))
                .map(SlotRef::to_vals)
                .unwrap_or_default();
            let fixture = self.index.symbol(provider).uid.clone();
            eprintln!(
                "fixture: {} {} <- {} (returns {}) = {:?}",
                self.index.symbol(*f).uid,
                self.names.text(*n),
                fixture,
                ret,
                vals.iter().map(|v| format!("{v:?}")).collect::<Vec<_>>()
            );
        }
    }

    /// Both solver phases over one state (module docs).
    fn solve_state(&self) -> State {
        let mut st = State {
            limits: self.settings.clone(),
            ..State::default()
        };
        for (&f, sp) in &self.selfs {
            st.seed_context(f, sp.declared());
        }
        let mut items: Vec<Vec<Item>> = Vec::with_capacity(self.constraints.len());
        items.resize_with(self.constraints.len(), Vec::new);
        let mut budget = eval_budget(&self.settings, self.constraints.len());
        st.stats.constraints = self.constraints.len() as u64;
        st.testing = false;
        st.stats.rounds_product = self.run_phase(&mut st, &mut items, View::Product, &mut budget);
        if self.has_tests {
            st.testing = true;
            st.stats.rounds_full = self.run_phase(&mut st, &mut items, View::Full, &mut budget);
        }
        st.stats.items = items.iter().map(|v| v.len() as u64).sum();
        drop(items);
        st.stats.contexts = st.contexts.values().map(|v| v.len() as u64).sum();
        let (slots, values) = st
            .ids
            .iter()
            .filter(|(d, _)| matches!(d, Dep::Slot(_)))
            .map(|(_, &id)| st.cells[id as usize].len() as u64)
            .filter(|&n| n > 0)
            .fold((0u64, 0u64), |(s, v), n| (s + 1, v + n));
        st.stats.slots = slots;
        st.stats.values = values;
        st
    }

    /// Rounds over the constraints, evaluating only dirty items (module docs). Returns the
    /// number of rounds used.
    fn run_phase(&self, st: &mut State, items: &mut [Vec<Item>], view: View, budget: &mut u64) -> u64 {
        let mut rounds = 0u64;
        let memo = RefCell::new(LookupMemo::default());
        let trace_items = trace_core::config::current().debug.flow_items;
        let profile = trace_core::env::profile();
        let started = std::time::Instant::now();
        for _ in 0..self.settings.max_iterations {
            rounds += 1;
            if profile {
                let items_n: usize = items.iter().map(Vec::len).sum();
                let deps_n: usize = items.iter().flatten().map(|it| it.deps.len()).sum();
                eprintln!(
                    "profile-flow: {view:?} round {rounds} evals={} skipped={} items={items_n} deps={deps_n} cells={} contexts={} elapsed={:.3}s",
                    st.stats.evals,
                    st.stats.skipped,
                    st.cells.len(),
                    st.contexts.values().map(Vec::len).sum::<usize>(),
                    started.elapsed().as_secs_f64()
                );
            }
            let mut changed = false;
            for (ci, c) in self.constraints.iter().enumerate() {
                if c.test && view == View::Product {
                    continue;
                }
                for ctx in self.contexts_of(st, c.scope, view) {
                    let pos = items[ci].iter().position(|it| it.ctx == ctx);
                    if let Some(p) = pos {
                        if !st.is_dirty(&items[ci][p]) {
                            st.stats.skipped += 1;
                            continue;
                        }
                    }
                    if *budget == 0 {
                        st.stats.budget_exhausted = true;
                        return rounds;
                    }
                    *budget -= 1;
                    st.clock += 1;
                    let t = st.clock;
                    if trace_items {
                        eprintln!(
                            "flow-item: {ci} {} {:?} {:?}",
                            self.index.file_path(c.file),
                            ctx,
                            std::mem::discriminant(&c.rule)
                        );
                    }
                    let rec = RefCell::new(Rec::default());
                    let sc = Sc {
                        scope: c.scope,
                        ctx,
                        file: c.file,
                    };
                    let eff = Ev {
                        f: self,
                        st: &*st,
                        view,
                        rec: Some(&rec),
                        memo: Some(&memo),
                    }
                    .transfer(c, sc);
                    st.stats.evals += 1;
                    let deps = st.deps_of(rec.into_inner());
                    match pos {
                        Some(p) => {
                            let it = &mut items[ci][p];
                            it.at = t;
                            it.deps = deps;
                        }
                        None => items[ci].push(Item { ctx, at: t, deps }),
                    }
                    changed |= st.apply(eff, t);
                }
            }
            if !changed {
                break;
            }
        }
        rounds
    }

    /// Work and size counters of this solve.
    pub fn stats(&self) -> &FlowStats {
        &self.state.stats
    }

    /// `flow_bound` diagnostics: saturated slots (candidates beyond the cap may be missing,
    /// affected sites are marked truncated) and an exhausted evaluation budget.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let s = &self.state.stats;
        let mut out = Vec::new();
        if s.saturated_slots > 0 || s.redirected_attributes > 0 {
            let examples: Vec<String> = self
                .state
                .bound_slots
                .iter()
                .map(|slot| self.describe(*slot))
                .collect();
            out.push(Diagnostic::new(
                "flow_bound",
                None,
                format!(
                    "{} value-flow slot(s) exceeded {} values after widening and \
                     were capped ({} widened, {} attribute writes of further allocations went \
                     to the class's any-object slot); candidate sets read from them are marked \
                     truncated. Examples: {}",
                    s.saturated_slots,
                    self.settings.max_slot_values,
                    s.widened_slots,
                    s.redirected_attributes,
                    if examples.is_empty() {
                        "-".to_string()
                    } else {
                        examples.join(", ")
                    }
                ),
            ));
        }
        if s.budget_exhausted {
            out.push(Diagnostic::new(
                "flow_bound",
                None,
                format!(
                    "value-flow evaluation budget exhausted after {} evaluations; the flow \
                     solution is partial (some flow candidates may be missing)",
                    s.evals
                ),
            ));
        }
        out
    }

    pub(super) fn describe(&self, slot: Slot) -> String {
        let n = |x: Name| self.names.text(x).to_string();
        let sym = |s: SymbolId| self.index.symbol(s).uid.clone();
        match slot {
            Slot::Var(ScopeKey::Symbol(s), _, x) | Slot::VarAll(ScopeKey::Symbol(s), x) => {
                format!("var {}:{}", sym(s), n(x))
            }
            Slot::Var(ScopeKey::Module(f), _, x) | Slot::VarAll(ScopeKey::Module(f), x) => {
                format!("var {}:{}", self.index.file_path(f), n(x))
            }
            Slot::Default(s, x) => format!("default {}:{}", sym(s), n(x)),
            Slot::Member(c, x) => format!("member {}.{}", sym(c), n(x)),
            Slot::Field(x) => format!("field .{}", n(x)),
            Slot::ObjAttr(o, x) => format!("attribute {}.{}", sym(o.class), n(x)),
            Slot::ClassAttr(c, x) => format!("class attribute {}.{}", sym(c), n(x)),
            Slot::Return(s, _) => format!("return {}", sym(s)),
            Slot::Prop(h, x) => format!("property {}.{}", self.describe_holder(h), n(x)),
            Slot::Delegates(h) => format!("delegates of {}", self.describe_holder(h)),
        }
    }

    fn describe_holder(&self, h: Holder) -> String {
        let at = |a: Alloc| match a {
            Alloc::Any => "?".to_string(),
            Alloc::At(f, b) => format!("{}@{b}", self.index.file_path(f)),
        };
        match h {
            Holder::Obj(a) => format!("object {}", at(a)),
            Holder::Fn(f) => self.index.symbol(f).uid.clone(),
            Holder::Lib(l) => {
                format!("library object {}", self.libraries.get(l as usize).map_or("?", String::as_str))
            }
            Holder::Inst(o) => format!("{} instance {}", self.index.symbol(o.class).uid, at(o.alloc)),
        }
    }

    pub(super) fn contexts_of(&self, st: &State, scope: ScopeKey, view: View) -> Vec<Recv> {
        match scope {
            ScopeKey::Symbol(f) => match self.selfs.get(&f) {
                Some(sp) => {
                    let visible: Vec<Recv> = st
                        .contexts
                        .get(&f)
                        .map(|l| l.iter().filter(|e| view == View::Full || !e.1).map(|e| e.0).collect())
                        .unwrap_or_default();
                    if visible.is_empty() {
                        vec![sp.declared()]
                    } else {
                        visible
                    }
                }
                None => vec![Recv::Unknown],
            },
            ScopeKey::Module(_) => vec![Recv::Unknown],
        }
    }

    pub(super) fn mro(&self, class: SymbolId) -> &[SymbolId] {
        match self.mros.get(&class) {
            Some(m) => m,
            None => std::slice::from_ref(&self.index.symbols[class.idx()].id),
        }
    }

    pub(super) fn is_subclass(&self, class: SymbolId, base: SymbolId) -> bool {
        self.mro(class).contains(&base)
    }

    /// Parameters bound by arguments of a bound call (receiver parameter dropped).
    pub(super) fn call_params(&self, f: SymbolId) -> &[Name] {
        let params: &[Name] = &self.params[f.idx()];
        match (self.selfs.get(&f), params.split_first()) {
            (Some(sp), Some((&first, rest))) if first == sp.name => rest,
            _ => params,
        }
    }

    pub(super) fn is_generator(&self, s: SymbolId) -> bool {
        matches!(self.index.symbol(s).execution, ExecutionModel::Generator | ExecutionModel::AsyncGenerator)
    }

    pub(super) fn points(
        map: &HashMap<(SymbolId, FileId), Vec<(u32, SymbolId)>>,
        owner: SymbolId,
        file: FileId,
        span: ByteSpan,
    ) -> BTreeSet<SymbolId> {
        map.get(&(owner, file))
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .filter(|(p, _)| span.contains(*p))
            .map(|&(_, t)| t)
            .collect()
    }

    /// Proven call targets whose evidence point lies in `span`.
    pub(super) fn proven(&self, owner: SymbolId, file: FileId, span: ByteSpan) -> BTreeSet<SymbolId> {
        Self::points(&self.calls, owner, file, span)
    }

    /// Class-hierarchy overrides of proven method targets.
    pub(super) fn cha(&self, proven: &BTreeSet<SymbolId>) -> BTreeSet<SymbolId> {
        let mut out = BTreeSet::new();
        for &t in proven {
            let Some(class) = self.hierarchy.class_of(self.index, t) else {
                continue;
            };
            let name = self.index.symbol(t).name.as_str();
            for k in self.hierarchy.family(class) {
                if k != class {
                    out.extend(self.hierarchy.method(k, name));
                }
            }
        }
        out
    }

    /// The function and parameter a lexical name refers to, when it is a parameter of the
    /// scope or (through closures) of an enclosing callable.
    pub(super) fn name_owner(&self, scope: SymbolId, name: Name) -> Option<(SymbolId, Name)> {
        let mut s = scope;
        // Bounded walk: a malformed (cyclic) parent chain must not hang the solver.
        for _ in 0..=self.index.symbols.len() {
            if self.selfs.get(&s).is_some_and(|sp| sp.name == name) {
                return None;
            }
            if self.params[s.idx()].contains(&name) {
                return Some((s, name));
            }
            if self.locals.contains(&(s, name)) {
                return None;
            }
            let parent = self.index.symbol(s).parent?;
            if !self.index.symbol(parent).kind.is_callable() {
                return None;
            }
            s = parent;
        }
        None
    }

    pub(super) fn arg_span(&self, node: &Node) -> Option<ByteSpan> {
        match node {
            Node::Name { span, .. } | Node::Attr { span, .. } => Some(*span),
            Node::Call(c) => Some(c.span),
            Node::Lambda(s) => Some(self.index.symbol(*s).span.bytes),
            Node::Object { span, .. } => Some(*span),
            Node::Choice(_) | Node::Yielded { .. } | Node::Literal | Node::Module(_) | Node::Opaque => None,
        }
    }

    /// Span of the write reference named `name` right after an object expression ending at
    /// `end` (`obj.name`, `obj->name`, `obj . name`): the attribute identifier of a store.
    pub(super) fn write_reference(&self, file: FileId, end: u32, name: &str) -> Option<ByteSpan> {
        let facts = self.index.file(file).facts.as_ref()?;
        facts
            .references
            .iter()
            .filter(|r| {
                r.kind == trace_core::facts::RefKind::Write
                    && r.name == name
                    && r.span.start >= end
                    && r.span.start <= end + 8
            })
            .min_by_key(|r| r.span.start)
            .map(|r| r.span)
    }

    pub(super) fn sorted_by_uid(&self, set: impl IntoIterator<Item = SymbolId>) -> Vec<SymbolId> {
        let mut v: Vec<SymbolId> = set.into_iter().collect();
        v.sort_by(|a, b| self.index.symbol(*a).uid.cmp(&self.index.symbol(*b).uid));
        v.dedup();
        v
    }

    pub(super) fn site_text(&self, file: FileId, span: ByteSpan) -> (u32, String) {
        self.call_sites
            .get(&(file, span))
            .and_then(|&ci| {
                let facts = self.index.file(file).facts.as_ref()?;
                let c = facts.calls.get(ci as usize)?;
                Some((c.line, c.callee.clone()))
            })
            .unwrap_or_default()
    }

    pub(super) fn ev(&self, view: View) -> Ev<'_, 'a> {
        Ev {
            f: self,
            st: &self.state,
            view,
            rec: None,
            memo: None,
        }
    }
}
