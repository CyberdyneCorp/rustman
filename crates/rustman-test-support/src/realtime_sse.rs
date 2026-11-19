//! A scripted Server-Sent Events server for realtime-session tests. Each
//! connection gets the next [`SseReply`] of the script (the last one
//! repeats): a status, a content type, chunks of the event stream with
//! pauses between them, and what follows (the end of the stream, a stream
//! that stays open, or numbered events forever). Every request is recorded,
//! with its method, headers and body.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use http::{Request, Response, header};
use http_body::Frame;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::graphql_http::RecordedRequest;

/// A piece of the event stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SseChunk {
    /// Raw `text/event-stream` text.
    Text(String),
    /// Wait before the next chunk.
    Pause(Duration),
}

/// What follows the chunks of a reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SseTail {
    /// The stream ends (the connection drops for the client).
    End,
    /// The stream stays open.
    Open,
    /// `data: <text>` with `{n}` replaced by a counter from 1, every
    /// `every`, until the client goes away.
    Forever { text: String, every: Duration },
}

/// The answer to one connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseReply {
    pub status: u16,
    pub content_type: String,
    pub chunks: Vec<SseChunk>,
    pub tail: SseTail,
}

impl SseReply {
    /// A `200 text/event-stream` reply streaming `text`, then ending.
    pub fn events(text: &str) -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream".to_owned(),
            chunks: vec![SseChunk::Text(text.to_owned())],
            tail: SseTail::End,
        }
    }

    /// A reply with `status` and no body (`204 No Content` stops clients).
    pub fn status(status: u16) -> Self {
        Self {
            status,
            content_type: "text/event-stream".to_owned(),
            chunks: Vec::new(),
            tail: SseTail::End,
        }
    }

    /// A `200 text/event-stream` reply sending numbered events forever.
    pub fn forever(text: &str, every: Duration) -> Self {
        Self {
            tail: SseTail::Forever {
                text: text.to_owned(),
                every,
            },
            ..Self::events("")
        }
    }

    #[must_use]
    pub fn content_type(mut self, value: &str) -> Self {
        value.clone_into(&mut self.content_type);
        self
    }

    /// Keep the stream open after the chunks.
    #[must_use]
    pub fn open(mut self) -> Self {
        self.tail = SseTail::Open;
        self
    }

    /// Wait `pause`, then stream `text`.
    #[must_use]
    pub fn then(mut self, pause: Duration, text: &str) -> Self {
        self.chunks.push(SseChunk::Pause(pause));
        self.chunks.push(SseChunk::Text(text.to_owned()));
        self
    }
}

type Body = StreamBody<BoxStream<'static, Result<Frame<Bytes>, Infallible>>>;

/// A running server; stops when dropped.
pub struct RealtimeSseServer {
    addr: SocketAddr,
    hits: Arc<Mutex<Vec<RecordedRequest>>>,
    task: JoinHandle<()>,
}

impl Drop for RealtimeSseServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl RealtimeSseServer {
    /// Serve `script`: connection `i` gets `script[i]`, or the last reply.
    pub async fn start(script: Vec<SseReply>) -> Self {
        assert!(!script.is_empty(), "a script has at least one reply");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let shared = hits.clone();
        let script = Arc::new(script);
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let hits = shared.clone();
                let script = script.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| handle(hits.clone(), script.clone(), req));
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
        self.hits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

async fn handle(
    hits: Arc<Mutex<Vec<RecordedRequest>>>,
    script: Arc<Vec<SseReply>>,
    req: Request<Incoming>,
) -> Result<Response<Body>, Infallible> {
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
    let reply = {
        let mut hits = hits.lock().unwrap_or_else(PoisonError::into_inner);
        hits.push(recorded);
        script[(hits.len() - 1).min(script.len() - 1)].clone()
    };
    let response = Response::builder()
        .status(reply.status)
        .header(header::CONTENT_TYPE, reply.content_type.as_str())
        .header(header::CACHE_CONTROL, "no-cache")
        .body(StreamBody::new(stream_of(reply)))
        .expect("a valid response");
    Ok(response)
}

fn stream_of(reply: SseReply) -> BoxStream<'static, Result<Frame<Bytes>, Infallible>> {
    let chunks = stream::iter(reply.chunks).filter_map(|chunk| async move {
        match chunk {
            SseChunk::Text(text) => Some(Ok(Frame::data(Bytes::from(text)))),
            SseChunk::Pause(pause) => {
                tokio::time::sleep(pause).await;
                None
            }
        }
    });
    match reply.tail {
        SseTail::End => chunks.boxed(),
        SseTail::Open => chunks.chain(stream::pending()).boxed(),
        SseTail::Forever { text, every } => {
            let numbered = stream::iter(1u64..).then(move |n| {
                let event = format!("data: {}\n\n", text.replace("{n}", &n.to_string()));
                async move {
                    tokio::time::sleep(every).await;
                    Ok(Frame::data(Bytes::from(event)))
                }
            });
            chunks.chain(numbered).boxed()
        }
    }
}
