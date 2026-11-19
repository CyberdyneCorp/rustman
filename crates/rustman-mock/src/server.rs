//! Binding and running the mock (spec: Serve command and bind address,
//! Loopback by default, Request limits, Mock exit codes and shutdown).
//! The bind rules are the webhook inspector's; HTTP/1.1 and h2c are
//! served on one port by `hyper`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use arc_swap::ArcSwap;

use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use rustman_core::Redactor;
use rustman_webhook::Limits;
use rustman_webhook::listener::{self, ListenError};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::config::MockConfig;
use crate::cors;
use crate::faults::Sequences;
use crate::log::{LogEntry, MockEvent};
use crate::overrides::Overrides;
use crate::proxy::Proxy;
use crate::record::Recorder;
use crate::service;
use crate::state::{MockState, load};
use crate::stateful::Store;

/// hyper's smallest allowed read buffer.
const MIN_HEAD_BUFFER: usize = 8192;

/// Why the mock did not start.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ServeError {
    #[error("mock: invalid host {0:?} (use an IP address or localhost)")]
    InvalidHost(String),
    #[error("mock: binding {0} exposes the mock to the network; pass --allow-remote")]
    RemoteNotAllowed(String),
    #[error("mock: cannot bind {addr}: {reason}")]
    Bind { addr: SocketAddr, reason: String },
}

impl From<ListenError> for ServeError {
    fn from(err: ListenError) -> Self {
        match err {
            ListenError::InvalidHost(host) => Self::InvalidHost(host),
            ListenError::RemoteNotAllowed(host) => Self::RemoteNotAllowed(host),
            ListenError::Bind { addr, reason } => Self::Bind { addr, reason },
        }
    }
}

/// The socket address `config` asks for, refused when it is reachable from
/// the network without `allow_remote`.
pub fn bind_addr(config: &MockConfig) -> Result<SocketAddr, ServeError> {
    Ok(listener::bind_addr(&config.listener())?)
}

/// `mock: serving 19 operations on http://127.0.0.1:4010`.
pub fn banner(operations: usize, addr: SocketAddr) -> String {
    let noun = if operations == 1 {
        "operation"
    } else {
        "operations"
    };
    format!("mock: serving {operations} {noun} on http://{addr}")
}

/// The warning printed when the mock is reachable from the network.
pub fn remote_warning(addr: SocketAddr) -> String {
    format!(
        "the mock on {addr} is reachable from the network; it is a development tool, not a security boundary"
    )
}

/// Request log entries and notices buffered for slow subscribers.
const EVENT_BUFFER: usize = 1024;

/// What every connection shares. The loaded document is swapped whole on
/// reload (design D2); the stateful store is kept apart from it, so it
/// survives reloads.
pub(crate) struct Shared {
    pub state: ArcSwap<MockState>,
    pub config: MockConfig,
    pub store: Store,
    pub sequences: Sequences,
    /// CORS is on (explicitly, or for a loopback bind).
    pub cors: bool,
    /// Knows the expected security values; applied to every log entry.
    pub redactor: Redactor,
    pub events: broadcast::Sender<MockEvent>,
    pub proxy: Option<Proxy>,
    pub recorder: OnceLock<Arc<Recorder>>,
    builder: auto::Builder<TokioExecutor>,
}

impl Shared {
    /// Publish `event` to the subscribers, if any.
    pub fn emit(&self, event: MockEvent) {
        let _ = self.events.send(event);
    }

    /// Publish the request log entry `make` builds, only when someone
    /// listens.
    pub fn log(&self, make: impl FnOnce() -> LogEntry) {
        if self.events.receiver_count() > 0 {
            self.emit(MockEvent::Request(Box::new(make())));
        }
    }
}

/// HTTP/1.1 (keep-alive) and h2c on one listener. hyper's own head limits
/// are twice the configured ones, so requests over the configured limits
/// still reach the service, which answers them with problem+json.
fn builder(limits: &Limits) -> auto::Builder<TokioExecutor> {
    let head_bytes = limits.max_header_bytes.saturating_mul(2);
    let mut builder = auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .max_headers(limits.max_headers.saturating_mul(2).max(1))
        .max_buf_size(head_bytes.saturating_add(MIN_HEAD_BUFFER))
        .header_read_timeout(limits.header_timeout);
    builder
        .http2()
        .timer(TokioTimer::new())
        .max_header_list_size(u32::try_from(head_bytes).unwrap_or(u32::MAX))
        .max_concurrent_streams(limits.max_streams);
    builder
}

/// A bound mock, not yet accepting.
pub struct MockServer {
    tcp: TcpListener,
    addr: SocketAddr,
    shared: Arc<Shared>,
}

impl MockServer {
    /// Bind the address of `config` to serve `state`.
    pub async fn bind(config: &MockConfig, state: Arc<MockState>) -> Result<Self, ServeError> {
        let addr = bind_addr(config)?;
        let bind_error = |err: std::io::Error| ServeError::Bind {
            addr,
            reason: listener::bind_reason(&err),
        };
        let tcp = TcpListener::bind(addr).await.map_err(bind_error)?;
        let addr = tcp.local_addr().map_err(bind_error)?;
        let redactor = config
            .security
            .values()
            .fold(Redactor::new(), |r, secret| r.with(secret));
        let shared = Shared {
            builder: builder(&config.limits),
            state: ArcSwap::new(state),
            store: Store::new(config.max_items, config.seed).with_max_bytes(config.max_store_bytes),
            sequences: Sequences::default(),
            cors: cors::enabled(config.cors, addr.ip().is_loopback()),
            redactor,
            events: broadcast::channel(EVENT_BUFFER).0,
            proxy: config.proxy_unmatched.clone().map(Proxy::new),
            recorder: OnceLock::new(),
            config: config.clone(),
        };
        Ok(Self {
            tcp,
            addr,
            shared: Arc::new(shared),
        })
    }

    /// The bound address, with the actual port when `0` was asked.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn banner(&self) -> String {
        banner(self.shared.state.load().operations(), self.addr)
    }

    /// How many operations are served.
    pub fn operations(&self) -> usize {
        self.shared.state.load().operations()
    }

    /// Whether CORS headers are added (explicitly, or for loopback).
    pub fn cors(&self) -> bool {
        self.shared.cors
    }

    /// Save every proxied exchange with `recorder` (`--record`).
    #[must_use]
    pub fn with_recorder(self, recorder: Recorder) -> Self {
        let _ = self.shared.recorder.set(Arc::new(recorder));
        self
    }

    /// The request log and notices from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<MockEvent> {
        self.shared.events.subscribe()
    }

    /// Reloads the served document and overrides from `source`.
    pub fn reloader(&self, source: Reload) -> Reloader {
        Reloader {
            shared: Arc::clone(&self.shared),
            source,
        }
    }

    /// Run on a new task; the handle stops it.
    pub fn spawn(self) -> MockHandle {
        let stops = Stops::default();
        let tracker = TaskTracker::new();
        let addr = self.addr;
        let events = self.shared.events.clone();
        let task = tokio::spawn(self.serve(stops.clone(), tracker.clone()));
        MockHandle {
            addr,
            events,
            stops,
            tracker,
            task,
        }
    }

    async fn serve(self, stops: Stops, tracker: TaskTracker) {
        loop {
            let accepted = tokio::select! {
                () = stops.drain.cancelled() => return,
                accepted = self.tcp.accept() => accepted,
            };
            match accepted {
                Ok((stream, _)) => {
                    let _ = stream.set_nodelay(true);
                    tracker.spawn(connection(stream, Arc::clone(&self.shared), stops.clone()));
                }
                // Out of file descriptors and the like: back off briefly.
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    }
}

/// How connections are told to end: `drain` lets the requests in flight
/// finish (hyper's graceful shutdown), `stop` closes at once.
#[derive(Clone, Debug, Default)]
struct Stops {
    drain: CancellationToken,
    stop: CancellationToken,
}

async fn connection(stream: TcpStream, shared: Arc<Shared>, stops: Stops) {
    let service = {
        let shared = Arc::clone(&shared);
        service_fn(move |req| service::handle(req, Arc::clone(&shared)))
    };
    let conn = shared
        .builder
        .serve_connection(TokioIo::new(stream), service);
    tokio::pin!(conn);
    tokio::select! {
        _ = conn.as_mut() => {}
        () = stops.drain.cancelled() => {
            conn.as_mut().graceful_shutdown();
            tokio::select! {
                _ = conn.as_mut() => {}
                () = stops.stop.cancelled() => {}
            }
        }
        () = stops.stop.cancelled() => {}
    }
}

/// A running mock.
#[derive(Debug)]
pub struct MockHandle {
    addr: SocketAddr,
    events: broadcast::Sender<MockEvent>,
    stops: Stops,
    tracker: TaskTracker,
    task: JoinHandle<()>,
}

impl MockHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// The request log and notices from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<MockEvent> {
        self.events.subscribe()
    }

    /// Stop accepting, close open connections and release the port.
    pub async fn stop(self) {
        self.stops.stop.cancel();
        self.stops.drain.cancel();
        let _ = self.task.await;
        self.tracker.close();
        self.tracker.wait().await;
    }

    /// Stop accepting, let the requests in flight be answered for up to
    /// `grace`, then close what is left and release the port.
    pub async fn finish(self, grace: Duration) {
        self.stops.drain.cancel();
        let _ = self.task.await;
        self.tracker.close();
        let _ = tokio::time::timeout(grace, self.tracker.wait()).await;
        self.stops.stop.cancel();
        self.tracker.wait().await;
    }
}

/// Where the served document and overrides are read from on reload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reload {
    /// The document: a path or URL.
    pub document: String,
    pub allow_remote_refs: bool,
    /// The directory of `*.rman-mock.yaml` files, if any.
    pub mocks_dir: Option<PathBuf>,
}

/// `message`, prefixed with the document unless it already names it.
fn located(document: &str, message: &str) -> String {
    if message.contains(document) {
        message.to_owned()
    } else {
        format!("{document}: {message}")
    }
}

/// Swaps the served state of a running mock (design D2).
#[derive(Clone)]
pub struct Reloader {
    shared: Arc<Shared>,
    pub source: Reload,
}

impl std::fmt::Debug for Reloader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reloader")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl Reloader {
    /// Read the document and overrides again; on success serve them from
    /// the next request on, else keep the current version. Requests in
    /// flight finish with the version they started with, and the stateful
    /// store is kept. The outcome is also published to subscribers.
    pub async fn reload(&self) -> MockEvent {
        let event = match self.build().await {
            Ok(state) => {
                let event = MockEvent::Reloaded {
                    operations: state.operations(),
                    overrides: state.overrides.len(),
                };
                self.shared.state.store(Arc::new(state));
                event
            }
            Err(message) => MockEvent::ReloadFailed(message),
        };
        self.shared.emit(event.clone());
        event
    }

    async fn build(&self) -> Result<MockState, String> {
        let source = &self.source;
        let overrides = match &source.mocks_dir {
            Some(dir) => Overrides::load_dir(dir).map_err(|e| e.to_string())?,
            None => Overrides::default(),
        };
        let state = load(&source.document, source.allow_remote_refs)
            .await
            .map_err(|e| located(&source.document, &e.to_string()))?;
        Ok(state.with_overrides(overrides))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(host: &str, allow_remote: bool) -> MockConfig {
        MockConfig {
            host: host.into(),
            allow_remote,
            ..MockConfig::default()
        }
    }

    #[test]
    fn reload_errors_name_the_document_once() {
        assert_eq!(
            located("api.yaml", "api.yaml: invalid YAML at line 4"),
            "api.yaml: invalid YAML at line 4"
        );
        assert_eq!(
            located("api.yaml", "OpenAPI 2.0 is not supported"),
            "api.yaml: OpenAPI 2.0 is not supported"
        );
    }

    #[test]
    fn the_default_bind_and_banner() {
        let addr = bind_addr(&MockConfig::default()).unwrap();
        assert_eq!(
            banner(19, addr),
            "mock: serving 19 operations on http://127.0.0.1:4010"
        );
        assert_eq!(
            banner(1, addr),
            "mock: serving 1 operation on http://127.0.0.1:4010"
        );
    }

    #[test]
    fn a_network_address_needs_allow_remote() {
        assert_eq!(
            bind_addr(&config("0.0.0.0", false))
                .unwrap_err()
                .to_string(),
            "mock: binding 0.0.0.0 exposes the mock to the network; pass --allow-remote"
        );
        assert!(bind_addr(&config("0.0.0.0", true)).is_ok());
        assert!(bind_addr(&config("localhost", false)).is_ok());
        assert!(matches!(
            bind_addr(&config("example.com", false)),
            Err(ServeError::InvalidHost(_))
        ));
    }
}
