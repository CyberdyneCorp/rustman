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
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rustman_core::RequestRef;
use rustman_core::model::{Assertion, GraphqlPreset, GraphqlRequest, GraphqlTransport};
use rustman_core::run::{
    EventRecord, GraphqlRecord, MAX_RECORDED_EVENTS, PresetRecord, SubgraphFacts,
    SubscriptionRecord, redact_value,
};
use rustman_core::settings::Settings;
use rustman_core::vars::ResolvedGraphql;
use rustman_core::vars::ResolvedRequest;
use rustman_http::runner::{GraphqlCall, GraphqlExecutor, GraphqlFuture, GraphqlOutcome};
use rustman_http::{Exchange, HttpEngine, HttpVersion, SendOptions, Timings};
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;

use crate::cache::SchemaCache;
use crate::document::load_document;
use crate::error::GraphqlError;
use crate::execute::{HttpOperation, HttpResult, execute_http};
use crate::paginate::{self, Pager, Paginate};
use crate::preset;
use crate::response::NOT_GRAPHQL;
use crate::source::{ActiveSchema, active_schema};
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
    /// The blockchain preset in force, from `preset:` or the schema.
    pub preset: Option<PresetRecord>,
}

/// Validate and send the request of `exec`. Validation errors, an
/// ambiguous document, a mutation over GET or a missing upload fail before
/// anything is sent.
pub async fn execute(exec: &Execution<'_>) -> Result<Executed, GraphqlError> {
    execute_with(exec, exec.resolved).await
}

/// [`execute`] with `resolved` in place of the request's own resolution
/// (a later page of a paginated operation).
async fn execute_with(
    exec: &Execution<'_>,
    resolved: &ResolvedGraphql,
) -> Result<Executed, GraphqlError> {
    let document = load_document(exec.request, exec.graphql)?;
    let schema = schema(exec, resolved)?;
    let warnings = match exec.validate {
        true => check(schema.as_ref(), &document, resolved)?,
        false => Vec::new(),
    };
    let preset = preset_of(exec.graphql, schema.as_ref(), &document, resolved);
    let operation = HttpOperation::for_request(exec.request, exec.graphql, document, resolved)?;
    let delivery = deliver(exec, resolved, &operation).await?;
    Ok(Executed {
        operation,
        warnings,
        delivery,
        preset,
    })
}

/// The active schema; without validation, one that cannot be read is
/// none.
fn schema(
    exec: &Execution<'_>,
    resolved: &ResolvedGraphql,
) -> Result<Option<ActiveSchema>, GraphqlError> {
    let cache = SchemaCache::for_workspace(exec.root);
    let endpoint = &resolved.http.url.text;
    match active_schema(exec.request, &exec.settings.graphql, endpoint, &cache) {
        Err(_) if !exec.validate => Ok(None),
        other => other,
    }
}

/// Validate `document`; the warnings when nothing blocks the send.
fn check(
    schema: Option<&ActiveSchema>,
    document: &str,
    resolved: &ResolvedGraphql,
) -> Result<Vec<String>, GraphqlError> {
    let validation = validate(
        document,
        schema.map(|s| &s.schema),
        resolved.operation_name.as_deref(),
        &resolved.variables,
    );
    if validation.has_errors() {
        let errors = validation.errors().map(ToString::to_string).collect();
        return Err(GraphqlError::Invalid(errors));
    }
    Ok(validation.warnings().map(ToString::to_string).collect())
}

/// The preset in force and the blockchain scalars of the operation.
fn preset_of(
    graphql: &GraphqlRequest,
    schema: Option<&ActiveSchema>,
    document: &str,
    resolved: &ResolvedGraphql,
) -> Option<PresetRecord> {
    let schema = schema.map(|s| &s.schema);
    let mut record = preset::choose(graphql.preset, schema)?;
    if let Some(schema) = schema {
        let operation = resolved.operation_name.as_deref();
        record.scalars = preset::scalar_paths(schema, document, operation);
    }
    Some(record)
}

/// Send the request of `exec` page by page (`--paginate`): see
/// [`crate::paginate`]. When a later page fails (a transport error, a
/// result with errors or not a GraphQL result), the pages read before it
/// are kept and an error saying which page failed is added to the result;
/// a failing first page is the result as it is.
pub async fn execute_paged(
    exec: &Execution<'_>,
    paginate: Paginate,
) -> Result<Executed, GraphqlError> {
    if exec.graphql.effective_transport() != GraphqlTransport::Http {
        return Err(GraphqlError::Paginate(
            "subscriptions cannot be paginated".to_owned(),
        ));
    }
    let document = load_document(exec.request, exec.graphql)?;
    let mut resolved = exec.resolved.clone();
    let operation = resolved.operation_name.as_deref();
    let fields = paginate::plan(&document, operation, &resolved.variables, paginate.mode)
        .map_err(GraphqlError::Paginate)?;
    let mut pager = Pager::new(fields, paginate.mode);
    pager.start(&mut resolved.variables);
    let mut last: Option<(Executed, Value)> = None;
    loop {
        let page = pager.pages() + 1;
        let executed = match execute_with(exec, &resolved).await {
            Ok(executed) => executed,
            Err(error) => {
                return keep_pages(last, pager, page, &error.to_string(), Vec::new()).ok_or(error);
            }
        };
        let data = match clean_data(&executed) {
            Ok(data) => data,
            Err(errors) => {
                let why = page_failure(&executed);
                return Ok(keep_pages(last, pager, page, &why, errors).unwrap_or(executed));
            }
        };
        let bytes = body_bytes(&executed);
        let more = match pager.take(&data, bytes, &mut resolved.variables) {
            Ok(more) => more,
            Err(why) => {
                let kept = keep_pages(last, pager, page, &why, Vec::new());
                return kept.ok_or(GraphqlError::Paginate(why));
            }
        };
        let stop = pager.stop_warning(more, paginate.max_pages);
        if more && stop.is_none() {
            last = Some((executed, data));
            continue;
        }
        let mut executed = executed;
        executed.warnings.extend(stop);
        replace_data(&mut executed, pager.merge(data), Vec::new());
        return Ok(executed);
    }
}

/// The pages read before page `failed` as the result, with an error saying
/// why paging ended after the page's own `errors`; `None` on the first page.
fn keep_pages(
    last: Option<(Executed, Value)>,
    pager: Pager,
    failed: u32,
    why: &str,
    mut errors: Vec<Value>,
) -> Option<Executed> {
    let (mut executed, data) = last?;
    let message = paginate::partial_error(failed, why);
    errors.push(serde_json::json!({ "message": message }));
    replace_data(&mut executed, pager.merge(data), errors);
    Some(executed)
}

/// Why a page that is not a clean GraphQL result failed.
fn page_failure(executed: &Executed) -> String {
    let Delivery::Http(result) = &executed.delivery else {
        return "not an HTTP result".to_owned();
    };
    match &result.response {
        Some(response) if !response.errors.is_empty() => "it returned errors".to_owned(),
        Some(_) => "it returned no data".to_owned(),
        None => format!("HTTP {}, not a GraphQL result", result.exchange.status),
    }
}

/// The body size of an HTTP result.
fn body_bytes(executed: &Executed) -> u64 {
    match &executed.delivery {
        Delivery::Http(result) => result.exchange.size_bytes,
        _ => 0,
    }
}

/// The `data` of an HTTP result without errors; else its errors (none
/// when it is not a GraphQL result).
fn clean_data(executed: &Executed) -> Result<Value, Vec<Value>> {
    let Delivery::Http(result) = &executed.delivery else {
        return Err(Vec::new());
    };
    let Some(response) = result.response.as_ref() else {
        return Err(Vec::new());
    };
    if !response.errors.is_empty() {
        return Err(response
            .errors
            .iter()
            .filter_map(|e| serde_json::to_value(e).ok())
            .collect());
    }
    response.data.clone().ok_or_else(Vec::new)
}

/// Make `data` (with `errors`, when any) the result of `executed`: its
/// body and parsed response.
fn replace_data(executed: &mut Executed, data: Value, errors: Vec<Value>) {
    let Delivery::Http(result) = &mut executed.delivery else {
        return;
    };
    let mut body = serde_json::Map::new();
    body.insert("data".to_owned(), data);
    if !errors.is_empty() {
        body.insert("errors".to_owned(), Value::Array(errors));
    }
    if let Some(extensions) = result.response.as_ref().and_then(|r| r.extensions.clone()) {
        body.insert("extensions".to_owned(), Value::Object(extensions));
    }
    let bytes = Bytes::from(Value::Object(body).to_string());
    result.response = crate::response::GraphqlResponse::parse(&bytes);
    result.exchange.size_bytes = bytes.len() as u64;
    result.exchange.body = bytes;
    result.exchange.spill = None;
}

async fn deliver(
    exec: &Execution<'_>,
    resolved: &ResolvedGraphql,
    operation: &HttpOperation,
) -> Result<Delivery, GraphqlError> {
    let options = exec.options;
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

/// Reads the chain head from an EVM node, for a subgraph's freshness.
/// `rustman-evm` implements it; front ends hand it to [`GraphqlRunner`].
pub trait ChainHead: Send + Sync + std::fmt::Debug {
    /// The latest block number of `endpoint` (redacted error).
    fn head<'a>(
        &'a self,
        endpoint: &'a ResolvedRequest,
        options: &'a SendOptions,
    ) -> Pin<Box<dyn Future<Output = Result<u64, String>> + 'a>>;
}

/// The query of a subgraph's indexing status.
pub const META_QUERY: &str = "{ _meta { block { number } hasIndexingErrors } }";

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
    /// `--paginate`: send HTTP operations page by page.
    pub paginate: Option<Paginate>,
    /// Reads the chain head for subgraph freshness; without it, the lag is
    /// unknown (no rpc).
    pub chain_head: Option<Arc<dyn ChainHead>>,
    /// Check every subgraph's freshness (the TUI, `rustman graphql
    /// query`), not only for `subgraph` assertions.
    pub freshness: bool,
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
            paginate: None,
            chain_head: None,
            freshness: false,
        }
    }
}

impl GraphqlExecutor for GraphqlRunner {
    fn execute<'a>(&'a self, call: GraphqlCall<'a>) -> GraphqlFuture<'a> {
        Box::pin(async move { self.run(&call).await.map_err(|e| e.to_string()) })
    }

    fn wants_chain_rpc(&self, graphql: &GraphqlRequest) -> bool {
        let not_subgraph = graphql.preset.is_some_and(|p| p != GraphqlPreset::Subgraph);
        self.chain_head.is_some() && !not_subgraph && self.reads_freshness(graphql)
    }
}

/// Whether `graphql` has a `subgraph` assertion.
fn asserts_freshness(graphql: &GraphqlRequest) -> bool {
    graphql
        .assertions
        .iter()
        .flatten()
        .any(|a| matches!(a, Assertion::Subgraph(_)))
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
            match self.paginate {
                Some(paginate) => execute_paged(&exec, paginate).await,
                None => execute(&exec).await,
            }
        };
        let forward = self.forward(events, call);
        let (executed, ()) = tokio::join!(send, forward);
        let mut outcome = outcome(executed?, call.graphql);
        if self.reads_freshness(call.graphql) {
            self.freshness(call, &mut outcome.record).await;
        }
        Ok(outcome)
    }

    fn reads_freshness(&self, graphql: &GraphqlRequest) -> bool {
        self.freshness || asserts_freshness(graphql)
    }

    /// Read a subgraph's `_meta` and the chain head into its preset.
    async fn freshness(&self, call: &GraphqlCall<'_>, record: &mut GraphqlRecord) {
        let Some(preset) = record.preset.as_mut() else {
            return;
        };
        if preset.name != GraphqlPreset::Subgraph.as_str() {
            return;
        }
        let mut facts = SubgraphFacts {
            rpc: call.chain_rpc.is_some(),
            ..SubgraphFacts::default()
        };
        match fetch_meta(call).await {
            Ok((block, errors)) => {
                facts.indexed_block = Some(block);
                facts.has_indexing_errors = errors;
            }
            Err(problem) => facts.problem = Some(problem),
        }
        let head = match (&call.chain_rpc, &self.chain_head) {
            (Some(Ok((endpoint, options))), Some(chain)) => {
                Some(chain.head(endpoint, options).await)
            }
            (Some(Err(problem)), _) => Some(Err(format!("rpc: {problem}"))),
            _ => None,
        };
        match head {
            Some(Ok(head)) => facts.head = Some(head),
            Some(Err(problem)) => facts.problem = facts.problem.or(Some(problem)),
            None => {}
        }
        preset.freshness = Some(facts);
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

/// The block a subgraph indexed and whether it has indexing errors, from
/// its `_meta` (redacted error).
async fn fetch_meta(call: &GraphqlCall<'_>) -> Result<(u64, bool), String> {
    let mut graphql = call.graphql.clone();
    graphql.apq = Some(false);
    graphql.method = None;
    graphql.transport = None;
    let mut resolved = call.resolved.clone();
    resolved.variables = serde_json::Map::new();
    resolved.operation_name = None;
    // The query has no variables, so no upload to resolve.
    let no_uploads = |path: &str| Err(GraphqlError::UploadNotFound(path.to_owned()));
    let operation = HttpOperation::prepare(META_QUERY.to_owned(), &graphql, &resolved, no_uploads)
        .map_err(|e| e.to_string())?;
    let result = execute_http(call.engine, call.options, &resolved, &operation)
        .await
        .map_err(|e| format!("_meta: {}", e.redacted(&resolved.redactor())))?;
    let meta = result
        .response
        .and_then(|r| r.data)
        .and_then(|d| d.get("_meta").cloned())
        .ok_or("_meta: no result")?;
    let block = meta
        .pointer("/block/number")
        .and_then(Value::as_u64)
        .ok_or("_meta: no block number")?;
    let errors = meta.get("hasIndexingErrors").and_then(Value::as_bool);
    Ok((block, errors.unwrap_or(false)))
}

/// What the runner checks and records for `executed`.
pub fn outcome(executed: Executed, graphql: &GraphqlRequest) -> GraphqlOutcome {
    let Executed {
        operation,
        mut warnings,
        delivery,
        preset,
    } = executed;
    if let Some(warning) = preset.as_ref().and_then(|p| p.warning.clone()) {
        warnings.insert(0, warning);
    }
    let mut record = GraphqlRecord {
        operation_name: operation.operation.name.clone(),
        operation_type: operation.operation.kind.as_str().to_owned(),
        transport: transport_name(graphql.effective_transport()).to_owned(),
        protocol: None,
        variables: Value::Object(operation.variables.clone()),
        result: None,
        apq: None,
        subscription: None,
        preset,
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
