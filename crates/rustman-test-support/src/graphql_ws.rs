//! A scripted GraphQL WebSocket server for subscription tests: it speaks
//! `graphql-transport-ws` or only the legacy `graphql-ws`
//! (subscriptions-transport-ws), never acknowledges the connection, or
//! closes it with a `graphql-transport-ws` error code.
//! Every session is recorded: the subprotocols offered and chosen, the
//! handshake headers and each message the client sent.
//!
//! Each subscription receives `events` results
//! `{"data": {"onMessage": {"n": <i>, "vars": <variables>}}}`, numbered
//! from 1, then `complete` when the script says so.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

/// The modern subprotocol.
pub const TRANSPORT_WS: &str = "graphql-transport-ws";
/// The legacy subprotocol (subscriptions-transport-ws).
pub const LEGACY_WS: &str = "graphql-ws";

/// How the server behaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WsScript {
    /// `graphql-transport-ws`: `events` results, then `complete` if set.
    Modern { events: usize, complete: bool },
    /// Accepts only the legacy `graphql-ws`: `events` results, then
    /// `complete`.
    Legacy { events: usize },
    /// `graphql-transport-ws`, but never sends `connection_ack`.
    NoAck,
    /// `graphql-transport-ws`, but closes the connection with `code` on
    /// `subscribe` (or on `connection_init` with `before_ack`). The reason
    /// echoes the handshake's `Authorization`, as a careless server might.
    Close { code: u16, before_ack: bool },
    /// `graphql-transport-ws`: one result, then a `next` message of more
    /// than `bytes` bytes (`{"data": {"blob": "xx..."}}`), then nothing.
    Large { bytes: usize },
}

impl WsScript {
    fn protocol(self) -> &'static str {
        match self {
            Self::Legacy { .. } => LEGACY_WS,
            Self::Modern { .. } | Self::NoAck | Self::Close { .. } | Self::Large { .. } => {
                TRANSPORT_WS
            }
        }
    }
}

/// One connection the server accepted (or refused).
#[derive(Clone, Debug, Default)]
pub struct WsSession {
    /// The `Sec-WebSocket-Protocol` the client offered.
    pub offered: Vec<String>,
    /// The subprotocol the server chose; `None` when it refused.
    pub chosen: Option<String>,
    pub headers: Vec<(String, String)>,
    /// Every text message the client sent, parsed as JSON.
    pub messages: Vec<Value>,
}

impl WsSession {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The `type` of every message the client sent, in order.
    pub fn message_types(&self) -> Vec<String> {
        self.messages
            .iter()
            .map(|m| m["type"].as_str().unwrap_or_default().to_owned())
            .collect()
    }
}

type Sessions = Arc<Mutex<Vec<WsSession>>>;

/// A running server; stops when dropped.
pub struct GraphqlWsServer {
    addr: SocketAddr,
    sessions: Sessions,
    task: JoinHandle<()>,
}

impl Drop for GraphqlWsServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl GraphqlWsServer {
    pub async fn start(script: WsScript) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sessions = Sessions::default();
        let shared = sessions.clone();
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                tokio::spawn(serve(tcp, script, shared.clone()));
            }
        });
        Self {
            addr,
            sessions,
            task,
        }
    }

    /// `<scheme>://127.0.0.1:<port><path>`, for `ws` or `http`.
    pub fn url(&self, scheme: &str, path: &str) -> String {
        format!("{scheme}://{}{path}", self.addr)
    }

    /// Every session so far, in order.
    pub fn sessions(&self) -> Vec<WsSession> {
        lock(&self.sessions).clone()
    }
}

fn lock(sessions: &Sessions) -> std::sync::MutexGuard<'_, Vec<WsSession>> {
    sessions.lock().unwrap_or_else(PoisonError::into_inner)
}

// The handshake callback's signature is tungstenite's.
#[allow(clippy::result_large_err)]
async fn serve(tcp: TcpStream, script: WsScript, sessions: Sessions) {
    let index = {
        let mut all = lock(&sessions);
        all.push(WsSession::default());
        all.len() - 1
    };
    let record = sessions.clone();
    let callback = move |request: &Request, response: Response| {
        negotiate(request, response, script, &record, index)
    };
    let Ok(socket) = tokio_tungstenite::accept_hdr_async(tcp, callback).await else {
        return;
    };
    converse(socket, script, &sessions, index).await;
}

/// Record the handshake and pick the script's subprotocol, refusing a
/// client that does not offer it.
#[allow(clippy::result_large_err)]
fn negotiate(
    request: &Request,
    mut response: Response,
    script: WsScript,
    sessions: &Sessions,
    index: usize,
) -> Result<Response, ErrorResponse> {
    let offered: Vec<String> = request
        .headers()
        .get_all("sec-websocket-protocol")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|p| p.trim().to_owned())
        .collect();
    let wanted = script.protocol();
    let chosen = offered
        .iter()
        .any(|p| p == wanted)
        .then(|| wanted.to_owned());
    let mut all = lock(sessions);
    let session = &mut all[index];
    session.headers = request
        .headers()
        .iter()
        .map(|(n, v)| {
            let value = String::from_utf8_lossy(v.as_bytes()).into_owned();
            (n.as_str().to_owned(), value)
        })
        .collect();
    session.offered = offered;
    session.chosen = chosen.clone();
    let Some(chosen) = chosen else {
        let mut refused = ErrorResponse::new(Some(format!("only {wanted} is supported")));
        *refused.status_mut() = StatusCode::BAD_REQUEST;
        return Err(refused);
    };
    let value = HeaderValue::from_str(&chosen).expect("a valid subprotocol");
    response
        .headers_mut()
        .insert("sec-websocket-protocol", value);
    Ok(response)
}

/// Answer the client's messages until it closes the connection.
async fn converse(
    mut socket: WebSocketStream<TcpStream>,
    script: WsScript,
    sessions: &Sessions,
    index: usize,
) {
    while let Some(Ok(message)) = socket.next().await {
        let Message::Text(text) = message else {
            continue;
        };
        let Ok(message) = serde_json::from_str::<Value>(text.as_str()) else {
            continue;
        };
        lock(sessions)[index].messages.push(message.clone());
        if let Some(frame) = closing(script, &message, sessions, index) {
            // The client's close reply ends the loop.
            let _ = socket.close(Some(frame)).await;
            continue;
        }
        for reply in replies(script, &message) {
            if socket.send(Message::text(reply.to_string())).await.is_err() {
                return;
            }
        }
    }
}

/// The close frame `script` answers `message` with, if any.
fn closing(
    script: WsScript,
    message: &Value,
    sessions: &Sessions,
    index: usize,
) -> Option<CloseFrame> {
    let WsScript::Close { code, before_ack } = script else {
        return None;
    };
    let trigger = if before_ack {
        "connection_init"
    } else {
        "subscribe"
    };
    if message["type"] != trigger {
        return None;
    }
    let auth = lock(sessions)[index]
        .header("authorization")
        .unwrap_or_default()
        .to_owned();
    Some(CloseFrame {
        code: CloseCode::from(code),
        reason: format!("Forbidden: {auth}").into(),
    })
}

/// What the server sends in answer to `message`.
fn replies(script: WsScript, message: &Value) -> Vec<Value> {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let variables = message
        .pointer("/payload/variables")
        .cloned()
        .unwrap_or(Value::Null);
    let event = |n: usize| json!({"data": {"onMessage": {"n": n, "vars": variables}}});
    match (script, message["type"].as_str().unwrap_or_default()) {
        (WsScript::NoAck, _) => Vec::new(),
        (WsScript::Close { .. }, "connection_init") => vec![json!({"type": "connection_ack"})],
        (_, "connection_init") => vec![json!({"type": "connection_ack"})],
        (_, "ping") => vec![json!({"type": "pong"})],
        (WsScript::Modern { events, complete }, "subscribe") => {
            let mut out: Vec<Value> = (1..=events)
                .map(|n| json!({"type": "next", "id": id, "payload": event(n)}))
                .collect();
            if complete {
                out.push(json!({"type": "complete", "id": id}));
            }
            out
        }
        (WsScript::Large { bytes }, "subscribe") => {
            let blob = json!({"data": {"blob": "x".repeat(bytes)}});
            vec![
                json!({"type": "next", "id": id, "payload": event(1)}),
                json!({"type": "next", "id": id, "payload": blob}),
            ]
        }
        (WsScript::Legacy { events }, "start") => {
            let mut out: Vec<Value> = vec![json!({"type": "ka"})];
            out.extend(
                (1..=events).map(|n| json!({"type": "data", "id": id, "payload": event(n)})),
            );
            out.push(json!({"type": "complete", "id": id}));
            out
        }
        _ => Vec::new(),
    }
}
