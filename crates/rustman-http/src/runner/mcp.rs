//! `kind: mcp` requests in a run (add-mcp-client tasks 7.2 to 7.4).
//!
//! The runner resolves an mcp request's params, evaluates its assertions
//! (the MCP checks and the implicit `output_schema`) and captures, and
//! records it in history with its notifications, as for HTTP requests.
//! Connecting and sending belong to the `rustman-mcp` crate, which depends
//! on this one: front ends hand the runner an [`McpExecutor`] through
//! [`super::RunOptions::mcp`], which keeps one session per server for the
//! whole run. Without one, mcp requests are skipped as an unsupported kind.

use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use rustman_core::Redactor;
use rustman_core::assertions::{self, Context, McpFacts, ResponseView};
use rustman_core::history::{Entry, NewEntry};
use rustman_core::model::McpRequest;
use rustman_core::redact::remember_for_logs;
use rustman_core::run::{
    McpRecord, RequestRecord, RequestStatus, SentRequest, SkipReason, redact_value,
};
use rustman_core::settings::Settings;
use rustman_core::snapshot::{CheckedSnapshots, Exchange};
use rustman_core::trace::TracePropagation;
use rustman_core::vars::{ResolvedMcp, Resolver, resolve_mcp};
use rustman_core::workspace::RequestRef;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::snapshots::SnapshotTarget;
use super::{Runner, Sent};

/// What an [`McpExecutor`] gets to send one mcp request.
pub struct McpCall<'a> {
    pub request: &'a RequestRef<'a>,
    pub mcp: &'a McpRequest,
    /// The request with every placeholder of its params resolved.
    pub resolved: &'a ResolvedMcp,
    /// The run's precedence chain, for the server definition's templates.
    pub resolver: &'a Resolver<'a>,
    /// The workspace root (server definitions, trust and the default root).
    pub root: &'a Path,
    pub settings: &'a Settings,
    /// `--timeout` of the run: replaces the request's and server's timeout.
    pub timeout: Option<Duration>,
    /// Cancelled on Ctrl+C.
    pub cancel: &'a CancellationToken,
    /// The run's trace propagation, for the HTTP requests of a session
    /// the call opens; stdio servers have no HTTP layer and ignore it.
    pub trace: Option<&'a TracePropagation>,
}

/// What sending an mcp request produced. Nothing here is redacted yet: the
/// runner redacts it with every secret of the run plus `secrets`.
pub struct McpOutcome {
    /// The MCP result the server answered (what assertions read), or why
    /// there is none (connect, protocol, timeout and validation errors).
    pub result: Result<Value, String>,
    /// What captures read: `structuredContent`, else the text content.
    pub capture_body: String,
    pub facts: McpFacts,
    /// The request as sent: the method, the server's URL or command line,
    /// its headers (credentials already shown as placeholders) and params.
    pub sent: SentRequest,
    pub record: McpRecord,
    /// Knows the secrets of the server definition and of its session (OAuth
    /// tokens); merged into the run's redactor.
    pub redactor: Redactor,
    /// Non-blocking problems, such as headless sampling that was refused.
    pub warnings: Vec<String>,
    pub elapsed: Duration,
}

/// The future an [`McpExecutor`] returns.
pub type McpFuture<'a> = Pin<Box<dyn Future<Output = McpOutcome> + 'a>>;

/// Sends mcp requests for a run, one session per server.
pub trait McpExecutor: Sync {
    fn execute<'a>(&'a self, call: McpCall<'a>) -> McpFuture<'a>;
}

impl Runner<'_> {
    /// Resolve, send, check and capture the mcp request `request`.
    pub(super) async fn execute_mcp(
        &self,
        request: &RequestRef<'_>,
        mcp: &McpRequest,
        record: RequestRecord,
        iteration: u32,
    ) -> Sent {
        let Some(executor) = self.options.mcp else {
            return Sent::skipped(record, SkipReason::UnsupportedKind);
        };
        let captured = self.lock_captures().clone();
        let mut resolver = self.resolver(request, &captured, iteration);
        let resolve = |r: &Resolver<'_>| resolve_mcp(request, r);
        let resolved = match self.with_secrets(&mut resolver, resolve).await {
            Ok(resolved) => resolved,
            Err(e) => return Sent::errored(record, e.to_string()),
        };
        self.remember(&resolved.secrets);
        remember_for_logs(&self.known());
        let call = McpCall {
            request,
            mcp,
            resolved: &resolved,
            resolver: &resolver,
            root: self.ctx.root,
            settings: self.ctx.settings,
            timeout: self.options.timeout,
            cancel: &self.options.cancel,
            trace: self.trace.as_ref(),
        };
        let outcome = tokio::select! {
            biased;
            () = self.options.cancel.cancelled() => None,
            outcome = executor.execute(call) => Some(outcome),
        };
        let Some(outcome) = outcome else {
            return Sent::skipped(record, SkipReason::Interrupted);
        };
        self.remember_all(&outcome.redactor);
        let checked = McpChecked {
            request,
            resolver: &resolver,
            assertions: mcp.assertions.as_deref().unwrap_or_default(),
            captures: mcp.captures.as_deref().unwrap_or_default(),
        };
        self.evaluate_mcp(&checked, outcome, record)
    }

    fn evaluate_mcp(
        &self,
        checked: &McpChecked<'_, '_>,
        outcome: McpOutcome,
        mut record: RequestRecord,
    ) -> Sent {
        let mut redactor = self.known();
        remember_for_logs(&redactor);
        let McpOutcome {
            result,
            capture_body,
            facts,
            sent,
            record: mcp,
            warnings,
            elapsed,
            ..
        } = outcome;
        let mut snapshots = Vec::new();
        match result {
            Err(message) => record = record.errored(message),
            Ok(result) => {
                let view = view(result.to_string().into_bytes(), elapsed);
                let plugins = self.bare_plugin_calls();
                let ctx = Context {
                    resolver: checked.resolver,
                    request: Some(checked.request),
                    redactor: &redactor,
                    plugins: plugins.as_ref(),
                };
                record.assertions =
                    assertions::evaluate_mcp(checked.assertions, &view, &ctx, &facts);
                let checked_snapshots =
                    self.check_mcp_snapshots(checked, &view, &redactor, &mut record.assertions);
                record.warnings.extend(checked_snapshots.warnings);
                snapshots = checked_snapshots.reviews;
                let captures = view_of_text(capture_body, elapsed);
                let report = self.capture(checked.captures, &captures, &mut redactor);
                let passed = record.assertions.iter().all(|a| a.passed()) && !report.failed();
                record.status = if passed {
                    RequestStatus::Passed
                } else {
                    RequestStatus::Failed
                };
                if report.failed() {
                    record.error = Some(report.failures.join("; "));
                }
                record.warnings.extend(report.warnings);
                record.captures = report.results;
            }
        }
        record.warnings.splice(0..0, warnings);
        redact_record(&mut record, &redactor);
        record.mcp = Some(mcp.redacted(&redactor));
        let mut done = Sent::finished(record);
        done.mcp_sent = Some(redact_sent(sent, &redactor));
        done.snapshots = snapshots;
        done
    }

    /// Check the `snapshot` assertions of an mcp request against its result
    /// (the JSON body of the exchange, with no status or headers).
    fn check_mcp_snapshots(
        &self,
        checked: &McpChecked<'_, '_>,
        view: &ResponseView<'_>,
        redactor: &Redactor,
        results: &mut [assertions::AssertionResult],
    ) -> CheckedSnapshots {
        let exchange = Exchange {
            kind: "mcp",
            status: None,
            headers: &view.headers,
            body: &view.body,
            events: None,
        };
        let target = SnapshotTarget {
            request: checked.request,
            assertions: checked.assertions,
        };
        self.check_exchange_snapshots(&target, &exchange, redactor, results)
    }

    /// Record an MCP exchange in history (history on): what was sent, and
    /// the record with its result and notifications. MCP entries are not
    /// replayable.
    pub(super) fn record_mcp_history(
        &self,
        sent: &mut Sent,
        redactor: &Redactor,
        request: Option<String>,
    ) {
        let (Some(store), Some(mcp_sent)) = (&self.options.history, sent.mcp_sent.take()) else {
            return;
        };
        let entry = Entry::new(NewEntry {
            record: &sent.record,
            sent: redact_sent(mcp_sent, redactor),
            request: request.as_deref(),
            environment: self.ctx.environment.map(|e| e.name.as_str()),
            spill: None,
            replay: None,
        });
        if let Err(e) = store.record(&entry) {
            let warning = format!("history: {e}");
            sent.record
                .warnings
                .push(redactor.text(&warning).into_owned());
        }
    }
}

/// An mcp request whose result is being checked.
struct McpChecked<'c, 'r> {
    request: &'c RequestRef<'r>,
    resolver: &'c Resolver<'c>,
    assertions: &'c [rustman_core::model::Assertion],
    captures: &'c [rustman_core::model::Checked<rustman_core::model::Capture>],
}

impl Sent {
    fn finished(record: RequestRecord) -> Self {
        Self::from_record(record)
    }
}

/// The result as assertions read it: a JSON body, timed by the operation.
fn view(body: Vec<u8>, elapsed: Duration) -> ResponseView<'static> {
    ResponseView {
        status: 200,
        headers: vec![("content-type".into(), "application/json".into())],
        body: body.into(),
        binary: false,
        ttfb: elapsed,
        total: elapsed,
        grpc: None,
    }
}

fn view_of_text(text: String, elapsed: Duration) -> ResponseView<'static> {
    view(text.into_bytes(), elapsed)
}

fn redact_record(record: &mut RequestRecord, redactor: &Redactor) {
    let text = |t: &str| redactor.text(t).into_owned();
    record.error = record.error.as_deref().map(text);
    for warning in &mut record.warnings {
        *warning = text(warning);
    }
    for capture in &mut record.captures {
        if let Some(message) = &mut capture.message {
            *message = text(message);
        }
    }
}

fn redact_sent(mut sent: SentRequest, redactor: &Redactor) -> SentRequest {
    sent.url = redactor.text(&sent.url).into_owned();
    for header in &mut sent.headers {
        header.value = redactor.header_value(&header.name, &header.value);
    }
    sent.body = sent.body.map(|b| redact_body(&b, redactor));
    sent
}

/// The params as sent, redacted value by value when they are JSON (a
/// secret's encoded form inside a string is escaped again once the params
/// are serialized, which text matching would miss), then as text.
fn redact_body(body: &str, redactor: &Redactor) -> String {
    let structured = serde_json::from_str::<Value>(body)
        .map(|value| redact_value(&value, redactor).to_string())
        .unwrap_or_else(|_| body.to_owned());
    redactor.text(&structured).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::run::HeaderRecord;
    use rustman_core::secret::Secret;

    #[test]
    fn an_escaped_secret_inside_json_params_is_redacted() {
        let redactor = Redactor::new().with(&Secret::new("token", "zq/x+1".to_owned()));
        let params = serde_json::json!({"text": "json=zq\\/x+1"});
        let body = redact_body(&params.to_string(), &redactor);
        assert!(!body.contains("zq"), "{body}");
        assert_eq!(body, r#"{"text":"json=«redacted:token»"}"#);
    }

    #[test]
    fn sent_mcp_requests_are_redacted() {
        let redactor = Redactor::new().with(&Secret::new("token", "s3cr3t".to_owned()));
        let sent = SentRequest {
            method: "tools/call".into(),
            url: "https://mcp.local/mcp?key=s3cr3t".into(),
            headers: vec![HeaderRecord {
                name: "Authorization".into(),
                value: "Bearer literal-token".into(),
            }],
            body: Some(r#"{"token":"s3cr3t"}"#.into()),
        };
        let sent = redact_sent(sent, &redactor);
        assert_eq!(sent.url, "https://mcp.local/mcp?key=«redacted:token»");
        assert_eq!(sent.headers[0].value, "«redacted:Authorization»");
        assert_eq!(
            sent.body.as_deref(),
            Some(r#"{"token":"«redacted:token»"}"#)
        );
    }
}
