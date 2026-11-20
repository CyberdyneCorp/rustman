//! `kind: grpc` requests in a run (add-grpc-client task 5.2, "Runner and
//! history integration").
//!
//! The runner resolves a grpc request, learns its secrets, evaluates its
//! assertions (`grpc_status: OK` when it has none) and captures against
//! the call's status, metadata and response document, and records the
//! exchange with `protocol: grpc`, redacted. Sending it (schema discovery,
//! encoding, the HTTP/2 or gRPC-Web transport) belongs to the
//! `rustman-grpc` crate, which depends on this one: front ends hand the
//! runner a [`GrpcExecutor`] through [`super::RunOptions::grpc`]. Without
//! one, grpc requests are skipped as an unsupported kind.

use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use rustman_core::Redactor;
use rustman_core::assertions::{self, Context, ResponseView};
use rustman_core::model::{Assertion, GrpcRequest, GrpcStatusExpectation};
use rustman_core::redact::remember_for_logs;
use rustman_core::run::{RequestRecord, RequestStatus, SkipReason, redact_value};
use rustman_core::settings::Settings;
use rustman_core::vars::{ResolvedGrpc, Resolver, resolve_grpc};
use rustman_core::workspace::RequestRef;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::graphql::until_cancelled;
use super::record::redact_capture_messages;
use super::{Runner, Sent};
use crate::engine::SendOptions;

/// What a [`GrpcExecutor`] gets to send one grpc request.
pub struct GrpcCall<'a> {
    pub request: &'a RequestRef<'a>,
    pub grpc: &'a GrpcRequest,
    /// The request with every placeholder of the run resolved.
    pub resolved: &'a ResolvedGrpc,
    /// The transport options of the run (TLS, proxy, `--insecure`), for
    /// TLS settings and gRPC-Web.
    pub options: &'a SendOptions,
    /// The workspace root (the descriptor cache lives under it).
    pub root: &'a Path,
    pub settings: &'a Settings,
    /// Inside a workspace: reflection results are cached.
    pub in_workspace: bool,
    /// Cancelled on Ctrl+C.
    pub cancel: &'a CancellationToken,
}

/// What sending a grpc request produced: a call that ended with a status.
pub struct GrpcOutcome {
    /// What assertions and captures read: the status and trailers
    /// (`grpc`), the response headers, the response document as the body
    /// (the response message, or `{"messages": [...]}` for a stream) and
    /// the timings.
    pub view: ResponseView<'static>,
    /// Why the call counts as a request error although it ended with a
    /// status (an expired deadline).
    pub error: Option<String>,
    /// Notices that do not fail the request, such as a schema read from
    /// the descriptor cache.
    pub warnings: Vec<String>,
    /// The credentials of the sensitive response metadata (`set-cookie`,
    /// `authorization`, ...): redacted wherever the request is recorded,
    /// like an HTTP response's.
    pub credentials: Redactor,
    /// The client stopped a response stream at its headless bounds, so
    /// the status is the client's own `CANCELLED`: the default
    /// `grpc_status: OK` expectation does not apply.
    pub stopped: bool,
    /// The exchange as recorded (`protocol: grpc`), not yet redacted: the
    /// runner redacts it with every secret of the run.
    pub record: Value,
}

/// Why a grpc request ended without a status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrpcFailure {
    pub message: String,
    /// The request cannot be sent as written (schema, method, field,
    /// metadata): a configuration error rather than a request error.
    pub config: bool,
}

/// The future a [`GrpcExecutor`] returns.
pub type GrpcFuture<'a> = Pin<Box<dyn Future<Output = Result<GrpcOutcome, GrpcFailure>> + 'a>>;

/// Sends grpc requests for a run.
pub trait GrpcExecutor: Sync {
    fn execute<'a>(&'a self, call: GrpcCall<'a>) -> GrpcFuture<'a>;
}

/// The `kind` of `record.grpc.error` for a configuration error.
pub const GRPC_CONFIG_ERROR: &str = "config";

/// The `kind` of `record.grpc.error` for a request error.
pub const GRPC_REQUEST_ERROR: &str = "request";

impl Runner<'_> {
    /// Resolve, send, check and capture the grpc request `request`.
    pub(super) async fn execute_grpc(
        &self,
        request: &RequestRef<'_>,
        grpc: &GrpcRequest,
        record: RequestRecord,
        iteration: u32,
    ) -> Sent {
        let Some(executor) = self.options.grpc else {
            return Sent::skipped(record, SkipReason::UnsupportedKind);
        };
        let captured = self.lock_captures().clone();
        let mut resolver = self.resolver(request, &captured, iteration);
        let resolve = |r: &Resolver<'_>| resolve_grpc(request, r);
        let resolved = match self.with_secrets(&mut resolver, resolve).await {
            Ok(resolved) => resolved,
            Err(e) => return Sent::errored(record, e.to_string()),
        };
        self.remember(&resolved.request.secrets);
        remember_for_logs(&self.request_redactor(Some(&resolved.request), None));
        let options = match self.send_options(&resolved.request) {
            Ok(options) => options,
            Err(e) => return Sent::errored(record, e.to_string()).with_grpc(resolved),
        };
        let call = GrpcCall {
            request,
            grpc,
            resolved: &resolved,
            options: &options,
            root: self.ctx.root,
            settings: self.ctx.settings,
            in_workspace: self.ctx.in_workspace,
            cancel: &self.options.cancel,
        };
        let cancel = &self.options.cancel;
        let sent = match until_cancelled(executor.execute(call), cancel, Duration::ZERO).await {
            None => return Sent::skipped(record, SkipReason::Interrupted),
            Some(Err(failure)) => failed(record, &resolved, &failure),
            Some(Ok(outcome)) => {
                let checked = GrpcChecked {
                    request,
                    resolver: &resolver,
                    grpc,
                    resolved: &resolved,
                };
                self.check_grpc(&checked, outcome, record)
            }
        };
        sent.with_grpc(resolved)
    }

    /// Evaluate the assertions and captures of a call that ended with a
    /// status, and decide how the request ended.
    fn check_grpc(
        &self,
        checked: &GrpcChecked<'_, '_>,
        outcome: GrpcOutcome,
        mut record: RequestRecord,
    ) -> Sent {
        let mut redactor = self.request_redactor(Some(&checked.resolved.request), None);
        redactor.merge(&outcome.credentials);
        let assertions = expectations(checked.grpc, outcome.stopped);
        let ctx = Context {
            resolver: checked.resolver,
            request: Some(checked.request),
            redactor: &redactor,
        };
        record.assertions = assertions::evaluate(&assertions, &outcome.view, &ctx);
        let captures = checked.grpc.captures.as_deref().unwrap_or_default();
        let report = self.capture(captures, &outcome.view, &mut redactor);
        remember_for_logs(&redactor);
        let passed = record.assertions.iter().all(|a| a.passed()) && !report.failed();
        record.status = match (&outcome.error, passed) {
            (Some(_), _) => RequestStatus::Errored,
            (None, true) => RequestStatus::Passed,
            (None, false) => RequestStatus::Failed,
        };
        let mut errors: Vec<String> = outcome.error.into_iter().collect();
        errors.extend(report.failures);
        if !errors.is_empty() {
            record.error = Some(redactor.text(&errors.join("; ")).into_owned());
        }
        let warnings = outcome.warnings.iter().chain(&report.warnings);
        record.warnings = warnings.map(|w| redactor.text(w).into_owned()).collect();
        record.captures = report.results;
        redact_capture_messages(&mut record.captures, &redactor);
        record.grpc = Some(redact_value(&outcome.record, &redactor));
        Sent {
            redactor: outcome.credentials,
            ..Sent::from_record(record)
        }
    }
}

/// A grpc request whose call is being checked.
struct GrpcChecked<'c, 'r> {
    request: &'c RequestRef<'r>,
    resolver: &'c Resolver<'c>,
    grpc: &'c GrpcRequest,
    resolved: &'c ResolvedGrpc,
}

/// The assertions of `grpc`, or `grpc_status: OK` when it has none (spec
/// "Assertions and captures"), unless the client `stopped` the stream at
/// its bounds (spec "Endless stream bounded": the step completes).
fn expectations(grpc: &GrpcRequest, stopped: bool) -> Vec<Assertion> {
    let assertions = grpc.assertion_list();
    if assertions.is_empty() && !stopped {
        return vec![Assertion::GrpcStatus(GrpcStatusExpectation::OK)];
    }
    assertions
}

/// A request whose call ended without a status: errored, with the error
/// and its kind recorded under `grpc`.
fn failed(record: RequestRecord, resolved: &ResolvedGrpc, failure: &GrpcFailure) -> Sent {
    let kind = if failure.config {
        GRPC_CONFIG_ERROR
    } else {
        GRPC_REQUEST_ERROR
    };
    let mut record = record.errored(failure.message.clone());
    record.grpc = Some(json!({
        "protocol": "grpc",
        "target": resolved.target(),
        "method": resolved.method,
        "error": {"kind": kind, "message": failure.message},
    }));
    Sent::from_record(record)
}
