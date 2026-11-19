//! `kind: mcp` requests in `rustman run` (mcp-client "Session reuse in
//! runs", design D7): [`McpRunner`] is the run's [`McpExecutor`]. It keeps
//! one session per server for the whole run (initialized once, the stdio
//! process spawned once), connects headless (untrusted stdio servers are
//! refused unless `--trust`, sampling rejected, elicitation declined) and
//! shuts every session down at the end of the run.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustman_core::Redactor;
use rustman_core::assertions::McpFacts;
use rustman_core::run::{HeaderRecord, McpRecord, SentRequest};
use rustman_core::trace::TracePropagation;
use rustman_core::trust::{TrustKey, TrustStore};
use rustman_core::vars::{ResolvedMcp, Resolver};
use rustman_http::runner::{McpCall, McpExecutor, McpFuture, McpOutcome};
use serde_json::Value;

use super::broker::HeadlessBroker;
use super::definition::find_server;
use super::error::ClientError;
use super::oauth::ACCESS_TOKEN;
use super::operation::{McpOp, OpResult};
use super::output::printable;
use super::request::RequestOptions;
use super::resolve::{ResolvedServer, ResolvedTransport};
use super::session::{ConnectOptions, McpSession};
use super::tools::CallOptions;
use super::trust::{command_line, require_trust};

/// A connected session, or why the server could not be reached.
type Pooled = Result<Arc<McpSession>, String>;

/// Sends the mcp requests of one run.
pub struct McpRunner {
    /// `--trust`: approve (and record) untrusted stdio servers.
    pub trust: bool,
    /// Seals the approvals (this machine's key unless replaced).
    trust_key: TrustKey,
    connect: ConnectOptions,
    sessions: tokio::sync::Mutex<HashMap<String, (Pooled, Arc<ResolvedServer>)>>,
    warnings: Arc<Mutex<Vec<String>>>,
}

impl McpRunner {
    /// A runner that connects with `connect`; its broker is replaced by a
    /// headless one whose warnings land on the request that caused them.
    pub fn new(trust: bool, mut connect: ConnectOptions) -> Self {
        let warnings: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = warnings.clone();
        connect.broker = Arc::new(HeadlessBroker::new(move |w| {
            sink.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(w.to_owned());
        }));
        Self {
            trust,
            trust_key: TrustKey::machine(),
            connect,
            sessions: tokio::sync::Mutex::default(),
            warnings,
        }
    }

    /// Check and record approvals with `key` instead of this machine's.
    #[must_use]
    pub fn with_trust_key(mut self, key: TrustKey) -> Self {
        self.trust_key = key;
        self
    }

    /// How many servers the run connected to (or tried to).
    pub async fn session_count(&self) -> usize {
        self.sessions.lock().await.len()
    }

    /// Shut every session down (stdio servers get their grace period).
    pub async fn shutdown(&self) {
        let sessions: Vec<_> = self.sessions.lock().await.drain().collect();
        for (_, (pooled, _)) in sessions {
            if let Ok(session) = pooled
                && let Ok(session) = Arc::try_unwrap(session)
            {
                session.shutdown().await;
            }
        }
    }

    /// The session of `server`, connecting on first use.
    async fn session(
        &self,
        name: &str,
        resolver: &Resolver<'_>,
        root: &Path,
        trace: Option<&TracePropagation>,
    ) -> (Pooled, Option<Arc<ResolvedServer>>) {
        let mut sessions = self.sessions.lock().await;
        if let Some((pooled, server)) = sessions.get(name) {
            return (pooled.clone(), Some(server.clone()));
        }
        let server = match find_server(root, name).and_then(|d| d.resolve(resolver, root)) {
            Ok(server) => Arc::new(server),
            Err(e) => return (Err(e.to_string()), None),
        };
        let pooled = self
            .connect(&server, root, trace)
            .await
            .map(Arc::new)
            .map_err(|e| e.to_string());
        sessions.insert(name.to_owned(), (pooled.clone(), server.clone()));
        (pooled, Some(server))
    }

    async fn connect(
        &self,
        server: &ResolvedServer,
        root: &Path,
        trace: Option<&TracePropagation>,
    ) -> Result<McpSession, ClientError> {
        let mut store = TrustStore::load_with_key(root, self.trust_key.clone())?;
        let approved = require_trust(&mut store, server, self.trust)?;
        let mut connect = self.connect.clone();
        if let Some(trace) = trace {
            connect.send.trace = Some(trace.clone());
        }
        Box::pin(McpSession::connect(server, &approved, &connect)).await
    }

    async fn run(&self, call: McpCall<'_>) -> McpOutcome {
        let resolved = call.resolved;
        let options = call_options(&call);
        let (pooled, server) = self
            .session(&resolved.server, call.resolver, call.root, call.trace)
            .await;
        let mut outcome = Outcome::new(resolved, server.as_deref());
        let session = match pooled {
            Ok(session) => session,
            Err(message) => return outcome.failed(message, self.drain()),
        };
        outcome.session(&session);
        let performed = match McpOp::from_params(resolved.operation, &resolved.params) {
            Ok(op) => session.perform(&op, &options).await,
            Err(e) => Err(e),
        };
        outcome.redactor.merge(&session.redactor());
        match performed {
            Ok(done) => outcome.answered(done, self.drain()),
            Err(e) => {
                outcome.record.outcome = e.label();
                outcome.failed(e.to_string(), self.drain())
            }
        }
    }

    fn drain(&self) -> Vec<String> {
        std::mem::take(&mut *self.warnings.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl McpExecutor for McpRunner {
    fn execute<'a>(&'a self, call: McpCall<'a>) -> McpFuture<'a> {
        Box::pin(self.run(call))
    }
}

/// The request's timeout (`--timeout` first), cancellation and validation.
fn call_options(call: &McpCall<'_>) -> CallOptions {
    CallOptions {
        request: RequestOptions {
            timeout: call.timeout.or(call.mcp.timeout),
            cancel: Some(call.cancel.child_token()),
            progress: None,
        },
        validate: call.mcp.effective_validate(),
    }
}

/// An outcome being built.
struct Outcome {
    record: McpRecord,
    sent: SentRequest,
    redactor: Redactor,
}

impl Outcome {
    fn new(resolved: &ResolvedMcp, server: Option<&ResolvedServer>) -> Self {
        let params = Value::Object(resolved.params.clone());
        let mut redactor = resolved.redactor();
        if let Some(server) = server {
            redactor.merge(&server.redactor());
        }
        Self {
            record: McpRecord {
                server: resolved.server.clone(),
                transport: server.map(transport_name).unwrap_or_default().to_owned(),
                method: resolved.operation.as_str().to_owned(),
                params: params.clone(),
                outcome: "error".into(),
                ..McpRecord::default()
            },
            sent: SentRequest {
                method: resolved.operation.as_str().to_owned(),
                url: server.map(|s| endpoint(s, &redactor)).unwrap_or_default(),
                headers: server.map(definition_headers).unwrap_or_default(),
                body: Some(params.to_string()),
            },
            redactor,
        }
    }

    /// What the session adds: its transport, version and OAuth token.
    fn session(&mut self, session: &McpSession) {
        let info = session.info();
        self.record.transport = info.transport.label().to_owned();
        self.record.protocol_version = Some(info.protocol_version.clone());
        let redactor = session.redactor();
        let bearer = redactor.secret_names().iter().any(|n| n == ACCESS_TOKEN);
        let has_auth = self
            .sent
            .headers
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case("authorization"));
        if bearer && !has_auth {
            self.sent.headers.push(HeaderRecord {
                name: "Authorization".into(),
                value: "Bearer".into(),
            });
        }
    }

    fn failed(mut self, message: String, warnings: Vec<String>) -> McpOutcome {
        let message = printable(&message).into_owned();
        self.record.error = Some(message.clone());
        self.finish(
            Err(message),
            String::new(),
            McpFacts::default(),
            warnings,
            Duration::ZERO,
        )
    }

    fn answered(mut self, done: OpResult, warnings: Vec<String>) -> McpOutcome {
        self.record.outcome = done.outcome.to_owned();
        self.record.result = Some(done.result.clone());
        self.record.duration_ms = done.elapsed.as_secs_f64() * 1000.0;
        self.record.validation_override = done.overridden.iter().map(ToString::to_string).collect();
        let notes = done
            .notifications
            .iter()
            .filter_map(|n| serde_json::to_value(n).ok())
            .collect();
        self.record = std::mem::take(&mut self.record).with_notifications(notes);
        let facts = McpFacts {
            output_violations: done
                .output_violations
                .map(|v| v.iter().map(ToString::to_string).collect()),
        };
        self.finish(
            Ok(done.result),
            done.capture_body,
            facts,
            warnings,
            done.elapsed,
        )
    }

    fn finish(
        self,
        result: Result<Value, String>,
        capture_body: String,
        facts: McpFacts,
        warnings: Vec<String>,
        elapsed: Duration,
    ) -> McpOutcome {
        McpOutcome {
            result,
            capture_body,
            facts,
            sent: self.sent,
            record: self.record,
            redactor: self.redactor,
            warnings,
            elapsed,
        }
    }
}

fn transport_name(server: &ResolvedServer) -> &'static str {
    match server.transport {
        ResolvedTransport::Stdio(_) => "stdio",
        ResolvedTransport::Http(_) => "streamable HTTP",
    }
}

/// The URL of an HTTP server, `stdio:<command line>` of a stdio one.
fn endpoint(server: &ResolvedServer, redactor: &Redactor) -> String {
    match &server.transport {
        ResolvedTransport::Stdio(stdio) => format!("stdio:{}", command_line(stdio, redactor)),
        ResolvedTransport::Http(http) => redactor.resolved(&http.url),
    }
}

/// The headers an HTTP server definition sends, with its auth as an
/// `Authorization` header (shown as a placeholder once redacted).
fn definition_headers(server: &ResolvedServer) -> Vec<HeaderRecord> {
    let ResolvedTransport::Http(http) = &server.transport else {
        return Vec::new();
    };
    let mut headers: Vec<HeaderRecord> = http
        .headers
        .iter()
        .map(|h| HeaderRecord {
            name: h.name.text.clone(),
            value: h.value.text.clone(),
        })
        .collect();
    if http.auth.is_some() {
        headers.push(HeaderRecord {
            name: "Authorization".into(),
            value: "credentials".into(),
        });
    }
    headers
}
