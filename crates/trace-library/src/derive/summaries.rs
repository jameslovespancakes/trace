//! Summaries of a derivation (child of `derive`): parameter selectors, effect masks and
//! channel facts turned into the [`FunctionSummary`] of every function of the target file.

use super::*;

impl<'c> Program<'c> {
    pub(super) fn selector(&self, f: u32, i: u16) -> Option<ArgSel> {
        let func = &self.funcs[f as usize];
        let p = func.params.get(i as usize)?;
        let offset = usize::from(
            func.params
                .first()
                .is_some_and(|p0| func.self_param.as_deref() == Some(p0.name.as_str())),
        );
        let idx = (i as usize).checked_sub(offset)? as u32;
        Some(match p.kind {
            ParamKind::Positional if self.spec.keyword_args => ArgSel::PosOrKw(idx, p.name.clone()),
            ParamKind::Positional => ArgSel::Pos(idx),
            ParamKind::KeywordOnly | ParamKind::VarKeyword => ArgSel::Kw(p.name.clone()),
            ParamKind::VarPositional => ArgSel::Rest(idx),
        })
    }

    pub(super) fn verb_sel(&self, st: &State, f: u32, verb: &Verb) -> VerbSel {
        match verb {
            Verb::Any => VerbSel::Any,
            Verb::Const(l) => VerbSel::Const(st.lits.get(*l as usize).cloned().unwrap_or_default()),
            Verb::Param(i) => self.selector(f, *i).map(VerbSel::Arg).unwrap_or(VerbSel::Any),
        }
    }

    pub(super) fn mask_effects(mask: u8, sel: &ArgSel) -> Vec<Effect> {
        let mut out = Vec::new();
        for e in ALL_EFFECTS {
            if mask & e == 0 {
                continue;
            }
            out.push(match e {
                CALLS => Effect::Calls(sel.clone()),
                STORED => Effect::StoredThenCalled(sel.clone()),
                ITERATES => Effect::Iterates(sel.clone()),
                RETURNS => Effect::Returns(sel.clone()),
                WRAPS => Effect::Wraps(sel.clone()),
                _ => Effect::Property(sel.clone()),
            });
        }
        out
    }

    pub(super) fn chan_effect(&self, st: &State, f: u32, c: &Chan) -> Option<Effect> {
        Some(match c {
            Chan::RegistersSelf { .. } => return None,
            Chan::Sends { channel, key, verb } => Effect::Sends {
                channel: *channel,
                key: self.selector(f, *key)?,
                verb: self.verb_sel(st, f, verb),
            },
            Chan::Registers {
                channel,
                key,
                handler,
                verb,
            } => Effect::Registers {
                channel: *channel,
                key: self.selector(f, *key)?,
                handler: self.handler_selector(f, *handler)?,
                verb: self.verb_sel(st, f, verb),
            },
            Chan::Mounts { key, target } => Effect::Mounts {
                key: self.selector(f, *key)?,
                target: self.selector(f, *target)?,
            },
            Chan::MountsSelf { key } => Effect::Mounts {
                key: self.selector(f, *key)?,
                target: ArgSel::Receiver,
            },
            Chan::DecoratesEffect { effect, param } => Effect::Decorates {
                inner: Box::new(Self::mask_effects(*effect, &ArgSel::Pos(u32::from(*param))).pop()?),
            },
            Chan::DecoratesRegisters {
                channel,
                key,
                handler,
                verb,
            } => Effect::Decorates {
                inner: Box::new(Effect::Registers {
                    channel: *channel,
                    key: self.selector(f, *key)?,
                    handler: ArgSel::Pos(u32::from(*handler)),
                    verb: self.verb_sel(st, f, verb),
                }),
            },
        })
    }

    pub(super) fn function_summary(
        &self,
        st: &State,
        f: u32,
        symbol: String,
        line: u32,
        column: u32,
        qualified: String,
    ) -> FunctionSummary {
        let func = &self.funcs[f as usize];
        let mut params = Vec::new();
        for i in 0..func.params.len() as u16 {
            let m = st.mask(f, i);
            if m == 0 {
                continue;
            }
            let Some(sel) = self.selector(f, i) else { continue };
            let effects = Self::mask_effects(m, &sel);
            params.push((sel, effects));
        }
        // Methods called on parameters (`CallsMethod`).
        for (_, i, method) in st
            .pmethods
            .range((f, 0, String::new())..)
            .take_while(|(g, _, _)| *g == f)
        {
            let Some(sel) = self.selector(f, *i) else { continue };
            let effect = Effect::CallsMethod {
                arg: sel.clone(),
                method: method.clone(),
            };
            match params.iter_mut().find(|(x, _)| *x == sel) {
                Some((_, list)) => {
                    if !list.contains(&effect) {
                        list.push(effect);
                    }
                }
                None => params.push((sel, vec![effect])),
            }
        }
        // Keywords collected by `**kwargs` and forwarded to a callee that runs them.
        for ((_, keyword), m) in st
            .kw_masks
            .range((f, String::new())..)
            .take_while(|((g, _), _)| *g == f)
        {
            let sel = ArgSel::Kw(keyword.clone());
            let effects = Self::mask_effects(*m, &sel);
            if !effects.is_empty() && !params.iter().any(|(x, _)| *x == sel) {
                params.push((sel, effects));
            }
        }
        let mut effects: Vec<Effect> = st
            .chan
            .get(&f)
            .into_iter()
            .flatten()
            .filter_map(|c| self.chan_effect(st, f, c))
            .collect();
        for (_, from, to) in st.copies.range((f, 0, 0)..).take_while(|(g, _, _)| *g == f) {
            if let (Some(from), Some(to)) = (self.selector(f, *from), self.selector(f, *to)) {
                let effect = Effect::CopiesMembers { from, to };
                if !effects.contains(&effect) {
                    effects.push(effect);
                }
            }
        }
        typed::prefer_named_verbs(&mut effects);
        FunctionSummary {
            symbol,
            qualified,
            line,
            column,
            params,
            effects,
        }
    }

    pub(super) fn summaries(&self, st: &mut State, root: u32) -> FileSummaries {
        let mut out = FileSummaries::default();
        for (f, func) in self.funcs.iter().enumerate() {
            if func.unit != root || func.is_module {
                continue;
            }
            let (line, column) = self.position(root, func.decl);
            let summary = self.function_summary(
                st,
                f as u32,
                self.func_symbol(f as u32),
                line,
                column,
                func.qualified.clone(),
            );
            // Clauses / overloads / redefinitions of one symbol: one summary (the first
            // declaration's position, every clause's effects).
            match out.functions.get_mut(&summary.symbol) {
                Some(existing) => merge_summary(existing, summary),
                None => {
                    out.functions.insert(summary.symbol.clone(), summary);
                }
            }
        }
        for (c, class) in self.classes.iter().enumerate() {
            if class.unit != root || self.is_synthetic(c as u32) {
                continue;
            }
            let (line, column) = self.position(root, class.decl);
            let symbol = self.class_symbol(c as u32);
            let mut merged = FunctionSummary {
                symbol: symbol.clone(),
                qualified: class.qualified.clone(),
                line,
                column,
                ..FunctionSummary::default()
            };
            for g in self.ctors(st, c as u32) {
                let s = self.function_summary(st, g, String::new(), 0, 0, String::new());
                merge_summary(&mut merged, s);
            }
            out.functions.entry(symbol).or_insert(merged);
        }
        out
    }
}

/// Merge the effects of `other` into `into` (parameters by selector, then function-level
/// effects); `into` keeps its symbol and position.
fn merge_summary(into: &mut FunctionSummary, other: FunctionSummary) {
    for (sel, effects) in other.params {
        match into.params.iter_mut().find(|(x, _)| *x == sel) {
            Some((_, existing)) => {
                for e in effects {
                    if !existing.contains(&e) {
                        existing.push(e);
                    }
                }
            }
            None => into.params.push((sel, effects)),
        }
    }
    for e in other.effects {
        if !into.effects.contains(&e) {
            into.effects.push(e);
        }
    }
}
