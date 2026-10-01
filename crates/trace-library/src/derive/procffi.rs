//! Process and FFI derivation rules (DESIGN-bridges §2 rules 1, 7, 9): generic language
//! features the spawn / symbol-lookup chains of installed libraries go through.
//!
//! * **Positional spread forwarding**: `g(*rest)` / `g(...rest)` where `rest` is this
//!   function's variadic parameter passes this function's positional arguments from the
//!   spread position on; a channel key of `g` at exactly that position is this function's
//!   rest selector (its first element). Other offsets are unknown and give nothing.
//! * **Sent fields** (process and FFI channels): a key argument that is the content of a
//!   slot (`lp := c.Path; StartProcess(lp, ..)`) makes the slot *sent*; a function storing
//!   its own parameter into a sent slot (a constructor / factory filling the field a method
//!   later sends) sends that parameter. Slot flows carry the mark back to the slots feeding
//!   the sent one.
//! * **Records**: an object literal naming its entries is a record; `r.name` reads that
//!   field only (`const parsed = parse(file); spawn(parsed.file)`: a sent field).
//! * **Base construction without source**: constructing a class that has no constructor of
//!   its own and bases without readable source constructs those bases (their rows apply to
//!   the call's arguments).

use std::collections::BTreeSet;

use trace_core::facts::{Expr, ParamKind};
use trace_core::model::ByteSpan;
use trace_syntax::lower::{CONTAINER_CALLEE, KEYWORD_SPREAD, POSITIONAL_SPREAD};

use super::{Chan, How, Program, SlotKey, SlotOwner, State, Verb, V};
use crate::model::Channel;

impl Program<'_> {
    /// The parameter of `g` a trailing positional spread of this function's own variadic
    /// parameter starts at (`(parameter, spread expression)`), when the spread's first
    /// element lands exactly on it: a positional parameter at the spread position, or `g`'s
    /// own variadic parameter when the spread starts right after `g`'s positional ones.
    pub(super) fn spread_binding<'e>(
        &self,
        st: &mut State,
        f: u32,
        g: u32,
        skip: bool,
        args: &[Expr],
        kwargs: &'e [(String, Expr)],
    ) -> Option<(u16, &'e Expr)> {
        let (_, spread) = kwargs.iter().find(|(k, _)| k == POSITIONAL_SPREAD)?;
        let own = &self.funcs[f as usize].params;
        let own_rest = self.eval(st, f, spread, 0).into_iter().any(|v| {
            matches!(v, V::Param(p, i, How::Direct)
                if p == f && own.get(i as usize).is_some_and(|q| q.kind == ParamKind::VarPositional))
        });
        if !own_rest {
            return None;
        }
        let callee = &self.funcs[g as usize];
        let receiver = skip
            && callee
                .params
                .first()
                .is_some_and(|p| callee.self_param.as_deref() == Some(p.name.as_str()));
        let positional: Vec<usize> = (usize::from(receiver)..callee.params.len())
            .filter(|&i| callee.params[i].kind == ParamKind::Positional)
            .collect();
        let at = args.len();
        let j = match positional.get(at) {
            Some(&j) => j,
            None if at == positional.len() => callee
                .params
                .iter()
                .position(|p| p.kind == ParamKind::VarPositional)?,
            None => return None,
        };
        // A keyword argument naming the same parameter wins (Python raises otherwise).
        if kwargs.iter().any(|(k, _)| *k == callee.params[j].name) {
            return None;
        }
        Some((j as u16, spread))
    }

    /// Record that the content of the slots among `vals` is sent on `channel`: the whole key
    /// of a process start (the program) or of a symbol lookup (the name). HTTP / message keys
    /// held by a field are prefixes the sender extends (base URLs), never a whole key.
    pub(super) fn send_slots(&self, st: &mut State, vals: &BTreeSet<V>, channel: Channel, verb: &Verb) {
        if !matches!(channel, Channel::Process | Channel::Ffi) {
            return;
        }
        let verb = match verb {
            Verb::Param(_) => Verb::Any,
            v => v.clone(),
        };
        for v in vals {
            if let V::Slot(s, How::Direct | How::Wrapped) = *v {
                if st.slot_sends.entry(s).or_default().insert((channel, verb.clone())) {
                    st.changed = true;
                }
            }
        }
    }

    /// Sent fields: slots feeding a sent slot are sent; a parameter stored into a sent slot
    /// by its own function is sent by that function.
    pub(super) fn propagate_slot_sends(&self, st: &mut State) {
        if st.slot_sends.is_empty() {
            return;
        }
        for _ in 0..16 {
            let mut grew = false;
            for &(from, to) in &st.slot_flow {
                let Some(sent) = st.slot_sends.get(&to).cloned() else { continue };
                let entry = st.slot_sends.entry(from).or_default();
                for s in sent {
                    grew |= entry.insert(s);
                }
            }
            if !grew {
                break;
            }
            st.changed = true;
        }
        let stored: Vec<(u32, u32, V)> = st.stored.iter().copied().collect();
        for (s, f, v) in stored {
            let V::Param(p, i, _) = v else { continue };
            if p != f {
                continue;
            }
            let Some(sent) = st.slot_sends.get(&s).cloned() else { continue };
            for (channel, verb) in sent {
                st.add_chan(
                    f,
                    Chan::Sends {
                        channel,
                        key: i,
                        verb,
                    },
                );
            }
        }
    }

    /// Construction of class `c` without a constructor of its own whose bases have no
    /// readable source: the bases' construction rows apply to the call.
    pub(super) fn construct_unresolved_bases(
        &self,
        st: &mut State,
        f: u32,
        c: u32,
        func: &Expr,
        args: &[Expr],
        kwargs: &[(String, Expr)],
    ) {
        if !self.ctors(st, c).is_empty() {
            return;
        }
        let symbols = self.unresolved_bases(c);
        if symbols.is_empty() {
            return;
        }
        let mut effects = Vec::new();
        for s in &symbols {
            effects.extend(self.cx.leaves.by_symbol(self.language, s, args.len() as u32));
        }
        self.call_leaf(st, f, func, &effects, &symbols, args, kwargs);
    }
}

impl Program<'_> {
    /// Records: an object literal naming its entries (`{file, args: a}`) is a record whose
    /// fields are slots of that literal; each evaluation stores the entries' values there.
    /// Reading `r.file` of the record is the field's slot (never the other entries).
    pub(super) fn record_value(
        &self,
        st: &mut State,
        f: u32,
        func: &Expr,
        kwargs: &[(String, Expr)],
        at: ByteSpan,
        depth: usize,
    ) -> Option<V> {
        match func {
            Expr::Name { name, .. } if name == CONTAINER_CALLEE => {}
            _ => return None,
        }
        let fields: Vec<&(String, Expr)> = kwargs
            .iter()
            .filter(|(k, _)| k != KEYWORD_SPREAD && k != POSITIONAL_SPREAD)
            .collect();
        if fields.is_empty() {
            return None;
        }
        let u = self.funcs[f as usize].unit;
        let next = st.records.len() as u32;
        let id = *st.records.entry((u, at.start)).or_insert(next);
        for (name, value) in fields {
            let s = st.slot(SlotKey {
                owner: SlotOwner::Record(id),
                name: name.clone(),
            });
            let vals = self.eval(st, f, value, depth + 1);
            self.store(st, f, &[s], &vals, None);
        }
        Some(V::Rec(id))
    }
}

#[cfg(test)]
#[path = "../../tests/unit/derive/procffi.rs"]
mod tests;
