//! An MCP client session (design D1 `McpSession`): connects over stdio,
//! Streamable HTTP or legacy SSE, initializes, checks the negotiated
//! version, and shuts down cleanly. rmcp stays behind this facade.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{ClientRequest, ServerPeerInfo, SetLevelRequest, SetLevelRequestParams};
use rmcp::service::{ClientInitializeError, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{Peer, RoleClient, ServiceExt};
use rustman_core::Redactor;
use rustman_http::{HttpEngine, SendOptions};
use tokio::sync::watch;

use super::broker::{ApprovalBroker, HeadlessBroker};
use super::error::ClientError;
use super::handler::{ClientState, RustmanClient};
use super::http::EngineClient;
use super::info::{Operation, SessionInfo, TransportKind};
use super::legacy::LegacySse;
use super::line_limit::{EndReason, LineLimit, MAX_STDIO_MESSAGE};
use super::notify::{Notifications, level_name};
use super::oauth::{Authorizer, OAuthOptions};
use super::request::{DEFAULT_TIMEOUT, RequestOptions, Sender, format_timeout};
use super::resolve::{ResolvedHttp, ResolvedServer, ResolvedStdio, ResolvedTransport};
use super::roots::{default_roots, root_uris};
use super::shared::SharedRedactor;
use super::stdio::{self, ProcessExit, SHUTDOWN_GRACE, STDERR_TAIL, Shutdown, StderrLog};
use super::tap::tap;
use super::trust::Approved;
use super::version;

/// How sessions connect.
#[derive(Clone)]
pub struct ConnectOptions {
    /// Sends the HTTP requests; share one across sessions to share pools.
    pub engine: Arc<HttpEngine>,
    /// TLS, proxy and timeouts of HTTP servers.
    pub send: SendOptions,
    /// How long a stdio server has to exit once its stdin is closed.
    pub shutdown_grace: Duration,
    /// Where sampling and elicitation requests go; headless by default.
    pub broker: Arc<dyn ApprovalBroker>,
    /// OAuth for HTTP servers that ask for it; `None` fails on a `401`.
    pub oauth: Option<OAuthOptions>,
    /// The request timeout of the session, `initialize` included, above
    /// the server's `timeout_ms` (the CLI's `--timeout`).
    pub timeout: Option<Duration>,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            engine: Arc::default(),
            send: SendOptions::default(),
            shutdown_grace: SHUTDOWN_GRACE,
            broker: Arc::new(HeadlessBroker::default()),
            oauth: None,
            timeout: None,
        }
    }
}

/// Whether the session's server is still there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Connected,
    /// The connection ended; for stdio servers, how the process exited and
    /// its last [`STDERR_TAIL`] stderr lines.
    Disconnected {
        exit: Option<ProcessExit>,
        stderr_tail: Vec<String>,
    },
}

/// The child process of a stdio session.
struct Process {
    stderr: StderrLog,
    exit: watch::Receiver<Option<ProcessExit>>,
    shutdown: Shutdown,
}

pub struct McpSession {
    name: String,
    service: RunningService<RoleClient, RustmanClient>,
    info: SessionInfo,
    process: Option<Process>,
    grace: Duration,
    state: Arc<ClientState>,
    workspace: std::path::PathBuf,
}

impl std::fmt::Debug for McpSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpSession")
            .field("name", &self.name)
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

type Service = RunningService<RoleClient, RustmanClient>;

impl McpSession {
    /// Connect to `server` and initialize. `approved` comes from the trust
    /// check of this very definition; nothing is spawned without it.
    pub async fn connect(
        server: &ResolvedServer,
        approved: &Approved,
        options: &ConnectOptions,
    ) -> Result<Self, ClientError> {
        if !approved.covers(server) {
            return Err(ClientError::Untrusted(server.name.clone()));
        }
        let redactor = SharedRedactor::new(server.redactor());
        let state = client_state(server, &redactor, options)?;
        let client = RustmanClient::new(state.clone());
        let (service, transport, process) = match &server.transport {
            ResolvedTransport::Stdio(stdio) => {
                let (service, process) =
                    connect_stdio(server, stdio, &redactor.get(), options, client).await?;
                (service, TransportKind::Stdio, Some(process))
            }
            ResolvedTransport::Http(http) => {
                let (service, kind) =
                    connect_http(server, http, &redactor, options, client).await?;
                (service, kind, None)
            }
        };
        let mut session = Self {
            name: server.name.clone(),
            info: SessionInfo::new(&peer_info(&service), transport),
            service,
            process,
            grace: options.shutdown_grace,
            state,
            workspace: server.workspace.clone(),
        };
        if let Err(err) = version::negotiate(&peer_info(&session.service).protocol_version) {
            session.close().await;
            return Err(err);
        }
        Ok(session)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn info(&self) -> &SessionInfo {
        &self.info
    }

    /// The peer, to send requests and notifications.
    pub fn peer(&self) -> &Peer<RoleClient> {
        self.service.peer()
    }

    pub(crate) fn state(&self) -> &ClientState {
        &self.state
    }

    pub(crate) fn sender(&self) -> Sender {
        self.state.sender(self.peer().clone())
    }

    /// Redacts the secrets of the definition (and OAuth tokens) from what
    /// the session shows.
    pub fn redactor(&self) -> Redactor {
        self.state.redactor.get()
    }

    /// `Ok` when the server declared the capability `operation` needs.
    pub fn require(&self, operation: Operation) -> Result<(), ClientError> {
        self.info
            .capabilities
            .check(operation)
            .map_err(|d| ClientError::Disabled(d.reason))
    }

    /// The server's notifications.
    pub fn notifications(&self) -> &Notifications {
        &self.state.notifications
    }

    /// `logging/setLevel`: the server sends log messages from `level` up.
    pub async fn set_log_level(
        &self,
        level: rmcp::model::LoggingLevel,
        options: &RequestOptions,
    ) -> Result<(), ClientError> {
        self.require(Operation::SetLogLevel)?;
        let params = SetLevelRequestParams::new(level);
        let request = ClientRequest::SetLevelRequest(SetLevelRequest::new(params));
        tracing::debug!(level = level_name(level), "mcp: logging/setLevel");
        self.sender().send(request, options).await.map(|_| ())
    }

    /// The roots answered to `roots/list`.
    pub fn roots(&self) -> Vec<rmcp::model::Root> {
        self.state.roots()
    }

    /// Replace the roots (as written in a definition) and send
    /// `notifications/roots/list_changed`.
    pub async fn set_roots(&self, written: &[String]) -> Result<(), ClientError> {
        self.state.set_roots(root_uris(written, &self.workspace)?);
        self.peer()
            .notify_roots_list_changed()
            .await
            .map_err(|e| self.sender().service_error(e))
    }

    /// The stderr log of a stdio server.
    pub fn stderr(&self) -> Option<&StderrLog> {
        self.process.as_ref().map(|p| &p.stderr)
    }

    pub fn status(&self) -> SessionStatus {
        let exit = self.process.as_ref().and_then(|p| p.exit.borrow().clone());
        if exit.is_none() && !self.service.is_closed() {
            return SessionStatus::Connected;
        }
        SessionStatus::Disconnected {
            stderr_tail: self
                .stderr()
                .map(|s| s.tail(STDERR_TAIL))
                .unwrap_or_default(),
            exit,
        }
    }

    /// Wait until the stdio server process exits, then report the status.
    /// Returns at once for HTTP sessions.
    pub async fn disconnected(&mut self) -> SessionStatus {
        if let Some(process) = &mut self.process {
            let _ = process.exit.wait_for(Option::is_some).await;
        }
        self.status()
    }

    /// Send `ping` and wait for the answer.
    pub async fn ping(&self) -> Result<(), ClientError> {
        self.peer()
            .send_request(rmcp::model::ClientRequest::PingRequest(Default::default()))
            .await
            .map(|_| ())
            .map_err(|e| ClientError::Connect {
                name: self.name.clone(),
                message: e.to_string(),
            })
    }

    /// Close the connection: stdin is closed (or the HTTP session deleted),
    /// then a stdio server gets the grace period before it is killed.
    pub async fn shutdown(mut self) -> Option<ProcessExit> {
        self.close().await
    }

    async fn close(&mut self) -> Option<ProcessExit> {
        let _ = self.service.close().await;
        let process = self.process.as_mut()?;
        process.shutdown.begin(self.grace);
        let _ = process.exit.wait_for(Option::is_some).await;
        process.exit.borrow().clone()
    }
}

fn client_state(
    server: &ResolvedServer,
    redactor: &SharedRedactor,
    options: &ConnectOptions,
) -> Result<Arc<ClientState>, ClientError> {
    let written = server.roots.clone().unwrap_or_else(default_roots);
    Ok(Arc::new(ClientState {
        server: server.name.clone(),
        notifications: Notifications::new(redactor.clone()),
        broker: options.broker.clone(),
        roots: std::sync::RwLock::new(root_uris(&written, &server.workspace)?),
        tools: Default::default(),
        subscriptions: Default::default(),
        redactor: redactor.clone(),
        timeout: request_timeout(server, options),
        ended: EndReason::default(),
        refetch: Default::default(),
    }))
}

fn peer_info(service: &Service) -> Arc<ServerPeerInfo> {
    service
        .peer_info()
        .expect("an initialized session knows its server")
}

async fn connect_stdio(
    server: &ResolvedServer,
    stdio: &ResolvedStdio,
    redactor: &Redactor,
    options: &ConnectOptions,
    client: RustmanClient,
) -> Result<(Service, Process), ClientError> {
    let spawned = stdio::spawn(&server.name, stdio, redactor)?;
    let mut process = Process {
        stderr: spawned.stderr,
        exit: spawned.exit,
        shutdown: spawned.shutdown,
    };
    let state = client.state();
    let ended = state.ended.clone();
    let stdout = LineLimit::new(spawned.stdout, MAX_STDIO_MESSAGE, ended.clone());
    let serve = client.serve(tap((stdout, spawned.stdin), state));
    match initialize(request_timeout(server, options), serve).await {
        Ok(service) => Ok((service, process)),
        Err(err) => {
            process.shutdown.begin(options.shutdown_grace);
            Err(match ended.get() {
                Some(reason) => ClientError::Transport {
                    name: server.name.clone(),
                    message: reason.clone(),
                },
                None => err.into_error(&server.name, redactor),
            })
        }
    }
}

/// Why `initialize` did not complete.
enum InitFailure {
    /// The server did not answer within the timeout.
    TimedOut(Duration),
    Failed(Box<ClientInitializeError>),
}

impl InitFailure {
    fn into_error(self, name: &str, redactor: &Redactor) -> ClientError {
        match self {
            Self::TimedOut(timeout) => ClientError::Timeout(format_timeout(timeout)),
            Self::Failed(err) => connect_error(name, redactor, &err),
        }
    }
}

/// How long a request, `initialize` included, may take: the connect
/// options' timeout, else the server's (60 s by default).
fn request_timeout(server: &ResolvedServer, options: &ConnectOptions) -> Duration {
    options
        .timeout
        .or(server.timeout)
        .unwrap_or(DEFAULT_TIMEOUT)
}

/// Run `serve` (the `initialize` handshake) within `timeout`.
async fn initialize(
    timeout: Duration,
    serve: impl Future<Output = Result<Service, ClientInitializeError>>,
) -> Result<Service, InitFailure> {
    match tokio::time::timeout(timeout, serve).await {
        Ok(served) => served.map_err(|e| InitFailure::Failed(Box::new(e))),
        Err(_) => Err(InitFailure::TimedOut(timeout)),
    }
}

/// Streamable HTTP first; after a `401`, an OAuth login and a second try;
/// on a 4xx other than 401 to the `initialize` POST, the legacy HTTP+SSE
/// transport.
async fn connect_http(
    server: &ResolvedServer,
    http: &ResolvedHttp,
    redactor: &SharedRedactor,
    options: &ConnectOptions,
    handler: RustmanClient,
) -> Result<(Service, TransportKind), ClientError> {
    let client = EngineClient::new(
        options.engine.clone(),
        options.send.clone(),
        http.clone(),
        server.secrets.clone(),
        redactor.clone(),
        authorizer(server, http, redactor, options),
    );
    let timeout = request_timeout(server, options);
    let failed = |e: InitFailure| e.into_error(&server.name, &redactor.get());
    let err = match initialize(timeout, serve_streamable(&client, http, &handler)).await {
        Ok(service) => return Ok((service, TransportKind::StreamableHttp)),
        Err(err) => err,
    };
    if let (Some(oauth), Some(challenge)) = (client.authorizer(), client.challenge()) {
        oauth
            .login(Some(challenge.as_str()).filter(|c| !c.is_empty()))
            .await?;
        let service = initialize(timeout, serve_streamable(&client, http, &handler))
            .await
            .map_err(failed)?;
        return Ok((service, TransportKind::StreamableHttp));
    }
    if !wants_legacy(client.initialize_status()) {
        return Err(failed(err));
    }
    let legacy = tokio::time::timeout(timeout, LegacySse::connect(client, &http.url.text))
        .await
        .map_err(|_| ClientError::Timeout(format_timeout(timeout)))?
        .map_err(|e| ClientError::Connect {
            name: server.name.clone(),
            message: e.to_string(),
        })?;
    let state = handler.state();
    let service = initialize(timeout, handler.serve(tap(legacy, state)))
        .await
        .map_err(failed)?;
    Ok((service, TransportKind::LegacySse))
}

async fn serve_streamable(
    client: &EngineClient,
    http: &ResolvedHttp,
    handler: &RustmanClient,
) -> Result<Service, ClientInitializeError> {
    let config = StreamableHttpClientTransportConfig::with_uri(http.url.text.as_str());
    let transport = StreamableHttpClientTransport::with_client(client.clone(), config);
    handler.clone().serve(tap(transport, handler.state())).await
}

/// OAuth applies to servers without static `auth` when a token store is
/// available.
fn authorizer(
    server: &ResolvedServer,
    http: &ResolvedHttp,
    redactor: &SharedRedactor,
    options: &ConnectOptions,
) -> Option<Arc<Authorizer>> {
    if http.auth.is_some() {
        return None;
    }
    let oauth = options.oauth.as_ref()?;
    Some(Arc::new(Authorizer::new(
        &server.name,
        &http.url.text,
        http.oauth.clone(),
        oauth,
        options.engine.clone(),
        &options.send,
        redactor.clone(),
    )))
}

/// A 4xx other than 401 to `initialize` marks a pre-2025 server.
fn wants_legacy(status: Option<u16>) -> bool {
    status.is_some_and(|s| (400..500).contains(&s) && s != 401)
}

fn connect_error(name: &str, redactor: &Redactor, err: &ClientInitializeError) -> ClientError {
    ClientError::Connect {
        name: name.to_owned(),
        message: redactor.text(&err.to_string()).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_fallback_only_on_4xx_other_than_401() {
        assert!(wants_legacy(Some(405)));
        assert!(wants_legacy(Some(404)));
        assert!(wants_legacy(Some(400)));
        assert!(!wants_legacy(Some(401)));
        assert!(!wants_legacy(Some(500)));
        assert!(!wants_legacy(Some(200)));
        assert!(!wants_legacy(None));
    }
}
