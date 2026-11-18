//! Binding and running the listener (spec: Listener lifecycle and bind
//! address, Non-loopback bind warning, Standards-compliant HTTP handling).

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use hyper_util::rt::{TokioExecutor, TokioTimer};
use hyper_util::server::conn::auto;
use time::UtcOffset;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::config::{Limits, ListenerConfig};
use crate::conn::Stops;
use crate::mocks::Mocks;
use crate::store::CaptureStore;
use crate::verifiers::Verifiers;

/// hyper's smallest allowed read buffer.
const MIN_HEAD_BUFFER: usize = 8192;

/// Why the listener did not start.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ListenError {
    #[error("webhook: invalid host {0:?} (use an IP address or localhost)")]
    InvalidHost(String),
    #[error("webhook: binding {0} exposes the listener to the network; pass --allow-remote")]
    RemoteNotAllowed(String),
    #[error("webhook: cannot bind {addr}: {reason}")]
    Bind { addr: SocketAddr, reason: String },
}

/// The socket address `config` asks for, refused when it is reachable from
/// the network without `allow_remote`.
pub fn bind_addr(config: &ListenerConfig) -> Result<SocketAddr, ListenError> {
    let ip = parse_host(&config.host)?;
    if !ip.is_loopback() && !config.allow_remote {
        return Err(ListenError::RemoteNotAllowed(config.host.clone()));
    }
    Ok(SocketAddr::new(ip, config.port))
}

fn parse_host(host: &str) -> Result<IpAddr, ListenError> {
    if host.eq_ignore_ascii_case("localhost") {
        return Ok(IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    bare.parse()
        .map_err(|_| ListenError::InvalidHost(host.to_owned()))
}

/// Whether `addr` is reachable from other machines.
pub fn is_remote(addr: SocketAddr) -> bool {
    !addr.ip().is_loopback()
}

/// `Listening on http://127.0.0.1:9090`.
pub fn banner(addr: SocketAddr) -> String {
    format!("Listening on http://{addr}")
}

/// The warning printed when the listener is reachable from the network.
pub fn remote_warning(addr: SocketAddr) -> String {
    format!("the webhook listener on {addr} is reachable from the network")
}

/// The reason shown after `cannot bind <addr>:`, without OS error numbers.
fn bind_reason(err: &io::Error) -> String {
    match err.kind() {
        io::ErrorKind::AddrInUse => "address in use".to_owned(),
        io::ErrorKind::AddrNotAvailable => "address not available".to_owned(),
        io::ErrorKind::PermissionDenied => "permission denied".to_owned(),
        _ => {
            let text = err.to_string();
            match text.find(" (os error") {
                Some(at) => text[..at].to_owned(),
                None => text,
            }
        }
    }
}

/// What every connection of one listener shares.
pub(crate) struct Shared {
    pub limits: Limits,
    pub store: CaptureStore,
    pub mocks: Mocks,
    pub verifiers: Verifiers,
    pub offset: UtcOffset,
    pub builder: auto::Builder<TokioExecutor>,
}

impl Shared {
    fn new(config: &ListenerConfig, store: CaptureStore, mocks: Mocks) -> Self {
        Self {
            builder: builder(&config.limits),
            limits: config.limits.clone(),
            store,
            mocks,
            verifiers: Verifiers::default(),
            offset: config.offset,
        }
    }

    fn head_buffer(&self) -> usize {
        self.limits.max_header_bytes.max(MIN_HEAD_BUFFER)
    }

    /// How much of a connection's start is kept to read the head again.
    pub fn tap_cap(&self) -> usize {
        self.head_buffer() + MIN_HEAD_BUFFER
    }
}

/// HTTP/1.1 and h2c on one listener, with the head limits and timeouts.
/// HTTP/1 keep-alive is off: one request per connection (design D1).
fn builder(limits: &Limits) -> auto::Builder<TokioExecutor> {
    let mut builder = auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .keep_alive(false)
        .preserve_header_case(true)
        .max_headers(limits.max_headers)
        .max_buf_size(limits.max_header_bytes.max(MIN_HEAD_BUFFER))
        // The connection's own head deadline (conn.rs) answers 408 first;
        // hyper's, which closes without a reply, is only a backstop.
        .header_read_timeout(limits.header_timeout.saturating_mul(2));
    builder
        .http2()
        .timer(TokioTimer::new())
        .max_header_list_size(h2_header_list_size(limits))
        .max_concurrent_streams(limits.max_streams);
    builder
}

/// HPACK counts 32 bytes per field besides its name and value
/// (RFC 7541 section 4.1).
const H2_FIELD_OVERHEAD: usize = 32;

/// The h2 layer answers a header list past this with 431 before the
/// request reaches the service, so nothing would be logged. It is set to
/// twice the header-byte limit plus HPACK's per-field overhead, so a list
/// over the configured limits still reaches the service, which refuses and
/// logs it; only a list past this bound is refused unlogged.
fn h2_header_list_size(limits: &Limits) -> u32 {
    let size = limits
        .max_header_bytes
        .saturating_mul(2)
        .saturating_add(limits.max_headers.saturating_mul(H2_FIELD_OVERHEAD));
    u32::try_from(size).unwrap_or(u32::MAX)
}

/// A bound listener, not yet accepting.
pub struct Listener {
    tcp: TcpListener,
    addr: SocketAddr,
    shared: Shared,
}

impl Listener {
    /// Bind the address of `config`. Captures go to `store`; `mocks` decide
    /// responses.
    pub async fn bind(
        config: &ListenerConfig,
        store: CaptureStore,
        mocks: Mocks,
    ) -> Result<Self, ListenError> {
        let addr = bind_addr(config)?;
        let bind_error = |err: io::Error| ListenError::Bind {
            addr,
            reason: bind_reason(&err),
        };
        let tcp = TcpListener::bind(addr).await.map_err(bind_error)?;
        let addr = tcp.local_addr().map_err(bind_error)?;
        Ok(Self {
            tcp,
            addr,
            shared: Shared::new(config, store, mocks),
        })
    }

    /// Check request signatures with `verifiers` (none by default).
    #[must_use]
    pub fn with_verifiers(mut self, verifiers: Verifiers) -> Self {
        self.shared.verifiers = verifiers;
        self
    }

    /// The bound address, with the actual port when `0` was asked.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn banner(&self) -> String {
        banner(self.addr)
    }

    /// Accept connections until `stop` is cancelled, which also closes the
    /// open ones.
    pub async fn run(self, stop: CancellationToken) {
        let stops = Stops {
            drain: stop.clone(),
            stop,
        };
        self.serve(stops, TaskTracker::new()).await;
    }

    /// Accept connections until `stops.drain` is cancelled; each runs on a
    /// task of `tracker`.
    async fn serve(self, stops: Stops, tracker: TaskTracker) {
        let slots = Arc::new(Semaphore::new(self.shared.limits.max_connections.max(1)));
        let shared = Arc::new(self.shared);
        loop {
            // A free connection slot first: past the limit, new connections
            // wait in the OS backlog.
            let accepted = tokio::select! {
                () = stops.drain.cancelled() => return,
                accepted = accept(&self.tcp, &slots) => accepted,
            };
            match accepted {
                Ok((stream, remote, slot)) => {
                    let _ = stream.set_nodelay(true);
                    let shared = Arc::clone(&shared);
                    let serve = crate::conn::serve(stream, remote, shared, stops.child());
                    tracker.spawn(async move {
                        serve.await;
                        drop(slot);
                    });
                }
                // Out of file descriptors and the like: back off briefly.
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    }

    /// Run on a new task; the handle stops it.
    pub fn spawn(self) -> ListenerHandle {
        let stops = Stops::default();
        let tracker = TaskTracker::new();
        let addr = self.addr;
        let task = tokio::spawn(self.serve(stops.clone(), tracker.clone()));
        ListenerHandle {
            addr,
            stops,
            tracker,
            task,
        }
    }
}

/// The next connection, once a slot is free; the permit holds the slot.
async fn accept(
    tcp: &TcpListener,
    slots: &Arc<Semaphore>,
) -> io::Result<(TcpStream, SocketAddr, OwnedSemaphorePermit)> {
    let slot = Arc::clone(slots)
        .acquire_owned()
        .await
        .map_err(io::Error::other)?;
    let (stream, remote) = tcp.accept().await?;
    Ok((stream, remote, slot))
}

/// A running listener.
#[derive(Debug)]
pub struct ListenerHandle {
    addr: SocketAddr,
    stops: Stops,
    tracker: TaskTracker,
    task: JoinHandle<()>,
}

impl ListenerHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(host: &str, allow_remote: bool) -> ListenerConfig {
        ListenerConfig {
            host: host.into(),
            allow_remote,
            ..ListenerConfig::default()
        }
    }

    #[test]
    fn default_bind_is_loopback_9090() {
        let addr = bind_addr(&ListenerConfig::default()).unwrap();
        assert_eq!(banner(addr), "Listening on http://127.0.0.1:9090");
        assert!(!is_remote(addr));
    }

    #[test]
    fn non_loopback_needs_allow_remote() {
        assert_eq!(
            bind_addr(&config("0.0.0.0", false))
                .unwrap_err()
                .to_string(),
            "webhook: binding 0.0.0.0 exposes the listener to the network; pass --allow-remote"
        );
        let addr = bind_addr(&config("0.0.0.0", true)).unwrap();
        assert!(is_remote(addr));
        assert_eq!(
            remote_warning(addr),
            "the webhook listener on 0.0.0.0:9090 is reachable from the network"
        );
        assert!(bind_addr(&config("[::1]", false)).is_ok());
        assert!(bind_addr(&config("localhost", false)).is_ok());
        assert!(matches!(
            bind_addr(&config("example.com", false)),
            Err(ListenError::InvalidHost(_))
        ));
    }

    #[test]
    fn bind_reasons_drop_os_error_numbers() {
        let in_use = io::Error::from(io::ErrorKind::AddrInUse);
        assert_eq!(bind_reason(&in_use), "address in use");
        let other = io::Error::other("boom (os error 99)");
        assert_eq!(bind_reason(&other), "boom");
    }
}
