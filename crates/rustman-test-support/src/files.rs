//! A server of fixed files over HTTPS (or cleartext HTTP), counting the
//! requests of each path: plugin downloads (add-plugin-system).
//!
//! | Route | Behavior |
//! |-------|----------|
//! | a served path | `200` with its bytes |
//! | `/redirect?to=<url>` | `302` to `<url>` |
//! | anything else | `404` |

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use bytes::Bytes;
use http::{Request, Response, StatusCode, header};
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use crate::certs::TestCerts;
use crate::server::{Tls, server_config};

#[derive(Default)]
struct State {
    files: Mutex<BTreeMap<String, Bytes>>,
    hits: Mutex<BTreeMap<String, u64>>,
}

/// A running file server; stops when dropped.
pub struct FileServer {
    addr: SocketAddr,
    scheme: &'static str,
    state: Arc<State>,
    task: JoinHandle<()>,
}

impl Drop for FileServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FileServer {
    /// HTTPS with the test CA's `localhost` certificate.
    pub async fn https(certs: &TestCerts) -> Self {
        let acceptor = TlsAcceptor::from(Arc::new(server_config(certs, Tls::Http1Only)));
        Self::start("https", Some(acceptor)).await
    }

    /// Cleartext HTTP.
    pub async fn http() -> Self {
        Self::start("http", None).await
    }

    async fn start(scheme: &'static str, acceptor: Option<TlsAcceptor>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(State::default());
        let served = state.clone();
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let (state, acceptor) = (served.clone(), acceptor.clone());
                tokio::spawn(async move {
                    let service = service_fn(move |req| {
                        let state = state.clone();
                        async move { Ok::<_, Infallible>(handle(&state, &req)) }
                    });
                    let builder = auto::Builder::new(TokioExecutor::new());
                    match acceptor {
                        Some(acceptor) => {
                            if let Ok(tls) = acceptor.accept(tcp).await {
                                let _ = builder.serve_connection(TokioIo::new(tls), service).await;
                            }
                        }
                        None => {
                            let _ = builder.serve_connection(TokioIo::new(tcp), service).await;
                        }
                    }
                });
            }
        });
        Self {
            addr,
            scheme,
            state,
            task,
        }
    }

    /// Serve `bytes` at `path` (replacing what was served there).
    pub fn serve(&self, path: &str, bytes: impl Into<Bytes>) {
        let mut files = self
            .state
            .files
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        files.insert(path.to_owned(), bytes.into());
    }

    /// How many requests `path` received.
    pub fn hits(&self, path: &str) -> u64 {
        let hits = self
            .state
            .hits
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        hits.get(path).copied().unwrap_or(0)
    }

    /// `<scheme>://localhost:<port><path>`.
    pub fn url(&self, path: &str) -> String {
        format!("{}://localhost:{}{path}", self.scheme, self.addr.port())
    }
}

fn handle(state: &State, req: &Request<Incoming>) -> Response<Full<Bytes>> {
    let path = req.uri().path().to_owned();
    *state
        .hits
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(path.clone())
        .or_default() += 1;
    if path == "/redirect" {
        let query = req.uri().query().unwrap_or_default();
        let to = form_urlencoded::parse(query.as_bytes())
            .find(|(k, _)| k == "to")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
        return Response::builder()
            .status(StatusCode::FOUND)
            .header(header::LOCATION, to)
            .body(Full::new(Bytes::new()))
            .unwrap();
    }
    let files = state.files.lock().unwrap_or_else(PoisonError::into_inner);
    match files.get(&path) {
        Some(bytes) => Response::new(Full::new(bytes.clone())),
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::from_static(b"no such file")))
            .unwrap(),
    }
}
