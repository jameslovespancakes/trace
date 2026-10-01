//! Calls resolved to the language's member-lookup hook (DESIGN-bridges §2 rule 7).
//!
//! `lib.compress_buf(1)` on an object whose class has no member `compress_buf` runs the hook
//! (`__getattr__(self, name)`, language specification) with the member NAME, then calls the
//! value it returns with the call's arguments. The hook's derived summary therefore applies
//! to the member name, never to the call's arguments: effects keyed by the hook's name
//! parameter select [`ArgSel::Member`]; every other effect of the hook is dropped (the
//! call's arguments go to the returned value, whose behaviour the hook does not describe).

use trace_core::Language;

use crate::derive::FunctionSummary;
use crate::languages;
use crate::model::{ArgSel, Effect};

/// The summary as seen by a call spelled `spelling`: unchanged unless the summary is the
/// member-lookup hook of `language` reached through another member name.
pub(crate) fn at_call(language: Language, summary: FunctionSummary, spelling: &str) -> FunctionSummary {
    let Some(hook) = languages::adapter(language).and_then(|s| s.member_hook) else {
        return summary;
    };
    let name = summary.qualified.rsplit('.').next().unwrap_or(&summary.qualified);
    if name != hook || spelling == hook || spelling.is_empty() {
        return summary;
    }
    let names_member = |sel: &ArgSel| sel.picks(Some(0), None);
    let effects = summary
        .effects
        .iter()
        .filter_map(|e| match e {
            Effect::Sends { channel, key, verb } if names_member(key) => Some(Effect::Sends {
                channel: *channel,
                key: ArgSel::Member,
                verb: verb.clone(),
            }),
            _ => None,
        })
        .collect();
    FunctionSummary {
        params: Vec::new(),
        effects,
        ..summary
    }
}

#[cfg(test)]
#[path = "../tests/unit/member_hook.rs"]
mod tests;
