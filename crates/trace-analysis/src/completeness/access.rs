//! How an occurrence names the target: member access (`obj.x`, `self.x`), bare name or
//! package-qualified call; free functions and members.

use trace_core::facts::{CallSite, FileFacts};
use trace_core::model::{ByteSpan, SymbolId};
use trace_core::{Index, SymbolKind};

use super::{family::stores_into_value, Access};
use crate::languages::rules;

/// Identifier span of a call's member (the callee's last segment), else the callee span.
pub fn member_span(c: &CallSite) -> ByteSpan {
    match c.member.as_deref() {
        Some(m) if !m.is_empty() && c.callee.ends_with(m) && c.callee_span.len() >= m.len() as u32 => {
            ByteSpan::new(c.callee_span.end - m.len() as u32, c.callee_span.end)
        }
        _ => c.callee_span,
    }
}

// ------------------------------------------------------------------ access / bindings

/// The language's self references (receiver roots that denote the enclosing instance).
fn is_self_word(word: &str) -> bool {
    matches!(word, "self" | "this" | "cls" | "$this" | "@" | "Self")
}

/// Leading identifier of a callee text (`obj` in `obj.a.f`, `$this` in `$this->f`).
fn leading_identifier(text: &str) -> Option<&str> {
    let end = text
        .char_indices()
        .find(|&(i, c)| !(c == '_' || c.is_alphanumeric() || (i == 0 && (c == '$' || c == '@'))))
        .map_or(text.len(), |(i, _)| i);
    let word = &text[..end];
    (!word.is_empty() && word != "$" && word != "@").then_some(word)
}

/// Access of a call occurrence: the member-access fact when the extractor recorded one;
/// a bare callee; otherwise, for facts without member accesses (older extractors), the
/// separator in front of the member in the callee *fact text* (`.`, `->`, `?.`, `:` —
/// never `::`, a static path).
pub(super) fn call_access(facts: &FileFacts, c: &CallSite, span: ByteSpan) -> Access {
    if let Some(m) = facts.member_access(span) {
        return Access::Member {
            receiver_root: m.receiver_root.clone(),
            self_receiver: m.self_receiver,
        };
    }
    let Some(member) = c.member.as_deref().filter(|m| !m.is_empty()) else {
        return Access::Unknown;
    };
    if c.callee == member {
        return Access::Bare;
    }
    if !facts.member_accesses.is_empty() || !c.callee.ends_with(member) {
        return Access::Unknown;
    }
    let prefix = &c.callee[..c.callee.len() - member.len()];
    let member_separator =
        (prefix.ends_with('.') || prefix.ends_with("->") || prefix.ends_with(':')) && !prefix.ends_with("::");
    if !member_separator {
        return Access::Unknown;
    }
    let root = leading_identifier(prefix);
    let self_receiver = root.is_some_and(is_self_word);
    Access::Member {
        receiver_root: root.filter(|r| !is_self_word(r)).map(str::to_string),
        self_receiver,
    }
}

/// Access of a reference occurrence: a member-access fact, else bare when the extractor
/// records member accesses and the identifier is not the tail of a dotted / static path
/// (`FileFacts::qualified_names`), else unknown.
pub(super) fn reference_access(facts: &FileFacts, span: ByteSpan) -> Access {
    if let Some(m) = facts.member_access(span) {
        return Access::Member {
            receiver_root: m.receiver_root.clone(),
            self_receiver: m.self_receiver,
        };
    }
    if facts.member_accesses.is_empty() {
        return Access::Unknown;
    }
    let path_tail = facts
        .qualified_names
        .iter()
        .any(|q| q.span.end == span.end && q.span.start < span.start);
    if path_tail {
        Access::Unknown
    } else {
        Access::Bare
    }
}

/// Access of the evidence span of a `uses` row (a call's member or a reference).
pub(crate) fn access_at(facts: &FileFacts, span: ByteSpan) -> Access {
    if let Some(c) = facts
        .calls
        .iter()
        .find(|c| c.callee_span.end == span.end && c.callee_span.start <= span.start)
    {
        return call_access(facts, c, member_span(c));
    }
    if let Some(r) = facts
        .references
        .iter()
        .find(|r| r.span.end == span.end && r.span.start >= span.start)
    {
        return reference_access(facts, r.span);
    }
    match facts.member_access(span) {
        Some(m) => Access::Member {
            receiver_root: m.receiver_root.clone(),
            self_receiver: m.self_receiver,
        },
        None => Access::Unknown,
    }
}

/// `root` is the first segment of a package path that holds `m`'s file, in a language whose
/// top-level functions are reached through package qualifiers
/// (`AnalysisRules::package_qualified_calls`): a package-qualified call.
pub(super) fn package_qualifier(index: &Index, m: SymbolId, root: &str) -> bool {
    let s = index.symbol(m);
    if !rules(s.language).package_qualified_calls {
        return false;
    }
    let path = index.file_path(s.file);
    let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
    dir.split('/').any(|segment| segment == root)
}

/// A free function: callable, no declaring type, no out-of-line container, named directly
/// in its module or its enclosing function (never a property-assigned function).
pub(crate) fn is_free_function(index: &Index, id: SymbolId) -> bool {
    let s = index.symbol(id);
    if s.is_synthetic() || s.kind != SymbolKind::Function {
        return false;
    }
    if s.container.as_deref().is_some_and(|c| !c.is_empty()) {
        return false;
    }
    match s.parent.map(|p| index.symbol(p)) {
        Some(p) if !p.kind.is_callable() => false,
        Some(p) => s.qualified_name == format!("{}.{}", p.qualified_name, s.name),
        None => s.qualified_name == s.name,
    }
}

/// A member: a callable declared on a type (parent of a type kind), with an out-of-line
/// container (`impl X`, Go receivers, JS `obj.prop = function`), or stored into a local value's field inside a function.
/// Namespace-qualified free functions (Rust `mod`, C++ / PHP namespaces) are not members.
/// Constructors are named by their class at call sites and are never members here.
pub fn is_member(index: &Index, id: SymbolId) -> bool {
    let s = index.symbol(id);
    if s.is_synthetic() || !s.kind.is_callable() || trace_infer::hierarchy::is_constructor(s) {
        return false;
    }
    if s.container.as_deref().is_some_and(|c| !c.is_empty()) {
        return true;
    }
    match s.parent.map(|p| index.symbol(p)) {
        Some(p) if p.kind.is_type() => true,
        Some(_) => stores_into_value(index, id),
        None => false,
    }
}
