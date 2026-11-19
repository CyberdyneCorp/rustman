//! Server-initiated requests that need a human (design D3): sampling and
//! elicitation go to an [`ApprovalBroker`]. The two implementations are
//! [`InteractiveBroker`], which queues each request for a front-end dialog
//! and waits for the user's answer, and [`HeadlessBroker`], which rejects
//! sampling (unless a canned `--sampling-response` is given) and declines
//! elicitation, warning each time. There is no auto-approving broker, and an
//! elicitation can only be accepted with content that passed validation.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rmcp::model::{CreateMessageRequestParams, CreateMessageResult};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use super::error::ClientError;
use super::output::printable;
use super::schema::{self, Field, Violation};

/// The JSON-RPC error a rejected sampling request gets.
pub const SAMPLING_REJECTED_CODE: i32 = -1;
pub const SAMPLING_REJECTED: &str = "User rejected sampling request";

/// A `sampling/createMessage` request, shown in full to the user.
#[derive(Clone, Debug)]
pub struct SamplingRequest {
    pub server: String,
    /// A one-line description for warnings and badges, redacted.
    pub label: String,
    /// The messages, system prompt, model preferences and max tokens.
    pub params: CreateMessageRequestParams,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SamplingDecision {
    Reject,
    /// The response the user typed or approved.
    Respond(Box<CreateMessageResult>),
}

impl SamplingDecision {
    /// The response the user typed, as the assistant's text, from model
    /// `human`.
    pub fn typed(text: &str) -> Self {
        let message = rmcp::model::SamplingMessage::new(rmcp::model::Role::Assistant, text);
        Self::Respond(Box::new(CreateMessageResult::new(message, "human".into())))
    }
}

/// An `elicitation/create` request.
#[derive(Clone, Debug)]
pub struct ElicitationRequest {
    pub server: String,
    /// The server's message, redacted.
    pub message: String,
    /// `requestedSchema` (form mode); `None` for URL elicitation.
    pub schema: Option<Value>,
    /// The URL to open (URL mode).
    pub url: Option<String>,
}

/// Content that passed the requested schema.
#[derive(Clone, Debug, PartialEq)]
pub struct Accepted(Value);

impl Accepted {
    pub fn content(&self) -> &Value {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ElicitationDecision {
    Accept(Accepted),
    Decline,
    Cancel,
}

impl ElicitationRequest {
    /// The form generated from the requested schema.
    pub fn form(&self) -> Option<Field> {
        self.schema.as_ref().map(schema::form)
    }

    /// Accept with `content`, once it satisfies the requested schema.
    pub fn accept(&self, content: Value) -> Result<ElicitationDecision, Vec<Violation>> {
        if let Some(requested) = &self.schema {
            let violations =
                schema::validate(requested, &content).map_err(|e| vec![Violation::new("/", e)])?;
            if !violations.is_empty() {
                return Err(violations);
            }
        }
        Ok(ElicitationDecision::Accept(Accepted(content)))
    }

    /// Accept a URL elicitation once the user opened it.
    pub fn accept_url(&self) -> Option<ElicitationDecision> {
        self.url
            .as_ref()
            .map(|_| ElicitationDecision::Accept(Accepted(Value::Null)))
    }
}

/// Where sampling and elicitation requests go.
pub trait ApprovalBroker: Send + Sync + 'static {
    fn sampling(&self, request: SamplingRequest) -> BoxFuture<'static, SamplingDecision>;
    fn elicitation(&self, request: ElicitationRequest) -> BoxFuture<'static, ElicitationDecision>;
}

type Warn = Arc<dyn Fn(&str) + Send + Sync>;

/// The broker of commands without a user to ask: sampling is rejected
/// (or answered from a `--sampling-response` file), elicitation declined;
/// each one is reported through `warn`.
#[derive(Clone)]
pub struct HeadlessBroker {
    sampling_response: Option<CreateMessageResult>,
    warn: Warn,
}

impl HeadlessBroker {
    pub fn new(warn: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self {
            sampling_response: None,
            warn: Arc::new(warn),
        }
    }

    /// Answer every sampling request with `response`.
    pub fn with_sampling_response(mut self, response: CreateMessageResult) -> Self {
        self.sampling_response = Some(response);
        self
    }
}

impl Default for HeadlessBroker {
    fn default() -> Self {
        Self::new(|_| {})
    }
}

/// Read a canned `CreateMessageResult` (JSON) for `--sampling-response`.
pub fn load_sampling_response(path: &Path) -> Result<CreateMessageResult, ClientError> {
    let shown = path.display();
    let text = fs::read_to_string(path)
        .map_err(|e| ClientError::Invalid(format!("cannot read {shown}: {e}")))?;
    serde_json::from_str(&text).map_err(|e| {
        ClientError::Invalid(format!(
            "{shown} is not a sampling response (CreateMessageResult): {e}"
        ))
    })
}

impl ApprovalBroker for HeadlessBroker {
    fn sampling(&self, request: SamplingRequest) -> BoxFuture<'static, SamplingDecision> {
        let decision = match &self.sampling_response {
            Some(response) => SamplingDecision::Respond(Box::new(response.clone())),
            None => {
                (self.warn)(&printable(&format!(
                    "mcp: rejected sampling request from {} ({}); pass --sampling-response to answer it",
                    request.server, request.label
                )));
                SamplingDecision::Reject
            }
        };
        std::future::ready(decision).boxed()
    }

    fn elicitation(&self, request: ElicitationRequest) -> BoxFuture<'static, ElicitationDecision> {
        (self.warn)(&printable(&format!(
            "mcp: declined elicitation request from {}: {}",
            request.server, request.message
        )));
        std::future::ready(ElicitationDecision::Decline).boxed()
    }
}

/// A request waiting for the user, as a front-end receives it.
#[derive(Debug)]
pub enum Approval {
    Sampling {
        request: Box<SamplingRequest>,
        reply: Reply<SamplingDecision>,
    },
    Elicitation {
        request: ElicitationRequest,
        reply: Reply<ElicitationDecision>,
    },
}

/// The answer channel of one approval; dropping it unanswered rejects a
/// sampling request and cancels an elicitation.
#[derive(Debug)]
pub struct Reply<T> {
    sender: oneshot::Sender<T>,
    _pending: Pending,
}

impl<T> Reply<T> {
    pub fn send(self, decision: T) {
        let _ = self.sender.send(decision);
    }
}

/// Counts an approval as pending until its reply is sent or dropped.
#[derive(Debug)]
struct Pending(Arc<AtomicUsize>);

impl Pending {
    fn new(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count.clone())
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The broker of the TUI: every request is queued as an [`Approval`] and
/// nothing is answered until the user acts.
#[derive(Clone, Debug)]
pub struct InteractiveBroker {
    queue: mpsc::UnboundedSender<Approval>,
    pending: Arc<AtomicUsize>,
}

impl InteractiveBroker {
    /// The broker and the queue the front-end reads approvals from.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<Approval>) {
        let (queue, receiver) = mpsc::unbounded_channel();
        let broker = Self {
            queue,
            pending: Arc::default(),
        };
        (broker, receiver)
    }

    /// How many approvals wait for the user (the pending-approval badge).
    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::SeqCst)
    }

    fn ask<T: Send + 'static>(
        &self,
        approval: impl FnOnce(Reply<T>) -> Approval,
        unanswered: T,
    ) -> BoxFuture<'static, T> {
        let (sender, receiver) = oneshot::channel();
        let reply = Reply {
            sender,
            _pending: Pending::new(&self.pending),
        };
        let _ = self.queue.send(approval(reply));
        async move { receiver.await.unwrap_or(unanswered) }.boxed()
    }
}

impl ApprovalBroker for InteractiveBroker {
    fn sampling(&self, request: SamplingRequest) -> BoxFuture<'static, SamplingDecision> {
        self.ask(
            |reply| Approval::Sampling {
                request: Box::new(request),
                reply,
            },
            SamplingDecision::Reject,
        )
    }

    fn elicitation(&self, request: ElicitationRequest) -> BoxFuture<'static, ElicitationDecision> {
        self.ask(
            |reply| Approval::Elicitation { request, reply },
            ElicitationDecision::Cancel,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use rmcp::model::{Role, SamplingMessage};
    use serde_json::json;

    use super::*;

    fn sampling() -> SamplingRequest {
        SamplingRequest {
            server: "files".into(),
            label: "1 message".into(),
            params: serde_json::from_value(json!({
                "messages": [{"role": "user", "content": {"type": "text", "text": "hi"}}],
                "maxTokens": 10
            }))
            .unwrap(),
        }
    }

    fn email() -> ElicitationRequest {
        ElicitationRequest {
            server: "files".into(),
            message: "Your email?".into(),
            schema: Some(json!({
                "type": "object",
                "required": ["email"],
                "properties": {"email": {"type": "string", "format": "email"}}
            })),
            url: None,
        }
    }

    #[tokio::test]
    async fn headless_sampling_is_rejected_with_a_warning() {
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let seen = warnings.clone();
        let broker = HeadlessBroker::new(move |w| seen.lock().unwrap().push(w.to_owned()));
        assert_eq!(broker.sampling(sampling()).await, SamplingDecision::Reject);
        let warnings = warnings.lock().unwrap();
        assert_eq!(
            warnings[0],
            "mcp: rejected sampling request from files (1 message); pass --sampling-response to answer it"
        );
    }

    #[tokio::test]
    async fn a_canned_sampling_response_is_returned() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("answer.json");
        fs::write(
            &file,
            r#"{"role": "assistant", "content": {"type": "text", "text": "canned"}, "model": "human"}"#,
        )
        .unwrap();
        let response = load_sampling_response(&file).unwrap();
        let broker = HeadlessBroker::default().with_sampling_response(response.clone());
        assert_eq!(
            broker.sampling(sampling()).await,
            SamplingDecision::Respond(Box::new(response))
        );
        let expected = CreateMessageResult::new(
            SamplingMessage::new(Role::Assistant, "canned"),
            "human".into(),
        );
        assert_eq!(
            load_sampling_response(&file).unwrap().message,
            expected.message
        );
        assert!(load_sampling_response(&dir.path().join("none")).is_err());
    }

    #[tokio::test]
    async fn headless_elicitation_is_declined() {
        let broker = HeadlessBroker::default();
        assert_eq!(
            broker.elicitation(email()).await,
            ElicitationDecision::Decline
        );
    }

    #[test]
    fn elicitation_content_must_match_the_schema() {
        let request = email();
        let accepted = request.accept(json!({"email": "a@example.com"})).unwrap();
        let ElicitationDecision::Accept(content) = accepted else {
            panic!("accepted");
        };
        assert_eq!(content.content(), &json!({"email": "a@example.com"}));
        let violations = request.accept(json!({})).unwrap_err();
        assert_eq!(
            violations[0].to_string(),
            "/email: required property missing"
        );
        assert!(request.accept(json!({"email": "nope"})).is_err());
        assert!(request.form().is_some());
    }

    #[tokio::test]
    async fn interactive_requests_wait_for_the_user_and_count_as_pending() {
        let (broker, mut queue) = InteractiveBroker::new();
        let answer = tokio::spawn({
            let broker = broker.clone();
            async move { broker.sampling(sampling()).await }
        });
        let Some(Approval::Sampling { request, reply }) = queue.recv().await else {
            panic!("a sampling approval");
        };
        assert_eq!(request.server, "files");
        assert_eq!(broker.pending(), 1);
        assert!(!answer.is_finished());
        drop(reply);
        assert_eq!(answer.await.unwrap(), SamplingDecision::Reject);
        assert_eq!(broker.pending(), 0);

        let answer = tokio::spawn({
            let broker = broker.clone();
            async move { broker.elicitation(email()).await }
        });
        let Some(Approval::Elicitation { request, reply }) = queue.recv().await else {
            panic!("an elicitation approval");
        };
        reply.send(request.accept(json!({"email": "a@example.com"})).unwrap());
        assert!(matches!(
            answer.await.unwrap(),
            ElicitationDecision::Accept(_)
        ));
        assert_eq!(broker.pending(), 0);
    }
}
