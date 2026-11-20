//! A scriptable HTTP server for the stress-testing tests: each route
//! degrades in its own way, decided server-side from what it receives, so
//! tests need no client-side timing.
//!
//! The arrival rate of a path is the number of its requests in the last
//! second, this one included.
//!
//! | Route | Behavior |
//! |-------|----------|
//! | `/load[/<name>]?above=N&slow=MS&base=MS&hold=MS` | `200` after `base` ms, plus `slow` ms while the path's rate exceeds `N`, and for `hold` ms after it last did |
//! | `/leak[/<name>]?base=MS&per_sec=MS` | `200` after `base` ms plus `per_sec` ms for every second since the path's first request |
//! | `/throttle?above=N&share=P` | `429` for `P`% of requests while the rate exceeds `N`, `200` otherwise |
//! | `/status/<code>[?retry_after=S]` | that status, with `Retry-After` when asked |
//! | `/hang` | answers after 60 s (for client timeouts) |
//! | `/fall?above=N` | `200`; once the path's rate exceeds `N`, the whole server falls over: every later request and connection is closed without a response |
//! | `/heal?above=N&heal_ms=MS` | `503` while the path's rate exceeds `N` and for `heal_ms` after it last did, `200` otherwise |
//! | `/fuzz/<route>?base=MS` | every fuzz route: answers after `base` ms |
//! | `/fuzz/accept` | `201` to any body |
//! | `/fuzz/strict` | `201` to a JSON body, `400` otherwise |
//! | `/fuzz/nest?fail_depth=N` | `500` to a body nested `N` or more arrays deep, else as `/fuzz/strict` |
//! | `/fuzz/qty` | `500` to a JSON body whose `qty` is a string (`400` from [`StressServer::start_fixed`]), else as `/fuzz/strict` |
//! | `/fuzz/qty-load?above=N` | as `/fuzz/qty` while the path's rate exceeds `N`; alone, a string `qty` gets `400` |
//! | `/fuzz/poison?slow=MS&hold=MS` | as `/fuzz/strict`; a body that is not JSON slows every request by `slow` ms for `hold` ms |
//! | anything else | `200 ok` |
//!
//! Bodies that are not JSON are counted ([`StressServer::malformed`]), and
//! so are those that came with an `Authorization` header.
//!
//! [`StressServer::start_refusing`] starts a server that closes every
//! connection as soon as it is accepted, without a response.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
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

const SECOND: Duration = Duration::from_secs(1);

#[derive(Default)]
struct State {
    /// Arrival instants of the last second, per path.
    arrivals: Mutex<HashMap<String, VecDeque<Instant>>>,
    /// Requests per path since the start.
    counts: Mutex<BTreeMap<String, u64>>,
    /// Per path: until when `/load` stays slow.
    slow_until: Mutex<HashMap<String, Instant>>,
    /// Per path: the first request (for `/leak`).
    first: Mutex<HashMap<String, Instant>>,
    total: AtomicU64,
    /// Until when `/fuzz/poison` slows every request.
    poisoned_until: Mutex<Option<Instant>>,
    malformed: AtomicU64,
    malformed_with_auth: AtomicU64,
    /// When `/fall` made the server fall over.
    down_since: Mutex<Option<Instant>>,
    /// The last request or connection attempt.
    last_arrival: Mutex<Option<Instant>>,
    /// Per path: until when `/heal` answers 503.
    broken_until: Mutex<HashMap<String, Instant>>,
    /// The `/fuzz/qty` bug is fixed: a string `qty` gets `400`.
    fixed: bool,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl State {
    /// Count a request to `path` at `now`; its rate over the last second.
    fn arrive(&self, path: &str, now: Instant) -> usize {
        self.total.fetch_add(1, Ordering::SeqCst);
        *lock(&self.last_arrival) = Some(now);
        *lock(&self.counts).entry(path.to_owned()).or_default() += 1;
        lock(&self.first).entry(path.to_owned()).or_insert(now);
        let mut arrivals = lock(&self.arrivals);
        let queue = arrivals.entry(path.to_owned()).or_default();
        queue.push_back(now);
        while queue
            .front()
            .is_some_and(|at| now.duration_since(*at) > SECOND)
        {
            queue.pop_front();
        }
        queue.len()
    }
}

/// A running stress stub on `127.0.0.1`; stops when dropped.
pub struct StressServer {
    addr: SocketAddr,
    state: Arc<State>,
    task: JoinHandle<()>,
}

impl Drop for StressServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl StressServer {
    pub async fn start() -> Self {
        Self::listen(false, false).await
    }

    /// A server whose `/fuzz/qty` bug is fixed: a string `qty` gets `400`.
    pub async fn start_fixed() -> Self {
        Self::listen(false, true).await
    }

    /// A server that accepts connections and closes them at once.
    pub async fn start_refusing() -> Self {
        Self::listen(true, false).await
    }

    async fn listen(refuse: bool, fixed: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(State {
            fixed,
            ..State::default()
        });
        let shared = state.clone();
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                if refuse || shared.is_down() {
                    shared.total.fetch_add(1, Ordering::SeqCst);
                    *lock(&shared.last_arrival) = Some(Instant::now());
                    drop(tcp);
                    continue;
                }
                let state = shared.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| serve(state.clone(), req));
                    let _ = auto::Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(tcp), service)
                        .await;
                });
            }
        });
        Self { addr, state, task }
    }

    /// `http://127.0.0.1:<port>`.
    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.addr.port())
    }

    /// Requests (or, refusing, connections) received so far.
    pub fn count(&self) -> u64 {
        self.state.total.load(Ordering::SeqCst)
    }

    /// Requests to `path` so far.
    pub fn count_path(&self, path: &str) -> u64 {
        lock(&self.state.counts).get(path).copied().unwrap_or(0)
    }

    /// `/fuzz/*` bodies that were not JSON.
    pub fn malformed(&self) -> u64 {
        self.state.malformed.load(Ordering::SeqCst)
    }

    /// Of [`Self::malformed`], those sent with an `Authorization` header.
    pub fn malformed_with_auth(&self) -> u64 {
        self.state.malformed_with_auth.load(Ordering::SeqCst)
    }

    /// When `/fall` made the server fall over.
    pub fn down_since(&self) -> Option<Instant> {
        *lock(&self.state.down_since)
    }

    /// The last request or connection attempt received.
    pub fn last_arrival(&self) -> Option<Instant> {
        *lock(&self.state.last_arrival)
    }

    /// Until when `/load` requests to `path` were last held slow.
    pub fn slow_until(&self, path: &str) -> Option<Instant> {
        lock(&self.state.slow_until).get(path).copied()
    }
}

impl State {
    fn is_down(&self) -> bool {
        lock(&self.down_since).is_some()
    }
}

/// A server that fell over closes the connection instead of answering.
async fn serve(
    state: Arc<State>,
    req: Request<Incoming>,
) -> Result<Response<TestBody>, std::io::Error> {
    if state.is_down() {
        state.arrive(req.uri().path(), Instant::now());
        return Err(std::io::Error::other("fallen over"));
    }
    match handle(state, req).await {
        Ok(response) => Ok(response),
        Err(never) => match never {},
    }
}

async fn handle(
    state: Arc<State>,
    req: Request<Incoming>,
) -> Result<Response<TestBody>, Infallible> {
    let now = Instant::now();
    let path = req.uri().path().to_owned();
    let rate = state.arrive(&path, now);
    let query: HashMap<String, String> =
        form_urlencoded::parse(req.uri().query().unwrap_or_default().as_bytes())
            .into_owned()
            .collect();
    let number = |name: &str| -> u64 {
        query
            .get(name)
            .and_then(|v| v.parse().ok())
            .unwrap_or_default()
    };
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if let ["fuzz", route] = segments.as_slice() {
        let route = (*route).to_owned();
        return Ok(fuzz(&state, &route, rate, req, &number).await);
    }
    let response = match segments.as_slice() {
        ["load", ..] => {
            let delay = load_delay(&state, &path, now, rate, &number);
            sleep_ms(delay).await;
            text(StatusCode::OK)
        }
        ["leak", ..] => {
            let first = lock(&state.first).get(&path).copied().unwrap_or(now);
            let elapsed = now.duration_since(first).as_secs_f64();
            let extra = (number("per_sec") as f64 * elapsed) as u64;
            sleep_ms(number("base") + extra).await;
            text(StatusCode::OK)
        }
        ["throttle"] => {
            let count = lock(&state.counts).get(&path).copied().unwrap_or(0);
            // `share`% of requests, evenly spread.
            let share = number("share");
            let picked = count * share / 100 != count.saturating_sub(1) * share / 100;
            let throttled = rate as u64 > number("above") && picked;
            text(if throttled {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::OK
            })
        }
        ["status", code] => status(code, query.get("retry_after")),
        ["fall"] => {
            if rate as u64 > number("above") {
                lock(&state.down_since).get_or_insert(now);
            }
            text(StatusCode::OK)
        }
        ["heal"] => {
            let mut broken = lock(&state.broken_until);
            if rate as u64 > number("above") {
                broken.insert(path.clone(), now + Duration::from_millis(number("heal_ms")));
            }
            let sick = broken.get(&path).is_some_and(|until| now < *until);
            text(if sick {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::OK
            })
        }
        ["hang"] => {
            tokio::time::sleep(Duration::from_secs(60)).await;
            text(StatusCode::OK)
        }
        _ => text(StatusCode::OK),
    };
    Ok(response)
}

/// The `/fuzz/<route>` routes: they judge the body.
async fn fuzz(
    state: &State,
    route: &str,
    rate: usize,
    req: Request<Incoming>,
    number: &(dyn Fn(&str) -> u64 + Sync),
) -> Response<TestBody> {
    let with_auth = req.headers().contains_key(header::AUTHORIZATION);
    let body = req
        .into_body()
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    sleep_ms(number("base")).await;
    let valid = serde_json::from_slice::<serde_json::Value>(&body).is_ok();
    if !valid {
        state.malformed.fetch_add(1, Ordering::SeqCst);
        if with_auth {
            state.malformed_with_auth.fetch_add(1, Ordering::SeqCst);
        }
    }
    let judged = if valid {
        StatusCode::CREATED
    } else {
        StatusCode::BAD_REQUEST
    };
    let string_qty = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .is_some_and(|v| v.get("qty").is_some_and(serde_json::Value::is_string));
    match route {
        "accept" => text(StatusCode::CREATED),
        "qty" if string_qty && state.fixed => text(StatusCode::BAD_REQUEST),
        "qty" if string_qty => text(StatusCode::INTERNAL_SERVER_ERROR),
        "qty-load" if string_qty => text(if rate as u64 > number("above") {
            StatusCode::INTERNAL_SERVER_ERROR
        } else {
            StatusCode::BAD_REQUEST
        }),
        "nest" => {
            let depth = body.iter().take_while(|b| **b == b'[').count() as u64;
            let fails = number("fail_depth") > 0 && depth >= number("fail_depth");
            text(if fails {
                StatusCode::INTERNAL_SERVER_ERROR
            } else {
                judged
            })
        }
        "poison" => {
            let now = Instant::now();
            let slow = {
                let mut until = lock(&state.poisoned_until);
                if !valid {
                    *until = Some(now + Duration::from_millis(number("hold")));
                }
                until.is_some_and(|u| now < u)
            };
            if slow {
                sleep_ms(number("slow")).await;
            }
            text(judged)
        }
        _ => text(judged),
    }
}

/// The delay of a `/load` request: `base`, plus `slow` while the path is
/// overloaded or held slow.
fn load_delay(
    state: &State,
    path: &str,
    now: Instant,
    rate: usize,
    number: &dyn Fn(&str) -> u64,
) -> u64 {
    let overloaded = rate as u64 > number("above");
    let mut slow_until = lock(&state.slow_until);
    if overloaded {
        let until = now + Duration::from_millis(number("hold"));
        let entry = slow_until.entry(path.to_owned()).or_insert(until);
        *entry = (*entry).max(until);
    }
    let held = slow_until.get(path).is_some_and(|until| now < *until);
    number("base")
        + if overloaded || held {
            number("slow")
        } else {
            0
        }
}

async fn sleep_ms(ms: u64) {
    if ms > 0 {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
}

fn status(code: &str, retry_after: Option<&String>) -> Response<TestBody> {
    let status = code
        .parse()
        .ok()
        .and_then(|c| StatusCode::from_u16(c).ok())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = Response::builder().status(status);
    if let Some(after) = retry_after {
        builder = builder.header(header::RETRY_AFTER, after.as_str());
    }
    builder
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Full::new(Bytes::new()).boxed())
        .unwrap()
}

fn text(status: StatusCode) -> Response<TestBody> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Full::new(Bytes::from_static(b"ok")).boxed())
        .unwrap()
}
