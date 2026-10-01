//! Library behaviour model (DESIGN §1.11): what a library callee does with a function passed
//! to it, and where that knowledge came from.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use trace_core::model::ByteSpan;
use trace_core::semantics::{LibraryFile, SemCallbackParam};
use trace_core::Language;

/// Which argument of a call an effect applies to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArgSel {
    Pos(u32),
    Kw(String),
    PosOrKw(u32, String),
    Rest(u32),
    Receiver,
    Last,
    NamedBy(u32),
    Command(u32),
    Code(u32),
    Field {
        arg: u32,
        field: String,
    },
    /// The call's return value (`Object.create(p)`: `DelegatesMembers { object: Result, to:
    /// Pos(0) }`). Never an argument ([`ArgSel::picks`] is false).
    Result,
    /// The member name the call spells (`lib.compress_buf(1)` -> `compress_buf`): the name a
    /// call resolved to the language's member-lookup hook (Python `__getattr__`) looks up.
    /// Never an argument.
    Member,
}

/// What a library callee does with the selected argument.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Calls(ArgSel),
    StoredThenCalled(ArgSel),
    Iterates(ArgSel),
    Advances(ArgSel),
    Returns(ArgSel),
    Wraps(ArgSel),
    Partial(ArgSel),
    Property(ArgSel),
    CallsMethod {
        arg: ArgSel,
        method: String,
    },
    NeverCalls(ArgSel),
    Receiver,
    // Channel effects (PLAN decision 14, §1.15; derived by the same generic rules, never by
    // package name).
    /// Client side: sends a request / message / process call on `channel` under `key`.
    Sends {
        channel: Channel,
        key: ArgSel,
        verb: VerbSel,
    },
    /// Handler stored in a dispatch registry under `key`.
    Registers {
        channel: Channel,
        key: ArgSel,
        handler: ArgSel,
        verb: VerbSel,
    },
    /// Sub-registry mounted under a prefix.
    Mounts {
        key: ArgSel,
        target: ArgSel,
    },
    /// Returns a decorator whose argument gets `inner`.
    Decorates {
        inner: Box<Effect>,
    },
    /// Callable exposed to another language.
    Exports {
        channel: Channel,
        name: ArgSel,
    },
    /// Every own member of `from` becomes a member of `to` (member copy: `Object.assign(to,
    /// from)`, mixins).
    CopiesMembers {
        from: ArgSel,
        to: ArgSel,
    },
    /// Member lookups on `object` that find nothing fall back to `to` (prototype / metatable
    /// delegation).
    DelegatesMembers {
        object: ArgSel,
        to: ArgSel,
    },
}

impl Effect {
    /// Stable effect name (`"calls"`, `"stored_then_called"`, ...), as in the table files and
    /// `trace_core::model::LibraryBehaviour::effect`.
    pub fn name(&self) -> &'static str {
        match self {
            Effect::Calls(_) => "calls",
            Effect::StoredThenCalled(_) => "stored_then_called",
            Effect::Iterates(_) => "iterates",
            Effect::Advances(_) => "advances",
            Effect::Returns(_) => "returns",
            Effect::Wraps(_) => "wraps",
            Effect::Partial(_) => "partial",
            Effect::Property(_) => "property",
            Effect::CallsMethod { .. } => "calls_method",
            Effect::NeverCalls(_) => "never_calls",
            Effect::Receiver => "receiver",
            Effect::Sends { .. } => "sends",
            Effect::Registers { .. } => "registers",
            Effect::Mounts { .. } => "mounts",
            Effect::Decorates { .. } => "decorates",
            Effect::Exports { .. } => "exports",
            Effect::CopiesMembers { .. } => "copies_members",
            Effect::DelegatesMembers { .. } => "delegates_members",
        }
    }

    /// The argument a single-argument effect applies to (`None` for channel effects,
    /// `Decorates` and `Receiver`; `from` of `CopiesMembers`, `object` of `DelegatesMembers`).
    pub fn selector(&self) -> Option<&ArgSel> {
        match self {
            Effect::Calls(s)
            | Effect::StoredThenCalled(s)
            | Effect::Iterates(s)
            | Effect::Advances(s)
            | Effect::Returns(s)
            | Effect::Wraps(s)
            | Effect::Partial(s)
            | Effect::Property(s)
            | Effect::NeverCalls(s) => Some(s),
            Effect::CallsMethod { arg, .. } => Some(arg),
            Effect::CopiesMembers { from, .. } => Some(from),
            Effect::DelegatesMembers { object, .. } => Some(object),
            _ => None,
        }
    }

    /// Whether the effect runs the selected argument (now or later).
    pub fn runs_argument(&self) -> bool {
        matches!(
            self,
            Effect::Calls(_)
                | Effect::StoredThenCalled(_)
                | Effect::Wraps(_)
                | Effect::Partial(_)
                | Effect::Property(_)
                | Effect::CallsMethod { .. }
        )
    }

    /// Channel effects (PLAN decision 14) and `Decorates` of them.
    pub fn is_channel(&self) -> bool {
        match self {
            Effect::Sends { .. }
            | Effect::Registers { .. }
            | Effect::Mounts { .. }
            | Effect::Exports { .. } => true,
            Effect::Decorates { inner } => inner.is_channel(),
            _ => false,
        }
    }
}

impl ArgSel {
    /// Whether the selector picks the argument at positional `index` / keyword `keyword`
    /// (`None` index: a keyword argument).
    pub fn picks(&self, index: Option<u32>, keyword: Option<&str>) -> bool {
        match self {
            ArgSel::Pos(i) => index == Some(*i),
            ArgSel::Kw(k) => keyword == Some(k.as_str()),
            ArgSel::PosOrKw(i, k) => index == Some(*i) || keyword == Some(k.as_str()),
            ArgSel::Rest(i) => index.is_some_and(|x| x >= *i),
            ArgSel::NamedBy(i) | ArgSel::Command(i) | ArgSel::Code(i) => index == Some(*i),
            ArgSel::Field { arg, .. } => index == Some(*arg),
            ArgSel::Receiver | ArgSel::Last | ArgSel::Result | ArgSel::Member => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Http,
    Message,
    Rpc,
    Process,
    Ffi,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerbSel {
    Const(String),
    Arg(ArgSel),
    Any,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BehaviourSource {
    Derived,
    DeclaredType,
    Table,
}

impl BehaviourSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            BehaviourSource::Derived => "derived",
            BehaviourSource::DeclaredType => "declared_type",
            BehaviourSource::Table => "table",
        }
    }
}

/// What one library call does with the functions passed to it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallBehaviour {
    pub symbol: Option<String>,
    pub effects: Vec<Effect>,
    pub source: BehaviourSource,
    /// false = possible.
    pub inferred: bool,
    pub reason: String,
}

/// One call site that passes a function (named or anonymous) to a library callee, or whose
/// callee is a table-matched builtin.
#[derive(Clone, Debug)]
pub struct BehaviourRequest<'a> {
    pub file: &'a str,
    pub language: Language,
    pub callee: ByteSpan,
    /// Called member name (last segment of the callee, `CallSite::member` or the callee).
    pub spelling: &'a str,
    /// Receiver identifier before the member (`CallSite::receiver`): the module qualifier of
    /// spelling-only table lookups and stdlib module index lookups.
    pub qualifier: Option<&'a str>,
    pub positional_args: u32,
    pub keywords: Vec<&'a str>,
    /// (library file, declaration line, declaration column).
    pub target: Option<(&'a LibraryFile, u32, u32)>,
    /// Server- or URI-given symbol of the callee (`SemLibraryCall::symbol`).
    pub symbol: Option<&'a str>,
    pub callback_params: Vec<&'a SemCallbackParam>,
    /// The function-valued arguments of the call (named functions, lambdas).
    pub args: Vec<RequestArg>,
}

/// One function-valued argument of a behaviour request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestArg {
    /// `CallbackArg::arg_span` (or the lambda / block span).
    pub span: ByteSpan,
    pub index: Option<u32>,
    pub keyword: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LibraryKnowledge {
    /// (file, callee start byte) -> behaviour
    pub by_call: BTreeMap<(String, u32), CallBehaviour>,
    pub stats: KnowledgeStats,
    /// (file, base name start) -> the library class of a header base the server located
    /// outside the index (`FileSemantics::library_bases`, [`crate::library_class`]).
    #[serde(default)]
    pub classes: BTreeMap<(String, u32), crate::library_class::LibraryClass>,
    /// Provider decorator of a by-name injection row -> providers installed packages
    /// declare ([`crate::injected`]).
    #[serde(default)]
    pub providers: BTreeMap<String, crate::injected::Providers>,
    /// [`crate::Library::injected_key`] of the installation `providers` were read from.
    #[serde(default)]
    pub providers_key: String,
}

impl LibraryKnowledge {
    /// Recompute the deterministic counters from `by_call` for `sites` requested call sites
    /// (`cache_hits` and `seconds` are per-run measurements and stay as they are).
    pub fn recount(&mut self, sites: u32) {
        let count = |s: BehaviourSource| self.by_call.values().filter(|b| b.source == s).count() as u32;
        let derived = count(BehaviourSource::Derived);
        let declared = count(BehaviourSource::DeclaredType);
        let table = count(BehaviourSource::Table);
        let functions: std::collections::BTreeSet<&str> = self
            .by_call
            .values()
            .filter(|b| b.source == BehaviourSource::Derived)
            .filter_map(|b| b.symbol.as_deref())
            .collect();
        self.stats.sites = sites;
        self.stats.derived = derived;
        self.stats.declared = declared;
        self.stats.table = table;
        self.stats.unknown = sites.saturating_sub(self.by_call.len() as u32);
        self.stats.library_functions_derived = functions.len() as u32;
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeStats {
    pub sites: u32,
    pub derived: u32,
    pub declared: u32,
    pub table: u32,
    pub unknown: u32,
    pub library_functions_derived: u32,
    pub cache_hits: u32,
    pub seconds: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("The library table for {language} is invalid: {message}")]
    Table { language: String, message: String },
    #[error("The library precision gate file is invalid: {0}")]
    Gate(String),
    #[error("Could not use the library cache at {path}: {message}")]
    Cache { path: String, message: String },
}
