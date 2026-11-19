//! The tool catalog (spec "Structured tools with schemas"): every tool's
//! name, description, input and output JSON Schemas, and the dispatch of a
//! call to the engine. Each tool is a thin adapter over a library call; the
//! guardrails (audit, redaction, limits) wrap every call in
//! [`AgentContext::audited`](super::AgentContext::audited).

mod execute;
mod explore;
mod history;
pub mod reference;
pub(crate) mod reports;
pub mod rpc;
pub(crate) mod workspace;

use std::sync::Arc;

use rmcp::model::{JsonObject, ProgressToken, RequestId, Tool, ToolAnnotations};
use rmcp::service::{Peer, RoleServer};
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
            "A report by exploration id or run job id: a summary (default), or rendered as json, markdown, junit, sarif or html.",
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
/// `jsonrpc-requests`. `graphql_introspect` and `graphql_query`
/// (`graphql-requests`) and `list_webhook_captures` (`webhook-inspector`)
/// are not registered by this build yet.
fn protocol_tools() -> Vec<Tool> {
    vec![tool::<rpc::RpcCallArgs, rpc::RpcCallResult>(
        rpc::NAME,
        "Send one JSON-RPC call (endpoint, method, params) or a saved jsonrpc request through the safety policy. Only methods matching [jsonrpc] agent_read_only_methods are sent; subscriptions are refused.",
        Effect::Sends,
    )]
}

/// Whether `name` is a registered tool.
pub fn exists(name: &str) -> bool {
    catalog().iter().any(|t| t.name == name)
}

/// Run the tool `name`.
pub async fn call(name: &str, call: &Call) -> Result<ToolOutput, ToolError> {
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
        _ => Err(ToolError::new(
            ErrorCode::NotFound,
            format!("no tool '{name}'"),
        )),
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
