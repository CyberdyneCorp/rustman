//! `rustman mcp serve`: Rustman as an MCP server for AI agents
//! (agent-mcp-interface). Transports are stdio (default) and Streamable
//! HTTP; every session gets its own [`AgentContext`], where the guardrails
//! live: agent permissions, confirmation, redaction, output limits and the
//! audit log.

pub mod audit;
pub mod confirm;
pub mod context;
pub mod error;
pub mod handler;
pub mod http;
pub mod jobs;
pub mod limits;
pub mod plugin_net;
pub mod policy;
pub mod prompts;
pub mod redact;
pub mod resources;
pub mod token;
pub mod tools;

use std::sync::Arc;

use rmcp::ServiceExt;
use rustman_core::secrets::provider::SecretProviders;
use rustman_core::{Environment, SecretStore, Workspace};

pub use context::{AgentContext, Caller, Target, ToolOutput};
pub use error::{ErrorCode, ToolError};
pub use handler::{RustmanServer, SERVER_NAME};
pub use jobs::JobPool;

/// What every new session starts from: the workspace, its secret store and
/// the environment active when the server started. Every session's
/// background jobs share one [`JobPool`], so `max_jobs` holds for the server.
#[derive(Clone)]
pub struct Sessions {
    workspace: Arc<Workspace>,
    secrets: Arc<SecretStore>,
    environment: Option<Environment>,
    jobs: Arc<JobPool>,
    secret_providers: Option<Arc<SecretProviders>>,
    plugins: Option<Arc<dyn rustman_core::plugins::Extensions>>,
}

impl std::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sessions")
            .field("root", &self.workspace.root)
            .finish_non_exhaustive()
    }
}

impl Sessions {
    pub fn new(
        workspace: Workspace,
        secrets: SecretStore,
        environment: Option<Environment>,
    ) -> Self {
        Self {
            workspace: Arc::new(workspace),
            secrets: Arc::new(secrets),
            environment,
            jobs: Arc::default(),
            secret_providers: None,
            plugins: None,
        }
    }

    /// Resolve `{{secret:...}}` references of agent-sent requests through
    /// `providers`, shared by every session (and their cache).
    #[must_use]
    pub fn with_secret_providers(mut self, providers: Arc<SecretProviders>) -> Self {
        self.secret_providers = Some(providers);
        self
    }

    /// Give every session the workspace's plugins. The caller loads them
    /// only when their set is already trusted (plugins spec, "Agents
    /// cannot manage plugins").
    #[must_use]
    pub fn with_plugins(
        mut self,
        plugins: Option<Arc<dyn rustman_core::plugins::Extensions>>,
    ) -> Self {
        self.plugins = plugins;
        self
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// A new session, in the starting environment.
    pub fn open(&self) -> RustmanServer {
        let ctx = AgentContext::in_pool(
            self.workspace.clone(),
            self.secrets.clone(),
            self.environment.clone(),
            self.jobs.clone(),
        )
        .with_secret_providers(self.secret_providers.clone())
        .with_plugins(self.plugins.clone());
        RustmanServer::new(Arc::new(ctx))
    }
}

/// Serve one session over stdin and stdout until the client disconnects.
/// Nothing else may write to stdout meanwhile.
pub async fn serve_stdio(sessions: &Sessions) -> Result<(), String> {
    let service = sessions
        .open()
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| format!("MCP handshake failed: {e}"))?;
    service
        .waiting()
        .await
        .map(|_| ())
        .map_err(|e| format!("MCP session failed: {e}"))
}
