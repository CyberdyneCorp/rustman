//! Sending a graphql request of a workspace end to end, the one path for
//! `rustman run`, `rustman graphql query` and the TUI: load the document,
//! validate it against the active schema (syntax only without one), select
//! the operation, then send it over HTTP, WebSocket or SSE.
//!
//! [`GraphqlRunner`] plugs this into the `rustman-http` runner, which
//! resolves the request, evaluates assertions and captures and records
//! history. Under the runner, subscriptions are headless: they stop at
//! `max_events` or `duration`.

use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use rustman_core::RequestRef;
use rustman_core::model::{GraphqlRequest, GraphqlTransport};
use rustman_core::run::{
    EventRecord, GraphqlRecord, MAX_RECORDED_EVENTS, SubscriptionRecord, redact_value,
};
use rustman_core::settings::Settings;
use rustman_core::vars::ResolvedGraphql;
use rustman_http::runner::{GraphqlCall, GraphqlExecutor, GraphqlFuture, GraphqlOutcome};
use rustman_http::{Exchange, HttpEngine, HttpVersion, SendOptions, Timings};
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;

use crate::cache::SchemaCache;
use crate::document::load_document;
use crate::error::GraphqlError;
use crate::execute::{HttpOperation, HttpResult, execute_http};
use crate::response::NOT_GRAPHQL;
use crate::source::active_schema;
use crate::subscription::{
    DEFAULT_ACK_TIMEOUT, Limits, SubscribeOptions, Subscription, SubscriptionEvent, subscribe_sse,
    subscribe_ws,
};
use crate::validate::validate;

/// One graphql request to send.
pub struct Execution<'a> {
    pub request: &'a RequestRef<'a>,
    pub graphql: &'a GraphqlRequest,
    pub resolved: &'a ResolvedGraphql,
    pub engine: &'a HttpEngine,
    pub options: &'a SendOptions,
    pub root: &'a Path,
    pub settings: &'a Settings,
    /// Check the document before sending (`--no-validate` turns it off).
    pub validate: bool,
    pub subscribe: SubscribeOptions,
}

/// How the operation went out.
#[derive(Debug)]
pub enum Delivery {
    Http(Box<HttpResult>),
    Subscription(Box<Subscription>),
}

/// What [`execute`] produced.
#[derive(Debug)]
pub struct Executed {
    pub operation: HttpOperation,
    /// Validation warnings (deprecations), `line:column: message` each.
    pub warnings: Vec<String>,
    pub delivery: Delivery,
}

/// Validate and send the request of `exec`. Validation errors, an
/// ambiguous document, a mutation over GET or a missing upload fail before
/// anything is sent.
pub async fn execute(exec: &Execution<'_>) -> Result<Executed, GraphqlError> {
    let document = load_document(exec.request, exec.graphql)?;
    let warnings = if exec.validate {
        check(exec, &document)?
    } else {
        Vec::new()
    };
    let operation =
        HttpOperation::for_request(exec.request, exec.graphql, document, exec.resolved)?;
    let delivery = deliver(exec, &operation).await?;
    Ok(Executed {
        operation,
        warnings,
        delivery,
    })
}

/// Validate `document`; the warnings when nothing blocks the send.
fn check(exec: &Execution<'_>, document: &str) -> Result<Vec<String>, GraphqlError> {
    let cache = SchemaCache::for_workspace(exec.root);
    let endpoint = &exec.resolved.http.url.text;
    let schema = active_schema(exec.request, &exec.settings.graphql, endpoint, &cache)?;
    let validation = validate(
        document,
        schema.as_ref().map(|s| &s.schema),
        exec.resolved.operation_name.as_deref(),
        &exec.resolved.variables,
    );
    if validation.has_errors() {
        let errors = validation.errors().map(ToString::to_string).collect();
        return Err(GraphqlError::Invalid(errors));
    }
    Ok(validation.warnings().map(ToString::to_string).collect())
}

async fn deliver(
    exec: &Execution<'_>,
    operation: &HttpOperation,
) -> Result<Delivery, GraphqlError> {
    let (resolved, options) = (exec.resolved, exec.options);
    Ok(match exec.graphql.effective_transport() {
        GraphqlTransport::Http => Delivery::Http(Box::new(
            execute_http(exec.engine, options, resolved, operation).await?,
        )),
        GraphqlTransport::Ws => {
            let protocol = exec.graphql.effective_ws_protocol();
            let sub = subscribe_ws(resolved, operation, protocol, options, &exec.subscribe);
            Delivery::Subscription(Box::new(sub.await?))
        }
        GraphqlTransport::Sse => Delivery::Subscription(Box::new(
            subscribe_sse(exec.engine, options, resolved, operation, &exec.subscribe).await?,
        )),
    })
}

/// Sends the graphql requests of a run, headless.
#[derive(Clone, Debug)]
pub struct GraphqlRunner {
    /// `--max-events`: replaces every request's `max_events`.
    pub max_events: Option<u32>,
    /// `--duration`: replaces every request's `duration`.
    pub duration: Option<Duration>,
    /// `false` with `--no-validate`.
    pub validate: bool,
    pub ack_timeout: Duration,
    /// Set by interactive front ends (the TUI): subscriptions run until
    /// stopped or completed instead of being bounded.
    pub interactive: Option<Interactive>,
}

/// A subscription the user starts and stops.
#[derive(Clone, Debug)]
pub struct Interactive {
    /// Stops collecting; what arrived so far is the result.
    pub stop: CancellationToken,
    /// Receives every event as it arrives, its payload redacted.
    pub feed: Option<UnboundedSender<SubscriptionEvent>>,
}

impl Default for GraphqlRunner {
    fn default() -> Self {
        Self {
            max_events: None,
            duration: None,
            validate: true,
            ack_timeout: DEFAULT_ACK_TIMEOUT,
            interactive: None,
        }
    }
}

impl GraphqlExecutor for GraphqlRunner {
    fn execute<'a>(&'a self, call: GraphqlCall<'a>) -> GraphqlFuture<'a> {
        Box::pin(async move { self.run(&call).await.map_err(|e| e.to_string()) })
    }
}

impl GraphqlRunner {
    async fn run(&self, call: &GraphqlCall<'_>) -> Result<GraphqlOutcome, GraphqlError> {
        let (subscribe, events) = self.subscribe_options(call);
        let send = async move {
            let exec = Execution {
                request: call.request,
                graphql: call.graphql,
                resolved: call.resolved,
                engine: call.engine,
                options: call.options,
                root: call.root,
                settings: call.settings,
                validate: self.validate,
                subscribe,
            };
            // `exec` holds the feed's sender: dropping it ends `forward`.
            execute(&exec).await
        };
        let forward = self.forward(events, call);
        let (executed, ()) = tokio::join!(send, forward);
        Ok(outcome(executed?, call.graphql))
    }

    /// Headless: bounded and stopped by the run's cancel. Interactive:
    /// unbounded, stopped by the user, with a feed of its own when the
    /// front end wants events live.
    fn subscribe_options(
        &self,
        call: &GraphqlCall<'_>,
    ) -> (
        SubscribeOptions,
        Option<UnboundedReceiver<SubscriptionEvent>>,
    ) {
        let Some(interactive) = &self.interactive else {
            let limits = Limits::headless(call.graphql, self.max_events, self.duration);
            let mut options = SubscribeOptions::new(limits, call.cancel.clone());
            options.ack_timeout = self.ack_timeout;
            return (options, None);
        };
        let mut options = SubscribeOptions::new(Limits::interactive(), interactive.stop.clone());
        options.ack_timeout = self.ack_timeout;
        let events = interactive.feed.as_ref().map(|_| {
            let (sender, events) = unbounded_channel();
            options.feed = Some(sender);
            events
        });
        (options, events)
    }

    /// Pass each event on to the interactive feed with its secrets
    /// replaced, until the subscription drops its sender.
    async fn forward(
        &self,
        events: Option<UnboundedReceiver<SubscriptionEvent>>,
        call: &GraphqlCall<'_>,
    ) {
        let feed = self.interactive.as_ref().and_then(|i| i.feed.as_ref());
        let (Some(mut events), Some(feed)) = (events, feed) else {
            return;
        };
        let redactor = call.resolved.redactor();
        while let Some(mut event) = events.recv().await {
            event.payload = redact_value(&event.payload, &redactor);
            // A live view that went away does not stop the subscription.
            let _ = feed.send(event);
        }
    }
}

/// What the runner checks and records for `executed`.
pub fn outcome(executed: Executed, graphql: &GraphqlRequest) -> GraphqlOutcome {
    let Executed {
        operation,
        mut warnings,
        delivery,
    } = executed;
    let mut record = GraphqlRecord {
        operation_name: operation.operation.name.clone(),
        operation_type: operation.operation.kind.as_str().to_owned(),
        transport: transport_name(graphql.effective_transport()).to_owned(),
        protocol: None,
        variables: Value::Object(operation.variables.clone()),
        result: None,
        apq: None,
        subscription: None,
    };
    match delivery {
        Delivery::Http(result) => {
            record.result = result.response.as_ref().map(|r| r.status.to_string());
            record.apq = result.apq.map(|a| a.to_string());
            if result.response.is_none() {
                warnings.push(NOT_GRAPHQL.to_owned());
            }
            GraphqlOutcome {
                request: result.request,
                exchange: result.exchange,
                record,
                warnings,
            }
        }
        Delivery::Subscription(sub) => {
            record.result = Some(sub.status().to_string());
            record.protocol = Some(sub.protocol.label().to_owned());
            record.subscription = Some(subscription_record(&sub));
            warnings.extend(sub.warnings.iter().cloned());
            GraphqlOutcome {
                exchange: subscription_exchange(&sub),
                request: sub.request,
                record,
                warnings,
            }
        }
    }
}

fn transport_name(transport: GraphqlTransport) -> &'static str {
    match transport {
        GraphqlTransport::Http => "http",
        GraphqlTransport::Ws => "ws",
        GraphqlTransport::Sse => "sse",
    }
}

/// The events as recorded in history (the first 100), with how many
/// arrived and why the subscription failed.
pub fn subscription_record(sub: &Subscription) -> SubscriptionRecord {
    let events = sub
        .events
        .iter()
        .take(MAX_RECORDED_EVENTS)
        .map(|e| EventRecord {
            ts: e.ts.clone(),
            elapsed_ms: rustman_http::millis(e.elapsed),
            payload: e.payload.clone(),
        })
        .collect();
    SubscriptionRecord {
        event_count: sub.event_count,
        failure: sub.failure.clone(),
        ..SubscriptionRecord::new(events, sub.ended.as_str())
    }
}

/// The exchange assertions and reports read for a subscription: its
/// handshake (or stream) status and headers, with the collected result as
/// the body.
pub fn subscription_exchange(sub: &Subscription) -> Exchange {
    let body = Bytes::from(sub.result().to_string());
    Exchange {
        id: String::new(),
        url: sub.request.url.text.clone(),
        redirects: Vec::new(),
        http_version: HttpVersion::Http11,
        status: sub.status,
        headers: sub.headers.clone(),
        size_bytes: body.len() as u64,
        body,
        spill: None,
        binary: false,
        timings: Timings {
            ttfb: sub.connected,
            download: sub.elapsed.saturating_sub(sub.connected),
            total: sub.elapsed,
            ..Timings::default()
        },
        insecure: sub.insecure,
        warnings: Vec::new(),
        credentials: rustman_core::Redactor::new(),
        trace: None,
    }
}
