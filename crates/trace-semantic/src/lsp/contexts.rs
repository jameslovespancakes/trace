//! Project contexts (Roslyn): a file compiled by several projects / target frameworks is
//! asked per context in a deterministic order ([`order_project_contexts`]); an empty answer
//! is asked again in the next context.

use crate::SemanticError;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

use super::client::*;

/// Project contexts tried per request (Roslyn `_vs_projectContext`).
pub(super) const MAX_PROJECT_CONTEXTS: usize = 8;

impl LspClient {
    /// [`LspClient::request_many`] for servers answering per project context (module docs):
    /// fetch the contexts of open documents not seen yet, ask in each document's first
    /// context, then ask empty answers again in the next contexts.
    pub(super) fn batch_in_contexts(
        &mut self,
        calls: Vec<(String, Value)>,
        limit: Instant,
        timeout: Duration,
    ) -> Result<Vec<Result<Value, SemanticError>>, SemanticError> {
        let mut missing: Vec<String> = calls
            .iter()
            .filter_map(|(_, params)| document_uri(params))
            .filter(|uri| self.open_docs.contains_key(*uri) && !self.contexts.contains_key(*uri))
            .map(str::to_string)
            .collect();
        missing.sort();
        missing.dedup();
        if !missing.is_empty() {
            let asks: Vec<(String, Value)> = missing
                .iter()
                .map(|uri| {
                    (
                        "textDocument/_vs_getProjectContexts".to_string(),
                        json!({"_vs_textDocument": {"uri": uri}}),
                    )
                })
                .collect();
            let answers = self.batch(asks, limit, timeout)?;
            for (uri, answer) in missing.into_iter().zip(answers) {
                let ordered = answer.map(|v| order_project_contexts(&v)).unwrap_or_default();
                self.contexts.insert(uri, ordered);
            }
        }
        let calls: Vec<(String, Value)> = calls
            .into_iter()
            .map(|(method, params)| {
                let (Ok(params) | Err(params)) = self.in_context(params, 0);
                (method, params)
            })
            .collect();
        let mut results = self.batch(calls.clone(), limit, timeout)?;
        for round in 1..MAX_PROJECT_CONTEXTS {
            let mut retry: Vec<usize> = Vec::new();
            let mut asks: Vec<(String, Value)> = Vec::new();
            for (i, (method, params)) in calls.iter().enumerate() {
                if !is_empty_answer(&results[i]) {
                    continue;
                }
                if let Ok(params) = self.in_context(params.clone(), round) {
                    retry.push(i);
                    asks.push((method.clone(), params));
                }
            }
            if asks.is_empty() {
                break;
            }
            let answers = self.batch(asks, limit, timeout)?;
            for (i, answer) in retry.into_iter().zip(answers) {
                if !is_empty_answer(&answer) {
                    results[i] = answer;
                }
            }
        }
        Ok(results)
    }

    /// `params` with its `textDocument._vs_projectContext` set to context `round` of the
    /// document; `Err(params)` unchanged when the request names no document with that
    /// context.
    pub(super) fn in_context(&self, mut params: Value, round: usize) -> Result<Value, Value> {
        let Some(context) = document_uri(&params)
            .and_then(|uri| self.contexts.get(uri))
            .and_then(|list| list.get(round))
            .cloned()
        else {
            return Err(params);
        };
        match params.get_mut("textDocument").and_then(Value::as_object_mut) {
            Some(document) => {
                document.insert("_vs_projectContext".to_string(), context);
                Ok(params)
            }
            None => Err(params),
        }
    }
}

/// The `textDocument.uri` a request names.
pub(super) fn document_uri(params: &Value) -> Option<&str> {
    params.get("textDocument")?.get("uri")?.as_str()
}

/// An answer that says nothing (`null` / `[]`); errors are not empty answers.
pub(super) fn is_empty_answer(answer: &Result<Value, SemanticError>) -> bool {
    match answer {
        Ok(Value::Null) => true,
        Ok(Value::Array(items)) => items.is_empty(),
        _ => false,
    }
}

/// The contexts of a `textDocument/_vs_getProjectContexts` answer in the order they are
/// tried: newest target framework first (`netX.Y` / `netcoreappX.Y`, then `netstandardX.Y`,
/// then .NET Framework `netNN`, then labels without a framework), then by label and id;
/// miscellaneous-files contexts last. Deterministic whatever order the server lists them in
/// (its default index depends on load order).
pub fn order_project_contexts(answer: &Value) -> Vec<Value> {
    let mut contexts: Vec<Value> = answer
        .get("_vs_projectContexts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let text = |c: &Value, key: &str| c.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
    contexts.sort_by(|a, b| {
        let key = |c: &Value| {
            let misc = c
                .get("_vs_is_miscellaneous")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let label = text(c, "_vs_label");
            let (rank, version) = framework_rank(target_framework(&label).unwrap_or(""));
            (misc, std::cmp::Reverse(rank), std::cmp::Reverse(version), label, text(c, "_vs_id"))
        };
        key(a).cmp(&key(b))
    });
    contexts.truncate(MAX_PROJECT_CONTEXTS);
    contexts
}

/// The target framework named in a context label (`Newtonsoft.Json (net6.0)` -> `net6.0`).
pub(super) fn target_framework(label: &str) -> Option<&str> {
    let open = label.rfind('(')?;
    let close = open + label[open..].find(')')?;
    Some(label[open + 1..close].trim())
}

/// Rank (higher = newer family) and version of a target framework moniker.
pub(super) fn framework_rank(tfm: &str) -> (u8, Vec<u32>) {
    let tfm = tfm.split('-').next().unwrap_or(tfm).to_ascii_lowercase();
    let dotted = |v: &str| -> Vec<u32> { v.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    if let Some(v) = tfm.strip_prefix("netstandard") {
        return (2, dotted(v));
    }
    if let Some(v) = tfm.strip_prefix("netcoreapp") {
        return (3, dotted(v));
    }
    if let Some(v) = tfm.strip_prefix("net") {
        if v.contains('.') {
            return (3, dotted(v));
        }
        if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) {
            // .NET Framework: every digit is a version part (net462 = 4.6.2).
            return (1, v.bytes().map(|b| u32::from(b - b'0')).collect());
        }
    }
    (0, Vec::new())
}
