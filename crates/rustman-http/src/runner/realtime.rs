//! `kind: ws` and `kind: sse` requests in a run (add-realtime-client).
//!
//! The runner resolves a realtime request, learns its secrets, evaluates
//! its assertions and captures over the received messages
//! (`$.messages[*]`) and records the session (handshake, timeline, end) in
//! history under the handshake request, as for HTTP requests. Running the session (handshake, steps, bounds) belongs to the
//! `rustman-realtime` crate, which depends on this one: front ends hand the
//! runner a [`RealtimeExecutor`] through [`super::RunOptions::realtime`].
//! Without one, realtime requests are skipped as an unsupported kind.

use std::borrow::Cow;
use std::pin::Pin;
use std::time::Duration;

use rustman_core::Redactor;
use rustman_core::assertions::{self, AssertionResult, Context, RealtimeFacts, ResponseView};
use rustman_core::model::{Request, unlocated};
use rustman_core::redact::remember_for_logs;
use rustman_core::run::{RealtimeRecord, RequestRecord, RequestStatus, SkipReason};
use rustman_core::vars::{ResolvedRealtime, ResolvedRequest, Resolver, resolve_realtime};
use rustman_core::workspace::RequestRef;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::graphql::{STOP_GRACE, until_cancelled};
use super::{Runner, Sent};
use crate::engine::{HttpEngine, SendOptions};

/// What a [`RealtimeExecutor`] gets to run one session.
pub struct RealtimeCall<'a> {
    pub request: &'a RequestRef<'a>,
    /// The request with every placeholder of the run resolved.
    pub resolved: &'a ResolvedRealtime,
    /// The run's engine (SSE streams, auth helpers).
    pub engine: &'a HttpEngine,
    /// The transport options of the run (timeouts, TLS, proxies).
    pub options: &'a SendOptions,
    /// Cancelled on Ctrl+C: the session closes.
    pub cancel: &'a CancellationToken,
}

/// What running a session produced. Nothing here is redacted yet: the
/// runner redacts it with every secret of the run.
pub struct RealtimeOutcome {
    /// The request that opened the session (the handshake), as recorded.
    pub request: ResolvedRequest,
    /// Why the session failed (handshake, transport, a step).
    pub error: Option<String>,
    /// The handshake, timeline and end, not yet redacted.
    pub record: RealtimeRecord,
    /// Every received message in full, in order: its payload parsed as
    /// JSON, or its text. What `$.messages` holds for assertions.
    pub messages: Vec<Value>,
    /// The handshake response's status and headers (`0` and none without
    /// one).
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// When the first message arrived, since the session started.
    pub first_message: Option<Duration>,
    pub facts: RealtimeFacts,
    pub elapsed: Duration,
    /// What the session's opening requests added on the wire (auth
    /// helpers, cookies from the jar) and the credentials their responses
    /// set: redacted, with the run's secrets, wherever the run reports.
    pub credentials: Redactor,
}

impl RealtimeOutcome {
    /// What assertions and captures read: the handshake status and
    /// headers, `{"messages": [...]}` as the body, the time to the first
    /// message as `ttfb` and the session's length as `total`.
    pub fn view(&self) -> ResponseView<'static> {
        let body = json!({ "messages": self.messages }).to_string();
        ResponseView {
            status: self.status,
            headers: self.headers.clone(),
            body: Cow::Owned(body.into_bytes()),
            binary: false,
            ttfb: self.first_message.unwrap_or(self.elapsed),
            total: self.elapsed,
            grpc: None,
        }
    }
}

/// The future a [`RealtimeExecutor`] returns. The error is shown as the
/// request's error, redacted.
pub type RealtimeFuture<'a> = Pin<Box<dyn Future<Output = Result<RealtimeOutcome, String>> + 'a>>;

/// Runs realtime sessions for a run.
pub trait RealtimeExecutor: Sync {
    fn execute<'a>(&'a self, call: RealtimeCall<'a>) -> RealtimeFuture<'a>;
}

impl Runner<'_> {
    /// Resolve and run the realtime request `request`.
    pub(super) async fn execute_realtime(
        &self,
        request: &RequestRef<'_>,
        record: RequestRecord,
        iteration: u32,
    ) -> Sent {
        let Some(executor) = self.options.realtime else {
            return Sent::skipped(record, SkipReason::UnsupportedKind);
        };
        let captured = self.lock_captures().clone();
        let mut resolver = self.resolver(request, &captured, iteration);
        let resolve = |r: &Resolver<'_>| resolve_realtime(request, r);
        let resolved = match self.with_secrets(&mut resolver, resolve).await {
            Ok(resolved) => resolved,
            Err(e) => return Sent::errored(record, e.to_string()),
        };
        let http = resolved.http().clone();
        self.remember(&http.secrets);
        remember_for_logs(&self.request_redactor(Some(&http), None));
        let options = match self.send_options(&http) {
            Ok(options) => options,
            Err(e) => return Sent::errored(record, e.to_string()).with_resolved(http),
        };
        let call = RealtimeCall {
            request,
            resolved: &resolved,
            engine: &self.engine,
            options: &options,
            cancel: &self.options.cancel,
        };
        let outcome = until_cancelled(executor.execute(call), &self.options.cancel, STOP_GRACE);
        match outcome.await {
            None => Sent::skipped(record, SkipReason::Interrupted),
            Some(Err(message)) => Sent::errored(record, message).with_resolved(http),
            Some(Ok(outcome)) => self.check_realtime(request, &resolver, outcome, record),
        }
    }

    /// Evaluate the session's assertions and captures, and decide how it
    /// ended.
    fn check_realtime(
        &self,
        request: &RequestRef<'_>,
        resolver: &Resolver<'_>,
        outcome: RealtimeOutcome,
        mut record: RequestRecord,
    ) -> Sent {
        self.remember_all(&outcome.credentials);
        let mut redactor = self.request_redactor(Some(&outcome.request), None);
        match &outcome.error {
            Some(error) => record = record.errored(redactor.text(error).into_owned()),
            None => self.check_session(request, resolver, &outcome, &mut record, &mut redactor),
        }
        record.realtime = Some(outcome.record.redacted(&redactor));
        let sent = Sent {
            record,
            exchange: None,
            resolved: None,
            discard: None,
            redactor: Redactor::new(),
            snapshots: Vec::new(),
            mcp_sent: None,
            grpc: None,
        };
        sent.with_resolved(outcome.request)
    }

    /// Run the assertions and captures of `request` over the session.
    fn check_session(
        &self,
        request: &RequestRef<'_>,
        resolver: &Resolver<'_>,
        outcome: &RealtimeOutcome,
        record: &mut RequestRecord,
        redactor: &mut Redactor,
    ) {
        let (written, captures) = match request.request() {
            Request::Ws(ws) => (ws.assertions.as_deref(), ws.captures.as_deref()),
            Request::Sse(sse) => (sse.assertions.as_deref(), sse.captures.as_deref()),
            _ => (None, None),
        };
        let view = outcome.view();
        let ctx = Context {
            resolver,
            request: Some(request),
            redactor,
        };
        let results =
            assertions::evaluate_realtime(&unlocated(written), &view, &ctx, &outcome.facts);
        let report = self.capture(captures.unwrap_or_default(), &view, redactor);
        let passed = results.iter().all(AssertionResult::passed) && report.failures.is_empty();
        record.status = if passed {
            RequestStatus::Passed
        } else {
            RequestStatus::Failed
        };
        if !report.failures.is_empty() {
            record.error = Some(redactor.text(&report.failures.join("; ")).into_owned());
        }
        let warnings = report
            .warnings
            .iter()
            .map(|w| redactor.text(w).into_owned());
        record.warnings.extend(warnings);
        record.assertions = results;
        record.captures = report.results;
        for capture in &mut record.captures {
            if let Some(message) = &mut capture.message {
                *message = redactor.text(message).into_owned();
            }
        }
    }
}
