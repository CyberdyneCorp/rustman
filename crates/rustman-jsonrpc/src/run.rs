//! Sending a jsonrpc request of a workspace end to end, the one path for
//! `rustman run`, `rustman rpc` and the TUI: ask the method guards, check
//! the calls against the endpoint's OpenRPC catalog, then send them over
//! HTTP or WebSocket.
//!
//! [`JsonrpcRunner`] plugs this into the `rustman-http` runner, which
//! resolves the request, evaluates assertions and captures and records
//! history. Under the runner, subscriptions are headless: they stop at
//! `max_events` or `max_duration_ms`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rustman_core::model::{EndpointScheme, SubscriptionSpec};
use rustman_core::run::{JsonrpcRecord, RpcCallRecord, RpcRefusal};
use rustman_core::vars::ResolvedJsonrpc;
use rustman_http::runner::{
    JsonrpcCall, JsonrpcCallCheck, JsonrpcExecutor, JsonrpcFuture, JsonrpcOutcome,
};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::catalog::catalog_for;
use crate::client::JsonrpcClient;
use crate::envelope::{IdSequence, request_body};
use crate::guard;
use crate::http::{HttpSend, http_request};
use crate::outcome::{RpcExchange, TransportKind};
use crate::record::{call_checks, exchange_record};
use crate::subscription::{SubscribeOptions, SubscriptionLimits};
use crate::ws::WsPool;

/// Sends the jsonrpc requests of a run.
#[derive(Clone, Default)]
pub struct JsonrpcRunner {
    /// `--max-events`: replaces every subscription's `max_events`.
    pub max_events: Option<u32>,
    /// `--duration`: replaces every subscription's `max_duration_ms`.
    pub max_duration: Option<Duration>,
    /// `true` with `--no-validate`: calls are not checked against catalogs.
    pub skip_validation: bool,
    /// The run's WebSocket connections; [`JsonrpcRunner::close`] ends them.
    pub pool: WsPool,
    /// Set by interactive front ends (the TUI): subscriptions run until
    /// `stop` fires, and `feed` gets every event as it arrives, redacted.
    pub interactive: Option<Interactive>,
    /// Sends the HTTP POSTs in place of the run's engine (the agent tool's
    /// governed sender); `None` uses the engine.
    pub http: Option<Arc<dyn HttpSend + Send + Sync>>,
}

/// A subscription the user starts and stops.
#[derive(Clone, Debug)]
pub struct Interactive {
    pub stop: CancellationToken,
    pub feed: Option<tokio::sync::mpsc::UnboundedSender<Value>>,
}

impl JsonrpcExecutor for JsonrpcRunner {
    fn execute<'a>(&'a self, call: JsonrpcCall<'a>) -> JsonrpcFuture<'a> {
        Box::pin(async move { Ok(self.run(&call).await) })
    }
}

impl JsonrpcRunner {
    /// Close the connections the run kept open.
    pub async fn close(&self) {
        self.pool.close_all().await;
    }

    async fn run(&self, call: &JsonrpcCall<'_>) -> JsonrpcOutcome {
        if let Some(refusal) = guard::check_target(call.resolved) {
            return refused(call.resolved, RpcRefusal::Guard, &refusal.message);
        }
        if let Some(refusal) = self.validation(call) {
            return refused(call.resolved, RpcRefusal::Validation, &refusal);
        }
        let http: &dyn HttpSend = match &self.http {
            Some(sender) => sender.as_ref(),
            None => call.engine,
        };
        let client = JsonrpcClient::new(http)
            .with_pool(&self.pool)
            .with_subscribe(self.subscribe_options(call));
        let exchange = client.send(call.resolved, call.options).await;
        outcome(call.resolved, &exchange)
    }

    /// Why the catalog refuses the calls, when validation is on and the
    /// endpoint has a catalog.
    fn validation(&self, call: &JsonrpcCall<'_>) -> Option<String> {
        let enabled = call
            .jsonrpc
            .settings
            .as_ref()
            .and_then(|s| s.validate)
            .unwrap_or(true);
        if self.skip_validation || !enabled {
            return None;
        }
        let resolved = &call.resolved.http.url.text;
        let found = catalog_for(
            call.root,
            &call.settings.jsonrpc,
            &call.jsonrpc.endpoint,
            resolved,
        );
        let catalog = match found {
            Ok(Some(catalog)) => catalog,
            Ok(None) => return None,
            Err(e) => return Some(e.to_string()),
        };
        let violations: Vec<String> = call
            .resolved
            .calls
            .iter()
            .flat_map(|c| catalog.check(&c.method, c.params.as_ref()))
            .collect();
        (!violations.is_empty()).then(|| violations.join("; "))
    }

    fn subscribe_options(&self, call: &JsonrpcCall<'_>) -> SubscribeOptions {
        if let Some(interactive) = &self.interactive {
            return SubscribeOptions {
                limits: Some(SubscriptionLimits::interactive()),
                cancel: interactive.stop.clone(),
                events: interactive.feed.clone(),
                redactor: call.resolved.redactor(),
            };
        }
        SubscribeOptions {
            limits: call.resolved.subscription.as_ref().map(|s| self.limits(s)),
            cancel: call.cancel.clone(),
            ..SubscribeOptions::default()
        }
    }

    fn limits(&self, spec: &SubscriptionSpec) -> SubscriptionLimits {
        let headless = SubscriptionLimits::headless(spec);
        SubscriptionLimits {
            max_events: self.max_events.or(headless.max_events),
            max_duration: self.max_duration.or(headless.max_duration),
        }
    }
}

/// The runner's view of `exchange`.
fn outcome(target: &ResolvedJsonrpc, exchange: &RpcExchange) -> JsonrpcOutcome {
    let request = match exchange.transport {
        TransportKind::Http => {
            let body = serde_json::from_str(&exchange.sent).unwrap_or(Value::Null);
            http_request(target, body)
        }
        TransportKind::WebSocket => target.http.clone(),
    };
    JsonrpcOutcome {
        request,
        exchange: exchange.http.clone(),
        calls: call_checks(exchange),
        error: exchange.error.as_ref().map(ToString::to_string),
        record: exchange_record(exchange),
        elapsed: exchange.elapsed,
        trace: exchange.trace.clone(),
        traceresponse: exchange.traceresponse.clone(),
    }
}

/// An outcome where nothing was sent because `why`.
fn refused(target: &ResolvedJsonrpc, refusal: RpcRefusal, why: &str) -> JsonrpcOutcome {
    let started = Instant::now();
    let (envelopes, body) = request_body(&target.calls, target.batch, &mut IdSequence::default());
    let calls = envelopes
        .iter()
        .map(|e| RpcCallRecord {
            method: e.value["method"].as_str().unwrap_or_default().to_owned(),
            id: e.id.clone(),
            request: e.value.clone(),
            response: None,
            outcome: "failed".to_owned(),
            failure: None,
        })
        .collect::<Vec<_>>();
    let transport = match EndpointScheme::of(&target.http.url.text) {
        Some(EndpointScheme::WebSocket) => "ws",
        _ => "http",
    };
    JsonrpcOutcome {
        request: http_request(target, body),
        exchange: None,
        calls: calls
            .iter()
            .map(|_| JsonrpcCallCheck {
                envelope: Value::Object(Map::new()),
                failure: None,
                rpc_error: false,
            })
            .collect(),
        error: Some(why.to_owned()),
        record: JsonrpcRecord {
            transport: transport.to_owned(),
            batch: target.batch,
            calls,
            attempts: Vec::new(),
            subscription: None,
            note: None,
            refusal: Some(refusal),
            elapsed_ms: rustman_http::millis(started.elapsed()),
        },
        elapsed: started.elapsed(),
        trace: None,
        traceresponse: None,
    }
}
