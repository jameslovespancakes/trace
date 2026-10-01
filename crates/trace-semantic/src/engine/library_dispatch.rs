//! Library dispatch: calls answered only by a library declaration ask
//! `textDocument/implementation` once per distinct declaration; in-index members named like
//! the member become `FileSemantics::library_dispatch`.

use crate::mapping::{DeclRef, DeclTable};
use crate::SemanticError;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use trace_core::facts::CallSite;
use trace_core::model::{ByteSpan, SymbolKind};
use trace_core::semantics::{LibraryFile, SemLibraryDispatch};

use super::*;

/// The library declaration a call-hierarchy item outside the index designates (its
/// selection start, else its range start), classified like a definition location.
pub(super) fn library_item(
    item: &Value,
    cx: &crate::external::ExternalContext<'_>,
) -> Option<(LibraryKey, LibraryFile, crate::external::SemLibraryCallTarget)> {
    let uri = item.get("uri")?.as_str()?;
    let start = item
        .get("selectionRange")
        .or_else(|| item.get("range"))?
        .get("start")?;
    let (line, character) = position(start)?;
    let uri = crate::mapping::canonical_uri(uri);
    let (file, target) = crate::external::classify(&uri, line, character, cx)?;
    Some(((uri, line, character), file, target))
}

/// A library call through a receiver whose in-index implementations may be asked (library
/// dispatch, module docs).
#[derive(Clone, Debug)]
pub(super) struct DispatchCandidate {
    /// The library declaration the server answered (one request per distinct key).
    pub(super) key: LibraryKey,
    /// Executing owner of the call.
    pub(super) owner: u32,
    /// Callee span.
    pub(super) at: ByteSpan,
    pub(super) line: u32,
    /// Called member name (implementations are named like it).
    pub(super) member: String,
    /// Byte of the member identifier (where the request is asked).
    pub(super) member_point: u32,
    /// Library symbol of the abstract member (evidence text).
    pub(super) symbol: String,
}

/// Library dispatch candidate of call `c` answered by library declaration `key`: a member
/// call through a receiver value (`o.m()`, `self.m()`; never a module-qualified function or
/// a static path) whose member name a method of the partition carries (request diet: only
/// such a declaration can be an implementation).
pub(super) fn dispatch_candidate(
    path: &str,
    c: &CallSite,
    owner: u32,
    key: &LibraryKey,
    package: &str,
    symbol: Option<&str>,
    decls: &DeclTable<'_>,
) -> Option<DispatchCandidate> {
    let member = c.member.as_deref()?;
    let facts = decls.facts(path)?;
    let point = member_point(c);
    let access = facts.member_access(ByteSpan::new(point, c.callee_span.end))?;
    // A receiver that is an imported module names a function, not a value.
    let module = access
        .receiver_root
        .as_deref()
        .is_some_and(|root| facts.imports.iter().any(|i| i.local == root));
    if module {
        return None;
    }
    let method_named = decls.named(member).iter().any(|r| {
        let d = decls.decl(*r);
        d.kind.is_callable()
            && (d.container.is_some()
                || d.parent
                    .and_then(|p| {
                        decls.get(DeclRef {
                            path: r.path,
                            decl: p,
                        })
                    })
                    .is_some_and(|p| p.kind.is_type()))
    });
    if !method_named {
        return None;
    }
    Some(DispatchCandidate {
        key: key.clone(),
        owner,
        at: c.callee_span,
        line: c.line,
        member: member.to_string(),
        member_point: point,
        symbol: symbol.map_or_else(|| format!("{package}.{member}"), str::to_string),
    })
}

/// Library dispatch (module docs): one `textDocument/implementation` per distinct library
/// declaration, asked at the member identifier of its first call (in path / position
/// order), at most `semantic.max_library_dispatch_requests`; every call of that declaration whose
/// answer names concrete in-index members of the member's name becomes a
/// [`SemLibraryDispatch`] of its file.
pub(super) fn library_dispatch<'a>(
    session: &mut dyn Session,
    decls: &DeclTable<'a>,
    uris: &dyn UriResolver,
    uri_of: &HashMap<&'a str, String>,
    out: &mut BTreeMap<&'a str, FileOut>,
) -> Result<(), SemanticError> {
    let mut pending: Vec<(&'a str, DispatchCandidate)> = Vec::new();
    for (&path, o) in out.iter_mut() {
        let mut candidates = std::mem::take(&mut o.dispatch_candidates);
        candidates.sort_by_key(|d| (d.at.start, d.at.end));
        pending.extend(candidates.into_iter().map(|d| (path, d)));
    }
    // The first call of each distinct declaration asks.
    let mut order: Vec<&LibraryKey> = Vec::new();
    let mut asker: HashMap<&LibraryKey, (&'a str, u32)> = HashMap::new();
    for (path, d) in &pending {
        if let std::collections::hash_map::Entry::Vacant(slot) = asker.entry(&d.key) {
            slot.insert((*path, d.member_point));
            order.push(&d.key);
        }
    }
    let mut calls: Vec<(String, Value)> = Vec::new();
    let mut asked: Vec<&LibraryKey> = Vec::new();
    let mut bounded: Vec<(&'a str, u32)> = Vec::new();
    for key in order {
        let (path, point) = asker[key];
        if asked.len() >= trace_core::config::current().semantic.max_library_dispatch_requests {
            bounded.push((path, point));
            continue;
        }
        let (Some((line, character)), Some(uri)) = (decls.lsp_of(path, point), uri_of.get(path)) else {
            continue;
        };
        calls.push((
            "textDocument/implementation".to_string(),
            json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": character}}),
        ));
        asked.push(key);
    }
    for (path, point) in bounded {
        if let Some(o) = out.get_mut(path) {
            o.count_at("library_dispatch_bounded", point);
        }
    }
    if calls.is_empty() {
        return Ok(());
    }
    let results = request_unique(session, calls)?;
    let mut answers: HashMap<&LibraryKey, Value> = HashMap::new();
    for (key, result) in asked.into_iter().zip(results) {
        let (path, point) = asker[key];
        let Some(o) = out.get_mut(path) else { continue };
        match result {
            Ok(value) => {
                o.count_at("library_dispatch", point);
                answers.insert(key, value);
            }
            Err(e) => o.fail(&e, CountAt::At(point)),
        }
    }
    for (path, d) in &pending {
        let Some(value) = answers.get(&d.key) else { continue };
        let implementations: Vec<String> = dispatch_targets(value, &d.member, decls, uris)
            .into_iter()
            .map(|t| decls.uid(t))
            .collect();
        if implementations.is_empty() {
            continue;
        }
        if let Some(o) = out.get_mut(path) {
            o.library_dispatch.push(SemLibraryDispatch {
                owner: d.owner,
                at: d.at,
                line: d.line,
                library_symbol: Some(d.symbol.clone()),
                implementations,
            });
        }
    }
    Ok(())
}

/// Concrete in-index members named `member` that an implementation answer designates:
/// abstract ones (interface members, stubs) are declarations of the contract, not
/// implementations of it.
pub(super) fn dispatch_targets<'a>(
    value: &Value,
    member: &str,
    decls: &DeclTable<'a>,
    uris: &dyn UriResolver,
) -> Vec<DeclRef<'a>> {
    named_targets(value, member, decls, uris)
        .into_iter()
        .filter(|t| {
            let d = decls.decl(*t);
            let in_interface = d
                .parent
                .and_then(|p| {
                    decls.get(DeclRef {
                        path: t.path,
                        decl: p,
                    })
                })
                .is_some_and(|p| p.kind == SymbolKind::Interface);
            !d.is_stub && !in_interface
        })
        .collect()
}

impl Shard<'_, '_> {
    /// Library dispatch over the shard (module docs); without `implementationProvider` the
    /// candidates are dropped.
    pub(super) fn library_dispatch(&mut self, session: &mut dyn Session) -> Result<(), SemanticError> {
        // 5b. Library dispatch: implementations of the library declarations called through a
        //     receiver, one request per distinct declaration (module docs).
        if self.has_implementation {
            library_dispatch(session, self.decls, self.uris, &self.uri_of, &mut self.out)?;
        } else {
            for o in self.out.values_mut() {
                o.dispatch_candidates.clear();
            }
        }
        Ok(())
    }
}
