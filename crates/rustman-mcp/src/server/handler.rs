//! The MCP handler: what the server advertises at `initialize`, one
//! [`AgentContext`] per session, and the tools, resources and prompts.

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CancelledNotificationParam, GetPromptRequestParams,
    GetPromptResponse, Implementation, ListPromptsResult, ListResourceTemplatesResult,
    ListResourcesResult, ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams,
    ReadResourceResponse, ReadResourceResult, ServerCapabilities, ServerConfig,
};
use rmcp::service::{NotificationContext, RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler};
use serde_json::Value;

use super::context::{AgentContext, Caller};
use super::error::{ErrorCode, ToolError};
use super::jobs::CANCEL_WAIT;
use super::limits::{paginate, snapshot};
use super::{prompts, resources, tools};

/// The name the server reports at `initialize`.
pub const SERVER_NAME: &str = "rustman";

const INSTRUCTIONS: &str = "Rustman exposes this API workspace: its saved requests, environments, \
explorations and reports. Requests go through the workspace's safety policy: read-only unless \
rustman.toml [agent] allows more, and mutations need the human's confirmation. Secret values are \
never returned; refer to secrets by name.";

/// The Rustman MCP server for one session.
#[derive(Clone, Debug)]
pub struct RustmanServer {
    ctx: Arc<AgentContext>,
}

impl RustmanServer {
    pub fn new(ctx: Arc<AgentContext>) -> Self {
        Self { ctx }
    }

    pub fn context(&self) -> &Arc<AgentContext> {
        &self.ctx
    }
}

/// The capabilities every session advertises: tools, resources, prompts
/// and logging.
pub fn capabilities() -> ServerCapabilities {
    let mut capabilities = ServerCapabilities::builder()
        .enable_tools()
        .enable_resources()
        .enable_prompts()
        .build();
    capabilities.logging = Some(Default::default());
    capabilities
}

impl ServerHandler for RustmanServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(capabilities())
            .with_server_info(Implementation::new(SERVER_NAME, rustman_core::VERSION))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools::catalog()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = request.name.to_string();
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        let client = context
            .peer
            .peer_info()
            .map(|info| info.client_info.name.clone())
            .unwrap_or_default();
        let caller = Caller {
            client: &client,
            tool: &name,
            arguments: &arguments,
        };
        let ctx = &self.ctx;
        if !tools::exists(&name) {
            // Still one audit line, though the call is a protocol error.
            let message = format!("no tool '{name}'");
            let unknown = ToolError::new(ErrorCode::NotFound, message.clone());
            ctx.audit_refused(&caller, &unknown)
                .map_err(|e| ErrorData::internal_error(e.message, None))?;
            return Err(ErrorData::invalid_params(
                ctx.redactor().text(&message),
                None,
            ));
        }
        let tool = name.clone();
        let result = ctx
            .audited(&caller, |target| {
                let call = tools::Call {
                    ctx: ctx.clone(),
                    peer: context.peer.clone(),
                    target,
                    arguments: arguments.clone(),
                    request: context.id.clone(),
                    progress: context.meta.get_progress_token(),
                    cancelled: context.ct.clone(),
                };
                async move { tools::call(&tool, &call).await }
            })
            .await;
        Ok(result.into())
    }

    /// A cancelled `tools/call` that started a background job stops the
    /// job, as `explore_cancel` does.
    async fn on_cancelled(
        &self,
        notification: CancelledNotificationParam,
        _: NotificationContext<RoleServer>,
    ) {
        let job = notification
            .request_id
            .as_ref()
            .and_then(|id| self.ctx.jobs().for_request(id));
        if let Some(job) = job {
            tracing::info!("{} cancelled by the client", job.id);
            tokio::spawn(async move { job.cancel(CANCEL_WAIT).await });
        }
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let all = resources::list(&self.ctx);
        let keys: Vec<String> = all.iter().map(|r| r.uri.clone()).collect();
        let cursor = request.and_then(|r| r.cursor);
        let page = paginate(all, snapshot(&keys), cursor.as_deref(), None)
            .map_err(|e| ErrorData::invalid_params(e.message, None))?;
        let mut result = ListResourcesResult::with_all_items(page.items);
        result.next_cursor = page.next_cursor;
        Ok(result)
    }

    async fn list_resource_templates(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult::with_all_items(
            resources::templates(),
        ))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let contents = resources::read(&self.ctx, &request.uri).map_err(|mut err| {
            err.message = self.ctx.redactor().text(&err.message).into();
            err
        })?;
        Ok(ReadResourceResult::new(vec![contents]).into())
    }

    async fn list_prompts(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(prompts::list()))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        let redactor = self.ctx.redactor();
        let result = prompts::get(&request.name, request.arguments.as_ref(), &redactor)?;
        Ok(result.into())
    }
}
