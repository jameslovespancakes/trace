//! Channel effects (PLAN decision 14, DESIGN §1.15, DESIGN-bridges §2 rules 1-10; owner
//! derive). The generic rules live in the derivation engine ([`crate::derive`]): they compose
//! through cached summaries down to the irreducible rows read here:
//!
//! * rule 1 `Sends` / rule 9 processes: a parameter (or a string built from it) reaches the
//!   key argument of an `io_send` row (channel from the row: HTTP, message, process);
//! * rules 2, 8, 10 `Registers`: a callable parameter is stored in a *dispatch registry* - a
//!   slot whose content is called by a function reachable from an `io_entry` row (server
//!   protocol callables by `pattern` = `name[/arity]`, or primitives whose `handler`
//!   argument receives the entry function); the entry row's channel decides HTTP vs message;
//! * rule 3 `Mounts`: an object parameter whose attributes feed a registration, under a key;
//! * rule 4 `Decorates`: a returned closure whose parameter receives a registration / call;
//! * rule 5 reflection chains: [`annotation_chain`] up to a `reflection_roots` row;
//! * rule 7 `Exports`: [`expansion_exports`] (language-level exports in expanded macro code)
//!   and parameters reaching the name argument of an `ffi_conventions` row.
//!
//! Conventions for bridges: an `Exports` effect derived from an expanded macro has
//! `name: ArgSel::Kw(<exported symbol>)` (the exported name itself; there are no call
//! arguments). A row without a channel yields `Channel::Message` (raw sockets: the bridge
//! decides by the key's shape, rule 10).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use trace_core::facts::BoundaryRole;
use trace_core::model::BridgeKind;
use trace_core::Language;

use crate::model::{ArgSel, Channel, VerbSel};
use crate::table::{RowSel, RowVerb, Section, Tables};

/// HTTP method tokens (RFC 9110 section 9, protocol vocabulary): a literal token among the
/// arguments of a registering call is the registration's verb (`methods=["GET"]`).
const HTTP_METHOD_TOKENS: [&str; 9] =
    ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "TRACE", "CONNECT"];

/// The HTTP method token `text` spells (exact, case-insensitive), in canonical case.
pub fn http_method_token(text: &str) -> Option<&'static str> {
    HTTP_METHOD_TOKENS
        .iter()
        .copied()
        .find(|t| t.eq_ignore_ascii_case(text))
}

/// One irreducible row with typed selectors.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChannelRow {
    pub symbol: Option<String>,
    /// `io_entry` protocol pattern: `name` or `name/arity` (arity without the receiver).
    pub pattern: Option<String>,
    pub channel: Option<Channel>,
    pub key: Option<ArgSel>,
    pub verb: Option<VerbSel>,
    pub handler: Option<ArgSel>,
    pub activated_by: Option<String>,
}

impl ChannelRow {
    /// The channel a derived effect gets from this row (rule 10: a row without a channel is a
    /// raw transport; the bridge decides by the key's shape).
    pub fn channel_or_default(&self) -> Channel {
        self.channel.unwrap_or(Channel::Message)
    }

    /// `(name, arity)` of an `io_entry` protocol pattern.
    pub fn entry_pattern(&self) -> Option<(&str, Option<u32>)> {
        let pattern = self.pattern.as_deref()?;
        match pattern.rsplit_once('/') {
            Some((name, arity)) => Some((name, arity.parse().ok())),
            None => Some((pattern, None)),
        }
    }
}

/// Typed rows of one irreducible section (rows whose selectors do not parse are skipped:
/// the table test validates them).
pub fn rows_of(tables: &Tables, language: Language, section: Section) -> Vec<ChannelRow> {
    tables
        .irreducible(language, section)
        .iter()
        .map(|r| {
            let arg = |sel: Result<Option<RowSel>, String>| match sel {
                Ok(Some(RowSel::Arg(a))) => Some(a),
                _ => None,
            };
            ChannelRow {
                symbol: r.symbol.clone(),
                pattern: r.pattern.clone(),
                channel: r.channel,
                key: arg(r.key_sel()),
                verb: match r.verb_sel() {
                    Ok(Some(RowVerb::Sel(v))) => Some(v),
                    _ => None,
                },
                handler: arg(r.handler_sel()),
                activated_by: r.activated_by.clone(),
            }
        })
        .collect()
}

/// Exported names of expanded macro code (rule 7): the language-level exports the syntax
/// facts of the expansion carry (`#[no_mangle] extern "C" fn`, `export_name`, method
/// tables). The expansion is ordinary source of `language`.
pub fn expansion_exports(language: Language, text: &str) -> Vec<String> {
    let Ok(facts) = trace_syntax::extract(trace_syntax::SourceInput {
        path: "<expansion>",
        language,
        source: text.as_bytes(),
    }) else {
        return Vec::new();
    };
    let names: BTreeSet<String> = facts
        .boundaries
        .iter()
        .filter(|b| b.role == BoundaryRole::Provides)
        .filter(|b| {
            matches!(
                b.kind,
                BridgeKind::CAbi
                    | BridgeKind::Pyo3
                    | BridgeKind::WasmBindgen
                    | BridgeKind::Napi
                    | BridgeKind::Cpython
            )
        })
        .map(|b| b.name.clone())
        .filter(|n| !n.is_empty())
        .collect();
    names.into_iter().collect()
}

/// A reflection-driven registration chain (rule 5): the annotation used on a handler, the
/// irreducible root the runtime scans for, the verb the chain fixes (`method = X.GET`) and
/// the elements of the annotation that alias the root's path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReflectionChain {
    pub annotation: String,
    pub root: String,
    pub verb: Option<String>,
    pub path_elements: Vec<String>,
}

/// Follow meta-annotations from `annotation` through the annotation types declared in
/// `sources` (installed package sources) up to a `reflection_roots` row (matched on the
/// simple name of the row symbol). Bounded to 8 hops.
pub fn annotation_chain(
    language: Language,
    sources: &[&[u8]],
    annotation: &str,
    roots: &[ChannelRow],
) -> Option<ReflectionChain> {
    let simple = |s: &str| s.rsplit(['.', ':', '\\']).next().unwrap_or(s).to_string();
    let root_names: BTreeSet<String> = roots.iter().filter_map(|r| r.symbol.as_deref()).map(simple).collect();
    if root_names.is_empty() {
        return None;
    }
    let mut types: BTreeMap<String, trace_syntax::lower::AnnotationType> = BTreeMap::new();
    for source in sources {
        for t in trace_syntax::lower::annotation_types(language, source) {
            types.entry(t.name.clone()).or_insert(t);
        }
    }
    let start = simple(annotation);
    if root_names.contains(&start) {
        return Some(ReflectionChain {
            annotation: start.clone(),
            root: start,
            verb: None,
            path_elements: Vec::new(),
        });
    }
    let mut queue: VecDeque<(String, usize)> = VecDeque::from([(start.clone(), 0)]);
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut verb: Option<String> = None;
    while let Some((name, depth)) = queue.pop_front() {
        if depth > 8 || !seen.insert(name.clone()) {
            continue;
        }
        let Some(t) = types.get(&name) else { continue };
        for meta in &t.annotations {
            let meta_name = simple(&meta.name);
            if verb.is_none() {
                verb = meta
                    .values
                    .iter()
                    .find(|(k, _)| k.as_deref() == Some("method"))
                    .map(|(_, v)| v.rsplit('.').next().unwrap_or(v).to_ascii_uppercase());
            }
            if root_names.contains(&meta_name) {
                let path_elements = types
                    .get(&start)
                    .map(|s| {
                        s.elements
                            .iter()
                            .filter(|e| {
                                e.annotations.iter().any(|a| {
                                    a.values.iter().any(|(k, v)| {
                                        k.as_deref() == Some("annotation")
                                            && simple(v.trim_end_matches(".class")) == meta_name
                                    })
                                })
                            })
                            .map(|e| e.name.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                return Some(ReflectionChain {
                    annotation: start,
                    root: meta_name,
                    verb,
                    path_elements,
                });
            }
            queue.push_back((meta_name, depth + 1));
        }
    }
    None
}

#[cfg(test)]
#[path = "../tests/unit/channels.rs"]
mod tests;
