//! A scripted LSP session and URI mapping for engine tests.

use serde_json::{json, Value};
use trace_core::facts::{Activation, CallSite};
use trace_core::model::ByteSpan;

use crate::engine::{Session, UriResolver};
use crate::languages::Prepared;
use crate::SemanticError;

pub(crate) struct FakeUris;

impl UriResolver for FakeUris {
    fn uri_of(&self, rel: &str) -> Result<String, SemanticError> {
        Ok(format!("file:///ws/{rel}"))
    }
    fn rel_of(&self, uri: &str) -> Option<String> {
        uri.strip_prefix("file:///ws/").map(str::to_string)
    }
}

pub(crate) type Handler = Box<dyn FnMut(&str, &Value) -> Result<Value, SemanticError> + Send>;

pub(crate) struct FakeSession {
    pub caps: Value,
    pub requests: Vec<String>,
    pub batches: usize,
    pub handler: Handler,
    /// Server notifications (method, params) delivered before the queries.
    pub notifications: Vec<(String, Value)>,
}

impl FakeSession {
    pub(crate) fn new(caps: Value, handler: Handler) -> Self {
        FakeSession {
            caps,
            requests: Vec::new(),
            batches: 0,
            handler,
            notifications: Vec::new(),
        }
    }
}

impl Session for FakeSession {
    fn capabilities(&self) -> &Value {
        &self.caps
    }
    fn notifications_named(&self, method: &str) -> Vec<Value> {
        self.notifications
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }
    fn request_many(
        &mut self,
        calls: Vec<(String, Value)>,
    ) -> Result<Vec<Result<Value, SemanticError>>, SemanticError> {
        self.batches += 1;
        Ok(calls
            .iter()
            .map(|(method, params)| {
                self.requests.push(method.clone());
                (self.handler)(method.as_str(), params)
            })
            .collect())
    }
}

pub(crate) fn call(at: u32, callee: &str, owner: Option<u32>, line: u32) -> CallSite {
    CallSite {
        owner,
        lexical_owner: owner,
        span: ByteSpan::new(at, at + callee.len() as u32 + 2),
        callee_span: ByteSpan::new(at, at + callee.len() as u32),
        callee: callee.to_string(),
        member: Some(callee.rsplit('.').next().unwrap_or(callee).to_string()),
        receiver: None,
        line,
        activation: Activation::Plain,
        is_new: false,
        arg_count: 0,
    }
}

pub(crate) fn range(line: u32, start: u32, end: u32) -> Value {
    json!({"start": {"line": line, "character": start}, "end": {"line": line, "character": end}})
}

/// prepareCallHierarchy echoing an item at the requested position.
pub(crate) fn prepared(params: &Value) -> Value {
    let line = params["position"]["line"].as_u64().unwrap() as u32;
    let ch = params["position"]["character"].as_u64().unwrap() as u32;
    json!([{"name": "f", "kind": 12, "uri": params["textDocument"]["uri"],
            "range": range(line, 0, 40), "selectionRange": range(line, ch, ch + 1)}])
}

/// An empty preflight result (tests build `Options<'static>` with it).
pub(crate) fn empty_prepared() -> &'static Prepared {
    static EMPTY: std::sync::OnceLock<Prepared> = std::sync::OnceLock::new();
    EMPTY.get_or_init(Prepared::default)
}
