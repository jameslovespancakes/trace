//! Raw candidate rows of the evaluator: calls, argument consumption, writes and implicit
//! operations (child of [`crate::flow`]).

use super::*;

impl<'s> Ev<'s, '_> {
    /// Parameters of the callee value receiving argument `pos` / `kw`.
    pub(super) fn param_for(&self, v: Value, pos: Option<usize>, kw: Option<Name>) -> Vec<(SymbolId, Name)> {
        let one = |f: SymbolId, bound: bool| -> Option<(SymbolId, Name)> {
            let params: &[Name] = if bound {
                self.f.call_params(f)
            } else {
                &self.f.params[f.idx()]
            };
            match (pos, kw) {
                (Some(i), _) => params.get(i).map(|&p| (f, p)),
                (None, Some(k)) => params.contains(&k).then_some((f, k)),
                _ => None,
            }
        };
        match v {
            Value::Function(f) => one(f, false).into_iter().collect(),
            Value::Bound(f, _) => one(f, true).into_iter().collect(),
            Value::Class(c) => self
                .constructors(c)
                .into_iter()
                .filter_map(|k| one(k, true))
                .collect(),
            Value::Instance(o) => self
                .call_methods(o.class)
                .into_iter()
                .filter_map(|k| one(k, true))
                .collect(),
            Value::Generator(_) | Value::Property(_) | Value::Object(_) | Value::Library(_) => Vec::new(),
        }
    }

    /// Whether `object` is the scope's receiver parameter in its declared context.
    fn is_declared_self(&self, object: &Node, sc: Sc) -> bool {
        let (
            Node::Name {
                name, target: None, ..
            },
            ScopeKey::Symbol(f),
        ) = (object, sc.scope)
        else {
            return false;
        };
        self.f
            .selfs
            .get(&f)
            .is_some_and(|sp| sp.name == *name && sc.ctx == sp.declared())
    }

    /// Whether a callee expression reads through the receiver parameter `self_name`.
    fn rooted_at(node: &Node, self_name: Name) -> bool {
        match node {
            Node::Attr { object, .. } => match object.as_ref() {
                Node::Name { name, .. } => *name == self_name,
                inner @ Node::Attr { .. } => Self::rooted_at(inner, self_name),
                _ => false,
            },
            _ => false,
        }
    }

    /// Targets of the unresolved receiver-rooted calls inside method `m` when it runs with
    /// the concrete receiver `r` (strict: no field-name evidence).
    pub(super) fn specialised(&self, m: SymbolId, r: Recv) -> BTreeSet<SymbolId> {
        let mut out = BTreeSet::new();
        if !r.is_specific() {
            return out;
        }
        let Some(sp) = self.f.selfs.get(&m) else {
            return out;
        };
        let ctx = self.ctx_for(m, r);
        if !ctx.is_specific() {
            return out;
        }
        for &i in self.f.evals_by_scope.get(&m).map(Vec::as_slice).unwrap_or(&[]) {
            let c = &self.f.constraints[i];
            let Rule::Eval { call } = &c.rule else { continue };
            if !Self::rooted_at(&call.func, sp.name) || !self.f.proven(m, c.file, call.func_span).is_empty() {
                continue;
            }
            let sc = Sc {
                scope: c.scope,
                ctx,
                file: c.file,
            };
            let mut strict = Eff::strict();
            out.extend(self.call_targets(&self.callees(call, sc, &mut strict)));
        }
        out.remove(&m);
        out
    }

    fn push_specialised(
        &self,
        pairs: &[(SymbolId, Recv)],
        parent: RawKey,
        line: u32,
        callee: &str,
        out: &mut Vec<Raw>,
    ) {
        let mut seen: HashSet<(SymbolId, Recv)> = HashSet::new();
        for &(m, r) in pairs {
            if !seen.insert((m, r)) {
                continue;
            }
            let spec = self.specialised(m, r);
            if spec.is_empty() {
                continue;
            }
            out.push(Raw {
                key: RawKey {
                    operation: SiteOperation::Call,
                    kind: RawKind::Flow,
                    through: Some(m),
                    ..parent
                },
                line,
                callee: callee.to_string(),
                targets: spec.clone(),
                strong: spec,
                parent: Some(parent),
                bounded: false,
            });
        }
    }

    pub(super) fn call_raws(&self, owner: SymbolId, call: &CallNode, sc: Sc, out: &mut Vec<Raw>) {
        let f = self.f;
        let (file, span) = (sc.file, call.func_span);
        let (line, callee) = f.site_text(file, span);
        let mut eff = Eff::default();
        let proven = f.proven(owner, file, span);
        let callees = self.callees(call, sc, &mut eff);
        if !proven.is_empty() {
            // Resolved method call: other overrides for the receivers (member lookup only,
            // no field-based guesses); CHA only for unknown or virtual self receivers.
            if let Node::Attr { object, attr, .. } = &call.func {
                let is_super = matches!(object.as_ref(), Node::Call(c) if c.is_super())
                    || self.super_class(object, sc).is_some();
                // Only methods dispatch: a call the compiler binds to a module-level
                // function (`pkg.json.dumps(..)`) has no overrides, whatever values the
                // receiver expression may hold by field name.
                let dispatches = proven.iter().any(|t| f.hierarchy.class_of(f.index, *t).is_some());
                if !is_super && dispatches {
                    let objs = self.eval(object, sc, &mut eff);
                    let mut extra = function_ids(&self.attr_values(&objs, *attr, &mut eff));
                    let known = objs.iter().any(|v| matches!(v, Value::Class(_) | Value::Instance(_)));
                    if !known || self.is_declared_self(object, sc) {
                        extra.extend(f.cha(&proven));
                    }
                    extra.retain(|t| !proven.contains(t));
                    if !extra.is_empty() {
                        out.push(Raw {
                            key: RawKey::new(
                                owner,
                                file,
                                span,
                                SiteOperation::OverrideDispatch,
                                RawKind::Flow,
                            ),
                            line,
                            callee: callee.clone(),
                            targets: extra.clone(),
                            strong: extra,
                            parent: None,
                            bounded: false,
                        });
                    }
                }
            }
        } else {
            let mut targets = self.call_targets(&callees);
            let overrides = self.subclass_members(&call.func, sc);
            targets.extend(overrides.iter().copied());
            if !targets.is_empty() {
                // Candidates that survive without the field-name fallback have specific
                // evidence; the rest are weaker (field-only) evidence.
                let mut strict = Eff::strict();
                let mut strong = self.call_targets(&self.callees(call, sc, &mut strict));
                strong.extend(overrides);
                let parent = RawKey::new(owner, file, span, SiteOperation::Call, RawKind::Flow);
                out.push(Raw {
                    key: parent,
                    line,
                    callee: callee.clone(),
                    targets,
                    strong,
                    parent: None,
                    bounded: false,
                });
                let bound: Vec<(SymbolId, Recv)> = callees
                    .iter()
                    .filter_map(|v| match *v {
                        Value::Bound(m, r) => Some((m, r)),
                        _ => None,
                    })
                    .collect();
                self.push_specialised(&bound, parent, line, &callee, out);
            }
        }
        self.argument_raws(owner, call, &callees, sc, line, &callee, out);
    }

    /// Targets of an unresolved member call through the method's own receiver in its declared
    /// context (`self.response_class(...)`) under every subclass of the receiver class: a
    /// subclass may bind the member (a class-valued attribute, a stored function)
    /// differently, and the method runs for subclass instances too (virtual self reads, like
    /// class-hierarchy dispatch of virtual self-calls).
    fn subclass_members(&self, func: &Node, sc: Sc) -> BTreeSet<SymbolId> {
        let mut out = BTreeSet::new();
        let Node::Attr {
            object,
            attr,
            target: None,
            ..
        } = func
        else {
            return out;
        };
        let ScopeKey::Symbol(f) = sc.scope else {
            return out;
        };
        if !self.is_declared_self(object, sc) {
            return out;
        }
        let Some(sp) = self.f.selfs.get(&f).copied() else {
            return out;
        };
        let Some(family) = self.f.families.get(&sp.class) else {
            return out;
        };
        for &k in family {
            if k == sp.class {
                continue;
            }
            let receiver = if sp.is_class {
                Value::Class(k)
            } else {
                Value::Instance(Obj::any(k))
            };
            let vals = self.attr_values(&Vals::one(receiver), *attr, &mut Eff::strict());
            out.extend(self.call_targets(&vals));
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn argument_raws(
        &self,
        owner: SymbolId,
        call: &CallNode,
        callees: &Vals,
        sc: Sc,
        line: u32,
        callee: &str,
        out: &mut Vec<Raw>,
    ) {
        let f = self.f;
        let repository = has_repository_callee(callees);
        let effects: &[Effect] = if repository { &[] } else { &call.effects };
        if !repository && effects.is_empty() {
            return;
        }
        let positional = call.args.len();
        let mut eff = Eff::default();
        for (pos, kw, node) in call.arguments() {
            let Some(arg_span) = f.arg_span(node) else {
                continue;
            };
            let kw_text = kw.map(|k| f.names.text(k));
            let mut consume = library_consume(effects, pos, kw_text, positional);
            // Library `calls_method` positions: the named method of each passed object runs.
            let methods: Vec<&str> = effects
                .iter()
                .filter_map(|e| match e {
                    Effect::CallsMethod { arg, method } if selects(arg, pos, kw_text, positional) => {
                        Some(method.as_str())
                    }
                    _ => None,
                })
                .collect();
            if !methods.is_empty() {
                let vals = self.eval(node, sc, &mut eff);
                let mut targets = BTreeSet::new();
                for m in &methods {
                    targets.extend(self.method_callees(&vals, m).into_iter().map(|(t, _)| t));
                }
                for t in targets {
                    out.push(Raw {
                        key: RawKey {
                            arg: Some(arg_span),
                            target: Some(t),
                            ..RawKey::new(
                                owner,
                                sc.file,
                                call.func_span,
                                SiteOperation::Call,
                                RawKind::Callback,
                            )
                        },
                        line,
                        callee: callee.to_string(),
                        targets: BTreeSet::from([t]),
                        strong: BTreeSet::from([t]),
                        parent: None,
                        bounded: false,
                    });
                }
            }
            for v in callees {
                for key in self.param_for(*v, pos, kw) {
                    if let Some(c) = f.consumed.get(&key) {
                        consume.merge(*c);
                    }
                }
            }
            if !consume.any() {
                continue;
            }
            let vals = self.eval(node, sc, &mut eff);
            if vals.is_empty() {
                continue;
            }
            if consume.call {
                for t in self.call_targets(&vals) {
                    out.push(Raw {
                        key: RawKey {
                            arg: Some(arg_span),
                            target: Some(t),
                            ..RawKey::new(
                                owner,
                                sc.file,
                                call.func_span,
                                SiteOperation::Call,
                                RawKind::Callback,
                            )
                        },
                        line,
                        callee: callee.to_string(),
                        targets: BTreeSet::from([t]),
                        strong: BTreeSet::from([t]),
                        parent: None,
                        bounded: false,
                    });
                }
            }
            if consume.iterate || consume.advance {
                let pairs = self.iterate_targets(&vals, !consume.iterate, &mut eff);
                let set: BTreeSet<SymbolId> = pairs.iter().map(|p| p.0).collect();
                if set.is_empty() {
                    continue;
                }
                let parent = RawKey::new(owner, sc.file, arg_span, SiteOperation::Iterate, RawKind::Implicit);
                out.push(Raw {
                    key: parent,
                    line,
                    callee: String::new(),
                    targets: set.clone(),
                    strong: set,
                    parent: None,
                    bounded: false,
                });
                self.push_specialised(&pairs, parent, line, "", out);
            }
        }
    }

    /// Methods an attribute store `object.attr = value` overwrites: the first member named
    /// `attr` along the MRO of each class the object's tracked values belong to. Emitted
    /// only with specific evidence (the strict evaluation finds the same methods) and a
    /// write reference at the attribute identifier (the site span runs from the object's
    /// start to the identifier's end).
    pub(super) fn write_raws(&self, owner: SymbolId, object: &Node, attr: Name, sc: Sc, out: &mut Vec<Raw>) {
        let f = self.f;
        let Some(end) = node_end(object) else { return };
        let text = f.names.text(attr);
        let Some(attr_span) = f.write_reference(sc.file, end, text) else {
            return;
        };
        let methods = |eff: &mut Eff| -> BTreeSet<SymbolId> {
            let mut set = BTreeSet::new();
            let mut seen: Vec<SymbolId> = Vec::new();
            for v in &self.eval(object, sc, eff) {
                let class = match *v {
                    Value::Instance(o) => o.class,
                    Value::Class(k) => k,
                    _ => continue,
                };
                if seen.contains(&class) {
                    continue;
                }
                seen.push(class);
                match self.first_member(class, attr) {
                    Some(Err(m)) => {
                        set.insert(m);
                    }
                    Some(Ok(vals)) => set.extend(
                        function_ids(vals.iter())
                            .into_iter()
                            .filter(|m| f.hierarchy.class_of(f.index, *m).is_some()),
                    ),
                    None => {}
                }
            }
            set
        };
        let mut eff = Eff::default();
        let targets = methods(&mut eff);
        if targets.is_empty() {
            return;
        }
        let mut strict = Eff::strict();
        let strong = methods(&mut strict);
        if strong != targets {
            return;
        }
        let start = node_start(object).unwrap_or(attr_span.start).min(attr_span.start);
        let span = ByteSpan::new(start, attr_span.end);
        let (line, callee) = f.site_text(sc.file, span);
        out.push(Raw {
            key: RawKey::new(owner, sc.file, span, SiteOperation::FieldWrite, RawKind::Flow),
            line,
            callee,
            targets: targets.clone(),
            strong: targets,
            parent: None,
            bounded: false,
        });
    }

    pub(super) fn implicit_raws(&self, op: &Implicit, sc: Sc, out: &mut Vec<Raw>) {
        let mut eff = Eff::default();
        let pairs = self.implicit_targets(op, sc, &mut eff);
        let executed = Flow::points(&self.f.executed, op.owner, op.file, op.span);
        let set: BTreeSet<SymbolId> = pairs.iter().map(|p| p.0).filter(|t| !executed.contains(t)).collect();
        if set.is_empty() {
            return;
        }
        let parent = RawKey::new(op.owner, op.file, op.span, implicit_operation(op.kind), RawKind::Implicit);
        out.push(Raw {
            key: parent,
            line: op.line,
            callee: String::new(),
            targets: set.clone(),
            strong: set.clone(),
            parent: None,
            bounded: false,
        });
        let kept: Vec<(SymbolId, Recv)> = pairs.into_iter().filter(|p| set.contains(&p.0)).collect();
        self.push_specialised(&kept, parent, op.line, "", out);
    }
}
