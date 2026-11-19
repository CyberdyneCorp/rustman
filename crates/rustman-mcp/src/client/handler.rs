//! The client side of a session as the server sees it: Rustman's identity
//! and capabilities in `initialize`, and the handler of server-initiated
//! requests (sampling and elicitation through the [`ApprovalBroker`],
//! `roots/list`) and notifications: a tool list change refetches the
//! tools (the stream itself is recorded by [`super::tap`]).

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use rmcp::model::{
    ClientCapabilities, ClientConfig, CreateMessageRequestParams, CreateMessageResult,
    ElicitRequestParams, ElicitResult, ElicitationAction, ErrorCode, ErrorData, Implementation,
    ListRootsResult, Root,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{ClientHandler, RoleClient};

use super::broker::{
    ApprovalBroker, ElicitationDecision, ElicitationRequest, SAMPLING_REJECTED,
    SAMPLING_REJECTED_CODE, SamplingDecision, SamplingRequest,
};
use super::line_limit::EndReason;
use super::notify::Notifications;
use super::request::Sender;
use super::shared::SharedRedactor;
use super::tools::{self, ToolCatalog};

/// The name sent as `clientInfo.name`.
pub const CLIENT_NAME: &str = "rustman";

/// What the handler shares with its session.
pub(crate) struct ClientState {
    pub server: String,
    pub notifications: Notifications,
    pub broker: Arc<dyn ApprovalBroker>,
    pub roots: RwLock<Vec<Root>>,
    pub tools: ToolCatalog,
    pub subscriptions: Mutex<BTreeSet<String>>,
    pub redactor: SharedRedactor,
    pub timeout: Duration,
    /// Why the client closed the connection itself (a stdio message over
    /// the size limit), once it did.
    pub ended: EndReason,
    /// Coalesces tool list refetches.
    pub refetch: Coalesce,
}

/// At most one background task at a time: a request made while one runs
/// makes it run once more, however many requests arrive meanwhile, so a
/// server flooding `notifications/tools/list_changed` cannot multiply them.
#[derive(Debug, Default)]
pub(crate) struct Coalesce(Mutex<CoalesceState>);

#[derive(Debug, Default)]
struct CoalesceState {
    running: bool,
    again: bool,
}

impl Coalesce {
    /// True when the caller must start the task; else the running one
    /// will run again.
    pub fn start(&self) -> bool {
        let mut state = self.lock();
        if state.running {
            state.again = true;
            return false;
        }
        state.running = true;
        true
    }

    /// Called by the task after each run: true to run again, false once
    /// it has stopped.
    pub fn again(&self) -> bool {
        let mut state = self.lock();
        state.running = std::mem::take(&mut state.again);
        state.running
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CoalesceState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl ClientState {
    pub fn subscribe(&self, uri: &str) {
        self.lock_subscriptions().insert(uri.to_owned());
    }

    pub fn unsubscribe(&self, uri: &str) {
        self.lock_subscriptions().remove(uri);
    }

    pub fn subscribed(&self, uri: &str) -> bool {
        self.lock_subscriptions().contains(uri)
    }

    fn lock_subscriptions(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
        self.subscriptions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn roots(&self) -> Vec<Root> {
        self.roots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn set_roots(&self, roots: Vec<Root>) {
        *self.roots.write().unwrap_or_else(PoisonError::into_inner) = roots;
    }

    pub fn sender(&self, peer: rmcp::Peer<RoleClient>) -> Sender {
        Sender {
            name: self.server.clone(),
            peer,
            notifications: self.notifications.clone(),
            timeout: self.timeout,
            redactor: self.redactor.get(),
            ended: self.ended.clone(),
        }
    }
}

/// Rustman's [`ClientHandler`]. Nothing a server asks is answered on the
/// user's behalf: sampling and elicitation wait for the broker.
#[derive(Clone)]
pub struct RustmanClient {
    state: Arc<ClientState>,
}

impl RustmanClient {
    pub(crate) fn new(state: Arc<ClientState>) -> Self {
        Self { state }
    }

    pub(crate) fn state(&self) -> Arc<ClientState> {
        self.state.clone()
    }

    fn sampling_request(&self, params: CreateMessageRequestParams) -> SamplingRequest {
        let label = format!(
            "{} message(s), max {} tokens",
            params.messages.len(),
            params.max_tokens
        );
        SamplingRequest {
            server: self.state.server.clone(),
            label,
            params,
        }
    }

    fn elicitation_request(&self, params: ElicitRequestParams) -> Option<ElicitationRequest> {
        let redactor = self.state.redactor.get();
        let redact = |text: &str| redactor.text(text).into_owned();
        let server = self.state.server.clone();
        match params {
            ElicitRequestParams::FormElicitationParams {
                message,
                requested_schema,
                ..
            } => Some(ElicitationRequest {
                server,
                message: redact(&message),
                schema: serde_json::to_value(requested_schema).ok(),
                url: None,
            }),
            ElicitRequestParams::UrlElicitationParams { message, url, .. } => {
                Some(ElicitationRequest {
                    server,
                    message: redact(&message),
                    schema: None,
                    url: Some(url),
                })
            }
            _ => None,
        }
    }
}

fn elicit_result(decision: ElicitationDecision) -> ElicitResult {
    match decision {
        ElicitationDecision::Accept(content) if content.content().is_null() => {
            ElicitResult::new(ElicitationAction::Accept)
        }
        ElicitationDecision::Accept(content) => {
            ElicitResult::new(ElicitationAction::Accept).with_content(content.content().clone())
        }
        ElicitationDecision::Decline => ElicitResult::new(ElicitationAction::Decline),
        ElicitationDecision::Cancel => ElicitResult::new(ElicitationAction::Cancel),
    }
}

impl ClientHandler for RustmanClient {
    fn get_info(&self) -> ClientConfig {
        client_info()
    }

    async fn create_message(
        &self,
        params: CreateMessageRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<CreateMessageResult, ErrorData> {
        let request = self.sampling_request(params);
        match self.state.broker.sampling(request).await {
            SamplingDecision::Respond(result) => Ok(*result),
            SamplingDecision::Reject => Err(ErrorData::new(
                ErrorCode(SAMPLING_REJECTED_CODE),
                SAMPLING_REJECTED,
                None,
            )),
        }
    }

    async fn create_elicitation(
        &self,
        params: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, ErrorData> {
        let Some(request) = self.elicitation_request(params) else {
            return Ok(ElicitResult::new(ElicitationAction::Decline));
        };
        Ok(elicit_result(self.state.broker.elicitation(request).await))
    }

    async fn list_roots(
        &self,
        _context: RequestContext<RoleClient>,
    ) -> Result<ListRootsResult, ErrorData> {
        Ok(ListRootsResult::new(self.state.roots()))
    }

    /// Refetch a toolset that was listed, in the background: the handler
    /// must keep reading messages to receive the answer.
    async fn on_tool_list_changed(&self, context: NotificationContext<RoleClient>) {
        if self.state.tools.get().is_none() || !self.state.refetch.start() {
            return;
        }
        let state = self.state.clone();
        tokio::spawn(async move {
            let sender = state.sender(context.peer);
            loop {
                let _ = tools::list(&sender, &state.tools, &Default::default()).await;
                if !state.refetch.again() {
                    break;
                }
            }
        });
    }
}

/// `initialize` params: the latest handshake version Rustman supports,
/// `rustman/<version>`, and the `roots`, `sampling` and `elicitation`
/// capabilities.
pub fn client_info() -> ClientConfig {
    let capabilities = ClientCapabilities::builder()
        .enable_roots()
        .enable_roots_list_changed()
        .enable_sampling()
        .enable_elicitation()
        .build();
    ClientConfig::new(
        capabilities,
        Implementation::new(CLIENT_NAME, rustman_core::VERSION),
    )
    .with_protocol_version(super::version::LATEST)
}

#[cfg(test)]
mod tests {
    use super::Coalesce;

    #[test]
    fn a_flood_of_requests_runs_the_task_once_more() {
        let refetch = Coalesce::default();
        assert!(refetch.start(), "the first request starts the task");
        for _ in 0..1000 {
            assert!(!refetch.start(), "no second task while one runs");
        }
        assert!(refetch.again(), "the flood makes it run once more");
        assert!(!refetch.again(), "then it stops");
        assert!(refetch.start(), "a later request starts it again");
        assert!(!refetch.again());
    }
}
