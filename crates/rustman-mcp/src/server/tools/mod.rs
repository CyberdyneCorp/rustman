//! The tool catalog (spec "Structured tools with schemas"): every tool's
//! name, description, input and output JSON Schemas, and the dispatch of a
//! call to the engine. Each tool is a thin adapter over a library call; the
//! guardrails (audit, redaction, limits) wrap every call in
//! [`AgentContext::audited`](super::AgentContext::audited).

mod execute;
mod explore;
#[cfg(feature = "grpc")]
pub mod grpc;
mod history;
pub mod realtime;
pub mod reference;
pub(crate) mod reports;
pub mod rpc;
pub(crate) mod workspace;

use std::sync::Arc;

use rmcp::model::{JsonObject, ProgressToken, RequestId, Tool, ToolAnnotations};
use rmcp::service::{Peer, RoleServer};
use rustman_core::secrets::provider::mentions_reference;
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::context::{AgentContext, Target, ToolOutput};
use super::error::{ErrorCode, ToolError};
use super::jobs::{CANCEL_WAIT, Job, report_progress};

pub use explore::spec_source;

/// One tool call: the session, the client (to ask the human) and the
/// arguments.
#[derive(Clone)]
pub struct Call {
    pub ctx: Arc<AgentContext>,
    pub peer: Peer<RoleServer>,
    /// The outbound request of the call, for its audit line.
    pub target: Arc<Target>,
    pub arguments: Value,
    /// The id of the `tools/call` request.
    pub request: RequestId,
    /// The progress token the request carried, if any.
    pub progress: Option<ProgressToken>,
    /// Cancelled when the client cancels the request.
    pub cancelled: CancellationToken,
}

impl Call {
    /// The arguments as `T`; `invalid_input` when they do not fit.
    pub fn args<T: DeserializeOwned>(&self) -> Result<T, ToolError> {
        serde_json::from_value(self.arguments.clone())
            .map_err(|e| ToolError::new(ErrorCode::InvalidInput, format!("invalid arguments: {e}")))
    }

    /// Tie a job this call started to it: an MCP cancellation of the call
    /// stops the job, and its progress goes to the call's progress token.
    pub fn watch(&self, job: &Arc<Job>) {
        job.bind_request(self.request.clone());
        if let Some(token) = &self.progress {
            report_progress(
                self.ctx.clone(),
                job.clone(),
                self.peer.clone(),
                token.clone(),
            );
        }
        if self.cancelled.is_cancelled() {
            let job = job.clone();
            tokio::spawn(async move { job.cancel(CANCEL_WAIT).await });
        }
    }
}

/// A tool result: `value` as `structuredContent` and `summary` as text.
pub fn output<T: Serialize>(
    value: &T,
    summary: impl Into<String>,
) -> Result<ToolOutput, ToolError> {
    let structured = serde_json::to_value(value)
        .map_err(|e| ToolError::internal(format!("cannot serialize the result: {e}")))?;
    Ok(ToolOutput::new(structured, summary))
}

/// What a tool may do, for its annotations.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Effect {
    /// Reads the workspace or the session only.
    ReadOnly,
    /// Changes the workspace or a job, never sending requests.
    Writes,
    /// Sends requests, which may be `DELETE`s when `[agent]` allows them.
    Sends,
}

/// A tool whose input is `I` and whose `structuredContent` is `O`.
fn tool<I, O>(name: &'static str, description: &'static str, effect: Effect) -> Tool
where
    I: JsonSchema + 'static,
    O: JsonSchema + 'static,
{
    let read_only = effect == Effect::ReadOnly;
    let annotations = ToolAnnotations::new()
        .read_only(read_only)
        .destructive(effect == Effect::Sends)
        .open_world(!read_only);
    Tool::new(name, description, Arc::new(JsonObject::new()))
        .with_input_schema::<I>()
        .with_output_schema::<O>()
        .with_annotations(annotations)
}

/// Every tool this build registers, in a stable order.
pub fn catalog() -> Vec<Tool> {
    let mut tools = vec![
        tool::<workspace::ListRequestsArgs, workspace::RequestList>(
            "list_requests",
            "List the workspace's saved requests, filtered by collection folder, kind or text, 50 per page.",
            Effect::ReadOnly,
        ),
        tool::<workspace::GetRequestArgs, workspace::RequestFile>(
            "get_request",
            "Read one saved request file by its id or path. Secrets appear as {{name}} references, never as values.",
            Effect::ReadOnly,
        ),
        tool::<workspace::NoArgs, workspace::EnvironmentList>(
            "list_environments",
            "List the environments: names, which is active in this session, and variable and secret names.",
            Effect::ReadOnly,
        ),
        tool::<workspace::SelectEnvironmentArgs, workspace::Selected>(
            "select_environment",
            "Make an environment active for this session only; nothing is written to disk.",
            Effect::ReadOnly,
        ),
        tool::<execute::SendRequestArgs, execute::SendResult>(
            "send_request",
            "Send a saved request (path, optional vars) or an ad-hoc one (method, url, headers, body) through the safety policy. Returns the redacted exchange.",
            Effect::Sends,
        ),
        tool::<execute::RunCollectionArgs, execute::JobStarted>(
            "run_collection",
            "Run the requests under a folder or file like `rustman run`, in the background. Returns a job_id; get_report with it gives the run report.",
            Effect::Sends,
        ),
        tool::<explore::ImportArgs, explore::ImportResult>(
            "import_openapi",
            "Import an OpenAPI document (a workspace file or an allowed URL) as a request collection. dry_run (the default) only reports what would change.",
            Effect::Writes,
        ),
        tool::<explore::ExploreArgs, explore::PlanResult>(
            "explore_plan",
            "Plan the probes of an exploration against a target without sending anything. Review it before explore_run.",
            Effect::ReadOnly,
        ),
        tool::<explore::ExploreRunArgs, explore::ExplorationStarted>(
            "explore_run",
            "Start an exploration in the background (read-only unless rustman.toml allows more). Returns a job_id and exploration_id.",
            Effect::Sends,
        ),
        tool::<explore::StatusArgs, explore::ExplorationStatus>(
            "explore_status",
            "The phase, progress and findings by severity of an exploration, by job_id or exploration_id (default: the latest).",
            Effect::ReadOnly,
        ),
        tool::<explore::CancelArgs, explore::ExplorationStatus>(
            "explore_cancel",
            "Stop a running exploration job. It stays resumable with `rustman explore resume <id>`.",
            Effect::Writes,
        ),
        tool::<reports::GetReportArgs, reports::ReportResult>(
            "get_report",
            "A report by exploration id, run job id or JSON report file of the workspace (such as out/report.json): a summary (default), or rendered as json, markdown, junit, sarif or html.",
            Effect::ReadOnly,
        ),
        tool::<reports::ListFindingsArgs, reports::FindingList>(
            "list_findings",
            "The findings of an exploration (default: the latest), filtered by minimum severity and operation, 50 per page.",
            Effect::ReadOnly,
        ),
        tool::<history::SearchHistoryArgs, history::HistoryPage>(
            "search_history",
            "Search recorded exchanges by URL text, method, status range (500-599, 5xx, 404) and time range, newest first, 50 per page.",
            Effect::ReadOnly,
        ),
    ];
    tools.extend(protocol_tools());
    tools
}

/// Tools of optional protocol capabilities: `rpc_call` with
/// `jsonrpc-requests`, `realtime_listen` with `realtime-sessions`, and
/// `grpc_list`, `grpc_describe` and `grpc_call` with `grpc-requests` (the
/// `grpc` feature). `graphql_introspect` and
/// `graphql_query` (`graphql-requests`) and `list_webhook_captures`
/// (`webhook-inspector`) are not registered by this build yet.
fn protocol_tools() -> Vec<Tool> {
    let mut tools = vec![
        tool::<rpc::RpcCallArgs, rpc::RpcCallResult>(
            rpc::NAME,
            "Send one JSON-RPC call (endpoint, method, params) or a saved jsonrpc request through the safety policy. Only methods matching [jsonrpc] agent_read_only_methods are sent; subscriptions are refused.",
            Effect::Sends,
        ),
        tool::<realtime::RealtimeListenArgs, realtime::RealtimeListenResult>(
            realtime::NAME,
            "Open a WebSocket (ws:// or wss:// url) or SSE (http(s):// url) session, or a saved ws or sse request, through the safety policy, optionally send initial frames (a mutation: needs [agent] allow_mutations), and return the redacted messages received, after at most 100 messages or 60 s.",
            Effect::Sends,
        ),
    ];
    tools.extend(grpc_tools());
    tools
}

/// The gRPC tools, in a build with the `grpc` feature.
#[cfg(feature = "grpc")]
fn grpc_tools() -> Vec<Tool> {
    vec![
        tool::<grpc::GrpcListArgs, grpc::GrpcServices>(
            grpc::LIST_TOOL,
            "List the services and methods (with call types) of a gRPC target or saved grpc request, by reflection or its schema, through the safety policy's allowlist.",
            Effect::ReadOnly,
        ),
        tool::<grpc::GrpcDescribeArgs, grpc::GrpcDescription>(
            grpc::DESCRIBE_TOOL,
            "Describe a gRPC service, method (with a request message template) or message of a target or saved grpc request.",
            Effect::ReadOnly,
        ),
        tool::<grpc::GrpcCallArgs, grpc::GrpcCallResult>(
            grpc::CALL_TOOL,
            "Call a unary or server-streaming gRPC method (target, method, message, metadata) or a saved grpc request through the safety policy. Returns the redacted status, metadata and messages; streams stop at 100 messages or 30 s.",
            Effect::Sends,
        ),
    ]
}

#[cfg(not(feature = "grpc"))]
fn grpc_tools() -> Vec<Tool> {
    Vec::new()
}

/// Run a gRPC tool; `None` when `name` is not one of this build.
async fn call_grpc(name: &str, call: &Call) -> Option<Result<ToolOutput, ToolError>> {
    #[cfg(feature = "grpc")]
    {
        let result = match name {
            grpc::LIST_TOOL => grpc::grpc_list(call).await,
            grpc::DESCRIBE_TOOL => grpc::grpc_describe(call).await,
            grpc::CALL_TOOL => grpc::grpc_call(call).await,
            _ => return None,
        };
        Some(result)
    }
    #[cfg(not(feature = "grpc"))]
    {
        let _ = (name, call);
        None
    }
}

/// Whether `name` is a registered tool.
pub fn exists(name: &str) -> bool {
    catalog().iter().any(|t| t.name == name)
}

/// Refuse `{{secret:...}}` references anywhere in a tool's arguments
/// (secret-providers spec "Agents cannot write secret references"): an agent
/// may send saved requests that use references, but never choose which
/// secret is fetched or where it goes.
fn refuse_secret_references(arguments: &Value) -> Result<(), ToolError> {
    if !mentions_secret(arguments) {
        return Ok(());
    }
    Err(ToolError::new(
        ErrorCode::InvalidInput,
        "secret references ({{secret:...}}) are allowed only in saved request and environment files, not in tool arguments",
    ))
}

fn mentions_secret(value: &Value) -> bool {
    match value {
        Value::String(text) => mentions_reference(text),
        Value::Array(items) => items.iter().any(mentions_secret),
        Value::Object(map) => map
            .iter()
            .any(|(key, value)| mentions_reference(key) || mentions_secret(value)),
        _ => false,
    }
}

/// Run the tool `name`.
pub async fn call(name: &str, call: &Call) -> Result<ToolOutput, ToolError> {
    refuse_secret_references(&call.arguments)?;
    match name {
        "list_requests" => workspace::list_requests(call),
        "get_request" => workspace::get_request(call),
        "list_environments" => workspace::list_environments(call),
        "select_environment" => workspace::select_environment(call),
        "send_request" => execute::send_request(call).await,
        "run_collection" => execute::run_collection(call),
        "import_openapi" => explore::import_openapi(call).await,
        "explore_plan" => explore::explore_plan(call).await,
        "explore_run" => explore::explore_run(call).await,
        "explore_status" => explore::explore_status(call),
        "explore_cancel" => explore::explore_cancel(call).await,
        "get_report" => reports::get_report(call),
        "list_findings" => reports::list_findings(call),
        "search_history" => history::search_history(call),
        rpc::NAME => rpc::rpc_call(call).await,
        realtime::NAME => realtime::realtime_listen(call).await,
        _ => call_grpc(name, call).await.unwrap_or_else(|| {
            Err(ToolError::new(
                ErrorCode::NotFound,
                format!("no tool '{name}'"),
            ))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hints(name: &str) -> (bool, bool) {
        let tool = catalog().into_iter().find(|t| t.name == name).unwrap();
        let annotations = tool.annotations.unwrap();
        (
            annotations.read_only_hint.unwrap(),
            annotations.destructive_hint.unwrap(),
        )
    }

    const GRPC_TOOLS: [&str; 3] = ["grpc_list", "grpc_describe", "grpc_call"];

    #[cfg(feature = "grpc")]
    #[test]
    fn the_grpc_tools_are_registered_with_the_grpc_feature() {
        for name in GRPC_TOOLS {
            assert!(exists(name), "{name}");
        }
        assert_eq!(hints("grpc_list"), (true, false));
        assert_eq!(hints("grpc_describe"), (true, false));
        assert_eq!(hints("grpc_call"), (false, true));
    }

    /// `cargo test -p rustman-mcp --no-default-features --lib` (run in CI):
    /// a build without gRPC registers none of its tools.
    #[cfg(not(feature = "grpc"))]
    #[test]
    fn the_grpc_tools_are_absent_without_the_grpc_feature() {
        for name in GRPC_TOOLS {
            assert!(!exists(name), "{name}");
        }
        assert!(grpc_tools().is_empty());
    }

    /// stress-testing "Agents cannot start stress runs": no tool starts a
    /// stress run or sends malformed or wire-fault traffic. Agents read
    /// stress reports through the report tools instead.
    #[test]
    fn no_tool_starts_a_stress_run_or_sends_faults() {
        let words = [
            "stress",
            "breakpoint",
            "soak",
            "spike",
            "malformed",
            "wire fault",
            "wire-fault",
            "kill switch",
        ];
        for tool in catalog() {
            let name = tool.name.to_lowercase();
            let description = tool
                .description
                .as_deref()
                .unwrap_or_default()
                .to_lowercase();
            for word in words {
                assert!(
                    !name.contains(word) && !description.contains(word),
                    "{name} mentions {word}: {description}"
                );
            }
            let schema = serde_json::to_string(&tool.input_schema)
                .unwrap()
                .to_lowercase();
            for field in [
                "\"fuzz_ratio\"",
                "\"malformed\"",
                "\"wire\"",
                "\"allow_wire_faults\"",
                "\"slo\"",
            ] {
                assert!(!schema.contains(field), "{name} takes {field}");
            }
        }
    }

    /// load-testing "Agents cannot start a bench": no tool generates load.
    /// Agents read bench reports through the report tools instead.
    #[test]
    fn no_tool_starts_a_bench() {
        let words = [
            "bench",
            "load test",
            "load-test",
            "virtual user",
            "vus",
            "stress",
        ];
        for tool in catalog() {
            let name = tool.name.to_lowercase();
            let description = tool
                .description
                .as_deref()
                .unwrap_or_default()
                .to_lowercase();
            for word in words {
                assert!(
                    !name.contains(word) && !description.contains(word),
                    "{name} mentions {word}: {description}"
                );
            }
            let schema = serde_json::to_string(&tool.input_schema)
                .unwrap()
                .to_lowercase();
            for field in ["\"vus\"", "\"rate\"", "\"stages\"", "\"executor\""] {
                assert!(!schema.contains(field), "{name} takes {field}");
            }
        }
    }

    #[test]
    fn tools_that_send_requests_are_marked_destructive() {
        for name in ["send_request", "run_collection", "explore_run"] {
            assert_eq!(hints(name), (false, true), "{name}");
        }
        for name in ["import_openapi", "explore_cancel"] {
            assert_eq!(hints(name), (false, false), "{name}");
        }
        for name in [
            "list_requests",
            "explore_plan",
            "get_report",
            "search_history",
        ] {
            assert_eq!(hints(name), (true, false), "{name}");
        }
    }
}
