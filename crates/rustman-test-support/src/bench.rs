//! A counting HTTP server for the load-testing tests: it records every
//! request in arrival order, tracks how many are in flight at once, and can
//! stall once, server-side, so latency tests need no client-side timing.
//!
//! | Route | Response |
//! |-------|----------|
//! | `/delay/<ms>` | `200 done` after `<ms>` milliseconds |
//! | `/status/<code>` | an empty response with that status |
//! | `/reflect?k=v...` | `200` JSON object of the query parameters |
//! | anything else | `200 ok` |

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http::{Request, Response, StatusCode, header};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::routes::TestBody;

/// One request as the server saw it.
#[derive(Clone, Debug)]
pub struct BenchHit {
    pub method: String,
    /// The path and query, as sent.
    pub target: String,
    pub at: Instant,
}

impl BenchHit {
    /// The value of query parameter `name`, empty when absent.
    pub fn param(&self, name: &str) -> String {
        let query = self.target.split_once('?').map_or("", |(_, q)| q);
        form_urlencoded::parse(query.as_bytes())
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default()
    }

    /// The path without the query.
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or_default()
    }
}

/// A one-time stall: from the `after`-th request on, every request that
/// arrives within `pause` waits until the pause is over.
#[derive(Clone, Copy, Debug)]
struct Stall {
    after: usize,
    pause: Duration,
    until: Option<Instant>,
}

#[derive(Default)]
struct State {
    hits: Mutex<Vec<BenchHit>>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    stall: Mutex<Option<Stall>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl State {
    /// How long the request that just arrived as hit `index` waits for a
    /// stall to end.
    fn stall_wait(&self, index: usize, now: Instant) -> Duration {
        let mut stall = lock(&self.stall);
        let Some(stall) = stall.as_mut() else {
            return Duration::ZERO;
        };
        if index < stall.after {
            return Duration::ZERO;
        }
        let until = *stall.until.get_or_insert(now + stall.pause);
        until.saturating_duration_since(now)
    }
}

/// A running counting server on `127.0.0.1`; stops when dropped.
pub struct BenchServer {
    addr: SocketAddr,
    state: Arc<State>,
    task: JoinHandle<()>,
}

impl Drop for BenchServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl BenchServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(State::default());
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

    /// `http://127.0.0.1:<port><path>`.
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.addr.port())
    }

    /// `http://127.0.0.1:<port>`.
    pub fn origin(&self) -> String {
        self.url("")
    }

    /// Stall once: the `after`-th request (counting from 0) and every one
    /// arriving within `pause` of it wait until `pause` is over.
    pub fn stall_once(&self, after: usize, pause: Duration) {
        *lock(&self.state.stall) = Some(Stall {
            after,
            pause,
            until: None,
        });
    }

    /// Every request so far, in arrival order.
    pub fn hits(&self) -> Vec<BenchHit> {
        lock(&self.state.hits).clone()
    }

    /// How many requests arrived so far.
    pub fn count(&self) -> usize {
        lock(&self.state.hits).len()
    }

    /// The most requests that were in flight at once.
    pub fn max_in_flight(&self) -> usize {
        self.state.max_in_flight.load(Ordering::SeqCst)
    }
}

async fn handle(
    state: Arc<State>,
    req: Request<Incoming>,
) -> Result<Response<TestBody>, Infallible> {
    let now = Instant::now();
    let index = {
        let mut hits = lock(&state.hits);
        hits.push(BenchHit {
            method: req.method().to_string(),
            target: req
                .uri()
                .path_and_query()
                .map_or_else(|| "/".to_owned(), ToString::to_string),
            at: now,
        });
        hits.len() - 1
    };
    let current = state.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    state.max_in_flight.fetch_max(current, Ordering::SeqCst);
    let wait = state.stall_wait(index, now);
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
    let response = respond(&req).await;
    state.in_flight.fetch_sub(1, Ordering::SeqCst);
    Ok(response)
}

async fn respond(req: &Request<Incoming>) -> Response<TestBody> {
    let path = req.uri().path();
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match segments.as_slice() {
        ["delay", ms] => {
            tokio::time::sleep(Duration::from_millis(ms.parse().unwrap_or(0))).await;
            text(StatusCode::OK, "done")
        }
        ["status", code] => {
            let status = code
                .parse()
                .ok()
                .and_then(|c| StatusCode::from_u16(c).ok())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            text(status, "")
        }
        ["reflect"] => reflect(req),
        _ => text(StatusCode::OK, "ok"),
    }
}

fn reflect(req: &Request<Incoming>) -> Response<TestBody> {
    let query = req.uri().query().unwrap_or_default();
    let params: BTreeMap<String, String> = form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    let body = serde_json::to_string(&params).unwrap();
    Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)).boxed())
        .unwrap()
}

fn text(status: StatusCode, body: &'static str) -> Response<TestBody> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Full::new(Bytes::from_static(body.as_bytes())).boxed())
        .unwrap()
}
