//! `rpc_call` (add-jsonrpc-client "Agent read-only tool"): send one
//! JSON-RPC call, or a saved `kind: jsonrpc` request, for an agent. A thin
//! adapter: [`rustman_jsonrpc::agent::AgentRunner`] checks the method
//! guards, the allowlist, the subscription refusal and
//! `agent_read_only_methods`, then sends through the session's governed
//! sender; the engine's runner resolves, asserts, records history and
//! redacts as for every request.

use std::collections::BTreeMap;
use std::time::Duration;

use rustman_core::Workspace;
use rustman_core::model::Request;
use rustman_core::run::RpcCallRecord;
use rustman_core::safety::Blocked;
use rustman_core::snapshot::SnapshotMode;
use rustman_core::workspace::RequestRef;
use rustman_http::runner::{RunContext, RunOptions, Runner, Sent};
use rustman_jsonrpc::agent::{AgentRefusal, AgentRpcPolicy, AgentRunner, UnknownPreset};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::execute::{AssertionView, history, outcome, overrides};
use super::{Call, output};
use crate::server::context::ToolOutput;
use crate::server::error::{ErrorCode, ToolError};
use crate::server::jobs::on_own_thread;

/// The name the tool is registered under.
pub const NAME: &str = rustman_jsonrpc::agent::TOOL_NAME;

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RpcCallArgs {
    /// A saved `kind: jsonrpc` request: its id or file. Give either `path`
    /// or `endpoint` with `method`.
    pub path: Option<String>,
    /// The http(s) or ws(s) URL of an ad-hoc call; `{{variables}}` resolve
    /// in the session's environment.
    pub endpoint: Option<String>,
    /// The method of an ad-hoc call. It must match `[jsonrpc]
    /// agent_read_only_methods`.
    pub method: Option<String>,
    /// The params of an ad-hoc call: a JSON array or object.
    pub params: Option<Value>,
    /// Variables for this call, above every other source.
    pub vars: Option<BTreeMap<String, String>>,
    /// Total timeout in milliseconds, replacing the request's own.
    pub timeout_ms: Option<u64>,
}

/// One call and its answer, redacted.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RpcCallView {
    pub method: String,
    /// `result`, `error`, `notification` or `failed`.
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// The error object, as received.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    /// Why the call failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

/// The redacted result of `rpc_call`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RpcCallResult {
    /// The saved request's id, or `ad-hoc`.
    pub request: String,
    /// `passed`, `failed` (an error object or an assertion), `errored` (no
    /// usable answer) or `skipped`.
    pub outcome: String,
    /// Why there is no usable answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub calls: Vec<RpcCallView>,
    pub assertions: Vec<AssertionView>,
}

/// The read-only method globs of `workspace`, presets expanded. The agent
/// server checks them when it starts: an unknown preset is a configuration
/// error (exit code 2).
pub fn read_only_policy(workspace: &Workspace) -> Result<AgentRpcPolicy, UnknownPreset> {
    AgentRpcPolicy::from_settings(&workspace.settings.jsonrpc)
}

/// Send on a thread of the session's own: the runner's futures are not
/// `Send`.
pub async fn rpc_call(call: &Call) -> Result<ToolOutput, ToolError> {
    let call = call.clone();
    on_own_thread(move || async move { call_here(&call).await }).await?
}

async fn call_here(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: RpcCallArgs = call.args()?;
    let ctx = &call.ctx;
    let ws = ctx.workspace();
    let policy =
        read_only_policy(ws).map_err(|e| ToolError::new(ErrorCode::InvalidInput, e.to_string()))?;
    let agent = AgentRunner::new(policy, ctx.rpc_sender()?);
    let options = RunOptions {
        overrides: overrides(args.vars.as_ref())?,
        timeout: args.timeout_ms.map(Duration::from_millis),
        snapshots: SnapshotMode::Ci,
        jsonrpc: Some(&agent),
        history: history(ctx),
        ..RunOptions::default()
    };
    let sent = match (&args.path, &args.endpoint) {
        (Some(path), None) if args.method.is_none() && args.params.is_none() => {
            let saved = super::workspace::find(ctx, path)?;
            if !matches!(saved.request(), Request::Jsonrpc(_)) {
                let message = format!("'{path}' is not a jsonrpc request");
                return Err(ToolError::new(ErrorCode::InvalidInput, message));
            }
            send(call, &saved, options, &agent).await
        }
        (None, Some(endpoint)) => {
            let entry = rustman_jsonrpc::ad_hoc_entry(endpoint, ad_hoc_fields(&args)?)
                .map_err(|e| ToolError::new(ErrorCode::InvalidInput, e.to_string()))?;
            let target = RequestRef {
                root: &ws.root,
                http: &ws.settings.http,
                entry: &entry,
                ancestors: Vec::new(),
            };
            let options = RunOptions {
                ad_hoc: true,
                ..options
            };
            send(call, &target, options, &agent).await
        }
        _ => {
            return Err(ToolError::new(
                ErrorCode::InvalidInput,
                "give either `path` (a saved jsonrpc request) or `endpoint` with `method` and optional `params`",
            ));
        }
    }?;
    let result = call_result(&sent);
    let line = match &result.error {
        Some(error) => format!("{} {}: {error}", result.request, result.outcome),
        None => format!("{} {}", result.request, result.outcome),
    };
    output(&result, line)
}

/// The request-file fields of an ad-hoc call.
fn ad_hoc_fields(args: &RpcCallArgs) -> Result<Map<String, Value>, ToolError> {
    let Some(method) = &args.method else {
        return Err(ToolError::new(
            ErrorCode::InvalidInput,
            "an ad-hoc call needs `method`",
        ));
    };
    let mut fields = Map::new();
    fields.insert("method".into(), Value::String(method.clone()));
    if let Some(params) = &args.params {
        fields.insert("params".into(), params.clone());
    }
    Ok(fields)
}

/// Send `target` with `agent` as the JSON-RPC transport. A refusal of the
/// agent policy is the tool's error; nothing was sent.
async fn send(
    call: &Call,
    target: &RequestRef<'_>,
    options: RunOptions<'_>,
    agent: &AgentRunner,
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
    agent.close().await;
    if let Err(err) = runner.finish() {
        tracing::warn!("cookies: {err}");
    }
    ctx.learn(&sent.redactor);
    if let Some(url) = sent.record.exchange.as_ref().map(|e| e.request.url.clone()) {
        call.target.set("POST", &url);
    }
    match agent.refused() {
        Some(refusal) => Err(refused(&refusal)),
        None => Ok(sent),
    }
}

fn refused(refusal: &AgentRefusal) -> ToolError {
    match refusal {
        AgentRefusal::Policy(blocked) => ToolError::blocked(Blocked::reason(*blocked)),
        other => ToolError::blocked(other.to_string()),
    }
}

/// The tool result of `sent`, from its redacted record.
fn call_result(sent: &Sent) -> RpcCallResult {
    let record = &sent.record;
    let calls = record
        .jsonrpc
        .iter()
        .flat_map(|j| j.calls.iter())
        .map(call_view)
        .collect();
    RpcCallResult {
        request: record.id.clone(),
        outcome: outcome(record.status).to_owned(),
        error: record.error.clone(),
        calls,
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

fn call_view(record: &RpcCallRecord) -> RpcCallView {
    let member = |name: &str| record.response.as_ref().and_then(|r| r.get(name)).cloned();
    RpcCallView {
        method: record.method.clone(),
        outcome: record.outcome.clone(),
        result: member("result"),
        error: member("error"),
        failure: record.failure.clone(),
    }
}
