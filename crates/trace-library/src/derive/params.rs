//! Value flow through parameters (child of `derive`).
//!
//! Rule "callable parameters carry reachability": a function value (or a slot's content) passed
//! for a parameter runs when a function calling that parameter runs (a wrapper factory's inner
//! closure calling the wrapped function: `wrap(handler)` returns a closure that awaits
//! `handler`), so entry reachability flows from the parameter's callers to the passed
//! functions and slots, not from the function that built the wrapper (it runs at registration
//! time).
//!
//! Rule "stores through parameters": a slot's content passed for a parameter that the callee
//! (or a function it passes the parameter on to) stores into another slot flows into that slot
//! (`describe(call=self.endpoint)` storing `call` into `Record.call`): the source slot
//! is dispatched when the destination is.

use super::{How, Program, State, CALLS, V, WRAPS};

impl<'c> Program<'c> {
    /// `f` calls parameter `i` of `p` (its own or a captured one).
    pub(super) fn note_param_caller(&self, st: &mut State, f: u32, p: u32, i: u16, how: How) {
        if how == How::Built {
            return;
        }
        if st.param_callers.entry((p, i)).or_default().insert(f) {
            st.changed = true;
        }
    }

    /// `f` stores parameter `i` of `p` (its own or a captured one) into slot `s`.
    pub(super) fn note_param_store(&self, st: &mut State, p: u32, i: u16, how: How, s: u32) {
        if how == How::Built {
            return;
        }
        if st.param_slots.entry((p, i)).or_default().insert(s) {
            st.changed = true;
        }
    }

    /// Argument `vals` bound to parameter `j` of `g` at a call in `f`.
    pub(super) fn bind_param_flow(
        &self,
        st: &mut State,
        f: u32,
        g: u32,
        j: u16,
        arg: &trace_core::facts::Expr,
    ) {
        let callers: Vec<u32> = if st.mask(g, j) & (CALLS | WRAPS) != 0 {
            st.param_callers.get(&(g, j)).into_iter().flatten().copied().collect()
        } else {
            Vec::new()
        };
        let slots: Vec<u32> = st.param_slots.get(&(g, j)).into_iter().flatten().copied().collect();
        if callers.is_empty() && slots.is_empty() {
            return;
        }
        for v in self.eval(st, f, arg, 0) {
            match v {
                V::Fn(x, _) => {
                    for &c in &callers {
                        if c != x {
                            st.edges.insert((c, x));
                        }
                    }
                }
                V::Param(p, i, how @ (How::Direct | How::Wrapped)) if p == f || self.is_ancestor(p, f) => {
                    for &c in &callers {
                        self.note_param_caller(st, c, p, i, how);
                    }
                    for &s in &slots {
                        self.note_param_store(st, p, i, how, s);
                    }
                }
                V::Slot(from, How::Direct | How::Wrapped) => {
                    for &c in &callers {
                        st.add_caller(from, c);
                    }
                    for &s in &slots {
                        if s != from && st.slot_flow.insert((from, s)) {
                            st.changed = true;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
