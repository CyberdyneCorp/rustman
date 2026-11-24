//! `graphql_introspect` and `graphql_query` (agent-mcp-interface
//! "Conditional protocol tools"), registered when the build includes the
//! `graphql` feature (the graphql-requests capability).
//!
//! Both reach the endpoint through the session's protocol sender: the
//! allowlist, the production rule and the session's rate and concurrency
//! limits apply to every request. Every GraphQL operation over HTTP is a
//! `POST`, so the method rule reads the operation instead: a query is a
//! read, a mutation needs `[agent] allow_mutations` and the human's
//! confirmation, and subscriptions are refused. Operations go through the
//! engine's runner, which validates against the active schema, asserts,
//! records history and redacts as for every request.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustman_core::collection::RequestEntry;
use rustman_core::model::{GraphqlRequest, GraphqlTransport, REQUEST_FILE_SUFFIX, Request};
use rustman_core::run::RequestRecord;
use rustman_core::snapshot::SnapshotMode;
use rustman_core::vars::{
    Layer, ResolvedBody, ResolvedGraphql, ResolvedRequest, Resolver, VariablesSource,
    resolve_graphql,
};
use rustman_core::workspace::RequestRef;
use rustman_graphql::{
    GraphqlRunner, HttpSender, OperationKind, SchemaCache, SchemaOrigin, SendFuture, SharedSender,
    active_schema, load_document, normalize_endpoint, refresh_schema, select_operation,
};
use rustman_http::runner::{RunContext, RunOptions, Runner, Sent};
use rustman_http::{GovernedError, GovernedSender, RequestError, SendOptions};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::execute::{AssertionView, history, outcome, overrides};
use super::{Call, output};
use crate::server::confirm::question;
use crate::server::context::{AgentContext, ToolOutput};
use crate::server::error::{ErrorCode, ToolError};
use crate::server::jobs::on_own_thread;
use crate::server::policy::check_method;

/// The names the tools are registered under.
pub const INTROSPECT_TOOL: &str = "graphql_introspect";
pub const QUERY_TOOL: &str = "graphql_query";

/// The name ad-hoc operations are reported under.
const AD_HOC: &str = "ad-hoc";

/// Why agents cannot subscribe.
const NO_SUBSCRIPTIONS: &str = "subscriptions are not available to agents (graphql_query sends queries and mutations over HTTP)";

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GraphqlIntrospectArgs {
    /// The http(s) URL of a GraphQL endpoint; `{{variables}}` resolve in
    /// the session's environment. Give either `endpoint` or `path`.
    pub endpoint: Option<String>,
    /// A saved `kind: graphql` request whose endpoint (and `schema_file`)
    /// to use.
    pub path: Option<String>,
    /// Describe this named type (fields with arguments, input fields, enum
    /// values, interfaces, possible types) instead of listing every type.
    pub type_name: Option<String>,
    /// Introspect the endpoint again instead of using its `schema_file` or
    /// cached schema.
    #[serde(default)]
    pub refresh: bool,
    /// Variables for this call, above every other source.
    pub vars: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GraphqlQueryArgs {
    /// A saved `kind: graphql` request: its id or file. Give either
    /// `path`, or `endpoint` with `query`.
    pub path: Option<String>,
    /// The http(s) URL of an ad-hoc operation.
    pub endpoint: Option<String>,
    /// The document of an ad-hoc operation: a query, or a mutation (needs
    /// `[agent] allow_mutations` and the human's confirmation).
    pub query: Option<String>,
    /// The operation to send when the ad-hoc document holds several.
    pub operation_name: Option<String>,
    /// The variables of an ad-hoc operation, a JSON object.
    pub variables: Option<Map<String, Value>>,
    /// Request headers of an ad-hoc operation.
    pub headers: Option<BTreeMap<String, String>>,
    /// Variables for this call, above every other source.
    pub vars: Option<BTreeMap<String, String>>,
    /// Total timeout in milliseconds, replacing the request's own.
    pub timeout_ms: Option<u64>,
}

/// A schema, from the endpoint, its cache or a `schema_file`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct GraphqlSchemaResult {
    /// The endpoint, normalized and redacted.
    pub endpoint: String,
    /// `introspected` (just now), `cache` or `schema_file`.
    pub source: String,
    /// When the schema was introspected (`introspected` and `cache`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<String>,
    /// The `schema_file` read, as written (`schema_file`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_file: Option<String>,
    /// How many named types the schema defines.
    pub type_count: usize,
    /// The root operation types, every named type (`name`, `kind`) and the
    /// directive names; absent with `type_name`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overview: Option<Value>,
    /// The type named by `type_name`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_detail: Option<Value>,
    /// Problems found reading the schema.
    pub warnings: Vec<String>,
}

/// The redacted result of `graphql_query`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct GraphqlQueryResult {
    /// The saved request's id, or `ad-hoc`.
    pub request: String,
    /// `passed`, `failed` (an assertion), `errored` (no usable answer, an
    /// invalid document) or `skipped`.
    pub outcome: String,
    /// Why there is no usable answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `query` or `mutation`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_name: Option<String>,
    /// `ok`, `partial` (data and errors) or `errors`; absent when the
    /// response is not a GraphQL result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// The HTTP status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    pub errors: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Value>,
    /// Deprecated fields used, and other non-blocking problems.
    pub warnings: Vec<String>,
    pub assertions: Vec<AssertionView>,
}

pub async fn graphql_introspect(call: &Call) -> Result<ToolOutput, ToolError> {
    let call = call.clone();
    on_own_thread(move || async move { introspect_here(&call).await }).await?
}

pub async fn graphql_query(call: &Call) -> Result<ToolOutput, ToolError> {
    let call = call.clone();
    on_own_thread(move || async move { query_here(&call).await }).await?
}

/// Run a GraphQL tool; `None` when `name` is not one of them.
pub async fn dispatch(name: &str, call: &Call) -> Option<Result<ToolOutput, ToolError>> {
    let result = match name {
        INTROSPECT_TOOL => graphql_introspect(call).await,
        QUERY_TOOL => graphql_query(call).await,
        _ => return None,
    };
    Some(result)
}

async fn introspect_here(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: GraphqlIntrospectArgs = call.args()?;
    let ctx = &call.ctx;
    let entry = match (&args.endpoint, &args.path) {
        (Some(endpoint), None) => Some(ad_hoc_entry(endpoint, "{ __typename }", None, None, None)?),
        (None, Some(_)) => None,
        _ => {
            return Err(invalid(
                "give either `endpoint` or `path` (a saved graphql request)",
            ));
        }
    };
    let request = match &entry {
        Some(entry) => ad_hoc(ctx, entry),
        None => saved(ctx, args.path.as_deref().unwrap_or_default())?,
    };
    let resolved = resolve(ctx, &request, args.vars.as_ref())?;
    let url = resolved.http.url.text.clone();
    call.target.set("POST", &url);
    let ws = ctx.workspace();
    let cache = SchemaCache::for_workspace(&ws.root);
    let redactor = resolved.redactor();
    let shown_endpoint = || {
        normalize_endpoint(&url)
            .map(|e| redactor.text(&e).into_owned())
            .map_err(|e| invalid(e.to_string()))
    };
    let active = match args.refresh {
        true => None,
        false => active_schema(&request, &ws.settings.graphql, &url, &cache)
            .map_err(|e| invalid(e.redacted(&redactor).to_string()))?,
    };
    let (mut result, schema) = match active {
        Some(active) => {
            let (source, fetched_at, schema_file) = match active.origin {
                SchemaOrigin::RequestFile(file) | SchemaOrigin::SettingsFile(file) => {
                    ("schema_file", None, Some(file))
                }
                SchemaOrigin::Cache { fetched_at, .. } => ("cache", Some(fetched_at), None),
            };
            (
                schema_result(shown_endpoint()?, source, fetched_at, schema_file),
                active.schema,
            )
        }
        None => {
            let sender = AgentSender::new(ctx.rpc_sender()?, Access::Read);
            precheck(&sender, &url)?;
            let options = SendOptions::for_request(&ws.settings, &ws.root, &resolved.http)
                .map_err(|e| invalid(e.to_string()))?;
            let max = ws.settings.graphql.max_schema_bytes;
            let fetched = refresh_schema(&sender, &options, &resolved, &cache, max).await;
            sender.check()?;
            let cached = fetched.map_err(|e| invalid(e.redacted(&redactor).to_string()))?;
            (
                schema_result(
                    cached.endpoint,
                    "introspected",
                    Some(cached.fetched_at),
                    None,
                ),
                cached.schema,
            )
        }
    };
    result.type_count = schema.type_count();
    result.warnings = schema.warnings().to_vec();
    match &args.type_name {
        Some(name) => {
            let detail = schema.type_detail(name).ok_or_else(|| {
                ToolError::new(
                    ErrorCode::NotFound,
                    format!("no type '{name}' in the schema"),
                )
            })?;
            result.type_detail = Some(serde_json::to_value(detail).unwrap_or_default());
        }
        None => result.overview = Some(overview(&schema)),
    }
    let line = format!(
        "{}: {} types ({})",
        result.endpoint, result.type_count, result.source
    );
    output(&result, line)
}

fn schema_result(
    endpoint: String,
    source: &str,
    fetched_at: Option<String>,
    schema_file: Option<String>,
) -> GraphqlSchemaResult {
    GraphqlSchemaResult {
        endpoint,
        source: source.to_owned(),
        fetched_at,
        schema_file,
        type_count: 0,
        overview: None,
        type_detail: None,
        warnings: Vec::new(),
    }
}

/// The root types, the named types (name and kind only, so a large schema
/// stays within the output limit: `type_name` gives the rest) and the
/// directive names.
fn overview(schema: &rustman_graphql::GraphqlSchema) -> Value {
    let overview = schema.overview();
    let types: Vec<Value> = overview
        .types
        .iter()
        .map(|t| json!({"name": t.name, "kind": t.kind}))
        .collect();
    let directives: Vec<&str> = overview
        .directives
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    json!({
        "query": overview.query,
        "mutation": overview.mutation,
        "subscription": overview.subscription,
        "types": types,
        "directives": directives,
    })
}

async fn query_here(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: GraphqlQueryArgs = call.args()?;
    let ctx = &call.ctx;
    let ad_hoc_fields = args.query.is_some()
        || args.operation_name.is_some()
        || args.variables.is_some()
        || args.headers.is_some();
    let entry = match (&args.path, &args.endpoint, &args.query) {
        (Some(_), None, _) if !ad_hoc_fields => None,
        (None, Some(endpoint), Some(query)) => Some(ad_hoc_entry(
            endpoint,
            query,
            args.operation_name.as_deref(),
            args.variables.as_ref(),
            args.headers.as_ref(),
        )?),
        _ => {
            return Err(invalid(
                "give either `path` (a saved graphql request) or `endpoint` with `query` and optional `operation_name`, `variables` and `headers`",
            ));
        }
    };
    if let Some(entry) = &entry {
        ctx.learn_request_literals(&entry.request);
    }
    let request = match &entry {
        Some(entry) => ad_hoc(ctx, entry),
        None => saved(ctx, args.path.as_deref().unwrap_or_default())?,
    };
    let access = authorize(call, &request, args.vars.as_ref()).await?;
    let sender = Arc::new(AgentSender::new(ctx.rpc_sender()?, access));
    let graphql = GraphqlRunner {
        sender: Some(SharedSender(sender.clone())),
        ..GraphqlRunner::default()
    };
    let options = RunOptions {
        overrides: overrides(args.vars.as_ref())?,
        timeout: args.timeout_ms.map(Duration::from_millis),
        snapshots: SnapshotMode::Ci,
        graphql: Some(&graphql),
        history: history(ctx),
        ad_hoc: entry.is_some(),
        ..RunOptions::default()
    };
    let sent = send(call, &request, options).await?;
    sender.check()?;
    let result = query_result(ctx, &sent);
    let line = match (&result.error, &result.result) {
        (Some(error), _) => format!("{} {}: {error}", result.request, result.outcome),
        (None, Some(status)) => format!("{} {} ({status})", result.request, result.outcome),
        (None, None) => format!("{} {}", result.request, result.outcome),
    };
    output(&result, line)
}

/// Let the operation of `request` through the agent permissions: a query
/// goes on; a mutation needs `[agent]` to allow mutations and the human's
/// confirmation, as an HTTP `POST` does; a subscription is refused. A host
/// the session does not allow is refused before the human is asked.
async fn authorize(
    call: &Call,
    request: &RequestRef<'_>,
    vars: Option<&BTreeMap<String, String>>,
) -> Result<Access, ToolError> {
    let ctx = &call.ctx;
    let Request::Graphql(graphql) = request.request() else {
        return Err(invalid("not a graphql request"));
    };
    if graphql.effective_transport() != GraphqlTransport::Http {
        return Err(ToolError::blocked(NO_SUBSCRIPTIONS));
    }
    let resolved = resolve(ctx, request, vars)?;
    let url = resolved.http.url.text.clone();
    call.target.set("POST", &url);
    let redactor = resolved.redactor();
    let document =
        load_document(request, graphql).map_err(|e| invalid(e.redacted(&redactor).to_string()))?;
    let operation = select_operation(&document, resolved.operation_name.as_deref())
        .map_err(|e| invalid(e.to_string()))?;
    let sender = ctx.rpc_sender()?;
    sender
        .policy()
        .check("POST", &url)
        .map_err(|blocked| ToolError::blocked(blocked.reason()))?;
    match operation.kind {
        OperationKind::Query => Ok(Access::Read),
        OperationKind::Subscription => Err(ToolError::blocked(NO_SUBSCRIPTIONS)),
        OperationKind::Mutation => {
            check_method(ctx.agent_settings().permission(), "POST")?;
            let shown = ctx.redactor().text(&url);
            let environment = ctx.environment().map(|e| e.name);
            let message = question("a GraphQL mutation to", &shown, environment.as_deref());
            ctx.ask(&message, &format!("GraphQL mutation {shown}"), &call.peer)
                .await?;
            Ok(Access::Mutation)
        }
    }
}

/// An ad-hoc graphql request: `document` POSTed to `endpoint`.
fn ad_hoc_entry(
    endpoint: &str,
    document: &str,
    operation_name: Option<&str>,
    variables: Option<&Map<String, Value>>,
    headers: Option<&BTreeMap<String, String>>,
) -> Result<RequestEntry, ToolError> {
    let lower = endpoint.trim_start().to_ascii_lowercase();
    if lower.starts_with("ws://") || lower.starts_with("wss://") {
        return Err(ToolError::blocked(NO_SUBSCRIPTIONS));
    }
    let mut fields = Map::new();
    fields.insert("version".into(), json!(1));
    fields.insert("kind".into(), json!("graphql"));
    fields.insert("name".into(), json!(AD_HOC));
    fields.insert("endpoint".into(), json!(endpoint));
    fields.insert("document".into(), json!(document));
    if let Some(name) = operation_name {
        fields.insert("operation_name".into(), json!(name));
    }
    if let Some(variables) = variables {
        fields.insert("variables".into(), Value::Object(variables.clone()));
    }
    if let Some(headers) = headers {
        let list: Vec<Value> = headers
            .iter()
            .map(|(name, value)| json!({"name": name, "value": value}))
            .collect();
        fields.insert("headers".into(), Value::Array(list));
    }
    // A JSON document is valid YAML.
    let text = Value::Object(fields).to_string();
    let path = PathBuf::from(format!("{AD_HOC}{REQUEST_FILE_SUFFIX}"));
    let request = GraphqlRequest::parse(&path, &text).map_err(|e| invalid(e.to_string()))?;
    Ok(RequestEntry {
        path,
        request: Request::Graphql(Box::new(request)),
    })
}

/// The saved graphql request `path`.
fn saved<'c>(ctx: &'c AgentContext, path: &str) -> Result<RequestRef<'c>, ToolError> {
    let found = super::workspace::find(ctx, path)?;
    if !matches!(found.request(), Request::Graphql(_)) {
        return Err(invalid(format!("'{path}' is not a graphql request")));
    }
    Ok(found)
}

/// `entry` as a request of the session's workspace.
fn ad_hoc<'c>(ctx: &'c AgentContext, entry: &'c RequestEntry) -> RequestRef<'c> {
    let ws = ctx.workspace();
    RequestRef {
        root: &ws.root,
        http: &ws.settings.http,
        entry,
        ancestors: Vec::new(),
    }
}

/// `request` resolved in the session's environment, its secrets learnt.
fn resolve(
    ctx: &AgentContext,
    request: &RequestRef<'_>,
    vars: Option<&BTreeMap<String, String>>,
) -> Result<ResolvedGraphql, ToolError> {
    let ws = ctx.workspace();
    let environment = ctx.environment();
    let vars = overrides(vars)?;
    let mut resolver =
        Resolver::for_request(&ws.settings, request, environment.as_ref(), ctx.secrets());
    resolver.push(VariablesSource::new(Layer::Override, &vars));
    let resolved = resolve_graphql(request, &resolver).map_err(|e| invalid(e.to_string()))?;
    ctx.learn(&resolved.redactor());
    Ok(resolved)
}

/// Send `target` in a run of its own.
async fn send(
    call: &Call,
    target: &RequestRef<'_>,
    options: RunOptions<'_>,
) -> Result<Sent, ToolError> {
    let ctx = &call.ctx;
    let environment = ctx.environment();
    let ws = ctx.workspace();
    let run_ctx = RunContext {
        root: &ws.root,
        settings: &ws.settings,
        environment: environment.as_ref(),
        secrets: ctx.secrets(),
        in_workspace: true,
    };
    let runner = Runner::new(run_ctx, options).map_err(|e| ToolError::internal(e.to_string()))?;
    let runner = ctx.with_providers(runner);
    let sent = runner.send(target, 1).await;
    if let Err(err) = runner.finish() {
        tracing::warn!("cookies: {err}");
    }
    ctx.learn(&sent.redactor);
    if let Some(exchange) = &sent.record.exchange {
        call.target
            .set(&exchange.request.method, &exchange.request.url);
    }
    Ok(sent)
}

/// The tool result of `sent`: the redacted record, and `data`, `errors`
/// and `extensions` from the body redacted with the session's and the
/// exchange's secrets.
fn query_result(ctx: &AgentContext, sent: &Sent) -> GraphqlQueryResult {
    let record: &RequestRecord = &sent.record;
    let body: Option<Value> = sent.exchange.as_ref().and_then(|e| {
        let session = ctx.redactor();
        let redacted = session.redactor().bytes(&e.body).into_owned();
        let redacted = sent.redactor.bytes(&redacted).into_owned();
        serde_json::from_slice(&redacted).ok()
    });
    let member = |name: &str| body.as_ref().and_then(|b| b.get(name)).cloned();
    let errors = match member("errors") {
        Some(Value::Array(errors)) => errors,
        _ => Vec::new(),
    };
    let graphql = record.graphql.as_ref();
    GraphqlQueryResult {
        request: record.id.clone(),
        outcome: outcome(record.status).to_owned(),
        error: record.error.clone(),
        operation_type: graphql.map(|g| g.operation_type.clone()),
        operation_name: graphql.and_then(|g| g.operation_name.clone()),
        result: graphql.and_then(|g| g.result.clone()),
        status: record.exchange.as_ref().map(|e| e.response.status),
        data: member("data").filter(|d| !d.is_null()),
        errors,
        extensions: member("extensions"),
        warnings: record.warnings.clone(),
        assertions: record
            .assertions
            .iter()
            .map(|a| AssertionView {
                passed: a.passed(),
                text: a.to_string(),
            })
            .collect(),
    }
}

fn invalid(message: impl ToString) -> ToolError {
    ToolError::new(ErrorCode::InvalidInput, message.to_string())
}

/// What the agent may send: queries only, or a mutation the human
/// confirmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    Read,
    Mutation,
}

/// Refused before anything is sent when the session does not allow `url`.
fn precheck(sender: &AgentSender, url: &str) -> Result<(), ToolError> {
    sender
        .governed
        .policy()
        .check("POST", url)
        .map_err(|blocked| ToolError::blocked(blocked.reason()))
}

/// The [`HttpSender`] of the GraphQL tools: every request goes through the
/// session's protocol sender (allowlist, production rule, rate limit), and
/// without a confirmed mutation, a request carrying anything but a query
/// is refused, whatever the document read before the run said. The first
/// refusal is kept for the tool's error code.
struct AgentSender {
    governed: Arc<GovernedSender>,
    access: Access,
    refused: Mutex<Option<ToolError>>,
}

impl AgentSender {
    fn new(governed: Arc<GovernedSender>, access: Access) -> Self {
        Self {
            governed,
            access,
            refused: Mutex::new(None),
        }
    }

    fn refuse(&self, reason: &str) -> RequestError {
        let mut refused = self.refused.lock().unwrap_or_else(PoisonError::into_inner);
        refused.get_or_insert_with(|| ToolError::blocked(reason));
        RequestError::Blocked(reason.to_owned())
    }

    /// The first refusal, as the tool's error.
    fn check(&self) -> Result<(), ToolError> {
        let refused = self.refused.lock().unwrap_or_else(PoisonError::into_inner);
        match refused.clone() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

impl HttpSender for AgentSender {
    fn send<'a>(
        &'a self,
        request: &'a ResolvedRequest,
        options: &'a SendOptions,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            if self.access == Access::Read
                && let Some(reason) = not_a_query(request)
            {
                return Err(self.refuse(reason));
            }
            self.governed
                .send(request, options)
                .await
                .map_err(|err| match err {
                    GovernedError::Blocked { reason, .. } => self.refuse(reason.reason()),
                    GovernedError::NotRun(why) => self.refuse(&why.to_string()),
                    GovernedError::Request(err) => err,
                })
        })
    }
}

/// Why `request` is not a plain query: a mutation or subscription document,
/// or a multipart upload. A body without a document (a persisted query's
/// hash) is the operation already read.
fn not_a_query(request: &ResolvedRequest) -> Option<&'static str> {
    let (document, name) = match &request.body {
        ResolvedBody::Json(Value::Object(payload)) => (
            payload
                .get("query")
                .and_then(Value::as_str)
                .map(str::to_owned),
            payload
                .get("operationName")
                .and_then(Value::as_str)
                .map(str::to_owned),
        ),
        ResolvedBody::None => {
            let param = |wanted: &str| {
                request
                    .query
                    .iter()
                    .find(|p| p.name.text == wanted)
                    .map(|p| p.value.text.clone())
            };
            (param("query"), param("operationName"))
        }
        _ => return Some("mutations not allowed for agents"),
    };
    let document = document?;
    match select_operation(&document, name.as_deref()) {
        Ok(operation) if operation.kind == OperationKind::Query => None,
        Ok(operation) if operation.kind == OperationKind::Subscription => Some(NO_SUBSCRIPTIONS),
        _ => Some("mutations not allowed for agents"),
    }
}

#[cfg(test)]
mod tests {
    use rustman_core::vars::{Resolved, ResolvedPair};

    use super::*;

    fn post(body: Value) -> ResolvedRequest {
        ResolvedRequest {
            method: "POST".to_owned(),
            url: Resolved::literal("http://127.0.0.1:9/graphql"),
            query: Vec::new(),
            headers: Vec::new(),
            body: ResolvedBody::Json(body),
            auth: None,
            settings: Default::default(),
            tls: Default::default(),
            secrets: Vec::new(),
        }
    }

    #[test]
    fn only_queries_pass_without_a_confirmed_mutation() {
        assert_eq!(not_a_query(&post(json!({"query": "{ me { id } }"}))), None);
        let mutation = post(json!({"query": "mutation { drop }"}));
        assert_eq!(
            not_a_query(&mutation),
            Some("mutations not allowed for agents")
        );
        let named = post(json!({
            "query": "query A { a } mutation B { b }",
            "operationName": "B",
        }));
        assert!(not_a_query(&named).is_some());
        let subscription = post(json!({"query": "subscription { ticks }"}));
        assert_eq!(not_a_query(&subscription), Some(NO_SUBSCRIPTIONS));
        // A persisted query's hash alone: the operation read before.
        assert_eq!(not_a_query(&post(json!({"extensions": {}}))), None);
        let mut get = post(Value::Null);
        get.body = ResolvedBody::None;
        get.query.push(ResolvedPair {
            name: Resolved::literal("query"),
            value: Resolved::literal("mutation { drop }"),
        });
        assert!(not_a_query(&get).is_some());
        let mut upload = post(Value::Null);
        upload.body = ResolvedBody::Multipart(Vec::new());
        assert!(not_a_query(&upload).is_some());
    }

    #[test]
    fn ad_hoc_operations_take_variables_headers_and_refuse_websockets() {
        let mut variables = Map::new();
        variables.insert("id".into(), json!(7));
        let headers = BTreeMap::from([("X-Trace".to_owned(), "1".to_owned())]);
        let entry = ad_hoc_entry(
            "{{base}}/graphql",
            "query Q($id: Int) { user(id: $id) { name } }",
            Some("Q"),
            Some(&variables),
            Some(&headers),
        )
        .unwrap();
        let Request::Graphql(graphql) = &entry.request else {
            panic!("a graphql request");
        };
        assert_eq!(graphql.endpoint, "{{base}}/graphql");
        assert_eq!(graphql.operation_name.as_deref(), Some("Q"));
        assert_eq!(graphql.variables.as_ref().unwrap()["id"], 7);
        assert_eq!(graphql.headers.as_ref().unwrap()[0].name, "X-Trace");
        let ws = ad_hoc_entry("wss://h/graphql", "subscription { t }", None, None, None);
        assert_eq!(ws.unwrap_err().code, ErrorCode::Blocked);
    }
}
