//! A GraphQL over Server-Sent Events server (distinct connections mode)
//! for subscription tests. Every request is recorded; each one receives
//! `events` `next` events `{"data": {"onMessage": {"n": <i>}}}`, numbered
//! from 1, then what the script's [`SseEnd`] says.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

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
use serde_json::json;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::graphql_http::RecordedRequest;

/// How the server streams.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SseScript {
    pub events: usize,
    pub end: SseEnd,
}

/// What follows the events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SseEnd {
    /// A `complete` event, then the end of the stream.
    Complete,
    /// Nothing: the stream stays open.
    Open,
    /// The end of the stream, without `complete`.
    Close,
    /// A `next` event whose data line never ends: `bytes` bytes of it in
    /// 64 KiB chunks, then the stream stays open.
    Unended { bytes: usize },
}

type Body = StreamBody<BoxStream<'static, Result<Frame<Bytes>, Infallible>>>;

/// A running server; stops when dropped.
pub struct GraphqlSseServer {
    addr: SocketAddr,
    hits: Arc<Mutex<Vec<RecordedRequest>>>,
    task: JoinHandle<()>,
}

impl Drop for GraphqlSseServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl GraphqlSseServer {
    pub async fn start(script: SseScript) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let shared = hits.clone();
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let hits = shared.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| handle(hits.clone(), script, req));
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
    script: SseScript,
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
    hits.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(recorded);
    let response = Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(StreamBody::new(events(script)))
        .expect("a valid response");
    Ok(response)
}

/// The event stream of `script`: a comment first (as servers send to open
/// the stream), the events, then the script's end.
fn events(script: SseScript) -> BoxStream<'static, Result<Frame<Bytes>, Infallible>> {
    let frame = |text: String| Ok(Frame::data(Bytes::from(text)));
    let opening = stream::iter([frame(": stream open\n\n".to_owned())]);
    let next = stream::iter((1..=script.events).map(move |n| {
        let data = json!({"data": {"onMessage": {"n": n}}});
        frame(format!("event: next\ndata: {data}\n\n"))
    }));
    match script.end {
        SseEnd::Complete => {
            let complete = stream::iter([frame("event: complete\ndata:\n\n".to_owned())]);
            opening.chain(next).chain(complete).boxed()
        }
        SseEnd::Open => opening.chain(next).chain(stream::pending()).boxed(),
        SseEnd::Close => opening.chain(next).boxed(),
        SseEnd::Unended { bytes } => {
            const CHUNK: usize = 64 * 1024;
            let start = stream::iter([frame("event: next\ndata: ".to_owned())]);
            let data = stream::iter((0..bytes.div_ceil(CHUNK)).map(move |i| {
                let len = CHUNK.min(bytes - i * CHUNK);
                frame("x".repeat(len))
            }));
            opening
                .chain(next)
                .chain(start)
                .chain(data)
                .chain(stream::pending())
                .boxed()
        }
    }
}
