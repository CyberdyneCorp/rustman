//! `kind: graphql` requests in a run (add-graphql-client task 6.4).
//!
//! The runner resolves a graphql request, learns its secrets, evaluates its
//! assertions and captures and records it, as for HTTP requests. Sending
//! it (validation, the HTTP, WebSocket or SSE transport, headless
//! subscription bounds) belongs to the `rustman-graphql` crate, which
//! depends on this one: front ends hand the runner a [`GraphqlExecutor`]
//! through [`super::RunOptions::graphql`]. Without one, graphql requests
//! are skipped as an unsupported kind.
//!
//! On Ctrl+C, an HTTP operation is dropped at once, while a subscription
//! gets [`STOP_GRACE`] to stop on the same cancel and return the events it
//! collected, which are recorded as for a subscription that ended.

use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use rustman_core::model::{GraphqlRequest, GraphqlTransport};
use rustman_core::redact::remember_for_logs;
use rustman_core::run::{GraphqlRecord, RequestRecord, SkipReason};
use rustman_core::settings::Settings;
use rustman_core::vars::{ResolvedGraphql, ResolvedRequest, resolve_graphql};
use rustman_core::workspace::RequestRef;
use tokio_util::sync::CancellationToken;

use super::execute::Checked;
use super::{Runner, Sent};
use crate::engine::{HttpEngine, SendOptions};
use crate::exchange::Exchange;

/// How long a subscription has, after Ctrl+C, to tell the server it stopped
/// and return its events (its own teardown gives up after 2 seconds).
pub const STOP_GRACE: Duration = Duration::from_secs(5);

/// What a [`GraphqlExecutor`] gets to send one graphql request.
pub struct GraphqlCall<'a> {
    pub request: &'a RequestRef<'a>,
    pub graphql: &'a GraphqlRequest,
    /// The request with every placeholder of the run resolved.
    pub resolved: &'a ResolvedGraphql,
    /// The run's engine (connection pools, Digest nonces).
    pub engine: &'a HttpEngine,
    /// The transport options of the run (timeout, TLS, cookie jar).
    pub options: &'a SendOptions,
    /// The workspace root (the schema cache lives under it).
    pub root: &'a Path,
    pub settings: &'a Settings,
    /// Cancelled on Ctrl+C: a subscription stops collecting.
    pub cancel: &'a CancellationToken,
}

/// What sending a graphql request produced.
pub struct GraphqlOutcome {
    /// The last HTTP request sent (for a WebSocket, the handshake).
    pub request: ResolvedRequest,
    /// The response assertions and captures read. For a subscription, its
    /// body is the collected result: `{"data": [...], "errors": [...]}`.
    pub exchange: Exchange,
    /// Not yet redacted: the runner redacts it with every secret of the run.
    pub record: GraphqlRecord,
    /// Non-blocking problems, such as deprecated fields.
    pub warnings: Vec<String>,
}

/// The future a [`GraphqlExecutor`] returns. The error is shown as the
/// request's error, redacted.
pub type GraphqlFuture<'a> = Pin<Box<dyn Future<Output = Result<GraphqlOutcome, String>> + 'a>>;

/// Sends graphql requests for a run.
pub trait GraphqlExecutor: Sync {
    fn execute<'a>(&'a self, call: GraphqlCall<'a>) -> GraphqlFuture<'a>;
}

impl Runner<'_> {
    /// Resolve, send, check and capture the graphql request `request`.
    pub(super) async fn execute_graphql(
        &self,
        request: &RequestRef<'_>,
        graphql: &GraphqlRequest,
        record: RequestRecord,
        iteration: u32,
    ) -> Sent {
        let Some(executor) = self.options.graphql else {
            return Sent::skipped(record, SkipReason::UnsupportedKind);
        };
        let captured = self.lock_captures().clone();
        let resolver = self.resolver(request, &captured, iteration);
        let resolved = match resolve_graphql(request, &resolver) {
            Ok(resolved) => resolved,
            Err(e) => return Sent::errored(record, e.to_string()),
        };
        self.remember(&resolved.http.secrets);
        remember_for_logs(&self.request_redactor(Some(&resolved.http), None));
        let options = match self.send_options(&resolved.http) {
            Ok(options) => options,
            Err(e) => return Sent::errored(record, e.to_string()).with_resolved(resolved.http),
        };
        let call = GraphqlCall {
            request,
            graphql,
            resolved: &resolved,
            engine: &self.engine,
            options: &options,
            root: self.ctx.root,
            settings: self.ctx.settings,
            cancel: &self.options.cancel,
        };
        let grace = match graphql.effective_transport() {
            GraphqlTransport::Http => Duration::ZERO,
            GraphqlTransport::Ws | GraphqlTransport::Sse => STOP_GRACE,
        };
        let outcome = until_cancelled(executor.execute(call), &self.options.cancel, grace).await;
        let outcome = match outcome {
            None => return Sent::skipped(record, SkipReason::Interrupted),
            Some(Err(message)) => {
                return Sent::errored(record, message).with_resolved(resolved.http);
            }
            Some(Ok(outcome)) => outcome,
        };
        let GraphqlOutcome {
            request: sent,
            exchange,
            record: graphql_record,
            warnings,
        } = outcome;
        let events = graphql_record
            .subscription
            .as_ref()
            .map(|s| s.events.iter().map(|e| e.payload.clone()).collect());
        let checked = Checked {
            assertions: graphql.assertions.as_deref().unwrap_or_default(),
            captures: graphql.captures.as_deref().unwrap_or_default(),
            request,
            resolver: &resolver,
            resolved: &sent,
            kind: "graphql",
            events,
        };
        let redactor = self.request_redactor(Some(&sent), Some(&exchange));
        let mut done = self.evaluate(&checked, exchange, record);
        let warnings = warnings.iter().map(|w| redactor.text(w).into_owned());
        done.record.warnings.splice(0..0, warnings);
        done.record.graphql = Some(graphql_record.redacted(&redactor));
        done.with_resolved(sent)
    }
}

/// Await `future`; once `cancel` fires, give it `grace` more to finish.
/// `None` when it did not.
pub(super) async fn until_cancelled<T>(
    future: impl Future<Output = T>,
    cancel: &CancellationToken,
    grace: Duration,
) -> Option<T> {
    let mut future = std::pin::pin!(future);
    tokio::select! {
        biased;
        () = cancel.cancelled() => {}
        output = &mut future => return Some(output),
    }
    tokio::time::timeout(grace, future).await.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_cancelled_future_gets_its_grace_to_finish() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let stopping = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            "events"
        };
        let kept = until_cancelled(stopping, &cancel, Duration::from_secs(30)).await;
        assert_eq!(kept, Some("events"));
        let dropped = until_cancelled(std::future::pending::<()>(), &cancel, Duration::ZERO).await;
        assert_eq!(dropped, None);
        let running = CancellationToken::new();
        assert_eq!(
            until_cancelled(async { 1 }, &running, Duration::ZERO).await,
            Some(1)
        );
    }
}
