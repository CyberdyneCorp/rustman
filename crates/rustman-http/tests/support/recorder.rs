//! A loopback HTTP/1.1 server that records when each request arrived, for
//! tests that count requests or measure the gaps between them.
//!
//! | Route | Response |
//! |-------|----------|
//! | `/ok` | `200 ok` |
//! | `/delay?ms=<n>` | `200 ok` after `<n>` milliseconds |
//! | `/redirect?to=<url>&status=<code>` | `<code>` (default `302`) to `<url>` |
//! | `/limited?retry_after=<v>` | `429` with `Retry-After: <v>` (none when empty) |

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bytes::Bytes;
use http::{Request, Response, StatusCode, header};
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// One request the server received.
#[derive(Clone, Debug)]
pub struct Hit {
    pub at: Instant,
    pub method: String,
    pub path: String,
    /// The `Authorization` header, when one was sent.
    pub authorization: Option<String>,
    /// Every header received.
    pub headers: http::HeaderMap,
}

/// A running recorder; stops when dropped.
pub struct Recorder {
    addr: SocketAddr,
    hits: Arc<Mutex<Vec<Hit>>>,
    task: JoinHandle<()>,
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Recorder {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let log = hits.clone();
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let log = log.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| handle(req, log.clone()));
                    let _ = http1::Builder::new()
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

    pub fn hits(&self) -> Vec<Hit> {
        self.hits.lock().unwrap().clone()
    }
}

async fn handle(
    req: Request<Incoming>,
    log: Arc<Mutex<Vec<Hit>>>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    log.lock().unwrap().push(Hit {
        at: Instant::now(),
        method: req.method().to_string(),
        path: req.uri().path().to_owned(),
        authorization: req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        headers: req.headers().clone(),
    });
    let query = req.uri().query().unwrap_or_default();
    let param = |name: &str| {
        form_urlencoded::parse(query.as_bytes())
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
            .unwrap_or_default()
    };
    let response = match req.uri().path() {
        "/ok" => Response::new(Full::new(Bytes::from_static(b"ok"))),
        "/delay" => {
            let ms = param("ms").parse().unwrap_or(0);
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            Response::new(Full::new(Bytes::from_static(b"ok")))
        }
        "/redirect" => Response::builder()
            .status(param("status").parse::<u16>().unwrap_or(302))
            .header(header::LOCATION, param("to"))
            .body(Full::default())
            .unwrap(),
        "/limited" => limited(&param("retry_after")),
        _ => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::default())
            .unwrap(),
    };
    Ok(response)
}

fn limited(retry_after: &str) -> Response<Full<Bytes>> {
    let mut response = Response::builder().status(StatusCode::TOO_MANY_REQUESTS);
    if !retry_after.is_empty() {
        response = response.header(header::RETRY_AFTER, retry_after);
    }
    response.body(Full::default()).unwrap()
}
