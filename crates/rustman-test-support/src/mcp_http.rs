//! Fake MCP servers over HTTP for the MCP client tests: a Streamable HTTP
//! server that hands out `Mcp-Session-Id: abc`, and a legacy HTTP+SSE
//! server (protocol 2024-11-05) that answers 405 to POSTs on its URL and
//! announces `/messages?sessionId=s1` on its event stream. Both record
//! every request and answer `initialize`, `ping` and `tools/list`. The
//! OAuth-protected variant ([`McpHttpServer::oauth`]) is a Streamable HTTP
//! server that also plays its own authorization server (see
//! [`crate::mcp_oauth`]).

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use http::{Method, Request, Response, StatusCode, header};
use http_body::Frame;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::graphql_http::RecordedRequest;
use crate::mcp_oauth::{AuthState, Reply};

/// The session id the Streamable HTTP server assigns.
pub const SESSION_ID: &str = "abc";

/// The protocol version the legacy server answers.
pub const LEGACY_VERSION: &str = "2024-11-05";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Streamable,
    Legacy,
    OAuth,
}

type Body = StreamBody<BoxStream<'static, Result<Frame<Bytes>, Infallible>>>;

struct State {
    mode: Mode,
    base: String,
    auth: Mutex<AuthState>,
    hits: Mutex<Vec<RecordedRequest>>,
    /// The legacy event stream, once opened.
    events: Mutex<Option<mpsc::UnboundedSender<String>>>,
}

/// A running server; stops when dropped.
pub struct McpHttpServer {
    addr: SocketAddr,
    state: Arc<State>,
    task: JoinHandle<()>,
}

impl Drop for McpHttpServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl McpHttpServer {
    /// A Streamable HTTP server at `/mcp`.
    pub async fn streamable() -> Self {
        Self::start(Mode::Streamable).await
    }

    /// A legacy HTTP+SSE server: event stream at `GET /mcp`, messages
    /// POSTed to `/messages`.
    pub async fn legacy() -> Self {
        Self::start(Mode::Legacy).await
    }

    /// A Streamable HTTP server at `/mcp` that requires OAuth; its
    /// authorization server offers dynamic registration when
    /// `registration` is set.
    pub async fn oauth(registration: bool) -> Self {
        let server = Self::start(Mode::OAuth).await;
        *server.auth() = AuthState::new(registration);
        server
    }

    fn auth(&self) -> std::sync::MutexGuard<'_, AuthState> {
        self.state
            .auth
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The `grant_type` of every token request, in order.
    pub fn grants(&self) -> Vec<String> {
        self.auth().grants.clone()
    }

    /// The parameters of the last authorization request.
    pub fn authorize_params(&self) -> std::collections::HashMap<String, String> {
        self.auth().authorize.clone()
    }

    /// How many clients registered dynamically.
    pub fn registrations(&self) -> usize {
        self.auth().registrations
    }

    /// Advertise `resource` in the protected resource metadata instead of
    /// the MCP endpoint's own URL (a server claiming another API).
    pub fn advertise_resource(&self, resource: &str) {
        self.auth().resource = Some(resource.to_owned());
    }

    /// Answer `issuer` in the authorization server metadata instead of the
    /// URL it is served for.
    pub fn advertise_issuer(&self, issuer: &str) {
        self.auth().issuer = Some(issuer.to_owned());
    }

    /// Start every access token issued with `prefix` (default `access-`),
    /// for tokens with characters encoders change.
    pub fn issue_access_tokens_like(&self, prefix: &str) {
        self.auth().access_prefix = Some(prefix.to_owned());
    }

    /// Accept `refresh` as a refresh token (a token issued earlier).
    pub fn accept_refresh_token(&self, refresh: &str) {
        self.auth().refresh.push(refresh.to_owned());
    }

    async fn start(mode: Mode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(State {
            mode,
            base: format!("http://{addr}"),
            auth: Mutex::default(),
            hits: Mutex::default(),
            events: Mutex::default(),
        });
        let shared = state.clone();
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let state = shared.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| handle(state.clone(), req));
                    let _ = auto::Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(tcp), service)
                        .await;
                });
            }
        });
        Self { addr, state, task }
    }

    /// The server URL, `http://127.0.0.1:<port>/mcp`.
    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    /// Every request received so far, in order.
    pub fn hits(&self) -> Vec<RecordedRequest> {
        self.state
            .hits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

async fn handle(state: Arc<State>, req: Request<Incoming>) -> Result<Response<Body>, Infallible> {
    let recorded = record(req).await;
    state
        .hits
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(recorded.clone());
    let response = match state.mode {
        Mode::Streamable => streamable(&recorded),
        Mode::Legacy => legacy(&state, &recorded),
        Mode::OAuth => protected(&state, &recorded),
    };
    Ok(response)
}

async fn record(req: Request<Incoming>) -> RecordedRequest {
    let (parts, body) = req.into_parts();
    let body = body
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    RecordedRequest {
        method: parts.method.to_string(),
        uri: parts.uri.to_string(),
        headers: parts
            .headers
            .iter()
            .map(|(n, v)| {
                let value = String::from_utf8_lossy(v.as_bytes()).into_owned();
                (n.as_str().to_owned(), value)
            })
            .collect(),
        body,
    }
}

fn streamable(req: &RecordedRequest) -> Response<Body> {
    match req.method.as_str() {
        "POST" => {
            let message: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            match answer(&message, None) {
                Some(reply) => {
                    let mut response = json_response(&reply);
                    if message["method"] == "initialize" {
                        let id = header::HeaderValue::from_static(SESSION_ID);
                        response.headers_mut().insert("mcp-session-id", id);
                    }
                    response
                }
                None => status(StatusCode::ACCEPTED),
            }
        }
        "DELETE" => status(StatusCode::OK),
        _ => status(StatusCode::METHOD_NOT_ALLOWED),
    }
}

fn protected(state: &State, req: &RecordedRequest) -> Response<Body> {
    let reply = state
        .auth
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .route(&state.base, req);
    match reply {
        None => streamable(req),
        Some(Reply::Json(code, value)) => {
            let mut response = json_response(&value);
            *response.status_mut() = StatusCode::from_u16(code).unwrap_or(StatusCode::OK);
            response
        }
        Some(Reply::Redirect(location)) => Response::builder()
            .status(StatusCode::FOUND)
            .header(header::LOCATION, location)
            .body(StreamBody::new(stream::empty().boxed()))
            .expect("a valid response"),
        Some(Reply::Unauthorized(challenge)) => Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(header::WWW_AUTHENTICATE, challenge)
            .body(StreamBody::new(stream::empty().boxed()))
            .expect("a valid response"),
    }
}

fn legacy(state: &State, req: &RecordedRequest) -> Response<Body> {
    let messages = req.uri.starts_with("/messages");
    match (req.method.parse::<Method>(), messages) {
        (Ok(Method::GET), false) => event_stream(state),
        (Ok(Method::POST), true) => {
            let message: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            if let Some(reply) = answer(&message, Some(LEGACY_VERSION)) {
                let events = state.events.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(tx) = events.as_ref() {
                    let _ = tx.send(format!("event: message\ndata: {reply}\n\n"));
                }
            }
            status(StatusCode::ACCEPTED)
        }
        _ => status(StatusCode::METHOD_NOT_ALLOWED),
    }
}

/// The legacy stream: the `endpoint` event, then every answer.
fn event_stream(state: &State) -> Response<Body> {
    let (tx, rx) = mpsc::unbounded_channel::<String>();
    let _ = tx.send("event: endpoint\ndata: /messages?sessionId=s1\n\n".to_owned());
    *state.events.lock().unwrap_or_else(PoisonError::into_inner) = Some(tx);
    let frames = stream::unfold(rx, |mut rx| async move {
        let text = rx.recv().await?;
        Some((Ok(Frame::data(Bytes::from(text))), rx))
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(StreamBody::new(frames.boxed()))
        .expect("a valid response")
}

/// The answer to a request; `None` for notifications and responses.
/// `version` overrides the protocol version answered to `initialize`.
///
/// Besides `tools/list`, the server answers `tools/call` (`echo` returns
/// its `text` argument, `fail` an `isError` result, any other tool a
/// `-32602` protocol error), `resources/list` and `resources/read` of
/// `file:///notes.md`, and `prompts/list` and `prompts/get` of `review`.
fn answer(message: &Value, version: Option<&str>) -> Option<Value> {
    let id = message.get("id")?;
    let method = message.get("method")?.as_str()?;
    let params = &message["params"];
    let result = match method {
        "initialize" => {
            let requested = params["protocolVersion"].as_str();
            json!({
                "protocolVersion": version.or(requested).unwrap_or(LEGACY_VERSION),
                "capabilities": {"tools": {}, "resources": {}, "prompts": {}},
                "serverInfo": {"name": "fake-http", "version": "0.0.1"}
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": [
            {"name": "echo", "description": "Echo the text back", "inputSchema": {"type": "object"}},
            {"name": "fail", "inputSchema": {"type": "object"}}
        ]}),
        "tools/call" => return Some(tool_call(id, params)),
        "resources/list" => json!({"resources": [
            {"uri": "file:///notes.md", "name": "notes", "mimeType": "text/markdown"}
        ]}),
        "resources/read" => json!({"contents": [
            {"uri": params["uri"], "mimeType": "text/markdown", "text": "# Notes"}
        ]}),
        "prompts/list" => json!({"prompts": [
            {"name": "review", "arguments": [{"name": "code", "required": true}]}
        ]}),
        "prompts/get" => json!({"messages": [
            {"role": "user", "content": {"type": "text", "text": format!("Review {}", params["arguments"]["code"].as_str().unwrap_or_default())}}
        ]}),
        _ => return Some(error(id, -32601, &format!("method not found: {method}"))),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn tool_call(id: &Value, params: &Value) -> Value {
    let text = |t: &str| json!([{"type": "text", "text": t}]);
    let result = match params["name"].as_str().unwrap_or_default() {
        "echo" => {
            json!({"content": text(params["arguments"]["text"].as_str().unwrap_or_default())})
        }
        "fail" => json!({"content": text("no such file"), "isError": true}),
        other => return error(id, -32602, &format!("unknown tool {other}")),
    };
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn json_response(value: &Value) -> Response<Body> {
    let body = stream::iter([Ok(Frame::data(Bytes::from(value.to_string())))]);
    Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .body(StreamBody::new(body.boxed()))
        .expect("a valid response")
}

fn status(code: StatusCode) -> Response<Body> {
    Response::builder()
        .status(code)
        .body(StreamBody::new(stream::empty().boxed()))
        .expect("a valid response")
}
