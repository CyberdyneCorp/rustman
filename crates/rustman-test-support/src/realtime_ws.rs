//! A scripted WebSocket server for realtime-session tests: it picks a
//! subprotocol from the client's offer, or refuses the upgrade with a
//! status and body; on connect and for every message it receives, it runs
//! a list of actions (send text or binary, ping, close, pause, stream
//! forever). Plain or TLS. Every session is recorded: the offered and
//! chosen subprotocols, the handshake headers and each message received.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;

use crate::certs::TestCerts;
use crate::server::{Tls, server_config};

/// Something the server does on a connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WsAction {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Close(u16, String),
    /// Wait before the next action.
    Pause(Duration),
    /// Send `text` with a counter (`{"n":1,...}` style through `{n}`)
    /// every `every`, until the connection breaks.
    Forever {
        text: String,
        every: Duration,
    },
}

type Reply = dyn Fn(&Message) -> Vec<WsAction> + Send + Sync;

/// How the server behaves.
#[derive(Clone)]
pub struct WsBehavior {
    /// The subprotocols the server accepts; it chooses the first one the
    /// client offered that is in this list.
    pub accept: Vec<String>,
    /// Refuse the upgrade with this status and body.
    pub refuse: Option<(u16, String)>,
    pub on_connect: Vec<WsAction>,
    /// The actions for each message received (pings and pongs included).
    pub reply: Arc<Reply>,
}

impl Default for WsBehavior {
    fn default() -> Self {
        Self {
            accept: Vec::new(),
            refuse: None,
            on_connect: Vec::new(),
            reply: Arc::new(|_| Vec::new()),
        }
    }
}

impl WsBehavior {
    /// Echo every text and binary message.
    pub fn echo() -> Self {
        Self::default().reply(|message| match message {
            Message::Text(text) => vec![WsAction::Text(text.as_str().to_owned())],
            Message::Binary(bytes) => vec![WsAction::Binary(bytes.to_vec())],
            _ => Vec::new(),
        })
    }

    #[must_use]
    pub fn reply(
        mut self,
        reply: impl Fn(&Message) -> Vec<WsAction> + Send + Sync + 'static,
    ) -> Self {
        self.reply = Arc::new(reply);
        self
    }

    #[must_use]
    pub fn accept(mut self, protocols: &[&str]) -> Self {
        self.accept = protocols.iter().map(|p| (*p).to_owned()).collect();
        self
    }

    #[must_use]
    pub fn on_connect(mut self, actions: Vec<WsAction>) -> Self {
        self.on_connect = actions;
        self
    }

    #[must_use]
    pub fn refuse(mut self, status: u16, body: &str) -> Self {
        self.refuse = Some((status, body.to_owned()));
        self
    }
}

/// One connection the server accepted (or refused).
#[derive(Clone, Debug, Default)]
pub struct RecordedWs {
    pub offered: Vec<String>,
    pub chosen: Option<String>,
    pub headers: Vec<(String, String)>,
    /// Every message received, in order.
    pub messages: Vec<Message>,
}

impl RecordedWs {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The text messages received.
    pub fn texts(&self) -> Vec<String> {
        self.messages
            .iter()
            .filter_map(|m| match m {
                Message::Text(t) => Some(t.as_str().to_owned()),
                _ => None,
            })
            .collect()
    }
}

type Sessions = Arc<Mutex<Vec<RecordedWs>>>;

/// A running server; stops when dropped.
pub struct RealtimeWsServer {
    addr: SocketAddr,
    scheme: &'static str,
    sessions: Sessions,
    task: JoinHandle<()>,
}

impl Drop for RealtimeWsServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl RealtimeWsServer {
    /// A plain `ws://` server.
    pub async fn start(behavior: WsBehavior) -> Self {
        Self::listen(behavior, None, "ws").await
    }

    /// A `wss://` server with the test certificates (`tls` picks the
    /// certificate: signed by the test CA, or self-signed).
    pub async fn start_tls(behavior: WsBehavior, certs: &TestCerts, tls: Tls) -> Self {
        let acceptor = TlsAcceptor::from(Arc::new(server_config(certs, tls)));
        Self::listen(behavior, Some(acceptor), "wss").await
    }

    async fn listen(behavior: WsBehavior, tls: Option<TlsAcceptor>, scheme: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sessions = Sessions::default();
        let shared = sessions.clone();
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let (behavior, sessions, tls) = (behavior.clone(), shared.clone(), tls.clone());
                tokio::spawn(async move {
                    match tls {
                        Some(acceptor) => {
                            if let Ok(stream) = acceptor.accept(tcp).await {
                                serve(stream, behavior, sessions).await;
                            }
                        }
                        None => serve(tcp, behavior, sessions).await,
                    }
                });
            }
        });
        Self {
            addr,
            scheme,
            sessions,
            task,
        }
    }

    /// `ws(s)://127.0.0.1:<port><path>`.
    pub fn url(&self, path: &str) -> String {
        format!("{}://{}{path}", self.scheme, self.addr)
    }

    /// `ws(s)://localhost:<port><path>`, for certificate checks by name.
    pub fn localhost_url(&self, path: &str) -> String {
        format!("{}://localhost:{}{path}", self.scheme, self.addr.port())
    }

    /// `host:port`, as a proxy's `CONNECT` names it.
    pub fn authority(&self) -> String {
        self.addr.to_string()
    }

    /// Every session so far, in order.
    pub fn sessions(&self) -> Vec<RecordedWs> {
        lock(&self.sessions).clone()
    }
}

fn lock(sessions: &Sessions) -> std::sync::MutexGuard<'_, Vec<RecordedWs>> {
    sessions.lock().unwrap_or_else(PoisonError::into_inner)
}

// The handshake callback's signature is tungstenite's.
#[allow(clippy::result_large_err)]
async fn serve<S>(stream: S, behavior: WsBehavior, sessions: Sessions)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let index = {
        let mut all = lock(&sessions);
        all.push(RecordedWs::default());
        all.len() - 1
    };
    let record = sessions.clone();
    let handshake = behavior.clone();
    let callback = move |request: &Request, response: Response| {
        negotiate(request, response, &handshake, &record, index)
    };
    let Ok(socket) = tokio_tungstenite::accept_hdr_async(stream, callback).await else {
        return;
    };
    converse(socket, behavior, sessions, index).await;
}

// The handshake callback's signature is tungstenite's.
#[allow(clippy::result_large_err)]
fn negotiate(
    request: &Request,
    mut response: Response,
    behavior: &WsBehavior,
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
    let chosen = offered
        .iter()
        .find(|p| behavior.accept.contains(p))
        .cloned();
    {
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
    }
    if let Some((status, body)) = &behavior.refuse {
        let mut refused = ErrorResponse::new(Some(body.clone()));
        *refused.status_mut() = StatusCode::from_u16(*status).unwrap();
        return Err(refused);
    }
    if let Some(chosen) = chosen {
        let value = HeaderValue::from_str(&chosen).expect("a valid subprotocol");
        response
            .headers_mut()
            .insert("sec-websocket-protocol", value);
    }
    Ok(response)
}

/// Read and record messages while a writer task performs the actions.
async fn converse<S>(
    socket: tokio_tungstenite::WebSocketStream<S>,
    behavior: WsBehavior,
    sessions: Sessions,
    index: usize,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sink, mut stream) = socket.split();
    let (actions, mut queue) = mpsc::unbounded_channel::<WsAction>();
    let writer = tokio::spawn(async move {
        while let Some(action) = queue.recv().await {
            if perform(&mut sink, action).await.is_err() {
                return;
            }
        }
    });
    for action in behavior.on_connect.iter().cloned() {
        let _ = actions.send(action);
    }
    while let Some(Ok(message)) = stream.next().await {
        lock(&sessions)[index].messages.push(message.clone());
        for action in (behavior.reply)(&message) {
            let _ = actions.send(action);
        }
        // After a close, the next read sends tungstenite's close reply and
        // ends the stream.
    }
    drop(actions);
    let _ = tokio::time::timeout(Duration::from_secs(5), writer).await;
}

async fn perform<W>(sink: &mut W, action: WsAction) -> Result<(), ()>
where
    W: futures_util::Sink<Message> + Unpin,
{
    let message = match action {
        WsAction::Text(text) => Message::text(text),
        WsAction::Binary(bytes) => Message::binary(bytes),
        WsAction::Ping(data) => Message::Ping(data.into()),
        WsAction::Close(code, reason) => Message::Close(Some(CloseFrame {
            code: code.into(),
            reason: reason.into(),
        })),
        WsAction::Pause(pause) => {
            tokio::time::sleep(pause).await;
            return Ok(());
        }
        WsAction::Forever { text, every } => return forever(sink, &text, every).await,
    };
    sink.send(message).await.map_err(|_| ())
}

async fn forever<W>(sink: &mut W, text: &str, every: Duration) -> Result<(), ()>
where
    W: futures_util::Sink<Message> + Unpin,
{
    for n in 1u64.. {
        let message = Message::text(text.replace("{n}", &n.to_string()));
        sink.send(message).await.map_err(|_| ())?;
        tokio::time::sleep(every).await;
    }
    Ok(())
}
