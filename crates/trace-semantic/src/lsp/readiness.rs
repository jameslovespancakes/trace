//! Readiness: the per-entry wait after start ([`Readiness`], `ReadySpec`), the settle waits
//! after workspace changes and the `workspace/symbol` poll ([`SymbolPoll`]).

use crate::registry::ReadySpec;
use crate::SemanticError;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use trace_core::SetupError;

use super::client::*;

/// Settling time after `experimental/serverStatus {quiescent: true}`.
pub const QUIESCENCE_SETTLE: Duration = Duration::from_millis(750);

/// Quiet time after the last progress token ended (a server often ends one phase and
/// begins the next at once).
pub const PROGRESS_SETTLE: Duration = Duration::from_millis(300);

/// Quiet time after jdtls `ServiceReady` and the end of all progress.
pub const LANGUAGE_STATUS_SETTLE: Duration = Duration::from_secs(2);

/// Interval of `symbol_poll` readiness requests.
pub const SYMBOL_POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Granularity of readiness waits.
pub(super) const POLL_STEP: Duration = Duration::from_millis(100);

/// Work-done progress seen from a server.
#[derive(Clone, Debug, Default)]
pub struct Readiness {
    /// Work-done progress tokens (and indexing phases) begun and not yet ended.
    pub(super) active: BTreeSet<String>,
    /// Ended progress tokens and their end `message` ("" when the server sent none), at most
    /// [`MAX_ENDED_TOKENS`].
    pub(super) ended: BTreeMap<String, String>,
    /// Tokens in the order they first ended (eviction order of `ended`).
    pub(super) ended_order: VecDeque<String>,
    /// Any progress / indexing signal seen.
    pub(super) signalled: bool,
    /// Last change of the active set.
    pub(super) changed: Option<Instant>,
    /// `Some(true)` once a readiness wait succeeded on a signal, `None` otherwise.
    pub(super) outcome: Option<bool>,
}

impl Readiness {
    /// Track one notification; returns whether it was a progress signal.
    pub fn observe(&mut self, method: &str, params: Option<&Value>, now: Instant) -> bool {
        match method {
            "$/progress" => {
                let Some(params) = params else { return false };
                let token = match params.get("token") {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => return false,
                };
                match params
                    .get("value")
                    .and_then(|v| v.get("kind"))
                    .and_then(Value::as_str)
                {
                    Some("begin") => {
                        self.ended.remove(&token);
                        self.active.insert(token);
                    }
                    Some("end") => {
                        let message = params
                            .get("value")
                            .and_then(|v| v.get("message"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        self.active.remove(&token);
                        self.remember_ended(token, message);
                    }
                    Some("report") => {}
                    _ => return false,
                }
            }
            "indexingStarted" => {
                self.active.insert("<indexing>".into());
            }
            "indexingEnded" => {
                self.active.remove("<indexing>");
            }
            _ => return false,
        }
        self.signalled = true;
        self.changed = Some(now);
        true
    }

    /// Record an ended token; the oldest ended tokens are forgotten beyond the bound (a
    /// server makes a new token per operation for the whole life of the session).
    pub(super) fn remember_ended(&mut self, token: String, message: String) {
        if !self.ended_order.contains(&token) {
            self.ended_order.push_back(token.clone());
        }
        self.ended.insert(token, message);
        while self.ended.len() > MAX_ENDED_TOKENS || self.ended_order.len() > 2 * MAX_ENDED_TOKENS {
            match self.ended_order.pop_front() {
                Some(old) => {
                    self.ended.remove(&old);
                }
                None => break,
            }
        }
    }

    /// No progress is running and the last change (or `since`) is at least `settle` ago.
    pub fn quiet(&self, now: Instant, since: Instant, settle: Duration) -> bool {
        let last = self.changed.map_or(since, |t| t.max(since));
        self.active.is_empty() && now >= last + settle
    }

    /// Whether the active set changed after `since`.
    pub fn changed_since(&self, since: Instant) -> bool {
        self.changed.is_some_and(|t| t > since)
    }

    /// Progress tokens still running.
    pub fn busy(&self) -> bool {
        !self.active.is_empty()
    }

    /// Whether progress `token` is running.
    pub fn running(&self, token: &str) -> bool {
        self.active.contains(token)
    }

    /// The end message of progress `token` once it ended ("" without a message).
    pub fn ended(&self, token: &str) -> Option<&str> {
        self.ended.get(token).map(String::as_str)
    }

    /// Whether any progress / indexing signal was seen.
    pub fn signalled(&self) -> bool {
        self.signalled
    }

    /// Record a successful readiness wait on a signal.
    pub fn record_ready(&mut self) {
        self.outcome = Some(true);
    }

    /// `Some(true)` a signal was waited for, `None` no signal.
    pub fn outcome(&self) -> Option<bool> {
        self.outcome
    }
}

/// Combine the readiness of several processes of one pool: any signalled readiness, else
/// `None` (a timeout is an error, never a value).
pub fn combine_ready(values: impl IntoIterator<Item = Option<bool>>) -> Option<bool> {
    let mut out = None;
    for v in values {
        if v.is_some() {
            out = v;
        }
    }
    out
}

/// `symbol_poll` readiness: ready once the count is non-zero and equal to the previous poll.
#[derive(Clone, Copy, Debug, Default)]
pub struct SymbolPoll {
    pub(super) last: Option<usize>,
}

impl SymbolPoll {
    /// Record one poll's symbol count; `true` = ready.
    pub fn observe(&mut self, count: usize) -> bool {
        let stable = count > 0 && self.last == Some(count);
        self.last = Some(count);
        stable
    }
}

impl LspClient {
    /// After the last hook that reads diagnostics ran for this process (`warm_up`): later
    /// `publishDiagnostics` are not kept (the reader drops them before queueing).
    pub fn mark_warmed(&mut self) {
        self.keep_diagnostics.store(false, Ordering::Relaxed);
    }

    /// After workspace changes: wait (bounded by the readiness timeout) until a quiescence
    /// server is quiescent again, and until progress a server began after the changes ended.
    pub fn settle_after_changes(&mut self) -> Result<(), SemanticError> {
        let deadline = Instant::now() + self.opts.ready_timeout;
        match self.opts.ready {
            ReadySpec::Quiescent => self.wait_until(deadline, &|c, now| {
                c.quiescent_since.is_some_and(|t| now >= t + QUIESCENCE_SETTLE)
            }),
            ReadySpec::Progress | ReadySpec::LanguageStatus | ReadySpec::Log { .. } => {
                if !self.readiness.busy() {
                    return Ok(());
                }
                let since = Instant::now();
                self.wait_until(deadline, &move |c, now| c.readiness.quiet(now, since, PROGRESS_SETTLE))
            }
            _ => Ok(()),
        }
    }

    /// Wait (bounded by the readiness timeout) for work-done progress `token` to end and
    /// return its end message ("" without one). `None` when the token did not begin within
    /// `grace` (the server had nothing to load). A server that does not end it in time is
    /// the readiness timeout error, never a silent answer.
    pub fn wait_progress(&mut self, token: &str, grace: Duration) -> Result<Option<String>, SemanticError> {
        let started = Instant::now();
        let deadline = started + self.opts.ready_timeout;
        let key = token.to_string();
        self.wait_until(deadline, &move |c, now| {
            c.readiness.ended(&key).is_some() || (!c.readiness.running(&key) && now >= started + grace)
        })?;
        Ok(self.readiness.ended(token).map(str::to_string))
    }

    /// Wait (bounded by the readiness timeout) for the work-done progress that the documents
    /// just opened start: up to `grace` for a first progress change after now, then until no
    /// progress runs and the last change is `settle` ago. For servers that load project state
    /// only for open documents (haskell-language-server: cradle + typecheck after `didOpen`).
    pub fn wait_progress_after_open(
        &mut self,
        grace: Duration,
        settle: Duration,
    ) -> Result<(), SemanticError> {
        let since = Instant::now();
        let deadline = since + self.opts.ready_timeout;
        self.wait_until(deadline, &move |c, now| {
            (c.readiness.changed_since(since) || c.readiness.busy() || now >= since + grace)
                && c.readiness.quiet(now, since, settle)
        })
    }

    /// `Some(true)` the server signalled readiness and it was waited for, `None` no signal.
    pub fn ready(&self) -> Option<bool> {
        self.readiness.outcome()
    }

    /// Whether the server reported quiescence (`experimental/serverStatus`).
    pub fn quiescent(&self) -> bool {
        self.quiescent
    }

    /// The readiness wait of the entry's [`ReadySpec`] (module docs).
    pub(super) fn wait_ready(&mut self) -> Result<(), SemanticError> {
        let deadline = self.ready_deadline;
        let since = self.initialized_at;
        match self.opts.ready.clone() {
            ReadySpec::None => return Ok(()),
            ReadySpec::Progress => {
                let (grace, settle) = (self.opts.progress_grace, self.opts.progress_settle);
                self.wait_until(deadline, &move |c, now| {
                    (c.readiness.signalled() || now >= since + grace) && c.readiness.quiet(now, since, settle)
                })?;
                if !self.readiness.signalled() {
                    return Ok(());
                }
            }
            ReadySpec::Quiescent => {
                self.wait_until(deadline, &|c, now| {
                    c.quiescent_since.is_some_and(|t| now >= t + QUIESCENCE_SETTLE)
                })?;
            }
            ReadySpec::LanguageStatus => {
                self.wait_until(deadline, &|c, _| c.service_ready)?;
                let at = Instant::now();
                self.wait_until(deadline, &move |c, now| c.readiness.quiet(now, at, LANGUAGE_STATUS_SETTLE))?;
            }
            ReadySpec::Notification { method } => {
                self.wait_until(deadline, &move |c, _| c.seen.contains(&method))?;
            }
            ReadySpec::Request { method, params } => {
                let timeout = deadline.saturating_duration_since(Instant::now());
                match self.batch(vec![(method, params)], deadline, timeout) {
                    Ok(mut results) => {
                        results
                            .pop()
                            .unwrap_or_else(|| Err(SemanticError::Protocol("missing response".into())))?;
                    }
                    Err(SemanticError::Timeout { .. }) | Err(SemanticError::Deadline) => {
                        return Err(self.timeout_error())
                    }
                    Err(e) => return Err(e),
                }
            }
            ReadySpec::Log { .. } => {
                self.wait_until(deadline, &|c, _| c.log_matched)?;
                let at = Instant::now();
                self.wait_until(deadline, &move |c, now| c.readiness.quiet(now, at, PROGRESS_SETTLE))?;
            }
            ReadySpec::SymbolPoll => self.wait_symbols(deadline)?,
        }
        self.readiness.record_ready();
        Ok(())
    }

    /// `symbol_poll`: `workspace/symbol` every [`SYMBOL_POLL_INTERVAL`] until stable.
    pub(super) fn wait_symbols(&mut self, deadline: Instant) -> Result<(), SemanticError> {
        // No declared name to poll for (a project of scripts only): the server is ready once
        // it answered one `workspace/symbol` request (it answers only after loading).
        let (query, answered_is_ready) = match self.opts.symbol_poll_query.clone() {
            Some(q) => (q, false),
            None => ("a".to_string(), true),
        };
        let mut poll = SymbolPoll::default();
        loop {
            let timeout = deadline.saturating_duration_since(Instant::now());
            let answer = match self.batch(
                vec![("workspace/symbol".to_string(), json!({"query": query}))],
                deadline,
                timeout,
            ) {
                Ok(mut results) => results.pop().unwrap_or(Ok(Value::Null)),
                Err(SemanticError::Timeout { .. }) | Err(SemanticError::Deadline) => {
                    return Err(self.timeout_error())
                }
                Err(e) => return Err(e),
            };
            let count = match answer? {
                Value::Array(items) => items.len(),
                _ => 0,
            };
            if answered_is_ready || poll.observe(count) {
                return Ok(());
            }
            let next = Instant::now() + SYMBOL_POLL_INTERVAL;
            if next >= deadline {
                return Err(self.timeout_error());
            }
            while Instant::now() < next {
                let _ = self.next_response(next)?;
            }
        }
    }

    /// Serve the server (notifications, requests) until `done` holds; the readiness timeout
    /// error when `deadline` passes first.
    pub(super) fn wait_until(
        &mut self,
        deadline: Instant,
        done: &dyn Fn(&LspClient, Instant) -> bool,
    ) -> Result<(), SemanticError> {
        loop {
            let now = Instant::now();
            if done(self, now) {
                return Ok(());
            }
            if now >= deadline {
                return Err(self.timeout_error());
            }
            let until = (now + POLL_STEP).min(deadline);
            // Stray responses (none expected here) are ignored.
            let _ = self.next_response(until)?;
        }
    }

    /// `SetupError::ServerTimeout` for this server.
    pub(super) fn timeout_error(&self) -> SemanticError {
        let secs = self.opts.ready_timeout.as_secs();
        let minutes = u32::try_from(secs.div_ceil(60).max(1)).unwrap_or(u32::MAX);
        SemanticError::Setup(SetupError::ServerTimeout {
            language: self.opts.language,
            minutes,
            log: self.log_path.clone(),
        })
    }
}
