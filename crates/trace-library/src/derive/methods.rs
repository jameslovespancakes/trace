//! Receiver methods over container vocabulary (child of `derive`).
//!
//! Rule "a receiver's own method wins over the container vocabulary": `get`, `put`, `add`,
//! `pop` ... are container reads / stores only when the typed receiver's class defines no
//! method of that name (`self.router.get(path)` on a router instance calls `Router.get`, not a
//! dictionary read).

use trace_core::facts::Expr;

use super::{Program, State, Vals, V};

impl<'c> Program<'c> {
    /// Instances among the typed receiver values whose class defines `attr`.
    pub(super) fn method_receivers(&self, st: &State, raw: &Vals, attr: &str) -> Vals {
        self.typed(st, raw)
            .into_iter()
            .filter(|v| matches!(v, V::Inst(c) | V::Class(c) if self.find_method(st, *c, attr).is_some()))
            .collect()
    }

    /// Whether `object.attr(..)` calls a method its typed receiver defines.
    pub(super) fn receiver_defines(&self, st: &mut State, f: u32, object: &Expr, attr: &str) -> bool {
        let raw = self.eval(st, f, object, 0);
        !self.method_receivers(st, &raw, attr).is_empty()
    }
}
