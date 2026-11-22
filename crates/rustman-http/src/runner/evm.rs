//! `kind: evm-call` requests in a run (add-evm-toolkit tasks 4.1, 4.2).
//!
//! The runner resolves an evm-call request, learns its secrets, evaluates
//! its assertions and captures against the decoded call, and records the
//! result under `evm`, redacted. Encoding, the node calls and decoding
//! belong to the `rustman-evm` crate, which depends on this one: front ends
//! hand the runner an [`EvmExecutor`] through [`super::RunOptions::evm`].
//! Without one, evm-call requests are skipped as an unsupported kind.
//! `evm-tx` requests go to the [`EvmTxExecutor`] of
//! [`super::RunOptions::evm_tx`], which only the signing gate
//! (`rustman-evm-signing`) implements; without one (no `--sign`) a run
//! skips them as `signing not requested`.

use std::borrow::Cow;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use rustman_core::Redactor;
use rustman_core::Secret;
use rustman_core::assertions::{self, AssertionResult, Context, ResponseView};
use rustman_core::environment::Environment;
use rustman_core::format::Located;
use rustman_core::history::{Entry, NewEntry};
use rustman_core::model::{
    Assertion, Capture, Checked, EvmCallRequest, EvmRequest, Request, unlocated,
};
use rustman_core::redact::remember_for_logs;
use rustman_core::run::{RequestRecord, RequestStatus, SentRequest, SkipReason, redact_value};
use rustman_core::settings::Settings;
use rustman_core::vars::{
    ResolvedEvmCall, ResolvedEvmTx, ResolvedRequest, Resolver, resolve_evm_call,
    resolve_evm_endpoint, resolve_evm_secret, resolve_evm_tx,
};
use rustman_core::workspace::RequestRef;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::graphql::until_cancelled;
use super::record::redact_capture_messages;
use super::{Runner, Sent};
use crate::engine::{HttpEngine, SendOptions};
use crate::timing::millis;

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

/// What an [`EvmTxExecutor`] gets to sign and send one evm-tx request.
pub struct EvmTxCall<'a> {
    /// The run: resolving the request and its secrets, and recording the
    /// receipt ([`Runner::finish_evm_tx`]).
    pub runner: &'a Runner<'a>,
    pub request: &'a RequestRef<'a>,
    /// The request's record, for a refusal before anything is sent.
    pub record: RequestRecord,
    pub root: &'a Path,
    pub settings: &'a Settings,
    /// The active environment, whose `chain_id` pins the endpoint.
    pub environment: Option<&'a Environment>,
}

/// The future an [`EvmTxExecutor`] returns: the request's record.
pub type EvmTxFuture<'a> = Pin<Box<dyn Future<Output = RequestRecord> + 'a>>;

/// Signs and sends evm-tx requests for a run. The runner never drops its
/// future on Ctrl+C: the executor watches the run's cancellation itself,
/// stops (skipped as interrupted) while nothing is signed, and once a
/// transaction may have been sent, ends with its audit entry.
pub trait EvmTxExecutor: Sync {
    fn execute<'a>(&'a self, call: EvmTxCall<'a>) -> EvmTxFuture<'a>;
}

/// The future an [`EvmExecutor`] returns.
pub type EvmFuture<'a> = Pin<Box<dyn Future<Output = EvmOutcome> + 'a>>;

/// Runs evm-call requests for a run.
pub trait EvmExecutor: Sync {
    fn execute<'a>(&'a self, call: EvmCall<'a>) -> EvmFuture<'a>;
}

impl Runner<'_> {
    /// Resolve, run, check and capture the evm-call request `request`.
    pub(super) async fn execute_evm(
        &self,
        request: &RequestRef<'_>,
        call: &EvmCallRequest,
        record: RequestRecord,
        iteration: u32,
    ) -> Sent {
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
                    assertions: call.assertions.as_deref(),
                    captures: call.captures.as_deref().unwrap_or_default(),
                    redactor: resolved.redactor(),
                };
                self.check_evm(&checked, outcome, record)
            }
        }
    }

    /// Hand the evm-tx `request` to the signing gate, or skip it when the
    /// run did not ask to sign. Its own boxed future (see
    /// `Runner::execute`), apart from the evm-call one.
    pub(super) async fn execute_evm_tx(
        &self,
        request: &RequestRef<'_>,
        record: RequestRecord,
    ) -> Sent {
        let Some(executor) = self.options.evm_tx else {
            return Sent::skipped(record, SkipReason::SigningNotRequested);
        };
        let call = EvmTxCall {
            runner: self,
            request,
            record: record.clone(),
            root: self.ctx.root,
            settings: self.ctx.settings,
            environment: self.ctx.environment,
        };
        // Not raced against the run's cancellation: once a transaction may
        // have left the process, dropping the executor would lose its
        // audit entry. The executor watches Ctrl+C itself.
        Sent::from_record(executor.execute(call).await)
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
        redactor.merge(&checked.redactor);
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
        let assertions = unlocated(checked.assertions);
        let plugins = self.bare_plugin_calls();
        let ctx = Context {
            resolver: checked.resolver,
            request: Some(checked.request),
            redactor: &redactor,
            plugins: plugins.as_ref(),
        };
        record.assertions = merge_results(&assertions, &view, &ctx, outcome.evm_assertions);
        let captures = checked.captures;
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

    /// Resolve the `evm-tx` request `request` with the run's variables and
    /// secrets, and the transport options to call its node with. Its
    /// secrets are remembered for redaction. Nothing is sent.
    pub async fn resolve_evm_tx(
        &self,
        request: &RequestRef<'_>,
    ) -> Result<(ResolvedEvmTx, SendOptions), String> {
        let captured = self.lock_captures().clone();
        let mut resolver = self.resolver(request, &captured, 1);
        let resolve = |r: &Resolver<'_>| resolve_evm_tx(request, r);
        let resolved = self
            .with_secrets(&mut resolver, resolve)
            .await
            .map_err(|e| e.to_string())?;
        self.remember(&resolved.endpoint.secrets);
        remember_for_logs(&self.request_redactor(Some(&resolved.endpoint), None));
        let options = self
            .send_options(&resolved.endpoint)
            .map_err(|e| e.to_string())?;
        Ok((resolved, options))
    }

    /// Resolve `text` in the context of `request` as the secret `name`
    /// (a signer's keystore password); it is remembered for redaction.
    pub async fn resolve_evm_secret(
        &self,
        request: &RequestRef<'_>,
        name: &str,
        text: &str,
    ) -> Result<Secret<String>, String> {
        let captured = self.lock_captures().clone();
        let mut resolver = self.resolver(request, &captured, 1);
        let resolve = |r: &Resolver<'_>| resolve_evm_secret(r, name, text);
        let (secret, used) = self
            .with_secrets(&mut resolver, resolve)
            .await
            .map_err(|e| e.to_string())?;
        self.remember(&used);
        Ok(secret)
    }

    /// Check the receipt of the sent evm-tx `request` (its node at
    /// `endpoint`): evaluate its assertions and captures on `outcome`,
    /// then record the audit entry (`outcome.record`) in history, with no
    /// replay. The record is redacted with every secret of the run.
    pub fn finish_evm_tx(
        &self,
        request: &RequestRef<'_>,
        endpoint: &ResolvedRequest,
        outcome: EvmOutcome,
    ) -> RequestRecord {
        let file = request.entry.path.to_string_lossy().replace('\\', "/");
        let name = request.request().name().to_owned();
        let mut record = RequestRecord::new(request.entry.id(), name, file.clone());
        record.duration_ms = millis(outcome.elapsed);
        let Request::Evm(evm) = request.request() else {
            return record.errored("not an evm-tx request".to_owned());
        };
        let EvmRequest::Tx(tx) = evm.as_ref() else {
            return record.errored("not an evm-tx request".to_owned());
        };
        let captured = self.lock_captures().clone();
        let resolver = self.resolver(request, &captured, 1);
        let checked = EvmChecked {
            request,
            resolver: &resolver,
            assertions: tx.assertions.as_deref(),
            captures: tx.captures.as_deref().unwrap_or_default(),
            redactor: Redactor::for_request(endpoint),
        };
        let mut record = self.check_evm(&checked, outcome, record).record;
        if let Err(warning) = self.record_evm_tx(&record, endpoint, &file) {
            record.warnings.push(warning);
        }
        record
    }

    /// Keep `record` in history: the node it went to (redacted) and the
    /// audit record. Never replayable: a replay would not sign again.
    fn record_evm_tx(
        &self,
        record: &RequestRecord,
        endpoint: &ResolvedRequest,
        file: &str,
    ) -> Result<(), String> {
        let Some(store) = &self.options.history else {
            return Ok(());
        };
        let mut redactor = self.known();
        redactor.merge(&Redactor::for_request(endpoint));
        let entry = Entry::new(NewEntry {
            record,
            sent: SentRequest::new(endpoint, &redactor),
            request: Some(file),
            environment: self.ctx.environment.map(|e| e.name.as_str()),
            spill: None,
            replay: None,
        });
        store
            .record(&entry)
            .map_err(|e| redactor.text(&format!("history: {e}")).into_owned())
    }

    /// Every secret value resolved so far in this run.
    pub fn known_secrets(&self) -> rustman_core::Redactor {
        self.known()
    }
}

/// An evm-call result or evm-tx receipt being checked.
struct EvmChecked<'c, 'r> {
    request: &'c RequestRef<'r>,
    resolver: &'c Resolver<'c>,
    assertions: Option<&'c [Located<Assertion>]>,
    captures: &'c [Checked<Capture>],
    /// Hides the request's own secrets.
    redactor: Redactor,
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
