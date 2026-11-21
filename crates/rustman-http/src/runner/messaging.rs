//! `kind: kafka`, `kind: mqtt` and `kind: amqp` requests in a run
//! (add-messaging-client).
//!
//! The runner resolves a messaging request, learns its secrets, evaluates
//! its assertions and captures over the exchange (`$.messages[*]` for a
//! consume, `$.produced[*]` for a produce) and records it in history.
//! Running the operation belongs to the `rustman-messaging` crate, which
//! depends on this one: front ends hand the runner a
//! [`MessagingExecutor`] through [`super::RunOptions::messaging`]. Without
//! one, messaging requests are skipped as an unsupported kind.
//!
//! A consume with `start: before_next` subscribes, then the next request
//! of its unit runs while it collects; see [`Runner::unit`].

use std::borrow::Cow;
use std::pin::Pin;
use std::sync::PoisonError;
use std::time::Duration;

use rustman_core::Redactor;
use rustman_core::assertions::{self, AssertionResult, Context, RealtimeFacts, ResponseView};
use rustman_core::model::{Operation, Request, Start, unlocated};
use rustman_core::redact::remember_for_logs;
use rustman_core::run::{MessagingRecord, RequestRecord, RequestStatus, SkipReason};
use rustman_core::vars::{ResolvedMessaging, Resolver, resolve_messaging};
use rustman_core::workspace::RequestRef;
use serde_json::Value;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::graphql::{STOP_GRACE, until_cancelled};
use super::{Runner, Sent};

/// What a [`MessagingExecutor`] gets to run one operation.
pub struct MessagingCall<'a> {
    pub request: &'a RequestRef<'a>,
    /// The request with every placeholder of the run resolved.
    pub resolved: &'a ResolvedMessaging,
    /// Cancelled on Ctrl+C: the operation stops.
    pub cancel: &'a CancellationToken,
    /// Fired once a consume is subscribed, for `start: before_next`.
    pub ready: Option<oneshot::Sender<()>>,
    /// Every secret of the run so far and the request's own: what a live
    /// feed of the operation must redact.
    pub redactor: &'a Redactor,
}

/// What running an operation produced. Nothing here is redacted yet: the
/// runner redacts it with every secret of the run.
pub struct MessagingOutcome {
    /// The exchange as JSON: `brokers`, `destination`, `produced`,
    /// `messages`, `warnings`. `Null` when nothing ran.
    pub exchange: Value,
    /// Why the operation failed.
    pub error: Option<String>,
    /// The safety policy refused the operation (`blocked: ...`): nothing
    /// was sent.
    pub blocked: bool,
    /// Checks the client made itself, such as the implicit
    /// `asyncapi_schema` assertion of a validated consume.
    pub implicit: Vec<AssertionResult>,
    pub elapsed: Duration,
}

impl MessagingOutcome {
    /// An operation the safety policy refused with `reason`.
    pub fn blocked(reason: String) -> Self {
        Self {
            exchange: Value::Null,
            error: Some(reason),
            blocked: true,
            implicit: Vec::new(),
            elapsed: Duration::ZERO,
        }
    }

    /// What assertions and captures read: the exchange as the body, the
    /// headers of every consumed message in order, and the operation's
    /// length as `ttfb` and `total`.
    pub fn view(&self) -> ResponseView<'static> {
        ResponseView {
            status: 0,
            headers: message_headers(&self.exchange),
            body: Cow::Owned(self.exchange.to_string().into_bytes()),
            binary: false,
            ttfb: self.elapsed,
            total: self.elapsed,
            grpc: None,
        }
    }

    /// The consumed messages (what `messages:` assertions count).
    pub fn received(&self) -> u64 {
        let messages = self.exchange.get("messages").and_then(Value::as_array);
        messages.map_or(0, |m| m.len() as u64)
    }
}

/// `name: value` of every header of every consumed message, in order.
fn message_headers(exchange: &Value) -> Vec<(String, String)> {
    let messages = exchange.get("messages").and_then(Value::as_array);
    let headers = messages
        .into_iter()
        .flatten()
        .filter_map(|m| m.get("headers").and_then(Value::as_array))
        .flatten();
    headers
        .filter_map(|h| {
            let name = h.get("name")?.as_str()?;
            let value = h.get("value")?.as_str()?;
            Some((name.to_owned(), value.to_owned()))
        })
        .collect()
}

/// The future a [`MessagingExecutor`] returns.
pub type MessagingFuture<'a> = Pin<Box<dyn Future<Output = MessagingOutcome> + 'a>>;

/// Runs the messaging operations of a run.
pub trait MessagingExecutor: Sync {
    fn execute<'a>(&'a self, call: MessagingCall<'a>) -> MessagingFuture<'a>;
}

/// Whether `request` is a consume that subscribes before the next request
/// runs.
pub(super) fn starts_before_next(request: &RequestRef<'_>) -> bool {
    match request.request() {
        Request::Messaging(m) => {
            m.effective_operation() == Operation::Consume
                && m.effective_start() == Start::BeforeNext
        }
        _ => false,
    }
}

impl Runner<'_> {
    /// Resolve and run the messaging request `request`.
    pub(super) async fn execute_messaging(
        &self,
        request: &RequestRef<'_>,
        record: RequestRecord,
        iteration: u32,
    ) -> Sent {
        let ready = self.take_ready(request);
        let Some(executor) = self.options.messaging else {
            return Sent::skipped(record, SkipReason::UnsupportedKind);
        };
        let captured = self.lock_captures().clone();
        let mut resolver = self.resolver(request, &captured, iteration);
        let resolve = |r: &Resolver<'_>| resolve_messaging(request, r);
        let resolved = match self.with_secrets(&mut resolver, resolve).await {
            Ok(resolved) => resolved,
            Err(e) => return Sent::errored(record, e.to_string()),
        };
        let connection = resolved.connection.clone();
        self.remember(&connection.secrets);
        let redactor = self.request_redactor(Some(&connection), None);
        remember_for_logs(&redactor);
        let call = MessagingCall {
            request,
            resolved: &resolved,
            cancel: &self.options.cancel,
            ready,
            redactor: &redactor,
        };
        let outcome = until_cancelled(executor.execute(call), &self.options.cancel, STOP_GRACE);
        let Some(outcome) = outcome.await else {
            return Sent::skipped(record, SkipReason::Interrupted);
        };
        let sent = self.check_messaging(request, &resolver, &resolved, &outcome, record);
        sent.with_resolved(connection)
    }

    /// The readiness signal [`Runner::unit`] left for `request`.
    fn take_ready(&self, request: &RequestRef<'_>) -> Option<oneshot::Sender<()>> {
        let mut waiting = self.ready.lock().unwrap_or_else(PoisonError::into_inner);
        let id = request.entry.id();
        let index = waiting.iter().position(|(waiting, _)| *waiting == id)?;
        Some(waiting.swap_remove(index).1)
    }

    /// Leave `ready` for the next execution of `request`, in place of any
    /// signal a skipped execution left.
    pub(super) fn leave_ready(&self, request: &RequestRef<'_>, ready: oneshot::Sender<()>) {
        let mut waiting = self.ready.lock().unwrap_or_else(PoisonError::into_inner);
        let id = request.entry.id();
        waiting.retain(|(waiting, _)| *waiting != id);
        waiting.push((id, ready));
    }

    /// Evaluate the operation's assertions and captures, and decide how it
    /// ended.
    fn check_messaging(
        &self,
        request: &RequestRef<'_>,
        resolver: &Resolver<'_>,
        resolved: &ResolvedMessaging,
        outcome: &MessagingOutcome,
        mut record: RequestRecord,
    ) -> Sent {
        let mut redactor = self.request_redactor(Some(&resolved.connection), None);
        match &outcome.error {
            Some(error) => record = record.errored(redactor.text(error).into_owned()),
            None => self.check_operation(request, resolver, outcome, &mut record, &mut redactor),
        }
        let exchange = (!outcome.exchange.is_null()).then(|| outcome.exchange.clone());
        let messaging = MessagingRecord {
            protocol: resolved.kind.as_str().to_owned(),
            operation: resolved.operation.as_str().to_owned(),
            blocked: outcome.blocked,
            exchange,
        };
        record.messaging = Some(messaging.redacted(&redactor));
        Sent::from_record(record)
    }

    /// Run the assertions and captures of `request` over the exchange.
    fn check_operation(
        &self,
        request: &RequestRef<'_>,
        resolver: &Resolver<'_>,
        outcome: &MessagingOutcome,
        record: &mut RequestRecord,
        redactor: &mut Redactor,
    ) {
        let Request::Messaging(messaging) = request.request() else {
            return;
        };
        let view = outcome.view();
        let plugins = self.bare_plugin_calls();
        let ctx = Context {
            resolver,
            request: Some(request),
            redactor,
            plugins: plugins.as_ref(),
        };
        let facts = RealtimeFacts {
            received: outcome.received(),
            close_code: None,
        };
        let written = messaging.assertions.as_deref();
        let mut results = assertions::evaluate_realtime(&unlocated(written), &view, &ctx, &facts);
        results.extend(outcome.implicit.iter().map(|r| redacted(r, ctx.redactor)));
        let captures = messaging.captures.as_deref().unwrap_or_default();
        let report = self.capture(captures, &view, redactor);
        let passed = results.iter().all(AssertionResult::passed) && report.failures.is_empty();
        record.status = if passed {
            RequestStatus::Passed
        } else {
            RequestStatus::Failed
        };
        if !report.failures.is_empty() {
            record.error = Some(redactor.text(&report.failures.join("; ")).into_owned());
        }
        let warnings = exchange_warnings(&outcome.exchange)
            .chain(report.warnings.iter().cloned())
            .map(|w| redactor.text(&w).into_owned());
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

/// The warnings the client recorded (a persistent session, a dropped
/// header, an undecodable value).
fn exchange_warnings(exchange: &Value) -> impl Iterator<Item = String> + '_ {
    let warnings = exchange.get("warnings").and_then(Value::as_array);
    warnings
        .into_iter()
        .flatten()
        .filter_map(|w| w.as_str().map(str::to_owned))
}

/// `result` with every secret of `redactor` replaced in its texts.
fn redacted(result: &AssertionResult, redactor: &Redactor) -> AssertionResult {
    let text = |t: &str| redactor.text(t).into_owned();
    AssertionResult {
        label: text(&result.label),
        expected: result.expected.as_deref().map(text),
        actual: result.actual.as_deref().map(text),
        message: text(&result.message),
        ..result.clone()
    }
}
