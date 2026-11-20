//! `grpc_list`, `grpc_describe` and `grpc_call` (add-grpc-client "Agent
//! tools"), registered when the build includes the `grpc` feature. Thin
//! adapters over [`rustman_grpc::agent::AgentGrpc`]: the target must be an
//! origin the session allows, every reach is paced by the session's
//! governor, and only unary and server-streaming methods are called. A
//! method not marked `idempotency_level = NO_SIDE_EFFECTS` is a mutation:
//! it needs `[agent] allow_mutations` and the human's confirmation. Calls
//! go through the engine's runner, which resolves, asserts, records
//! history and redacts as for every request.

use std::collections::BTreeMap;

use rustman_core::collection::RequestEntry;
use rustman_core::model::Request;
use rustman_core::run::RequestRecord;
use rustman_core::snapshot::SnapshotMode;
use rustman_core::vars::{Layer, ResolvedGrpc, Resolver, VariablesSource, resolve_grpc};
use rustman_core::workspace::RequestRef;
use rustman_grpc::agent::{Access, AgentError, AgentGrpc, AgentRefusal};
use rustman_grpc::{DescriptorCache, Schema, ServiceInfo};
use rustman_http::SendOptions;
use rustman_http::runner::{RunContext, RunOptions, Runner, Sent};
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

pub use rustman_grpc::agent::{CALL_TOOL, DESCRIBE_TOOL, LIST_TOOL};

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GrpcListArgs {
    /// The target: `host:port`, `grpc://host:port` or `grpcs://host:port`;
    /// `{{variables}}` resolve in the session's environment. Give either
    /// `target` or `path`.
    pub target: Option<String>,
    /// A saved `kind: grpc` request whose target (and schema) to use.
    pub path: Option<String>,
    /// Ask the server again instead of using the descriptor cache.
    #[serde(default)]
    pub refresh: bool,
    /// Variables for this call, above every other source.
    pub vars: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GrpcDescribeArgs {
    /// The target, as for `grpc_list`. Give either `target` or `path`.
    pub target: Option<String>,
    /// A saved `kind: grpc` request whose target (and schema) to use.
    pub path: Option<String>,
    /// A service, method (`package.Service/Method`) or message name.
    pub symbol: String,
    /// Ask the server again instead of using the descriptor cache.
    #[serde(default)]
    pub refresh: bool,
    /// Variables for this call, above every other source.
    pub vars: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GrpcCallArgs {
    /// A saved `kind: grpc` request: its id or file. Give either `path`,
    /// or `target` with `method`.
    pub path: Option<String>,
    /// The target of an ad-hoc call.
    pub target: Option<String>,
    /// The method of an ad-hoc call: `package.Service/Method`, unary or
    /// server streaming.
    pub method: Option<String>,
    /// The request message of an ad-hoc call, in the proto3 JSON mapping
    /// (default `{}`).
    pub message: Option<Value>,
    /// Request metadata of an ad-hoc call; names ending in `-bin` take
    /// base64.
    pub metadata: Option<BTreeMap<String, String>>,
    /// The deadline of an ad-hoc call in milliseconds.
    pub deadline_ms: Option<u64>,
    /// Variables for this call, above every other source.
    pub vars: Option<BTreeMap<String, String>>,
}

/// A method of a service.
#[derive(Debug, Serialize, JsonSchema)]
pub struct MethodView {
    /// `package.Service/Method`.
    pub path: String,
    /// `unary`, `server streaming`, `client streaming` or `bidi`.
    pub call_type: String,
    pub request: String,
    pub response: String,
    /// Marked `idempotency_level = NO_SIDE_EFFECTS`: callable without the
    /// mutation permission and confirmation.
    pub read_only: bool,
}

/// A service and its methods.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ServiceView {
    pub name: String,
    pub methods: Vec<MethodView>,
}

/// The services of a target.
#[derive(Debug, Serialize, JsonSchema)]
pub struct GrpcServices {
    pub services: Vec<ServiceView>,
    /// Set when the services come from the descriptor cache.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

/// A service, method (with a request template) or message.
#[derive(Debug, Serialize, JsonSchema)]
pub struct GrpcDescription {
    pub description: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

/// The redacted result of `grpc_call`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct GrpcCallResult {
    /// The saved request's id, or `ad-hoc`.
    pub request: String,
    /// `passed`, `failed` (a status or an assertion), `errored` (no
    /// status) or `skipped`.
    pub outcome: String,
    /// Why there is no usable answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The final status: code, name, message and decoded details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Value>,
    /// The received messages, in order.
    pub messages: Vec<Value>,
    pub headers: Vec<Value>,
    pub trailers: Vec<Value>,
    pub assertions: Vec<AssertionView>,
}

pub async fn grpc_list(call: &Call) -> Result<ToolOutput, ToolError> {
    let call = call.clone();
    on_own_thread(move || async move { list_here(&call).await }).await?
}

pub async fn grpc_describe(call: &Call) -> Result<ToolOutput, ToolError> {
    let call = call.clone();
    on_own_thread(move || async move { describe_here(&call).await }).await?
}

pub async fn grpc_call(call: &Call) -> Result<ToolOutput, ToolError> {
    let call = call.clone();
    on_own_thread(move || async move { call_here(&call).await }).await?
}

async fn list_here(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: GrpcListArgs = call.args()?;
    let target = Reach::new(args.target, args.path, args.vars);
    let schema = target.schema(call, args.refresh).await?;
    let result = GrpcServices {
        services: schema
            .services()
            .iter()
            .map(|s| service_view(s, &schema))
            .collect(),
        notice: schema.notice(),
    };
    let line = format!("{} services", result.services.len());
    output(&result, line)
}

async fn describe_here(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: GrpcDescribeArgs = call.args()?;
    let target = Reach::new(args.target, args.path, args.vars);
    let schema = target.schema(call, args.refresh).await?;
    let description = schema
        .describe(&args.symbol)
        .map_err(|e| ToolError::new(ErrorCode::NotFound, e.to_string()))?;
    let result = GrpcDescription {
        description: serde_json::to_value(&description).unwrap_or_default(),
        notice: schema.notice(),
    };
    output(&result, args.symbol)
}

/// What `grpc_list` and `grpc_describe` reach: a target or a saved request.
struct Reach {
    target: Option<String>,
    path: Option<String>,
    vars: Option<BTreeMap<String, String>>,
}

impl Reach {
    fn new(
        target: Option<String>,
        path: Option<String>,
        vars: Option<BTreeMap<String, String>>,
    ) -> Self {
        Self { target, path, vars }
    }

    /// The schema of the target, through the agent policy.
    async fn schema(&self, call: &Call, refresh: bool) -> Result<Schema, ToolError> {
        let ctx = &call.ctx;
        let entry = match (&self.target, &self.path) {
            (Some(target), None) => Some(rustman_grpc::target_entry(target).map_err(invalid)?),
            (None, Some(_)) => None,
            _ => {
                return Err(invalid(
                    "give either `target` or `path` (a saved grpc request)",
                ));
            }
        };
        let request = match &entry {
            Some(entry) => ad_hoc(ctx, entry),
            None => saved(ctx, self.path.as_deref().unwrap_or_default())?,
        };
        let resolved = resolve(ctx, &request, self.vars.as_ref())?;
        let ws = ctx.workspace();
        let options = SendOptions::for_request(&ws.settings, &ws.root, &resolved.request)
            .map_err(|e| invalid(e.to_string()))?;
        call.target.set("POST", resolved.target());
        let agent = AgentGrpc::new(ctx.rpc_sender()?);
        let cache = Some(DescriptorCache::for_workspace(&ws.root));
        let redactor = resolved.redactor();
        agent
            .schema(&resolved, &options, cache, refresh)
            .await
            .map_err(|e| schema_error(e, &redactor))
    }
}

fn schema_error(error: AgentError, redactor: &rustman_core::Redactor) -> ToolError {
    match error {
        AgentError::Refused(refusal) => refused(&refusal),
        AgentError::Halted(reason) => ToolError::blocked(reason),
        AgentError::Grpc(e) => {
            ToolError::new(ErrorCode::InvalidInput, e.redacted(redactor).to_string())
        }
    }
}

fn refused(refusal: &AgentRefusal) -> ToolError {
    match refusal {
        AgentRefusal::Policy(blocked) => ToolError::blocked(blocked.reason()),
        other => ToolError::blocked(other.to_string()),
    }
}

/// The saved grpc request `path`.
fn saved<'c>(ctx: &'c AgentContext, path: &str) -> Result<RequestRef<'c>, ToolError> {
    let found = super::workspace::find(ctx, path)?;
    if !matches!(found.request(), Request::Grpc(_)) {
        return Err(invalid(format!("'{path}' is not a grpc request")));
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
) -> Result<ResolvedGrpc, ToolError> {
    let ws = ctx.workspace();
    let environment = ctx.environment();
    let vars = overrides(vars)?;
    let mut resolver =
        Resolver::for_request(&ws.settings, request, environment.as_ref(), ctx.secrets());
    resolver.push(VariablesSource::new(Layer::Override, &vars));
    let resolved = resolve_grpc(request, &resolver).map_err(|e| invalid(e.to_string()))?;
    ctx.learn(&resolved.redactor());
    Ok(resolved)
}

fn invalid(message: impl ToString) -> ToolError {
    ToolError::new(ErrorCode::InvalidInput, message.to_string())
}

fn service_view(service: &ServiceInfo, schema: &Schema) -> ServiceView {
    let read_only = |path: &str| {
        schema
            .method(path)
            .is_ok_and(|m| rustman_grpc::has_no_side_effects(&m))
    };
    ServiceView {
        name: service.name.clone(),
        methods: service
            .methods
            .iter()
            .map(|m| MethodView {
                path: m.path.clone(),
                call_type: m.call_type.label().to_owned(),
                request: m.request.clone(),
                response: m.response.clone(),
                read_only: read_only(&m.path),
            })
            .collect(),
    }
}

async fn call_here(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: GrpcCallArgs = call.args()?;
    let ctx = &call.ctx;
    let entry = match (&args.path, &args.target) {
        (Some(_), None) if args.method.is_none() && args.message.is_none() => None,
        (None, Some(target)) => Some(
            rustman_grpc::ad_hoc_entry(target, ad_hoc_fields(&args)?)
                .map_err(|e| invalid(e.to_string()))?,
        ),
        _ => {
            return Err(invalid(
                "give either `path` (a saved grpc request) or `target` with `method` and optional `message` and `metadata`",
            ));
        }
    };
    let request = match &entry {
        Some(entry) => ad_hoc(ctx, entry),
        None => saved(ctx, args.path.as_deref().unwrap_or_default())?,
    };
    let mut agent = AgentGrpc::new(ctx.rpc_sender()?);
    authorize(call, &request, args.vars.as_ref(), &mut agent).await?;
    let options = RunOptions {
        overrides: overrides(args.vars.as_ref())?,
        snapshots: SnapshotMode::Ci,
        grpc: Some(&agent),
        history: history(ctx),
        ad_hoc: entry.is_some(),
        ..RunOptions::default()
    };
    let sent = send(call, &request, options).await?;
    if let Some(refusal) = agent.refused() {
        return Err(refused(&refusal));
    }
    let result = call_result(&sent.record);
    let line = match &result.error {
        Some(error) => format!("{} {}: {error}", result.request, result.outcome),
        None => format!("{} {}", result.request, result.outcome),
    };
    output(&result, line)
}

/// Let the call of `request` through the agent permissions: a read goes
/// on; a mutation needs `[agent]` to allow mutations and the human's
/// confirmation, as an HTTP `POST` does (every gRPC call is one).
async fn authorize(
    call: &Call,
    request: &RequestRef<'_>,
    vars: Option<&BTreeMap<String, String>>,
    agent: &mut AgentGrpc,
) -> Result<(), ToolError> {
    let ctx = &call.ctx;
    let resolved = resolve(ctx, request, vars)?;
    let ws = ctx.workspace();
    let options = SendOptions::for_request(&ws.settings, &ws.root, &resolved.request)
        .map_err(|e| invalid(e.to_string()))?;
    let url = format!("{}/{}", resolved.target(), resolved.method);
    call.target.set("POST", &url);
    let cache = Some(DescriptorCache::for_workspace(&ws.root));
    let access = agent
        .access(&resolved, &options, cache)
        .await
        .map_err(|e| schema_error(e, &resolved.redactor()))?;
    if access == Access::Read {
        return Ok(());
    }
    check_method(ctx.agent_settings().permission(), "POST")?;
    let shown = ctx.redactor().text(&url);
    let environment = ctx.environment().map(|e| e.name);
    let message = question("gRPC", &shown, environment.as_deref());
    ctx.ask(&message, &format!("gRPC {shown}"), &call.peer)
        .await?;
    agent.confirm(&resolved.method);
    Ok(())
}

/// The request-file fields of an ad-hoc call.
fn ad_hoc_fields(args: &GrpcCallArgs) -> Result<Map<String, Value>, ToolError> {
    let Some(method) = &args.method else {
        return Err(invalid("an ad-hoc call needs `method`"));
    };
    let mut fields = Map::new();
    fields.insert("method".into(), json!(method));
    let message = args.message.clone().unwrap_or_else(|| json!({}));
    if !message.is_object() {
        return Err(invalid("`message` must be a JSON object"));
    }
    fields.insert("message".into(), message);
    if let Some(metadata) = &args.metadata {
        let list: Vec<Value> = metadata
            .iter()
            .map(|(name, value)| json!({"name": name, "value": value}))
            .collect();
        fields.insert("metadata".into(), Value::Array(list));
    }
    if let Some(ms) = args.deadline_ms {
        fields.insert("deadline_ms".into(), json!(ms));
    }
    Ok(fields)
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
    if let Some(grpc) = &sent.record.grpc {
        let url = format!(
            "{}/{}",
            grpc["target"].as_str().unwrap_or_default(),
            grpc["method"].as_str().unwrap_or_default()
        );
        call.target.set("POST", &url);
    }
    Ok(sent)
}

/// The tool result of the (redacted) record.
fn call_result(record: &RequestRecord) -> GrpcCallResult {
    let grpc = record.grpc.clone().unwrap_or_default();
    let list = |key: &str| grpc[key].as_array().cloned().unwrap_or_default();
    GrpcCallResult {
        request: record.id.clone(),
        outcome: outcome(record.status).to_owned(),
        error: record.error.clone(),
        status: grpc.get("status").cloned(),
        messages: list("messages"),
        headers: list("headers"),
        trailers: list("trailers"),
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
