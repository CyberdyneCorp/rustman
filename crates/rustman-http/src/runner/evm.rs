//! `kind: evm-call` requests in a run (add-evm-toolkit tasks 4.1, 4.2).
//!
//! The runner resolves an evm-call request, learns its secrets, evaluates
//! its assertions and captures against the decoded call, and records the
//! result under `evm`, redacted. Encoding, the node calls and decoding
//! belong to the `rustman-evm` crate, which depends on this one: front ends
//! hand the runner an [`EvmExecutor`] through [`super::RunOptions::evm`].
//! Without one, evm-call requests are skipped as an unsupported kind.
//! `evm-tx` requests are never sent: a run skips them as `signing not
//! requested` (signing is not available in this build).

use std::borrow::Cow;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use rustman_core::assertions::{self, AssertionResult, Context, ResponseView};
use rustman_core::environment::Environment;
use rustman_core::model::{Assertion, EvmCallRequest, unlocated};
use rustman_core::redact::remember_for_logs;
use rustman_core::run::{RequestRecord, RequestStatus, SkipReason, redact_value};
use rustman_core::settings::Settings;
use rustman_core::vars::{
    ResolvedEvmCall, ResolvedRequest, Resolver, resolve_evm_call, resolve_evm_endpoint,
};
use rustman_core::workspace::RequestRef;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::graphql::until_cancelled;
use super::record::redact_capture_messages;
use super::{Runner, Sent};
use crate::engine::{HttpEngine, SendOptions};

/// What an [`EvmExecutor`] gets to run one evm-call request.
pub struct EvmCall<'a> {
    pub request: &'a RequestRef<'a>,
    pub evm: &'a EvmCallRequest,
    /// The request with every placeholder of the run resolved.
    pub resolved: &'a ResolvedEvmCall,
    /// The run's engine (connection pools).
    pub engine: &'a HttpEngine,
    /// The transport options of the run (timeout, TLS).
    pub options: &'a SendOptions,
    /// The workspace root (ABI paths and the ABI cache live under it).
    pub root: &'a Path,
    pub settings: &'a Settings,
    /// The active environment, whose `chain_id` pins the endpoint.
    pub environment: Option<&'a Environment>,
    /// Cancelled on Ctrl+C.
    pub cancel: &'a CancellationToken,
}

/// What running an evm-call produced.
pub struct EvmOutcome {
    /// What captures read: `{"outputs": [...], "events": [...],
    /// "receipt": ...}` with decimal quantities and checksummed addresses.
    pub document: Value,
    /// The results of the request's `evm` assertions, in file order.
    pub evm_assertions: Vec<AssertionResult>,
    /// Why no result came back; the request errors.
    pub error: Option<String>,
    /// Notices that do not fail the request (a degraded trace, a token
    /// without `decimals()`).
    pub warnings: Vec<String>,
    /// The result as recorded under `evm`, not yet redacted: the runner
    /// redacts it with every secret of the run. On an error it carries
    /// `{"error": {"kind", "message"}}`.
    pub record: Value,
    pub elapsed: Duration,
}

/// The future an [`EvmExecutor`] returns.
pub type EvmFuture<'a> = Pin<Box<dyn Future<Output = EvmOutcome> + 'a>>;

/// Runs evm-call requests for a run.
pub trait EvmExecutor: Sync {
    fn execute<'a>(&'a self, call: EvmCall<'a>) -> EvmFuture<'a>;
}

impl Runner<'_> {
    /// Resolve, run, check and capture the evm request `request`.
    pub(super) async fn execute_evm(
        &self,
        request: &RequestRef<'_>,
        evm: &rustman_core::model::EvmRequest,
        record: RequestRecord,
        iteration: u32,
    ) -> Sent {
        let rustman_core::model::EvmRequest::Call(call) = evm else {
            return Sent::skipped(record, SkipReason::SigningNotRequested);
        };
        let Some(executor) = self.options.evm else {
            return Sent::skipped(record, SkipReason::UnsupportedKind);
        };
        let captured = self.lock_captures().clone();
        let mut resolver = self.resolver(request, &captured, iteration);
        let evm_table = &self.ctx.settings.evm;
        let resolve = |r: &Resolver<'_>| resolve_evm_call(request, r, evm_table);
        let resolved = match self.with_secrets(&mut resolver, resolve).await {
            Ok(resolved) => resolved,
            Err(e) => return Sent::errored(record, e.to_string()),
        };
        self.remember(&resolved.endpoint.secrets);
        remember_for_logs(&resolved.redactor());
        let options = match self.send_options(&resolved.endpoint) {
            Ok(options) => options,
            Err(e) => return Sent::errored(record, e.to_string()),
        };
        let evm_call = EvmCall {
            request,
            evm: call,
            resolved: &resolved,
            engine: &self.engine,
            options: &options,
            root: self.ctx.root,
            settings: self.ctx.settings,
            environment: self.ctx.environment,
            cancel: &self.options.cancel,
        };
        let cancel = &self.options.cancel;
        match until_cancelled(executor.execute(evm_call), cancel, Duration::ZERO).await {
            None => Sent::skipped(record, SkipReason::Interrupted),
            Some(outcome) => {
                let checked = EvmChecked {
                    request,
                    resolver: &resolver,
                    call,
                    resolved: &resolved,
                };
                self.check_evm(&checked, outcome, record)
            }
        }
    }

    /// Evaluate the assertions and captures of a call, and decide how the
    /// request ended.
    fn check_evm(
        &self,
        checked: &EvmChecked<'_, '_>,
        outcome: EvmOutcome,
        mut record: RequestRecord,
    ) -> Sent {
        let mut redactor = self.known();
        redactor.merge(&checked.resolved.redactor());
        if let Some(error) = &outcome.error {
            record = record.errored(redactor.text(error).into_owned());
            record.evm = Some(redact_value(&outcome.record, &redactor));
            return Sent::from_record(record);
        }
        let view = ResponseView {
            total: outcome.elapsed,
            ttfb: outcome.elapsed,
            body: Cow::Owned(outcome.document.to_string().into_bytes()),
            ..ResponseView::default()
        };
        let assertions = unlocated(checked.call.assertions.as_deref());
        let plugins = self.bare_plugin_calls();
        let ctx = Context {
            resolver: checked.resolver,
            request: Some(checked.request),
            redactor: &redactor,
            plugins: plugins.as_ref(),
        };
        record.assertions = merge_results(&assertions, &view, &ctx, outcome.evm_assertions);
        let captures = checked.call.captures.as_deref().unwrap_or_default();
        let report = self.capture(captures, &view, &mut redactor);
        remember_for_logs(&redactor);
        let passed = record.assertions.iter().all(AssertionResult::passed) && !report.failed();
        record.status = if passed {
            RequestStatus::Passed
        } else {
            RequestStatus::Failed
        };
        if !report.failures.is_empty() {
            let failures = report.failures.join("; ");
            record.error = Some(redactor.text(&failures).into_owned());
        }
        let warnings = outcome.warnings.iter().chain(&report.warnings);
        record.warnings = warnings.map(|w| redactor.text(w).into_owned()).collect();
        record.captures = report.results;
        redact_capture_messages(&mut record.captures, &redactor);
        record.evm = Some(redact_value(&outcome.record, &redactor));
        Sent::from_record(record)
    }
}

impl Runner<'_> {
    /// Resolve `url` (a URL or a `{{variable}}`) as an EVM node endpoint of
    /// `request`, with the run's variables and secrets, and the transport
    /// options to call it with. Its secrets are remembered for redaction.
    pub async fn resolve_evm_endpoint(
        &self,
        request: &RequestRef<'_>,
        url: &str,
    ) -> Result<(ResolvedRequest, SendOptions), String> {
        let captured = self.lock_captures().clone();
        let mut resolver = self.resolver(request, &captured, 1);
        let resolve = |r: &Resolver<'_>| resolve_evm_endpoint(request, r, url);
        let resolved = self
            .with_secrets(&mut resolver, resolve)
            .await
            .map_err(|e| e.to_string())?;
        self.remember(&resolved.secrets);
        remember_for_logs(&self.request_redactor(Some(&resolved), None));
        let options = self.send_options(&resolved).map_err(|e| e.to_string())?;
        Ok((resolved, options))
    }

    /// Every secret value resolved so far in this run.
    pub fn known_secrets(&self) -> rustman_core::Redactor {
        self.known()
    }
}

/// An evm-call whose result is being checked.
struct EvmChecked<'c, 'r> {
    request: &'c RequestRef<'r>,
    resolver: &'c Resolver<'c>,
    call: &'c EvmCallRequest,
    resolved: &'c ResolvedEvmCall,
}

/// The results of `assertions` in file order: `evm` ones from the
/// executor, the others (`latency`, `json` over the result document)
/// evaluated here.
fn merge_results(
    assertions: &[Assertion],
    view: &ResponseView<'_>,
    ctx: &Context<'_, '_>,
    evm: Vec<AssertionResult>,
) -> Vec<AssertionResult> {
    let mut evm = evm.into_iter();
    let mut results = Vec::with_capacity(assertions.len());
    for assertion in assertions {
        let result = match assertion {
            Assertion::Evm(_) => evm.next(),
            other => assertions::evaluate(std::slice::from_ref(other), view, ctx).pop(),
        };
        results.extend(result);
    }
    results
}

/// The `evm` record of a request that errored before reaching the node.
pub fn evm_error_record(kind: &str, message: &str) -> Value {
    json!({"error": {"kind": kind, "message": message}})
}
