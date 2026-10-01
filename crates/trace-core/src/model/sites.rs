//! Edge-case sites filled by inference (categories, operations, candidates, library
//! behaviour) and their decisions.

use super::*;

/// Edge-case site categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SiteCategory {
    /// Call to an abstract/interface/protocol declaration with concrete implementations.
    Dispatch,
    /// Call with no semantic target whose member name matches in-index functions.
    NoTarget,
    /// Function passed as an argument (may be invoked by the callee).
    Callback,
    /// Value-flow candidates for an unresolved call or override dispatch.
    Flow,
    /// Python data-model implicit operation (`obj[k]=v` -> `__setitem__`, ...).
    Implicit,
}

impl SiteCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            SiteCategory::Dispatch => "dispatch",
            SiteCategory::NoTarget => "no_target",
            SiteCategory::Callback => "callback",
            SiteCategory::Flow => "flow",
            SiteCategory::Implicit => "implicit",
        }
    }
}

/// Operation performed at a flow/implicit site.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SiteOperation {
    Call,
    OverrideDispatch,
    SubscriptStore,
    SubscriptLoad,
    SubscriptDelete,
    WithEnter,
    Iterate,
    DescriptorGet,
    CallInstance,
    /// Store into an attribute naming a method of the receiver's tracked values
    /// (`obj.m = value`): a non-call use, materialized as [`EdgeKind::InferredWrite`].
    FieldWrite,
}

impl SiteOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            SiteOperation::Call => "call",
            SiteOperation::OverrideDispatch => "override_dispatch",
            SiteOperation::SubscriptStore => "subscript_store",
            SiteOperation::SubscriptLoad => "subscript_load",
            SiteOperation::SubscriptDelete => "subscript_delete",
            SiteOperation::WithEnter => "with_enter",
            SiteOperation::Iterate => "iterate",
            SiteOperation::DescriptorGet => "descriptor_get",
            SiteOperation::CallInstance => "call_instance",
            SiteOperation::FieldWrite => "field_write",
        }
    }
}

/// Stable site identifier: first 20 hex chars of blake3 over canonical JSON parts built from
/// uids and byte offsets (see SPEC §7.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SiteId(pub String);

impl fmt::Display for SiteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Maximum candidates kept per site (inventory.py `MAX_CANDIDATES`).
pub const MAX_CANDIDATES: usize = 20;

/// An unresolved edge case with generated candidates. Candidates never become edges by
/// themselves: they are `possible`; a [`Decision`] can promote some to `inferred`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Site {
    pub id: SiteId,
    pub category: SiteCategory,
    pub owner: SymbolId,
    /// Dispatch: the stub declaration the call proved to reach.
    pub declared_target: Option<SymbolId>,
    /// Edge kind of the activation (`calls`, `awaits`, `invoked_callback`, ...).
    pub activation: EdgeKind,
    /// Callee span (call sites) or operand span (implicit ops).
    pub at: Location,
    /// Exact callee / operand text.
    pub callee: String,
    /// Sorted by uid, at most [`MAX_CANDIDATES`].
    pub candidates: Vec<SymbolId>,
    /// Subset of candidates produced by value flow. Dispatch sites: the implementations the
    /// receiver evidence reaches (the receiver's syntax type narrows the family, or value
    /// flow reaches specific objects); empty when nothing is known about the receiver.
    pub flow_candidates: Vec<SymbolId>,
    /// Candidates supported only by field-name (non class-specific) attribute evidence.
    pub field_only: Vec<SymbolId>,
    pub truncated_candidates: bool,
    pub operation: Option<SiteOperation>,
    /// Callback sites: exact text of the passed argument.
    pub argument: Option<String>,
    /// Composed sites: index of the parent site whose target this site dispatches through.
    pub via: Option<u32>,
    /// Subset of candidates reached only through values originating in test code.
    pub test_only: Vec<SymbolId>,
    /// Flow call sites only: every receiver context of the owning method resolves the call
    /// to at most one candidate (strict evidence), so each candidate is *the* target under
    /// some receiver (`cls(...)` in a classmethod reached with different classes). Decided
    /// with every option.
    /// Dispatch sites: the receiver's type is proven by a language rule (a statically typed
    /// language, one concrete type whose method is the only one that can run), and
    /// `flow_candidates` holds exactly that implementation: decided, and materialized as a
    /// proven edge ([`crate::graph::proven_by_receiver_rule`]).
    pub receiver_exact: bool,
    /// Callback sites whose receiving call is a library call: what the library does with the
    /// passed function. Persisted with postcard, so no
    /// `skip_serializing_if` (a skipped field would break the binary layout).
    #[serde(default)]
    pub library: Option<LibraryBehaviour>,
    /// Dispatch sites through an abstract / interface / protocol / virtual member that a
    /// library declares (`FileSemantics::library_dispatch`): that member's library symbol.
    /// `declared_target` is None then (the declaration is not in the index).
    #[serde(default)]
    pub declared_library: Option<String>,
}

/// Library behaviour attached to a callback site (from `trace_library::LibraryKnowledge`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryBehaviour {
    /// "derived" | "declared_type" | "table"
    pub source: String,
    /// Effect on the passed function ("calls", "stored_then_called", "never_calls", ...).
    pub effect: String,
    pub reason: String,
    /// Library-qualified symbol of the callee.
    pub symbol: Option<String>,
    /// false = possible (derived fact below the precision gate, never_calls, or no evidence).
    #[serde(default)]
    pub inferred: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    Decided,
    Unknown,
}

/// Outcome for one site. Aligned 1:1 with [`Index::sites`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    /// Index into [`Index::sites`].
    pub site: u32,
    pub status: DecisionStatus,
    /// Decided targets (always a subset of the site's candidates).
    pub targets: Vec<SymbolId>,
    /// The rule that decided the site, or why it stays undecided (`trace_infer::decide`).
    pub reason: Option<String>,
}
