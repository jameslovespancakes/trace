//! Inactive preprocessor regions (clangd `textDocument/inactiveRegions`): calls there are
//! `inactive_code` and nothing inside is asked.

use crate::backend::SemanticFile;
use crate::mapping::DeclTable;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use trace_core::facts::CallSite;
use trace_core::model::{ByteSpan, UnresolvedKind};
use trace_core::semantics::SemUnresolved;

use crate::engine::*;

/// clangd's inactive preprocessor regions notification (`inactiveRegions` client capability).
pub(in crate::engine) const INACTIVE_REGIONS: &str = "textDocument/inactiveRegions";

/// Inactive preprocessor regions per queried file from the server's latest
/// `textDocument/inactiveRegions` notification of that document (clangd).
pub(in crate::engine) fn inactive_regions<'a>(
    session: &dyn Session,
    files: &[&SemanticFile<'a>],
    decls: &DeclTable<'a>,
    uris: &dyn UriResolver,
) -> HashMap<&'a str, Vec<ByteSpan>> {
    let wanted: HashSet<&str> = files.iter().map(|f| f.path).collect();
    let params = session.notifications_named(INACTIVE_REGIONS);
    let mut latest: HashMap<&'a str, usize> = HashMap::new();
    for (i, p) in params.iter().enumerate() {
        let Some(uri) = p
            .get("textDocument")
            .and_then(|d| d.get("uri"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(path) = uris.rel_of(uri).and_then(|rel| decls.path_key(&rel)) else {
            continue;
        };
        if wanted.contains(path) {
            latest.insert(path, i);
        }
    }
    let mut out: HashMap<&'a str, Vec<ByteSpan>> = HashMap::new();
    for (path, i) in latest {
        let file_end = decls.source(path).map_or(0, |s| s.len() as u32);
        let regions = params[i]
            .get("regions")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut spans: Vec<ByteSpan> = Vec::new();
        for region in regions {
            let Some((l0, c0)) = region.get("start").and_then(position) else { continue };
            let Some((l1, c1)) = region.get("end").and_then(position) else { continue };
            let Some(start) = decls.byte_of(path, l0, c0).or_else(|| decls.byte_of(path, l0, 0)) else {
                continue;
            };
            // A region may end past its last line's text: the next line's start, else the file end.
            let end = decls
                .byte_of(path, l1, c1)
                .or_else(|| decls.byte_of(path, l1 + 1, 0))
                .unwrap_or(file_end);
            if end > start {
                spans.push(ByteSpan::new(start, end));
            }
        }
        if !spans.is_empty() {
            spans.sort_by_key(|s| (s.start, s.end));
            out.insert(path, spans);
        }
    }
    out
}

/// Record call `ci` of `path` as inside an inactive preprocessor region (no request).
pub(in crate::engine) fn record_inactive<'a>(
    path: &'a str,
    ci: usize,
    c: &CallSite,
    owner: u32,
    out: &mut BTreeMap<&'a str, FileOut>,
    covered: &mut HashSet<(&'a str, usize)>,
) {
    if let Some(o) = out.get_mut(path) {
        o.unresolved.push(SemUnresolved {
            owner: Some(owner),
            kind: UnresolvedKind::InactiveCode,
            at: c.callee_span,
            line: c.line,
            callee: c.callee.clone(),
            candidates: Vec::new(),
        });
        o.count_at("inactive_code", c.callee_span.start);
    }
    covered.insert((path, ci));
}
