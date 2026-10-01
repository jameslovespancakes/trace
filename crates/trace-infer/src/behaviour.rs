//! Library behaviour as inference reads it (DESIGN §1.10 item 6, §1.10a).
//!
//! trace-library answers, per library call (file, callee start byte), what the callee does
//! with the arguments passed to it ([`trace_library::LibraryKnowledge`]): derived from the
//! installed library source (tier from the language's precision gate), from a declared
//! function type, or from the tiny native table. This module is the only reader:
//!
//! * [`selects`]: whether an effect's argument selector names a given argument of the call
//!   (positional index / keyword, as the syntax facts record them, `CallbackArg::{index,
//!   keyword}`);
//! * [`effect_for`]: the effect a library call has on one argument, with `Decorates`
//!   unwrapped for decorator applications (`app.route("/")(f)`, `@app.route("/")`);
//! * [`behaviour_for`]: the [`LibraryBehaviour`] attached to a callback site
//!   (`Site::library`), including the "library call without evidence" case;
//! * [`runs`]: which effects run the passed function (decided `inferred` when the fact is
//!   inferred), and [`NEVER_CALLS`].
//!
//! Library facts are never `proven`. In-index callees never consult this module (value flow
//! and the language rules decide them).

use trace_core::model::{ByteSpan, FileId, LibraryBehaviour, UnresolvedKind};
use trace_core::Index;
use trace_library::{ArgSel, CallBehaviour, Effect, LibraryKnowledge};

/// `LibraryBehaviour::source` of a library call without any evidence (no derived fact, no
/// declared type, no table row).
pub const NO_EVIDENCE: &str = "none";
/// `LibraryBehaviour::effect` when no effect concerns the argument.
pub(crate) const NO_EFFECT: &str = "none";
/// `LibraryBehaviour::effect` of a function-typed parameter that is never called (table row;
/// overrides the function-type rule).
pub(crate) const NEVER_CALLS: &str = "never_calls";

/// Whether selector `sel` names the argument at positional `index` / `keyword` of a call
/// with `positional` positional arguments.
///
/// `NamedBy(i)` / `Command(i)` / `Code(i)` name the function through the argument at
/// position `i` (a string, command word or code string), so they select that argument.
/// `Receiver` and `Field` select no argument
/// value that is itself the passed function.
pub fn selects(sel: &ArgSel, index: Option<usize>, keyword: Option<&str>, positional: usize) -> bool {
    let at = |i: u32| index == Some(i as usize);
    match sel {
        ArgSel::Pos(i) => at(*i),
        ArgSel::Kw(k) => keyword == Some(k.as_str()),
        ArgSel::PosOrKw(i, k) => at(*i) || keyword == Some(k.as_str()),
        ArgSel::Rest(i) => index.is_some_and(|j| j >= *i as usize),
        ArgSel::Last => index.is_some_and(|j| positional > 0 && j + 1 == positional),
        ArgSel::NamedBy(i) | ArgSel::Command(i) | ArgSel::Code(i) => at(*i),
        ArgSel::Receiver | ArgSel::Field { .. } | ArgSel::Result | ArgSel::Member => false,
    }
}

/// The argument selector an effect applies to (`None` for effects without one).
pub fn selector(effect: &Effect) -> Option<&ArgSel> {
    match effect {
        Effect::Calls(a)
        | Effect::StoredThenCalled(a)
        | Effect::Iterates(a)
        | Effect::Advances(a)
        | Effect::Returns(a)
        | Effect::Wraps(a)
        | Effect::Partial(a)
        | Effect::Property(a)
        | Effect::NeverCalls(a) => Some(a),
        Effect::CallsMethod { arg, .. } => Some(arg),
        Effect::Registers { handler, .. } => Some(handler),
        Effect::Receiver
        | Effect::Sends { .. }
        | Effect::Mounts { .. }
        | Effect::Decorates { .. }
        | Effect::Exports { .. }
        | Effect::CopiesMembers { .. }
        | Effect::DelegatesMembers { .. } => None,
    }
}

/// Effects that run the passed function (now or later): called, stored and later called,
/// a method of it called, registered as a handler of a dispatch registry.
pub fn runs(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Calls(_)
            | Effect::StoredThenCalled(_)
            | Effect::CallsMethod { .. }
            | Effect::Registers { .. }
    )
}

/// Whether an effect name (`LibraryBehaviour::effect`) runs the passed function.
pub(crate) fn runs_name(effect: &str) -> bool {
    matches!(effect, "calls" | "stored_then_called" | "calls_method" | "registers")
}

/// The effects of a call as they apply to its arguments: for a decorator application (the
/// callee is itself a call: `app.route("/")(f)`), the `inner` effects of that call's
/// `Decorates`; otherwise every effect except `Decorates`.
pub(crate) fn applied_effects(behaviour: &CallBehaviour, decorator_application: bool) -> Vec<Effect> {
    behaviour
        .effects
        .iter()
        .filter_map(|e| match (e, decorator_application) {
            (Effect::Decorates { inner }, true) => Some((**inner).clone()),
            (Effect::Decorates { .. }, false) | (_, true) => None,
            (other, false) => Some(other.clone()),
        })
        .collect()
}

/// The effect a call has on one argument: `never_calls` first (it overrides the declared
/// type), then an effect that runs the function, then any other effect naming it.
pub(crate) fn effect_for<'e>(
    effects: &'e [Effect],
    index: Option<usize>,
    keyword: Option<&str>,
    positional: usize,
) -> Option<&'e Effect> {
    let named: Vec<&Effect> = effects
        .iter()
        .filter(|e| selector(e).is_some_and(|s| selects(s, index, keyword, positional)))
        .collect();
    named
        .iter()
        .find(|e| matches!(e, Effect::NeverCalls(_)))
        .or_else(|| named.iter().find(|e| runs(e)))
        .or_else(|| named.first())
        .copied()
}

/// The behaviour of the library call whose callee starts at `start` in `path`.
pub fn lookup<'k>(knowledge: &'k LibraryKnowledge, path: &str, start: u32) -> Option<&'k CallBehaviour> {
    knowledge.by_call.get(&(path.to_string(), start))
}

/// Callee spans of the library calls of an index: calls the server answered only with
/// locations outside the index (`FileSemantics::library_calls`), and `external_or_ambiguous`
/// calls without in-index candidates.
#[derive(Default)]
pub(crate) struct LibraryCalls {
    spans: std::collections::HashSet<(FileId, ByteSpan)>,
}

impl LibraryCalls {
    pub fn new(index: &Index) -> LibraryCalls {
        let mut spans = std::collections::HashSet::new();
        for (fi, record) in index.files.iter().enumerate() {
            if let Some(s) = &record.semantic {
                for c in &s.library_calls {
                    spans.insert((FileId(fi as u32), c.at));
                }
            }
        }
        for u in &index.unresolved {
            if u.kind == UnresolvedKind::ExternalOrAmbiguous && u.candidates.is_empty() {
                spans.insert((u.at.file, u.at.bytes));
            }
        }
        LibraryCalls { spans }
    }

    pub fn contains(&self, file: FileId, callee: ByteSpan) -> bool {
        self.spans.contains(&(file, callee))
    }
}

/// The argument of a callback site as the syntax recorded it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SiteArg<'a> {
    pub index: Option<usize>,
    pub keyword: Option<&'a str>,
    /// Positional arguments of the receiving call.
    pub positional: usize,
    /// The receiving call's callee is itself a call (decorator application).
    pub decorator_application: bool,
}

/// `Site::library` of a callback site whose receiving call is `callee` in `file`: the
/// knowledge entry's effect on the argument; a library call without an entry (or whose entry
/// names no effect for the argument) carries [`NO_EVIDENCE`] / [`NO_EFFECT`]; `None` when the
/// receiving call is not a library call (in-index callees keep the flow / language rules).
pub(crate) fn behaviour_for(
    index: &Index,
    calls: &LibraryCalls,
    knowledge: &LibraryKnowledge,
    file: FileId,
    callee: ByteSpan,
    arg: SiteArg<'_>,
) -> Option<LibraryBehaviour> {
    let path = index.file_path(file);
    match lookup(knowledge, path, callee.start) {
        Some(b) => {
            let effects = applied_effects(b, arg.decorator_application);
            let effect = effect_for(&effects, arg.index, arg.keyword, arg.positional);
            Some(LibraryBehaviour {
                source: b.source.as_str().to_string(),
                effect: effect.map_or(NO_EFFECT, Effect::name).to_string(),
                reason: b.reason.clone(),
                symbol: b.symbol.clone(),
                inferred: b.inferred && effect.is_some(),
            })
        }
        None if calls.contains(file, callee) => Some(LibraryBehaviour {
            source: NO_EVIDENCE.to_string(),
            effect: NO_EFFECT.to_string(),
            reason: String::new(),
            symbol: None,
            inferred: false,
        }),
        None => None,
    }
}

/// One-line description of a site's library behaviour for evidence packets.
pub fn describe(b: &LibraryBehaviour) -> String {
    let what = match b.effect.as_str() {
        "calls" => "calls the passed function",
        "stored_then_called" => "stores the passed function and calls it later",
        "calls_method" => "calls a method of the passed value",
        "registers" => "registers the passed function as a handler and calls it on dispatch",
        "iterates" => "iterates the passed value",
        "advances" => "advances the passed value",
        "returns" | "wraps" => "returns a wrapper that calls the passed function",
        "partial" => "returns a partial application of the passed function",
        "property" => "makes the passed function a property getter",
        NEVER_CALLS => "never calls the passed function",
        _ => "is not known to run the passed function",
    };
    let source = match b.source.as_str() {
        "derived" => "derived from the installed library source",
        "declared_type" => "from the declared parameter type",
        "table" => "from the native table",
        _ => "no library evidence",
    };
    let symbol = b.symbol.as_deref().map(|s| format!("{s} ")).unwrap_or_default();
    let mut out = format!("library {symbol}{what} ({source}");
    if !b.inferred && b.source != NO_EVIDENCE {
        out.push_str(", below the precision gate");
    }
    out.push(')');
    if !b.reason.is_empty() {
        out.push_str(": ");
        out.push_str(&b.reason);
    }
    out
}

#[cfg(test)]
#[path = "../tests/unit/behaviour.rs"]
mod tests;
