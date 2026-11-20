//! `AgentContext` (design D3): the state of one MCP session. It holds the
//! session's active environment (a copy; `select_environment` never writes
//! to disk), the [`GovernedSender`] built from `[agent]`, the redactor of
//! everything sent to the client, the bodies kept for `rustman://bodies/`
//! and the audit log. Every guardrail runs here, so tools only wire.

use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use rmcp::model::{CallToolResult, ContentBlock};
use rustman_core::agent::AgentSettings;
use rustman_core::model::Request;
use rustman_core::run::now_rfc3339;
use rustman_core::safety::MethodClass;
use rustman_core::secrets::provider::SecretProviders;
use rustman_core::vars::ResolvedRequest;
use rustman_core::{Environment, Redactor, SecretStore, Workspace};
use rustman_http::runner::Runner;
use rustman_http::{Exchange, GovernedError, GovernedSender, SendOptions};
use serde_json::Value;

use super::audit::{AuditEntry, AuditLog, Outcome};
use super::confirm::{Confirm, Confirmation, question};
use super::error::{ErrorCode, ToolError};
use super::jobs::{JobPool, Jobs};
use super::limits::{BodyStore, BodyView, check_size};
use super::policy::{check_method, rpc_sender, session_sender};
use super::redact::OutputRedactor;

/// What a tool returns on success: its structured result and a one-line
/// summary for `content`.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutput {
    pub structured: Value,
    pub summary: String,
}

impl ToolOutput {
    pub fn new(structured: Value, summary: impl Into<String>) -> Self {
        Self {
            structured,
            summary: summary.into(),
        }
    }
}

/// The outbound request of a call, for its audit line.
#[derive(Debug, Default)]
pub struct Target(Mutex<Option<(String, String)>>);

impl Target {
    pub fn set(&self, method: &str, url: &str) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((method.to_owned(), url.to_owned()));
    }

    fn get(&self) -> Option<(String, String)> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Who made a call.
#[derive(Clone, Debug)]
pub struct Caller<'a> {
    pub client: &'a str,
    pub tool: &'a str,
    pub arguments: &'a Value,
}

/// The background job a request belongs to, for its own audit line.
#[derive(Clone, Debug)]
pub struct JobCaller {
    pub client: String,
    /// The tool that started the job.
    pub tool: &'static str,
    pub job_id: String,
}

struct SessionState {
    environment: Option<Environment>,
    sender: Result<Arc<GovernedSender>, String>,
    /// The sender of `rpc_call` (POSTs allowed; methods are gated by
    /// `agent_read_only_methods`).
    rpc_sender: Result<Arc<GovernedSender>, String>,
    redactor: OutputRedactor,
}

/// The state of one MCP session.
pub struct AgentContext {
    workspace: Arc<Workspace>,
    secrets: Arc<SecretStore>,
    session_id: String,
    state: RwLock<SessionState>,
    bodies: Mutex<BodyStore>,
    jobs: Jobs,
    audit: AuditLog,
    /// Where `{{secret:...}}` references of sent requests are fetched;
    /// values are resolved inside the engine and never returned.
    secret_providers: Option<Arc<SecretProviders>>,
    /// The workspace plugins, loaded only when their set is already
    /// trusted on this machine; agents can never trust or grant them.
    plugins: Option<Arc<dyn rustman_core::plugins::Extensions>>,
}

impl std::fmt::Debug for AgentContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentContext")
            .field("root", &self.workspace.root)
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

impl AgentContext {
    /// A session over `workspace` starting in `environment`, alone in its
    /// job pool.
    pub fn new(
        workspace: Arc<Workspace>,
        secrets: Arc<SecretStore>,
        environment: Option<Environment>,
    ) -> Self {
        Self::in_pool(workspace, secrets, environment, Arc::default())
    }

    /// A session whose background jobs share `pool` (and its `max_jobs`)
    /// with the other sessions of the server.
    pub fn in_pool(
        workspace: Arc<Workspace>,
        secrets: Arc<SecretStore>,
        environment: Option<Environment>,
        pool: Arc<JobPool>,
    ) -> Self {
        let audit = AuditLog::for_workspace(&workspace.root);
        let state = session_state(&workspace, &secrets, environment);
        Self {
            workspace,
            secrets,
            session_id: new_session_id(),
            state: RwLock::new(state),
            bodies: Mutex::new(BodyStore::default()),
            jobs: Jobs::in_pool(pool),
            audit,
            secret_providers: None,
            plugins: None,
        }
    }

    /// Resolve the secret references of sent requests through `providers`.
    #[must_use]
    pub fn with_secret_providers(mut self, providers: Option<Arc<SecretProviders>>) -> Self {
        self.secret_providers = providers;
        self
    }

    /// `runner`, fetching secret references through the session's
    /// providers when it has any.
    pub fn with_providers<'a>(&self, runner: Runner<'a>) -> Runner<'a> {
        match &self.secret_providers {
            Some(providers) => runner.with_secret_providers(providers.clone()),
            None => runner,
        }
    }

    /// Serve the tools with the workspace's trusted plugins.
    #[must_use]
    pub fn with_plugins(
        mut self,
        plugins: Option<Arc<dyn rustman_core::plugins::Extensions>>,
    ) -> Self {
        self.plugins = plugins;
        self
    }

    /// The trusted plugins the session's requests may use.
    pub fn plugins(&self) -> Option<Arc<dyn rustman_core::plugins::Extensions>> {
        self.plugins.clone()
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    pub fn secrets(&self) -> &SecretStore {
        &self.secrets
    }

    pub fn agent_settings(&self) -> &AgentSettings {
        &self.workspace.settings.agent
    }

    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    /// The session's background jobs.
    pub fn jobs(&self) -> &Jobs {
        &self.jobs
    }

    /// The session's active environment.
    pub fn environment(&self) -> Option<Environment> {
        self.read().environment.clone()
    }

    /// Make `name` the session's active environment. Nothing is written to
    /// disk; other sessions and the CLI keep theirs.
    pub fn select_environment(&self, name: &str) -> Result<(), ToolError> {
        let environment = self
            .workspace
            .environments
            .iter()
            .find(|e| e.name == name)
            .cloned()
            .ok_or_else(|| {
                ToolError::new(ErrorCode::NotFound, format!("no environment '{name}'"))
            })?;
        let mut state = session_state(&self.workspace, &self.secrets, Some(environment));
        let mut current = self.state.write().unwrap_or_else(PoisonError::into_inner);
        // Secrets seen earlier in the session stay redacted.
        state.redactor.merge(current.redactor.redactor());
        *current = state;
        Ok(())
    }

    /// The redactor of everything sent to the client in this session.
    pub fn redactor(&self) -> OutputRedactor {
        self.read().redactor.clone()
    }

    /// The session's sender, shared by every request of the session so the
    /// rate limit and concurrency apply across them.
    pub fn sender(&self) -> Result<Arc<GovernedSender>, ToolError> {
        self.read().sender.clone().map_err(ToolError::blocked)
    }

    /// The sender of the `rpc_call` tool, with the session's allowlist.
    pub fn rpc_sender(&self) -> Result<Arc<GovernedSender>, ToolError> {
        self.read().rpc_sender.clone().map_err(ToolError::blocked)
    }

    /// Redact what `redactor` knows for the rest of the session: a secret
    /// a request used stays hidden in every later result, log and audit
    /// line.
    pub fn learn(&self, redactor: &Redactor) {
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        state.redactor.merge(redactor);
    }

    /// Redact the literal credentials of `request` (an ad-hoc request's
    /// headers) for the rest of the session.
    pub fn learn_request_literals(&self, request: &Request) {
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        state.redactor.add_request_literals(request);
    }

    pub fn bodies(&self) -> MutexGuard<'_, BodyStore> {
        self.bodies.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, SessionState> {
        self.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether `method url` may be sent: the agent permissions, the
    /// allowlist and, for mutations, the human's confirmation.
    pub async fn authorize(
        &self,
        method: &str,
        url: &str,
        confirm: &impl Confirm,
    ) -> Result<(), ToolError> {
        check_method(self.agent_settings().permission(), method)?;
        let sender = self.sender()?;
        sender
            .policy()
            .check_origin(url)
            .map_err(|blocked| ToolError::blocked(blocked.reason()))?;
        if MethodClass::of(method) == MethodClass::ReadOnly {
            return Ok(());
        }
        let shown = self.redactor().text(url);
        let environment = self.environment().map(|e| e.name);
        let message = question(method, &shown, environment.as_deref());
        let what = format!("{method} {shown}");
        self.ask(&message, &what, confirm).await
    }

    /// Ask the human `message` about `what` (redacted), failing closed: only
    /// a yes, or a client that cannot ask under `require_confirmation =
    /// false`, lets the call go on.
    pub async fn ask(
        &self,
        message: &str,
        what: &str,
        confirm: &impl Confirm,
    ) -> Result<(), ToolError> {
        self.confirmed(confirm.confirm(message).await, what)
    }

    fn confirmed(&self, answer: Confirmation, what: &str) -> Result<(), ToolError> {
        match answer {
            Confirmation::Confirmed => Ok(()),
            Confirmation::NotSupported if !self.agent_settings().require_confirmation => Ok(()),
            Confirmation::Declined => Err(ToolError::new(
                ErrorCode::Declined,
                format!("the human declined {what}"),
            )),
            Confirmation::NotSupported => Err(ToolError::new(
                ErrorCode::ConfirmationUnavailable,
                "this client cannot ask the human to confirm (MCP elicitation); mutating calls need it unless [agent] require_confirmation = false",
            )),
            Confirmation::Failed => Err(ToolError::new(
                ErrorCode::ConfirmationUnavailable,
                "the human could not be asked to confirm",
            )),
        }
    }

    /// Send `request` through every guardrail: [`AgentContext::authorize`],
    /// then the session's governed sender, which checks the policy again on
    /// the final URL and on every redirect hop.
    pub async fn send(
        &self,
        request: &ResolvedRequest,
        options: &SendOptions,
        confirm: &impl Confirm,
        target: &Target,
    ) -> Result<Exchange, ToolError> {
        let url = &request.url.text;
        target.set(&request.method, url);
        self.learn(&Redactor::for_request(request));
        self.authorize(&request.method, url, confirm).await?;
        let sender = self.sender()?;
        let exchange = sender
            .send(request, options)
            .await
            .map_err(|err| match err {
                GovernedError::Blocked { reason, .. } => ToolError::blocked(reason.reason()),
                other => ToolError::internal(other.to_string()),
            })?;
        self.learn(&exchange.credentials);
        Ok(exchange)
    }

    /// The response body of `exchange` as a result shows it: redacted with
    /// the session's secrets (so they keep their names), then the
    /// exchange's own credentials, and truncated with a `body_ref`.
    pub fn body_view(&self, exchange: &Exchange, request: &ResolvedRequest) -> BodyView {
        let session = self.redactor();
        let body = session.redactor().bytes(&exchange.body).into_owned();
        let body = exchange.redactor(request).bytes(&body).into_owned();
        self.bodies()
            .view(body.into(), exchange.size_bytes, exchange.binary)
    }

    /// Redact `output` (second pass) and check it against
    /// `max_output_bytes`.
    pub fn finish(&self, output: ToolOutput) -> Result<ToolOutput, ToolError> {
        let redactor = self.redactor();
        let structured = redactor.json(&output.structured)?;
        check_size(&structured, self.agent_settings().max_output_bytes)?;
        Ok(ToolOutput {
            structured,
            summary: redactor.text(&output.summary),
        })
    }

    /// Run one tool call under the audit log: refused unless the log can be
    /// written, its result redacted and capped, and one line appended for
    /// it. A line that cannot be appended fails the call.
    pub async fn audited<F, Fut>(&self, caller: &Caller<'_>, run: F) -> CallToolResult
    where
        F: FnOnce(Arc<Target>) -> Fut,
        Fut: Future<Output = Result<ToolOutput, ToolError>>,
    {
        if let Err(err) = self.audit.open() {
            return err.into_result();
        }
        let target = Arc::new(Target::default());
        let result = run(target.clone())
            .await
            .and_then(|output| self.finish(output));
        let result =
            result.map_err(|err| ToolError::new(err.code, self.redactor().text(&err.message)));
        let entry = self.audit_entry(caller, result.as_ref().err(), target.get());
        if let Err(err) = self.audit.append(&entry) {
            return err.into_result();
        }
        match result {
            Ok(output) => success(output),
            Err(err) => err.into_result(),
        }
    }

    /// Append the audit line of a call refused before any tool ran, such as
    /// one naming no registered tool.
    pub fn audit_refused(&self, caller: &Caller<'_>, error: &ToolError) -> Result<(), ToolError> {
        self.audit
            .append(&self.audit_entry(caller, Some(error), None))
    }

    /// Refused unless the audit log can be written: checked before a
    /// background job sends a request, so a request that could not be
    /// audited is not sent.
    pub fn audit_ready(&self) -> Result<(), ToolError> {
        self.audit.open().map(drop)
    }

    /// Append the audit line of one request a background job sent (or had
    /// refused): the tool that started the job, its id, the method, the URL
    /// and the outcome.
    pub fn audit_job_request(
        &self,
        job: &JobCaller,
        method: &str,
        url: &str,
        error: Option<&ToolError>,
    ) -> Result<(), ToolError> {
        let arguments = serde_json::json!({ "job_id": job.job_id });
        let caller = Caller {
            client: &job.client,
            tool: job.tool,
            arguments: &arguments,
        };
        let target = Some((method.to_owned(), url.to_owned()));
        self.audit.append(&self.audit_entry(&caller, error, target))
    }

    fn audit_entry(
        &self,
        caller: &Caller<'_>,
        error: Option<&ToolError>,
        target: Option<(String, String)>,
    ) -> AuditEntry {
        let redactor = self.redactor();
        let arguments = redactor
            .json(caller.arguments)
            .unwrap_or_else(|_| Value::String("«unredactable arguments»".into()));
        let (outcome, reason) = match error {
            None => (Outcome::Ok, None),
            Some(err) => (
                Outcome::of_error(err.code),
                Some(redactor.text(&err.message)),
            ),
        };
        let (method, url) =
            target.map_or((None, None), |(m, u)| (Some(m), Some(redactor.text(&u))));
        AuditEntry {
            timestamp: now_rfc3339(),
            session: self.session_id.clone(),
            client: redactor.text(caller.client),
            tool: redactor.text(caller.tool),
            arguments,
            outcome,
            reason,
            method,
            url,
        }
    }
}

fn success(output: ToolOutput) -> CallToolResult {
    let mut result = CallToolResult::structured(output.structured);
    result.content = vec![ContentBlock::text(output.summary)];
    result
}

/// The state of a session in `environment`: its sender and the redactor
/// holding the environment's secret values.
fn session_state(
    workspace: &Workspace,
    secrets: &SecretStore,
    environment: Option<Environment>,
) -> SessionState {
    let settings = &workspace.settings;
    let sender =
        session_sender(&settings.safety, &settings.agent, environment.as_ref()).map(Arc::new);
    let rpc_sender = sender
        .clone()
        .and_then(|session| {
            rpc_sender(
                &session,
                &settings.safety,
                &settings.agent,
                environment.as_ref(),
            )
        })
        .map(Arc::new);
    let mut redactor = OutputRedactor::new();
    redactor.add_workspace_literals(workspace);
    if let Some(env) = &environment {
        for name in &env.secrets {
            match secrets.get(&env.name, name) {
                Ok(Some(secret)) => redactor.add_secret(&secret),
                Ok(None) => {}
                Err(err) => tracing::warn!("secret '{name}' of '{}': {err}", env.name),
            }
        }
    }
    SessionState {
        environment,
        sender,
        rpc_sender,
        redactor,
    }
}

/// A random session id: 16 bytes, hex.
fn new_session_id() -> String {
    let mut bytes = [0u8; 16];
    super::token::fill_random(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
