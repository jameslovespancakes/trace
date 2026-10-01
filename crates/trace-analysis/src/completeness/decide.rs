//! Classification of one occurrence: resolved to the target, resolved elsewhere (with the
//! rule that proves it) or unresolved (with the reason).

use std::collections::{BTreeSet, HashMap, HashSet};

use trace_core::facts::{BindTarget, FlowFact, ImportKind, RefKind, Scope};
use trace_core::model::{ByteSpan, EdgeKind, SymbolId};
use trace_core::{Graph, Index, Language, SymbolKind, Tier};

use super::{Access, Evidence, Occurrence, State, LOCAL_BINDING, MEMBER_BINDING, OTHER_LANGUAGE, SERVER};
use crate::languages::rules;

/// The state of a non-declaration occurrence from its evidence and the language rules
/// (module docs). Target evidence wins, except that non-proven links are rejected at a
/// local-binding occurrence (the name denotes the local binding: lexical shadowing) and by
/// the member-binding rule (a member access never denotes a free function, a bare name
/// never a member). A server dispatch through a library-declared member listing a family
/// member is never `server`: it stays `possible_only` unless an edge decided it. `negative`
/// (the syntax rules `library_object`, `unrelated_type`, `other_scope`, `arity`) is
/// evaluated only when nothing else decided the occurrence and no conflicting evidence
/// (value flow delivering a family member, a library dispatch) keeps it listed.
pub(super) fn decide(
    x: &Evidence,
    local: bool,
    member_binding: bool,
    negative: &mut dyn FnMut() -> Option<&'static str>,
    fallback: &'static str,
) -> State {
    if x.forced || x.target_proven {
        return State::Target;
    }
    if local {
        return State::Elsewhere(LOCAL_BINDING);
    }
    if x.target {
        return if member_binding {
            State::Elsewhere(MEMBER_BINDING)
        } else {
            State::Target
        };
    }
    if x.elsewhere || (!x.library_dispatch && (x.external || x.resolved_elsewhere)) {
        return State::Elsewhere(SERVER);
    }
    if member_binding {
        return State::Elsewhere(MEMBER_BINDING);
    }
    if x.other_language && !x.possible_bridge {
        return State::Elsewhere(OTHER_LANGUAGE);
    }
    if !x.flow_target && !x.library_dispatch {
        if let Some(reason) = negative() {
            return State::Elsewhere(reason);
        }
    }
    if x.library_dispatch {
        return State::Unresolved("possible_only");
    }
    if let Some(reason) = x.unresolved {
        State::Unresolved(reason)
    } else if x.possible_target {
        State::Unresolved("possible_only")
    } else if x.inferred_elsewhere {
        State::Unresolved("inferred_elsewhere")
    } else {
        State::Unresolved(fallback)
    }
}

/// Rule `other_language` (module docs): no family member's language can be named from
/// `language` without a bridge (the language namespaces of
/// [`trace_infer::narrow::name_interop`]). Contract files (and families declared in them)
/// never.
pub(super) fn other_language(language: Language, family_languages: &BTreeSet<Language>) -> bool {
    !language.is_contract()
        && !family_languages.is_empty()
        && family_languages
            .iter()
            .all(|&l| !l.is_contract() && !trace_infer::narrow::name_interop(language, l))
}

/// Proven `imports` / `reexports` edges keyed by (file, start, end) of their evidence span.
pub(super) type ImportEdges = HashMap<(u32, u32, u32), Vec<SymbolId>>;

pub(super) fn import_edges(graph: &Graph<'_>) -> ImportEdges {
    let mut map: ImportEdges = HashMap::new();
    for e in graph.edges() {
        if e.tier == Tier::Proven && matches!(e.kind, EdgeKind::Imports | EdgeKind::Reexports) {
            map.entry((e.at.file.0, e.at.bytes.start, e.at.bytes.end))
                .or_default()
                .push(e.to);
        }
    }
    map
}

/// Rule `other_scope` (module docs): a bare name that a nearer binding of its own file
/// denotes, with positive evidence that the binding is not a family member:
/// * a same-file declaration of that name (callable or type, not a prototype, not a member
///   stored on a container) visible at the occurrence: top level, or nested in a function
///   that encloses the occurrence;
/// * Python / JavaScript / TypeScript: every import binding the name in a scope enclosing
///   the occurrence is a single-name import whose imported identifier carries a proven
///   `imports` edge to another symbol, or which the server resolved outside the index.
///
/// Never when a family member is such a binding, when an import binding the name proves
/// nothing, in JavaScript / TypeScript script files (one shared global scope), with a
/// wildcard import in the file, or when module-level code rebinds the name.
pub(super) fn nearer_binding(
    index: &Index,
    imports: &ImportEdges,
    family: &HashSet<SymbolId>,
    o: &Occurrence,
) -> bool {
    if o.access != Access::Bare || o.local || !matches!(o.kind, "call" | "read" | "callback") {
        return false;
    }
    let rec = index.file(o.file);
    let rules = rules(rec.language);
    if !rules.file_scope {
        return false;
    }
    let Some(facts) = &rec.facts else { return false };
    if rules.file_modules {
        let script = rules.scripts_share_scope && facts.imports.is_empty() && facts.exports.is_empty();
        if script || facts.imports.iter().any(|i| i.kind == ImportKind::Wildcard) {
            return false;
        }
        let rebound = facts.flow.iter().any(|f| {
            matches!(
                f,
                FlowFact::Bind {
                    target: BindTarget::Var { scope: Scope::Module, name },
                    ..
                } if *name == o.name
            )
        });
        if rebound {
            return false;
        }
    }
    let visible = |s: &trace_core::Symbol| -> bool {
        match s.parent.map(|p| index.symbol(p)) {
            None => true,
            Some(p) if p.kind.is_callable() => p.span.bytes.contains(o.span.start),
            Some(_) => false,
        }
    };
    let mut binders = 0usize;
    for s in index.symbols_of(o.file) {
        if s.name != o.name
            || s.is_synthetic()
            || s.is_stub
            || s.kind == SymbolKind::Module
            || s.container.as_deref().is_some_and(|c| !c.is_empty())
            || !visible(s)
        {
            continue;
        }
        if family.contains(&s.id) {
            return false;
        }
        binders += 1;
    }
    if rules.file_modules {
        for imp in &facts.imports {
            if imp.local != o.name {
                continue;
            }
            let in_scope = match imp.scope {
                Scope::Module => true,
                Scope::Decl(d) => rec
                    .symbol_of_decl(d)
                    .is_some_and(|s| index.symbol(s).span.bytes.contains(o.span.start)),
            };
            if !in_scope {
                continue;
            }
            if imp.kind != ImportKind::Member {
                return false;
            }
            let spans: Vec<ByteSpan> = facts
                .references
                .iter()
                .filter(|r| r.kind == RefKind::Import && r.name == o.name && imp.span.encloses(r.span))
                .map(|r| r.span)
                .collect();
            if spans.is_empty() {
                return false;
            }
            for span in spans {
                match imports.get(&(o.file.0, span.start, span.end)) {
                    Some(targets) if targets.iter().any(|t| family.contains(t)) => return false,
                    Some(targets) if !targets.is_empty() => {}
                    _ => {
                        let outside = rec
                            .semantic
                            .as_ref()
                            .is_some_and(|s| s.resolved_elsewhere.contains(&span));
                        if !outside {
                            return false;
                        }
                    }
                }
            }
            binders += 1;
        }
    }
    binders > 0
}

/// Rule `arity` (module docs): the call occurrence's plain positional arguments are
/// accepted by no family member named like it (at least one such member).
pub(super) fn arity_excludes(index: &Index, family: &[SymbolId], o: &Occurrence) -> bool {
    let Some(ci) = o.call else { return false };
    let Some(facts) = &index.file(o.file).facts else {
        return false;
    };
    let Some(call) = facts.calls.get(ci as usize) else {
        return false;
    };
    let detail = facts.call_detail(ci as usize);
    let mut named = family
        .iter()
        .copied()
        .filter(|&m| {
            let s = index.symbol(m);
            !s.is_synthetic() && s.name == o.name
        })
        .peekable();
    named.peek().is_some() && named.all(|m| trace_infer::narrow::arity_rules_out(index, call, detail, m))
}
