//! Candidates of the solved flow: raw rows per constraint, consumed parameters, library
//! receivers and the merged [`FlowCandidate`]s (child of [`crate::flow`]).

use super::*;

/// Raw candidate row kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum RawKind {
    Flow,
    Implicit,
    Callback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct RawKey {
    pub(super) owner: SymbolId,
    pub(super) file: FileId,
    pub(super) span: ByteSpan,
    pub(super) operation: SiteOperation,
    pub(super) kind: RawKind,
    /// Callback argument span.
    pub(super) arg: Option<ByteSpan>,
    /// Callback target (one row per passed function).
    pub(super) target: Option<SymbolId>,
    /// Composed rows: the parent target this row is reached through.
    pub(super) through: Option<SymbolId>,
}

impl RawKey {
    pub(super) fn new(
        owner: SymbolId,
        file: FileId,
        span: ByteSpan,
        operation: SiteOperation,
        kind: RawKind,
    ) -> RawKey {
        RawKey {
            owner,
            file,
            span,
            operation,
            kind,
            arg: None,
            target: None,
            through: None,
        }
    }
}

/// Candidates of one site under one receiver context of one view.
pub(super) struct Raw {
    pub(super) key: RawKey,
    pub(super) line: u32,
    pub(super) callee: String,
    pub(super) targets: BTreeSet<SymbolId>,
    pub(super) strong: BTreeSet<SymbolId>,
    pub(super) parent: Option<RawKey>,
    pub(super) bounded: bool,
}

struct Row {
    pub(super) key: RawKey,
    pub(super) line: u32,
    pub(super) callee: String,
    pub(super) all: BTreeSet<SymbolId>,
    primary: BTreeSet<SymbolId>,
    pub(super) strong: BTreeSet<SymbolId>,
    pub(super) parent: Option<RawKey>,
    /// Every primary raw (one per receiver context) had at most one target, all strong.
    exact: bool,
    pub(super) bounded: bool,
}

/// Merges raw rows of every context and of both views.
#[derive(Default)]
struct Acc {
    pub(super) rows: Vec<Row>,
    by_key: HashMap<RawKey, usize>,
}

impl Acc {
    pub(super) fn merge(&mut self, raws: Vec<Raw>, primary: bool) {
        for raw in raws {
            let i = match self.by_key.get(&raw.key) {
                Some(&i) => i,
                None => {
                    self.rows.push(Row {
                        key: raw.key,
                        line: raw.line,
                        callee: raw.callee.clone(),
                        all: BTreeSet::new(),
                        primary: BTreeSet::new(),
                        strong: BTreeSet::new(),
                        parent: raw.parent,
                        exact: true,
                        bounded: false,
                    });
                    self.by_key.insert(raw.key, self.rows.len() - 1);
                    self.rows.len() - 1
                }
            };
            let row = &mut self.rows[i];
            if primary {
                row.exact &= raw.targets.len() <= 1 && raw.targets.is_subset(&raw.strong);
                row.primary.extend(raw.targets.iter().copied());
            }
            row.bounded |= raw.bounded;
            row.all.extend(raw.targets);
            row.strong.extend(raw.strong);
        }
    }

    fn finish(self, flow: &Flow<'_>) -> Vec<FlowCandidate> {
        let mut out: Vec<FlowCandidate> = Vec::new();
        let mut placed: HashMap<RawKey, usize> = HashMap::new();
        for row in self.rows {
            if row.all.is_empty() {
                continue;
            }
            let via = match row.parent {
                Some(parent) => match (placed.get(&parent), row.key.through) {
                    (Some(&p), Some(through)) => Some((p, through)),
                    _ => continue,
                },
                None => None,
            };
            let field_only: BTreeSet<SymbolId> =
                if row.key.kind == RawKind::Flow && row.key.operation == SiteOperation::Call {
                    row.all.difference(&row.strong).copied().collect()
                } else {
                    BTreeSet::new()
                };
            let test_only: BTreeSet<SymbolId> = row.all.difference(&row.primary).copied().collect();
            let kind = match row.key.kind {
                RawKind::Flow => CandidateKind::Flow,
                RawKind::Implicit => CandidateKind::Implicit,
                RawKind::Callback => CandidateKind::Callback {
                    arg: row.key.arg.unwrap_or(row.key.span),
                },
            };
            let receiver_exact = row.exact
                && row.parent.is_none()
                && row.key.kind == RawKind::Flow
                && row.key.operation == SiteOperation::Call
                && row.primary.len() >= 2;
            placed.insert(row.key, out.len());
            out.push(FlowCandidate {
                owner: row.key.owner,
                file: row.key.file,
                span: row.key.span,
                line: row.line,
                callee: row.callee,
                candidates: flow.sorted_by_uid(row.all),
                field_only: flow.sorted_by_uid(field_only),
                test_only: flow.sorted_by_uid(test_only),
                operation: row.key.operation,
                kind,
                via,
                receiver_exact,
                bounded: row.bounded,
            });
        }
        out
    }
}

impl<'a> Flow<'a> {
    /// Parameters consumed by their function: called or iterated directly, inside nested
    /// scopes (returned wrappers, closures), by library calls (knowledge), or forwarded to a
    /// consuming parameter of another function.
    pub(super) fn compute_consumed(&self) -> HashMap<(SymbolId, Name), Consume> {
        let ev = self.ev(View::Full);
        let mut consumed: HashMap<(SymbolId, Name), Consume> = HashMap::new();
        let mut forwards: Vec<(ParamKey, Vec<ParamKey>)> = Vec::new();
        for c in &self.constraints {
            let ScopeKey::Symbol(s) = c.scope else { continue };
            match &c.rule {
                Rule::Eval { call } => {
                    if let Node::Name {
                        name, target: None, ..
                    } = &call.func
                    {
                        if let Some(key) = self.name_owner(s, *name) {
                            consumed.entry(key).or_default().call = true;
                        }
                    }
                    let param_args: Vec<(Option<usize>, Option<Name>, ParamKey)> = call
                        .arguments()
                        .filter_map(|(pos, kw, node)| match node {
                            Node::Name {
                                name, target: None, ..
                            } => self.name_owner(s, *name).map(|key| (pos, kw, key)),
                            _ => None,
                        })
                        .collect();
                    if param_args.is_empty() {
                        continue;
                    }
                    let mut callees = Vals::new();
                    for ctx in self.contexts_of(&self.state, c.scope, View::Full) {
                        let sc = Sc {
                            scope: c.scope,
                            ctx,
                            file: c.file,
                        };
                        callees.extend(ev.callees(call, sc, &mut Eff::default()));
                    }
                    for (pos, kw, key) in param_args {
                        if !has_repository_callee(&callees) {
                            let kw_text = kw.map(|k| self.names.text(k));
                            let direct = library_consume(&call.effects, pos, kw_text, call.args.len());
                            if direct.any() {
                                consumed.entry(key).or_default().merge(direct);
                            }
                        }
                        let targets: Vec<(SymbolId, Name)> =
                            callees.iter().flat_map(|v| ev.param_for(*v, pos, kw)).collect();
                        if !targets.is_empty() {
                            forwards.push((key, targets));
                        }
                    }
                }
                Rule::Implicit(i) => {
                    let op = &self.implicit[*i];
                    if op.kind != ImplicitKind::Iterate {
                        continue;
                    }
                    if let Node::Name {
                        name, target: None, ..
                    } = &op.subject
                    {
                        if let Some(key) = self.name_owner(s, *name) {
                            consumed.entry(key).or_default().iterate = true;
                        }
                    }
                }
                _ => {}
            }
        }
        for _ in 0..self.settings.max_consume_rounds {
            let mut changed = false;
            for (key, targets) in &forwards {
                let mut acc = Consume::default();
                for t in targets {
                    if let Some(c) = consumed.get(t) {
                        acc.merge(*c);
                    }
                }
                if acc.any() {
                    changed |= consumed.entry(*key).or_default().merge(acc);
                }
            }
            if !changed {
                break;
            }
        }
        consumed
    }

    /// Owner of the candidates of a constraint: its callable scope, or the file's `<module>`
    /// symbol for module-level code (None when the file has none).
    fn candidate_owner(&self, c: &Constraint) -> Option<SymbolId> {
        match c.scope {
            ScopeKey::Symbol(s) if self.index.symbol(s).kind.is_callable() => Some(s),
            ScopeKey::Symbol(_) => None,
            ScopeKey::Module(f) => self.module_owner.get(f.idx()).copied().flatten(),
        }
    }

    /// Candidate sets for unresolved calls, override dispatch, argument consumption,
    /// implicit operations and receiver-specialised compositions (module docs). Rows are
    /// computed in parallel per constraint and merged in constraint order (deterministic).
    pub fn candidates(&self) -> Vec<FlowCandidate> {
        let per: Vec<(Vec<Raw>, Option<Vec<Raw>>)> = (0..self.constraints.len())
            .into_par_iter()
            .map(|ci| {
                let c = &self.constraints[ci];
                let Some(owner) = self.candidate_owner(c) else {
                    return (Vec::new(), None);
                };
                let field_write = match &c.rule {
                    Rule::Bind {
                        target: Target::FieldOf(_, attr),
                        ..
                    } => self.definers.contains_key(attr),
                    _ => false,
                };
                // Decorator calls (`@app.get("/")`) are calls of their scope too.
                let decorator_calls = match &c.rule {
                    Rule::Decorated { decorators, .. } => {
                        decorators.iter().any(|(d, _, _)| matches!(d, Node::Call(_)))
                    }
                    _ => false,
                };
                if !field_write
                    && !decorator_calls
                    && !matches!(c.rule, Rule::Eval { .. } | Rule::Implicit(_))
                {
                    return (Vec::new(), None);
                }
                if self.has_tests && !c.test {
                    (self.raws(View::Product, c, owner), Some(self.raws(View::Full, c, owner)))
                } else {
                    (self.raws(View::Full, c, owner), None)
                }
            })
            .collect();
        let mut acc = Acc::default();
        for (primary, secondary) in per {
            acc.merge(primary, true);
            if let Some(full) = secondary {
                acc.merge(full, false);
            }
        }
        acc.finish(self)
    }

    /// Call sites whose callee's receiver is only ever an object a library created
    /// (`Index::library_receivers`, sorted, deterministic):
    /// * an unresolved member call `recv.m(..)` whose receiver evaluates (strictly: no
    ///   field-name evidence; every receiver context; values of test code included) to
    ///   library objects only, when repository code never stored `m` on them nor made them
    ///   delegate to other objects (a patched library object keeps repository members),
    ///   or to repository instances / classes whose member `m` only a library base declares
    ///   (rule "member of a library base", [`library_bases`]);
    /// * an unresolved call of a parameter (`next()`, `done()`) of a function that only
    ///   library code runs (it is passed to library positions that call it, never called or
    ///   passed on by repository code) and that no repository value reaches: the library
    ///   passes that argument, so the called function is the library's.
    ///
    /// The evidence is the library symbol that created the object / runs the function.
    pub fn library_receivers(&self) -> Vec<LibraryReceiver> {
        let library_run = self.library_run_functions();
        let per: Vec<Option<LibraryReceiver>> = (0..self.constraints.len())
            .into_par_iter()
            .map(|ci| self.library_receiver(ci, &library_run))
            .collect();
        let mut out: Vec<LibraryReceiver> = per.into_iter().flatten().collect();
        out.sort();
        out.dedup();
        out
    }

    /// The library receiver entry of one constraint ([`Flow::library_receivers`]).
    fn library_receiver(
        &self,
        ci: usize,
        library_run: &HashMap<SymbolId, String>,
    ) -> Option<LibraryReceiver> {
        let c = &self.constraints[ci];
        let Rule::Eval { call } = &c.rule else {
            return None;
        };
        let owner = self.candidate_owner(c)?;
        if !self.proven(owner, c.file, call.func_span).is_empty() {
            return None;
        }
        if !self.call_sites.contains_key(&(c.file, call.func_span)) {
            return None;
        }
        let ev = self.ev(View::Full);
        let contexts = self.contexts_of(&self.state, c.scope, View::Full);
        let library = match &call.func {
            Node::Attr {
                object,
                attr,
                target: None,
                ..
            } => {
                let mut libs: BTreeSet<u32> = BTreeSet::new();
                // Members of library bases of repository classes ([`library_bases`]).
                let mut members: BTreeSet<&str> = BTreeSet::new();
                let attr_text = self.names.text(*attr);
                for ctx in contexts {
                    let sc = Sc {
                        scope: c.scope,
                        ctx,
                        file: c.file,
                    };
                    let objs = ev.eval(object, sc, &mut Eff::strict());
                    if objs.is_empty() {
                        return None;
                    }
                    for v in objs.iter() {
                        match v {
                            Value::Library(l) => {
                                libs.insert(*l);
                            }
                            Value::Instance(Obj { class, .. }) | Value::Class(class) => {
                                if !ev.attr_values(&Vals::one(*v), *attr, &mut Eff::strict()).is_empty() {
                                    return None;
                                }
                                members.insert(self.library_member(*class, attr_text)?);
                            }
                            _ => return None,
                        }
                    }
                }
                for &l in &libs {
                    let patched = ev.slot(Slot::Prop(Holder::Lib(l), *attr)).is_some()
                        || ev.slot(Slot::Delegates(Holder::Lib(l))).is_some();
                    if patched {
                        return None;
                    }
                }
                libs.iter()
                    .filter_map(|&l| self.libraries.get(l as usize).map(String::as_str))
                    .chain(members)
                    .min()
                    .map(str::to_string)?
            }
            Node::Name {
                name, target: None, ..
            } => {
                let ScopeKey::Symbol(scope) = c.scope else {
                    return None;
                };
                let (function, _) = self.name_owner(scope, *name)?;
                let library = library_run.get(&function)?;
                for ctx in contexts {
                    let sc = Sc {
                        scope: c.scope,
                        ctx,
                        file: c.file,
                    };
                    if !ev.eval(&call.func, sc, &mut Eff::strict()).is_empty() {
                        return None;
                    }
                }
                library.clone()
            }
            _ => return None,
        };
        let (line, _) = self.site_text(c.file, call.func_span);
        Some(LibraryReceiver {
            at: Location {
                file: c.file,
                bytes: call.func_span,
                line,
            },
            library,
        })
    }

    /// Functions only library code runs, with the library that runs them (the smallest
    /// library symbol when several do): every function value passed as an argument to a
    /// library position that calls it (library knowledge), unless repository code also calls
    /// it (proven edges, value-flow callees), passes it to a repository callee or to a call
    /// without such knowledge, or decorates it.
    fn library_run_functions(&self) -> HashMap<SymbolId, String> {
        type Seen = (Vec<(SymbolId, u32)>, Vec<SymbolId>);
        let per: Vec<Seen> = (0..self.constraints.len())
            .into_par_iter()
            .map(|ci| {
                let c = &self.constraints[ci];
                let mut run: Vec<(SymbolId, u32)> = Vec::new();
                let mut other: Vec<SymbolId> = Vec::new();
                match &c.rule {
                    Rule::Eval { call } => {
                        let ev = self.ev(View::Full);
                        for ctx in self.contexts_of(&self.state, c.scope, View::Full) {
                            let sc = Sc {
                                scope: c.scope,
                                ctx,
                                file: c.file,
                            };
                            let mut eff = Eff::strict();
                            let callees = ev.callees(call, sc, &mut eff);
                            other.extend(function_ids(&callees));
                            let repository = has_repository_callee(&callees);
                            let library = if repository {
                                None
                            } else {
                                call.library.or_else(|| {
                                    callees.iter().find_map(|v| match v {
                                        Value::Library(l) => Some(*l),
                                        _ => None,
                                    })
                                })
                            };
                            let positional = call.args.len();
                            for (pos, kw, node) in call.arguments() {
                                let passed = function_ids(&ev.eval(node, sc, &mut eff));
                                if passed.is_empty() {
                                    continue;
                                }
                                let kw_text = kw.map(|k| self.names.text(k));
                                let runs = !repository
                                    && library_consume(&call.effects, pos, kw_text, positional).call;
                                for f in passed {
                                    match library.filter(|_| runs) {
                                        Some(l) => run.push((f, l)),
                                        None => other.push(f),
                                    }
                                }
                            }
                        }
                    }
                    Rule::Decorated { function, .. } => other.push(*function),
                    _ => {}
                }
                (run, other)
            })
            .collect();
        let mut other: HashSet<SymbolId> = self
            .calls
            .values()
            .flat_map(|list| list.iter().map(|&(_, t)| t))
            .collect();
        let mut run: HashMap<SymbolId, String> = HashMap::new();
        for (r, o) in per {
            other.extend(o);
            for (f, l) in r {
                let Some(text) = self.libraries.get(l as usize) else {
                    continue;
                };
                let entry = run.entry(f).or_insert_with(|| text.clone());
                if *text < *entry {
                    *entry = text.clone();
                }
            }
        }
        run.retain(|f, _| !other.contains(f));
        run
    }

    pub(super) fn raws(&self, view: View, c: &Constraint, owner: SymbolId) -> Vec<Raw> {
        let mut out = Vec::new();
        for ctx in self.contexts_of(&self.state, c.scope, view) {
            let sc = Sc {
                scope: c.scope,
                ctx,
                file: c.file,
            };
            let rec = RefCell::new(Rec::default());
            let ev = Ev {
                f: self,
                st: &self.state,
                view,
                rec: Some(&rec),
                memo: None,
            };
            let start = out.len();
            match &c.rule {
                Rule::Eval { call } => ev.call_raws(owner, call, sc, &mut out),
                Rule::Implicit(i) => ev.implicit_raws(&self.implicit[*i], sc, &mut out),
                Rule::Bind {
                    target: Target::FieldOf(object, attr),
                    ..
                } => ev.write_raws(owner, object, *attr, sc, &mut out),
                Rule::Decorated { decorators, .. } => {
                    for (d, _, _) in decorators {
                        if let Node::Call(call) = d {
                            ev.call_raws(owner, call, sc, &mut out);
                        }
                    }
                }
                _ => {}
            }
            if rec.into_inner().saturated {
                for r in &mut out[start..] {
                    r.bounded = true;
                }
            }
        }
        out
    }
}
