//! `kind: jsonrpc` requests in a run (add-jsonrpc-client tasks 4.2, 4.3).
//!
//! The runner resolves a jsonrpc request, learns its secrets, evaluates
//! each call's assertions and captures against that call's response
//! envelope (design D3) and records the exchange, as for HTTP requests.
//! Sending it (catalog validation, the HTTP or WebSocket transport,
//! subscriptions) belongs to the `rustman-jsonrpc` crate, which depends on
//! this one: front ends hand the runner a [`JsonrpcExecutor`] through
//! [`super::RunOptions::jsonrpc`]. Without one, jsonrpc requests are
//! skipped as an unsupported kind.

use std::borrow::Cow;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use rustman_core::Redactor;
use rustman_core::assertions::{self, AssertionResult, Context, ResponseView};
use rustman_core::model::{Assertion, CallFields, JsonrpcRequest, RpcErrorExpectation, unlocated};
use rustman_core::redact::remember_for_logs;
use rustman_core::run::{JsonrpcRecord, RequestRecord, RequestStatus, SkipReason};
use rustman_core::settings::Settings;
use rustman_core::snapshot::{Exchange as SnapshotExchange, SnapshotReview};
use rustman_core::trace::SentTrace;
use rustman_core::vars::{ResolvedJsonrpc, ResolvedRequest, Resolver, resolve_jsonrpc};
use rustman_core::workspace::RequestRef;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::graphql::{STOP_GRACE, until_cancelled};
use super::record::{exchange_record, redact_capture_messages, sent_trace_record};
use super::snapshots::SnapshotTarget;
use super::{Runner, Sent};
use crate::engine::{HttpEngine, SendOptions};
use crate::exchange::Exchange;

/// What a [`JsonrpcExecutor`] gets to send one jsonrpc request.
pub struct JsonrpcCall<'a> {
    pub request: &'a RequestRef<'a>,
    pub jsonrpc: &'a JsonrpcRequest,
    /// The request with every placeholder of the run resolved.
    pub resolved: &'a ResolvedJsonrpc,
    /// The run's engine (connection pools, Digest nonces).
    pub engine: &'a HttpEngine,
    /// The transport options of the run (timeout, TLS, cookie jar).
    pub options: &'a SendOptions,
    /// The workspace root (the OpenRPC catalogs live under it).
    pub root: &'a Path,
    pub settings: &'a Settings,
    /// Cancelled on Ctrl+C: a subscription stops collecting.
    pub cancel: &'a CancellationToken,
}

/// How one call ended, as its assertions and captures see it.
#[derive(Clone, Debug, PartialEq)]
pub struct JsonrpcCallCheck {
    /// The document they read: the call's response envelope (plus
    /// `events` for a subscription), or `{}` without one.
    pub envelope: Value,
    /// Why the call failed: `rpc error <code>: <message>`, or why no
    /// usable response came back.
    pub failure: Option<String>,
    /// The failure is an error object, which an `rpc_error` assertion may
    /// expect.
    pub rpc_error: bool,
}

/// What sending a jsonrpc request produced.
pub struct JsonrpcOutcome {
    /// The request sent: the POST with its envelope, or the WebSocket
    /// handshake request.
    pub request: ResolvedRequest,
    /// The HTTP exchange, for HTTP endpoints that answered.
    pub exchange: Option<Exchange>,
    /// One per call, in file order.
    pub calls: Vec<JsonrpcCallCheck>,
    /// Why the exchange produced no usable answer (transport error,
    /// invalid response, a refusal); the request errors.
    pub error: Option<String>,
    /// Not yet redacted: the runner redacts it with every secret of the run.
    pub record: JsonrpcRecord,
    pub elapsed: Duration,
    /// Over WebSocket, where there is no HTTP exchange: the trace context
    /// of the connection.s handshake, which every call on it shares, and
    /// the handshake response.s `traceresponse` header.
    pub trace: Option<SentTrace>,
    pub traceresponse: Option<String>,
}

/// The future a [`JsonrpcExecutor`] returns. The error is shown as the
/// request's error, redacted.
pub type JsonrpcFuture<'a> = Pin<Box<dyn Future<Output = Result<JsonrpcOutcome, String>> + 'a>>;

/// Sends jsonrpc requests for a run.
pub trait JsonrpcExecutor: Sync {
    fn execute<'a>(&'a self, call: JsonrpcCall<'a>) -> JsonrpcFuture<'a>;
}

impl Runner<'_> {
    /// Resolve, send, check and capture the jsonrpc request `request`.
    pub(super) async fn execute_jsonrpc(
        &self,
        request: &RequestRef<'_>,
        jsonrpc: &JsonrpcRequest,
        record: RequestRecord,
        iteration: u32,
    ) -> Sent {
        let Some(executor) = self.options.jsonrpc else {
            return Sent::skipped(record, SkipReason::UnsupportedKind);
        };
        let captured = self.lock_captures().clone();
        let mut resolver = self.resolver(request, &captured, iteration);
        let resolve = |r: &Resolver<'_>| resolve_jsonrpc(request, r);
        let resolved = match self.with_secrets(&mut resolver, resolve).await {
            Ok(resolved) => resolved,
            Err(e) => return Sent::errored(record, e.to_string()),
        };
        self.remember(&resolved.http.secrets);
        remember_for_logs(&self.request_redactor(Some(&resolved.http), None));
        let options = match self.send_options(&resolved.http) {
            Ok(options) => options,
            Err(e) => return Sent::errored(record, e.to_string()).with_resolved(resolved.http),
        };
        let call = JsonrpcCall {
            request,
            jsonrpc,
            resolved: &resolved,
            engine: &self.engine,
            options: &options,
            root: self.ctx.root,
            settings: self.ctx.settings,
            cancel: &self.options.cancel,
        };
        let grace = match resolved.subscription {
            Some(_) => STOP_GRACE,
            None => Duration::ZERO,
        };
        let outcome = until_cancelled(executor.execute(call), &self.options.cancel, grace).await;
        let outcome = match outcome {
            None => return Sent::skipped(record, SkipReason::Interrupted),
            Some(Err(message)) => {
                return Sent::errored(record, message).with_resolved(resolved.http);
            }
            Some(Ok(outcome)) => outcome,
        };
        let checked = RpcChecked {
            request,
            resolver: &resolver,
            calls: jsonrpc.calls(),
            subscription: jsonrpc.subscription.is_some(),
        };
        self.check_jsonrpc(&checked, outcome, record)
    }

    /// Evaluate each call's assertions and captures, and decide how the
    /// request ended.
    fn check_jsonrpc(
        &self,
        checked: &RpcChecked<'_, '_>,
        outcome: JsonrpcOutcome,
        mut record: RequestRecord,
    ) -> Sent {
        let mut redactor = self.request_redactor(Some(&outcome.request), outcome.exchange.as_ref());
        if let Some(exchange) = &outcome.exchange {
            record.exchange = Some(exchange_record(&outcome.request, exchange, &redactor));
        }
        let mut snapshots = Vec::new();
        if let Some(error) = &outcome.error {
            record = record.errored(redactor.text(error).into_owned());
        } else {
            let mut verdict = self.check_calls(checked, &outcome, &mut redactor);
            snapshots = std::mem::take(&mut verdict.snapshots);
            verdict.apply(&mut record, &outcome.record, &redactor);
        }
        if outcome.exchange.is_none() {
            record.trace = outcome.trace.as_ref().map(|sent| {
                let traceresponse = outcome.traceresponse.as_deref();
                let link = self.link.as_ref();
                sent_trace_record(sent, traceresponse, outcome.elapsed, link, &redactor)
            });
        }
        record.jsonrpc = Some(outcome.record);
        let sent = Sent {
            exchange: outcome.exchange,
            snapshots,
            ..Sent::from_record(record)
        };
        sent.with_resolved(outcome.request)
    }

    fn check_calls(
        &self,
        checked: &RpcChecked<'_, '_>,
        outcome: &JsonrpcOutcome,
        redactor: &mut Redactor,
    ) -> Verdict {
        let mut verdict = Verdict::default();
        let batch = outcome.record.batch;
        for (index, (fields, call)) in checked.calls.iter().zip(&outcome.calls).enumerate() {
            let view = call_view(outcome, call);
            let assertions = unlocated(Some(fields.assertions));
            let plugins = self.bare_plugin_calls();
            let ctx = Context {
                resolver: checked.resolver,
                request: Some(checked.request),
                redactor,
                plugins: plugins.as_ref(),
            };
            let mut results = assertions::evaluate(&assertions, &view, &ctx);
            let target = SnapshotTarget {
                request: checked.request,
                assertions: &assertions,
            };
            let exchange = SnapshotExchange {
                kind: "jsonrpc",
                status: outcome.exchange.as_ref().map(|e| e.status),
                headers: &view.headers,
                body: &view.body,
                events: call_events(checked, call),
            };
            let snapshots =
                self.check_exchange_snapshots(&target, &exchange, redactor, &mut results);
            verdict.warnings.extend(snapshots.warnings);
            verdict.snapshots.extend(snapshots.reviews);
            if batch {
                let method = fields.method.unwrap_or_default();
                prefix(&mut results, &format!("call {} ({method}): ", index + 1));
            }
            verdict.assertions.extend(results);
            let report = self.capture(fields.captures, &view, redactor);
            verdict.capture_failures.extend(report.failures);
            verdict.warnings.extend(report.warnings);
            verdict.captures.extend(report.results);
            if let Some(failure) = &call.failure
                && !(call.rpc_error && expects_error(&assertions))
                && !verdict.failures.contains(failure)
            {
                verdict.failures.push(failure.clone());
            }
        }
        verdict
    }
}

/// A jsonrpc request whose calls are being checked.
struct RpcChecked<'c, 'r> {
    request: &'c RequestRef<'r>,
    resolver: &'c Resolver<'c>,
    calls: Vec<CallFields<'c>>,
    /// A subscription: its snapshots compare the event list.
    subscription: bool,
}

/// What checking the calls of an exchange found.
#[derive(Default)]
struct Verdict {
    assertions: Vec<AssertionResult>,
    captures: Vec<rustman_core::captures::CaptureResult>,
    /// Calls that failed with no `rpc_error` assertion expecting it.
    failures: Vec<String>,
    capture_failures: Vec<String>,
    warnings: Vec<String>,
    /// The failed, missing and new snapshots, for a review.
    snapshots: Vec<SnapshotReview>,
}

impl Verdict {
    fn apply(self, record: &mut RequestRecord, jsonrpc: &JsonrpcRecord, redactor: &Redactor) {
        let passed = self.assertions.iter().all(AssertionResult::passed)
            && self.failures.is_empty()
            && self.capture_failures.is_empty();
        record.status = if passed {
            RequestStatus::Passed
        } else {
            RequestStatus::Failed
        };
        let mut failures = self.failures;
        if let (Some(note), [first]) = (&jsonrpc.note, failures.as_mut_slice()) {
            *first = format!("{first} ({note})");
        }
        failures.extend(self.capture_failures);
        if !failures.is_empty() {
            record.error = Some(redactor.text(&failures.join("; ")).into_owned());
        }
        let warnings = self.warnings.iter().map(|w| redactor.text(w).into_owned());
        record.warnings.extend(warnings);
        record.assertions = self.assertions;
        record.captures = self.captures;
        redact_capture_messages(&mut record.captures, redactor);
    }
}

/// The response assertions and captures of `call` read: the HTTP status,
/// headers and timings when there are some, with the call's envelope as
/// the body.
fn call_view<'o>(outcome: &'o JsonrpcOutcome, call: &JsonrpcCallCheck) -> ResponseView<'o> {
    let mut view = outcome
        .exchange
        .as_ref()
        .and_then(|exchange| exchange.view(false).ok())
        .unwrap_or_else(|| ResponseView {
            total: outcome.elapsed,
            ttfb: outcome.elapsed,
            ..ResponseView::default()
        });
    view.body = Cow::Owned(call.envelope.to_string().into_bytes());
    view.binary = false;
    view
}

/// The events a subscription collected, which its snapshots compare (as
/// for GraphQL subscriptions); `None` for other calls.
fn call_events<'c>(
    checked: &RpcChecked<'_, '_>,
    call: &'c JsonrpcCallCheck,
) -> Option<&'c [Value]> {
    if !checked.subscription {
        return None;
    }
    call.envelope
        .get("events")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
}

/// Whether `assertions` expect the call to fail with an error code.
fn expects_error(assertions: &[Assertion]) -> bool {
    assertions
        .iter()
        .any(|a| matches!(a, Assertion::RpcError(RpcErrorExpectation::Code { .. })))
}

/// Name the batch call each result belongs to.
fn prefix(results: &mut [AssertionResult], label: &str) {
    for result in results {
        result.label.insert_str(0, label);
        if !result.message.is_empty() {
            result.message.insert_str(0, label);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_code_expectations_excuse_an_error() {
        let code = Assertion::RpcError(RpcErrorExpectation::Code {
            code: -1,
            message_contains: None,
        });
        assert!(expects_error(&[code]));
        assert!(!expects_error(&[Assertion::RpcError(
            RpcErrorExpectation::None
        )]));
        assert!(!expects_error(&[]));
    }

    #[test]
    fn batch_results_name_their_call() {
        let mut results = vec![AssertionResult {
            kind: "json",
            outcome: assertions::Outcome::Fail,
            label: "json $.result".into(),
            expected: None,
            actual: None,
            message: "json $.result: expected 1, got 2".into(),
            snapshot: None,
        }];
        prefix(&mut results, "call 2 (m): ");
        assert_eq!(
            results[0].message,
            "call 2 (m): json $.result: expected 1, got 2"
        );
        assert_eq!(results[0].label, "call 2 (m): json $.result");
    }
}
