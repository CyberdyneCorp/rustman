//! Scripted JSON-RPC servers for the JSON-RPC client tests.
//!
//! [`JsonrpcHttpServer`] records every request and answers with a closure
//! that sees the request and its index, so a test can play errors, HTML
//! gateway pages, rate limits with `Retry-After`, or a server-side delay.
//!
//! [`JsonrpcWsServer`] answers each client message with frames from a
//! closure: JSON messages, and pauses between them (server-side delays,
//! so a subscription can emit events over time while the client's next
//! messages are still read and recorded).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use serde_json::Value;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{
    Request as WsRequest, Response as WsResponse,
};

use crate::graphql_http::RecordedRequest;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The answer to one HTTP request.
#[derive(Clone, Debug)]
pub struct RpcReply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// How long the server waits before answering.
    pub delay: Duration,
    /// Close the connection without answering (a connection reset).
    pub dropped: bool,
}

impl RpcReply {
    /// A 200 `application/json` answer.
    pub fn json(body: &Value) -> Self {
        Self::raw(200, "application/json", &body.to_string())
    }

    pub fn raw(status: u16, content_type: &str, body: &str) -> Self {
        Self {
            status,
            headers: vec![("content-type".to_owned(), content_type.to_owned())],
            body: body.to_owned(),
            delay: Duration::ZERO,
            dropped: false,
        }
    }

    /// An answer with no body.
    pub fn empty(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: String::new(),
            delay: Duration::ZERO,
            dropped: false,
        }
    }

    /// No answer: the connection is closed (the client sees a reset).
    pub fn dropped() -> Self {
        Self {
            dropped: true,
            ..Self::empty(200)
        }
    }

    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    #[must_use]
    pub fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

type HttpResponder = dyn Fn(&RecordedRequest, usize) -> RpcReply + Send + Sync;

/// A running HTTP server; stops when dropped.
pub struct JsonrpcHttpServer {
    addr: SocketAddr,
    hits: Arc<Mutex<Vec<RecordedRequest>>>,
    task: JoinHandle<()>,
}

impl Drop for JsonrpcHttpServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl JsonrpcHttpServer {
    /// A server answering request number `n` (from 0) with `respond`.
    pub async fn start(
        respond: impl Fn(&RecordedRequest, usize) -> RpcReply + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let respond: Arc<HttpResponder> = Arc::new(respond);
        let shared = hits.clone();
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let hits = shared.clone();
                let respond = respond.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| handle(hits.clone(), respond.clone(), req));
                    let _ = auto::Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(tcp), service)
                        .await;
                });
            }
        });
        Self { addr, hits, task }
    }

    /// `http://127.0.0.1:<port><path>`.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// Every request received so far, in order.
    pub fn hits(&self) -> Vec<RecordedRequest> {
        lock(&self.hits).clone()
    }
}

async fn handle(
    hits: Arc<Mutex<Vec<RecordedRequest>>>,
    respond: Arc<HttpResponder>,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, std::io::Error> {
    let (parts, body) = req.into_parts();
    let body = body
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    let recorded = RecordedRequest {
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
    };
    let index = {
        let mut all = lock(&hits);
        all.push(recorded.clone());
        all.len() - 1
    };
    let reply = respond(&recorded, index);
    if !reply.delay.is_zero() {
        tokio::time::sleep(reply.delay).await;
    }
    if reply.dropped {
        // A service error makes hyper close the connection unanswered.
        return Err(std::io::Error::other(
            "connection dropped by the test server",
        ));
    }
    let mut response = Response::builder().status(StatusCode::from_u16(reply.status).unwrap());
    for (name, value) in &reply.headers {
        response = response.header(name, value);
    }
    Ok(response.body(Full::new(Bytes::from(reply.body))).unwrap())
}

/// What the WebSocket server sends in answer to one client message.
#[derive(Clone, Debug, PartialEq)]
pub enum WsFrame {
    Json(Value),
    /// Wait before the next frame (a server-side delay).
    Pause(Duration),
}

/// One WebSocket connection the server accepted.
#[derive(Clone, Debug, Default)]
pub struct RpcWsSession {
    pub headers: Vec<(String, String)>,
    /// Every text message the client sent, parsed as JSON.
    pub messages: Vec<Value>,
}

impl RpcWsSession {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The `method` of every message the client sent, in order.
    pub fn methods(&self) -> Vec<String> {
        self.messages
            .iter()
            .filter_map(|m| m.get("method").and_then(Value::as_str))
            .map(str::to_owned)
            .collect()
    }
}

type WsResponder = dyn Fn(&Value) -> Vec<WsFrame> + Send + Sync;
type Sessions = Arc<Mutex<Vec<RpcWsSession>>>;

/// A running WebSocket server; stops when dropped.
pub struct JsonrpcWsServer {
    addr: SocketAddr,
    sessions: Sessions,
    task: JoinHandle<()>,
}

impl Drop for JsonrpcWsServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl JsonrpcWsServer {
    /// A server answering each client message with `respond`'s frames.
    pub async fn start(respond: impl Fn(&Value) -> Vec<WsFrame> + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sessions = Sessions::default();
        let respond: Arc<WsResponder> = Arc::new(respond);
        let shared = sessions.clone();
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                tokio::spawn(serve(tcp, respond.clone(), shared.clone()));
            }
        });
        Self {
            addr,
            sessions,
            task,
        }
    }

    /// `ws://127.0.0.1:<port><path>`.
    pub fn url(&self, path: &str) -> String {
        format!("ws://{}{path}", self.addr)
    }

    /// Every session so far, in order.
    pub fn sessions(&self) -> Vec<RpcWsSession> {
        lock(&self.sessions).clone()
    }
}

// The handshake callback's signature is tungstenite's.
#[allow(clippy::result_large_err)]
async fn serve(tcp: TcpStream, respond: Arc<WsResponder>, sessions: Sessions) {
    let index = {
        let mut all = lock(&sessions);
        all.push(RpcWsSession::default());
        all.len() - 1
    };
    let record = sessions.clone();
    let callback = move |request: &WsRequest, response: WsResponse| {
        lock(&record)[index].headers = request
            .headers()
            .iter()
            .map(|(n, v)| {
                (
                    n.as_str().to_owned(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect();
        Ok(response)
    };
    let Ok(socket) = tokio_tungstenite::accept_hdr_async(tcp, callback).await else {
        return;
    };
    let (mut sink, mut stream) = socket.split();
    let (out, mut outgoing) = mpsc::unbounded_channel::<Message>();
    let writer = tokio::spawn(async move {
        while let Some(message) = outgoing.recv().await {
            if sink.send(message).await.is_err() {
                return;
            }
        }
    });
    while let Some(Ok(message)) = stream.next().await {
        let Message::Text(text) = message else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(text.as_str()) else {
            continue;
        };
        lock(&sessions)[index].messages.push(value.clone());
        let frames = respond(&value);
        let out = out.clone();
        tokio::spawn(async move {
            for frame in frames {
                match frame {
                    WsFrame::Pause(delay) => tokio::time::sleep(delay).await,
                    WsFrame::Json(value) => {
                        if out.send(Message::text(value.to_string())).is_err() {
                            return;
                        }
                    }
                }
            }
        });
    }
    writer.abort();
}
