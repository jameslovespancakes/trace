//! Chained calls sharing a callee start (`exec.Command("sh", "x.sh").Run()`,
//! `Command::new("x.sh").status()`): the knowledge of a call is keyed by its callee start,
//! which every call of a chain shares. A channel effect belongs to a call that passes the
//! argument its key selects: the outermost such call, else the outermost call.

use trace_core::facts::CallSite;
use trace_library::model::Effect;

use super::{arg_ref, Recognizer, Site};

impl Recognizer<'_, '_> {
    /// The call among those starting at `start` (outermost first) that `effect` applies to.
    pub(super) fn call_for<'f>(&self, site: &Site<'f>, start: u32, effect: &Effect) -> Option<&'f CallSite> {
        let mut at = site.facts.calls.iter().filter(|c| c.callee_span.start == start);
        let first = at.next()?;
        let key = match effect {
            Effect::Sends { key, .. } | Effect::Registers { key, .. } | Effect::Mounts { key, .. } => key,
            _ => return Some(first),
        };
        if arg_ref(key).is_none() {
            return Some(first);
        }
        std::iter::once(first)
            .chain(at)
            .find(|c| self.arg(site, c.callee_span, key).is_some())
            .or(Some(first))
    }
}
