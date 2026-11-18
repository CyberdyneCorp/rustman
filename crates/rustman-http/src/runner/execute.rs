//! Executing one request: resolve, send, assert, capture.

use std::sync::PoisonError;
use std::sync::atomic::Ordering;
use std::time::Instant;

use rustman_core::Redactor;
use rustman_core::assertions::{self, Context, needs_body};
use rustman_core::captures::{self, CaptureReport, Persistence, RunCaptures};
use rustman_core::model::{HttpRequest, Request};
use rustman_core::redact::remember_for_logs;
use rustman_core::run::{RequestRecord, RequestStatus, SkipReason};
use rustman_core::secret::Secret;
use rustman_core::vars::{Layer, ResolvedRequest, Resolver, VariablesSource, resolve_request};
use rustman_core::workspace::RequestRef;

use super::{LoginMode, Runner, Sent};
use crate::engine::SendOptions;
use crate::error::RequestError;
use crate::exchange::Exchange;
use crate::oauth2::{Login, OAuth2Context, cache_key};
use crate::timing::millis;

use super::record::exchange_record;

impl Sent {
    pub(super) fn skipped(record: RequestRecord, reason: SkipReason) -> Self {
        Self {
            record: record.skipped(reason),
            exchange: None,
            resolved: None,
            discard: None,
            redactor: Redactor::new(),
        }
    }

    pub(super) fn errored(record: RequestRecord, message: String) -> Self {
        Self {
            record: record.errored(message),
            exchange: None,
            resolved: None,
            discard: None,
            redactor: Redactor::new(),
        }
    }

    pub(super) fn with_resolved(mut self, resolved: ResolvedRequest) -> Self {
        self.resolved = Some(resolved);
        self
    }
}

impl Runner<'_> {
    /// Execute `request` as part of this run: with the captures made so far,
    /// adding its own. Under `--bail`, a failure or error makes every later
    /// request skip.
    pub async fn send(&self, request: &RequestRef<'_>, iteration: u32) -> Sent {
        self.seed(std::slice::from_ref(request));
        let started = Instant::now();
        let file = request.entry.path.to_string_lossy().replace('\\', "/");
        let name = request.request().name().to_owned();
        let record = RequestRecord::new(request.entry.id(), name, file, iteration);
        tracing::debug!(request = %record.id, iteration, "sending");
        let mut sent = self.execute(request, record).await;
        sent.record.duration_ms = millis(started.elapsed());
        let file = (!self.options.ad_hoc).then(|| sent.record.file.clone());
        let redactor = self.sink_redactor(&sent);
        redact_error(&mut sent.record, &redactor);
        sent.redactor = redactor.clone();
        let kept = self.record_history(&mut sent, &redactor, file, Some(cache_key(request)));
        sent.discard_spill_unless(kept);
        log_record(&sent.record);
        let failed = matches!(
            sent.record.status,
            RequestStatus::Failed | RequestStatus::Errored
        );
        if failed && self.options.bail {
            self.bailed.store(true, Ordering::SeqCst);
        }
        sent
    }

    async fn execute(&self, request: &RequestRef<'_>, record: RequestRecord) -> Sent {
        if self.options.cancel.is_cancelled() {
            return Sent::skipped(record, SkipReason::Interrupted);
        }
        if self.bailed.load(Ordering::SeqCst) {
            return Sent::skipped(record, SkipReason::Bail);
        }
        let Request::Http(http) = request.request() else {
            return Sent::skipped(record, SkipReason::UnsupportedKind);
        };
        let captured = self.lock_captures().clone();
        let resolver = self.resolver(request, &captured);
        let mut resolved = match resolve_request(request, &resolver) {
            Ok(resolved) => resolved,
            Err(e) => return Sent::errored(record, e.to_string()),
        };
        self.remember(&resolved.secrets);
        remember_for_logs(&self.request_redactor(Some(&resolved), None));
        let target = AuthTarget {
            request: request.entry.id(),
            cache_key: cache_key(request),
        };
        let exchange = match self.exchange(target, &mut resolved).await {
            Some(Ok(exchange)) => exchange,
            Some(Err(e)) => return Sent::errored(record, e.to_string()).with_resolved(resolved),
            None => return Sent::skipped(record, SkipReason::Interrupted),
        };
        let checked = Checked {
            http,
            request,
            resolver: &resolver,
            resolved: &resolved,
        };
        let sent = self.evaluate(&checked, exchange, record);
        sent.with_resolved(resolved)
    }

    /// Send the request, or `None` when the run is cancelled first.
    pub(super) async fn exchange(
        &self,
        target: AuthTarget,
        resolved: &mut ResolvedRequest,
    ) -> Option<Result<Exchange, RequestError>> {
        let options = match self.send_options(resolved) {
            Ok(options) => options,
            Err(e) => return Some(Err(e)),
        };
        let login = match self.options.login {
            LoginMode::Interactive(show) => Login::Interactive(show),
            LoginMode::NonInteractive => Login::NonInteractive {
                request: target.request,
            },
        };
        let context = OAuth2Context {
            store: self.ctx.secrets,
            env: self.ctx.environment.map(|e| e.name.as_str()),
            cache_key: target.cache_key,
            login,
        };
        let send = self.engine.send_authorized(resolved, &options, &context);
        tokio::select! {
            biased;
            () = self.options.cancel.cancelled() => None,
            result = send => Some(result),
        }
    }

    /// The request's options with the run's `--timeout`, `--insecure` and
    /// cookie jar applied.
    fn send_options(&self, resolved: &ResolvedRequest) -> Result<SendOptions, RequestError> {
        let mut options = SendOptions::for_request(self.ctx.settings, self.ctx.root, resolved)?;
        if !self.ctx.in_workspace {
            options.body.spill_dir = crate::body::default_spill_dir();
            options.body.workspace = None;
        }
        if let Some(timeout) = self.options.timeout {
            options.timeout = timeout;
        }
        if self.options.insecure {
            options.tls.verify = false;
        }
        Ok(options.with_cookie_jar(&resolved.settings, &self.jar))
    }

    /// The precedence chain for `request`: `--var` overrides, the run's
    /// captures and the persisted ones on top of the request's own chain.
    fn resolver<'r>(&'r self, request: &RequestRef<'r>, captured: &'r RunCaptures) -> Resolver<'r> {
        let ctx = &self.ctx;
        let mut resolver =
            Resolver::for_request(ctx.settings, request, ctx.environment, ctx.secrets);
        resolver.push(VariablesSource::new(
            Layer::Override,
            &self.options.overrides,
        ));
        resolver.push(captured);
        resolver.push(self.state.source());
        resolver
    }

    /// Evaluate assertions, then captures, and record the exchange.
    fn evaluate(
        &self,
        checked: &Checked<'_, '_>,
        exchange: Exchange,
        mut record: RequestRecord,
    ) -> Sent {
        let http = checked.http;
        let assertions = http.assertions.as_deref().unwrap_or_default();
        let captures = http.captures.as_deref().unwrap_or_default();
        let full_body = needs_body(assertions) || !captures.is_empty();
        let view = match exchange.view(full_body) {
            Ok(view) => view,
            Err(e) => return Sent::errored(record, format!("cannot read the response body: {e}")),
        };
        let mut redactor = self.request_redactor(Some(checked.resolved), Some(&exchange));
        let ctx = Context {
            resolver: checked.resolver,
            request: Some(checked.request),
            redactor: &redactor,
        };
        record.assertions = assertions::evaluate(assertions, &view, &ctx);
        let report = self.capture(captures, &view, &mut redactor);
        drop(view);
        remember_for_logs(&redactor);
        let passed = record.assertions.iter().all(|a| a.passed()) && !report.failed();
        record.status = if passed {
            RequestStatus::Passed
        } else {
            RequestStatus::Failed
        };
        if report.failed() {
            record.error = Some(redactor.text(&report.failures.join("; ")).into_owned());
        }
        let warnings = exchange.warnings.iter().chain(&report.warnings);
        record.warnings = warnings.map(|w| redactor.text(w).into_owned()).collect();
        record.captures = report.results;
        for capture in &mut record.captures {
            if let Some(message) = &mut capture.message {
                *message = redactor.text(message).into_owned();
            }
        }
        record.exchange = Some(exchange_record(checked.resolved, &exchange, &redactor));
        Sent {
            record,
            exchange: Some(exchange),
            resolved: None,
            discard: None,
            redactor: Redactor::new(),
        }
    }

    /// Apply captures and add the values to the run's scope. A captured
    /// value holding a known secret becomes a secret; `redactor` and the
    /// run learn every captured secret.
    fn capture(
        &self,
        captures: &[rustman_core::model::Checked<rustman_core::model::Capture>],
        view: &rustman_core::assertions::ResponseView<'_>,
        redactor: &mut Redactor,
    ) -> CaptureReport {
        let ctx = &self.ctx;
        let persistence = ctx.in_workspace.then_some(Persistence {
            root: ctx.root,
            environment: ctx.environment,
            secrets: ctx.secrets,
            redactor,
        });
        let mut scope = RunCaptures::new();
        let mut report = captures::apply(captures, view, &mut scope, persistence.as_ref());
        for secret in &report.secrets {
            redactor.add(secret);
        }
        for secret in scope.taint(redactor) {
            let result = report.results.iter_mut().find(|r| r.var == secret.name());
            if let Some(result) = result {
                result.secret = true;
            }
            redactor.add(&secret);
            report.secrets.push(secret);
        }
        self.remember(&report.secrets);
        self.lock_captures().merge(scope);
        report
    }

    /// Add `secrets` to the values every sink of this run redacts.
    pub(super) fn remember(&self, secrets: &[Secret<String>]) {
        let mut known = self.known.lock().unwrap_or_else(PoisonError::into_inner);
        for secret in secrets {
            known.add(secret);
        }
    }

    /// Every secret value resolved or captured so far in this run.
    pub(super) fn known(&self) -> Redactor {
        self.known
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The redactor for everything recorded about `sent`: the run's secrets,
    /// plus the request's own credentials and those of its exchange.
    pub(super) fn sink_redactor(&self, sent: &Sent) -> Redactor {
        self.request_redactor(sent.resolved.as_ref(), sent.exchange.as_ref())
    }

    /// The run's redactor (shared, not copied) plus what `request` and the
    /// credentials of its `exchange` add.
    pub(super) fn request_redactor(
        &self,
        request: Option<&ResolvedRequest>,
        exchange: Option<&Exchange>,
    ) -> Redactor {
        let mut redactor = self.known();
        if let Some(request) = request {
            redactor.add_request(request);
        }
        if let Some(exchange) = exchange {
            redactor.merge(&exchange.credentials);
        }
        redactor
    }

    pub(super) fn lock_captures(&self) -> std::sync::MutexGuard<'_, RunCaptures> {
        self.captures.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Who an OAuth2 token is for: the request id (for the login hint) and
/// where its token is cached.
pub(super) struct AuthTarget {
    pub request: String,
    pub cache_key: String,
}

/// A request whose response is being checked.
struct Checked<'c, 'r> {
    http: &'c HttpRequest,
    request: &'c RequestRef<'r>,
    resolver: &'c Resolver<'c>,
    resolved: &'c ResolvedRequest,
}

/// Redact the error message of `record`, which may quote what was sent.
pub(super) fn redact_error(record: &mut RequestRecord, redactor: &Redactor) {
    if let Some(error) = &mut record.error {
        *error = redactor.text(error).into_owned();
    }
}

/// Log how a request ended (`-v`), and its redacted headers (`-vv`). Every
/// field comes from the record, which is already redacted.
pub(super) fn log_record(record: &RequestRecord) {
    let Some(exchange) = &record.exchange else {
        tracing::info!(
            request = %record.id,
            status = ?record.status,
            reason = record.error.as_deref().unwrap_or_default(),
            "no response"
        );
        return;
    };
    let (sent, response) = (&exchange.request, &exchange.response);
    tracing::info!(
        request = %record.id,
        status = ?record.status,
        "{} {} -> {} ({:.0} ms)",
        sent.method,
        sent.url,
        response.status,
        exchange.timings.total_ms
    );
    for header in &sent.headers {
        tracing::debug!(request = %record.id, "> {}: {}", header.name, header.value);
    }
    for header in &response.headers {
        tracing::debug!(request = %record.id, "< {}: {}", header.name, header.value);
    }
}
