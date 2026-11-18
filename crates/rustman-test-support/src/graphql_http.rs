//! A scripted HTTP server for GraphQL execution tests: every request is
//! recorded (method, URI, headers, raw body) and answered by a closure, so
//! a test can assert the exact payload and play partial results, HTML
//! error pages or persisted-query misses.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http::{Request, Response, StatusCode, header};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// One request the server received.
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub method: String,
    /// Path and query string.
    pub uri: String,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The body parsed as JSON; panics when it is not.
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("a JSON body")
    }

    /// The decoded URL query parameters, in order.
    pub fn query_pairs(&self) -> Vec<(String, String)> {
        let query = self.uri.split_once('?').map_or("", |(_, q)| q);
        form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect()
    }

    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// The answer to one request.
#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

impl Reply {
    /// A 200 GraphQL result.
    pub fn graphql(body: Value) -> Self {
        Self {
            status: 200,
            content_type: "application/graphql-response+json",
            body: body.to_string(),
        }
    }

    pub fn raw(status: u16, content_type: &'static str, body: &str) -> Self {
        Self {
            status,
            content_type,
            body: body.to_owned(),
        }
    }
}

type Responder = dyn Fn(&RecordedRequest) -> Reply + Send + Sync;

/// A running server; stops when dropped.
pub struct GraphqlHttpServer {
    addr: SocketAddr,
    hits: Arc<Mutex<Vec<RecordedRequest>>>,
    task: JoinHandle<()>,
}

impl Drop for GraphqlHttpServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl GraphqlHttpServer {
    /// A server answering every request with `respond`.
    pub async fn start(
        respond: impl Fn(&RecordedRequest) -> Reply + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let respond: Arc<Responder> = Arc::new(respond);
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
        self.hits.lock().unwrap().clone()
    }
}

async fn handle(
    hits: Arc<Mutex<Vec<RecordedRequest>>>,
    respond: Arc<Responder>,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
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
    let reply = respond(&recorded);
    hits.lock().unwrap().push(recorded);
    let response = Response::builder()
        .status(StatusCode::from_u16(reply.status).unwrap())
        .header(header::CONTENT_TYPE, reply.content_type)
        .body(Full::new(Bytes::from(reply.body)))
        .unwrap();
    Ok(response)
}
