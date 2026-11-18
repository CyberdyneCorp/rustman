//! One hyper-util client per distinct transport setting, with the timeout
//! and timing logic around each request.

use std::error::Error as _;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http::{Request, response};
use http_body_util::Full;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::{CaptureConnection, capture_connection};
use hyper_util::rt::{TokioExecutor, TokioTimer};

use crate::body::{self, BodyOptions, ReadBody};
use crate::connect::{Connector, ConnectorConfig, TLS_FAILURE};
use crate::dns::Dns;
use crate::error::{Phase, RequestError};
use crate::proxy::ProxySettings;
use crate::timing::{ConnectTimings, FreshConnection, Timings};
use crate::tls::{ALPN_HTTP, TlsSettings};

/// ALPN towards an `https://` proxy, which only tunnels HTTP/1.1.
const ALPN_PROXY: &[&[u8]] = &[b"http/1.1"];

/// What makes two requests unable to share a connection pool.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TransportKey {
    pub tls: TlsSettings,
    pub connect_timeout: Duration,
    pub http2_prior_knowledge: bool,
    pub proxy: ProxySettings,
}

pub(crate) struct Transport {
    client: Client<Connector, Full<Bytes>>,
    opening: Arc<AtomicUsize>,
}

/// Which phase one request is in: `connect` while a connection for it is
/// being opened, `response` once it has one (fresh or pooled). Kept per
/// request, since requests share the connector.
#[derive(Default)]
pub(crate) struct PhaseProbe {
    capture: Mutex<Option<CaptureConnection>>,
    /// The connector's count of connections still opening.
    opening: Mutex<Option<Arc<AtomicUsize>>>,
    /// Set once a response head arrived.
    responded: AtomicBool,
}

impl PhaseProbe {
    /// Follow `request` (the latest hop) from now on.
    fn watch(&self, request: &mut Request<Full<Bytes>>, opening: Arc<AtomicUsize>) {
        self.responded.store(false, Ordering::Release);
        *lock(&self.capture) = Some(capture_connection(request));
        *lock(&self.opening) = Some(opening);
    }

    fn mark_responded(&self) {
        self.responded.store(true, Ordering::Release);
    }

    /// `response` when the head arrived, when hyper reports the request's
    /// connection, or when no connection is being opened (the captured
    /// metadata is not always visible yet while the server is slow to
    /// answer). With concurrent requests on one transport, another
    /// request's connection attempt can still make this say `connect`.
    pub fn phase(&self) -> Phase {
        if self.responded.load(Ordering::Acquire) || self.has_connection() {
            return Phase::Response;
        }
        match lock(&self.opening).as_ref() {
            Some(opening) if opening.load(Ordering::Acquire) == 0 => Phase::Response,
            _ => Phase::Connect,
        }
    }

    fn has_connection(&self) -> bool {
        lock(&self.capture)
            .as_ref()
            .is_some_and(|capture| capture.connection_metadata().is_some())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A response read in full, with its timings.
pub(crate) struct Received {
    pub head: response::Parts,
    pub body: ReadBody,
    pub timings: Timings,
}

impl Transport {
    pub fn new(dns: Arc<Dns>, key: &TransportKey) -> Result<Self, RequestError> {
        // The client certificate is for the origin server, never the proxy.
        let proxy_tls = TlsSettings {
            client_auth: None,
            ..key.tls.clone()
        };
        let config = ConnectorConfig {
            tls: Arc::new(key.tls.client_config(ALPN_HTTP)?),
            proxy_tls: Arc::new(proxy_tls.client_config(ALPN_PROXY)?),
            proxies: key.proxy.clone(),
            tunnel_plain: key.http2_prior_knowledge,
            timeout: key.connect_timeout,
        };
        let connector = Connector::new(dns, config);
        let opening = connector.opening();
        let client = Client::builder(TokioExecutor::new())
            .pool_timer(TokioTimer::new())
            .timer(TokioTimer::new())
            .http2_only(key.http2_prior_knowledge)
            .build(connector);
        Ok(Self { client, opening })
    }

    /// Send `request` and read the whole response; `id` names a spilled
    /// body. The caller bounds it with the total timeout, and asks `probe`
    /// which phase the request was in when it ran out.
    pub async fn send(
        &self,
        mut request: Request<Full<Bytes>>,
        options: &BodyOptions,
        id: &str,
        probe: &PhaseProbe,
    ) -> Result<Received, RequestError> {
        probe.watch(&mut request, self.opening.clone());
        let started = Instant::now();
        let response = self.client.request(request).await.map_err(client_error)?;
        probe.mark_responded();
        let head_at = Instant::now();
        let fresh = response
            .extensions()
            .get::<FreshConnection>()
            .and_then(FreshConnection::take);
        let (head, body) = response.into_parts();
        let body = body::read(body, &head.headers, options, id).await?;
        let timings = timings(started, head_at, Instant::now(), fresh);
        Ok(Received {
            head,
            body,
            timings,
        })
    }
}

fn timings(
    started: Instant,
    head_at: Instant,
    done: Instant,
    fresh: Option<ConnectTimings>,
) -> Timings {
    let ready_at = fresh.map_or(started, |c| c.ready_at.max(started));
    Timings {
        dns: fresh.and_then(|c| c.dns),
        connect: fresh.map(|c| c.connect),
        tls: fresh.and_then(|c| c.tls),
        ttfb: head_at.saturating_duration_since(ready_at),
        download: done.saturating_duration_since(head_at),
        total: done.saturating_duration_since(started),
        auth: None,
    }
}

/// The connector's own error when there is one, a TLS error when TLS failed
/// after the handshake (a rejected client certificate), else a network error.
fn client_error(err: hyper_util::client::legacy::Error) -> RequestError {
    let mut source = err.source();
    while let Some(cause) = source {
        if let Some(ours) = cause.downcast_ref::<RequestError>() {
            return ours.clone();
        }
        if let Some(tls) = tls_failure(cause) {
            return RequestError::Tls(tls);
        }
        source = cause.source();
    }
    network_error(&err)
}

/// The message of an I/O error marked as a TLS failure by the connection.
fn tls_failure(err: &(dyn std::error::Error + 'static)) -> Option<String> {
    let io = err.downcast_ref::<std::io::Error>()?;
    let message = io.to_string();
    (io.kind() == std::io::ErrorKind::InvalidData && message.starts_with(TLS_FAILURE))
        .then_some(message)
}

/// `network: <error>: <cause>: ...`
pub(crate) fn network_error(err: &dyn std::error::Error) -> RequestError {
    let mut message = format!("network: {err}");
    let mut source = err.source();
    while let Some(cause) = source {
        message.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    RequestError::Network(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timeout_after_the_response_head_is_in_the_response_phase() {
        let probe = PhaseProbe::default();
        assert_eq!(probe.phase(), Phase::Connect);
        let opening = Arc::new(AtomicUsize::new(1));
        let mut first_hop = Request::new(Full::new(Bytes::new()));
        probe.watch(&mut first_hop, opening.clone());
        assert_eq!(probe.phase(), Phase::Connect);
        // No captured connection metadata, as seen on Windows: the head
        // having arrived is enough.
        probe.mark_responded();
        assert_eq!(probe.phase(), Phase::Response);
        let mut next_hop = Request::new(Full::new(Bytes::new()));
        probe.watch(&mut next_hop, opening.clone());
        assert_eq!(probe.phase(), Phase::Connect);
        // The connection is open, the head has not arrived: a slow server.
        opening.store(0, Ordering::Release);
        assert_eq!(probe.phase(), Phase::Response);
    }

    #[test]
    fn a_reused_connection_has_no_connection_phases() {
        let started = Instant::now();
        let head_at = started + Duration::from_millis(5);
        let done = head_at + Duration::from_millis(2);
        let t = timings(started, head_at, done, None);
        assert_eq!((t.dns, t.connect, t.tls), (None, None, None));
        assert_eq!(t.ttfb, Duration::from_millis(5));
        assert_eq!(t.download, Duration::from_millis(2));
        assert_eq!(t.total, Duration::from_millis(7));
    }

    #[test]
    fn a_fresh_connection_counts_ttfb_from_ready() {
        let started = Instant::now();
        let fresh = ConnectTimings {
            dns: Some(Duration::from_millis(1)),
            connect: Duration::from_millis(1),
            tls: Some(Duration::from_millis(2)),
            ready_at: started + Duration::from_millis(4),
        };
        let head_at = started + Duration::from_millis(10);
        let t = timings(started, head_at, head_at, Some(fresh));
        assert_eq!(t.ttfb, Duration::from_millis(6));
        let sum = t.dns.unwrap() + t.connect.unwrap() + t.tls.unwrap() + t.ttfb + t.download;
        assert!(t.total >= sum);
    }

    #[test]
    fn tls_failures_are_found_inside_io_errors() {
        let io = std::io::Error::new(std::io::ErrorKind::InvalidData, "tls: alert");
        assert_eq!(tls_failure(&io).as_deref(), Some("tls: alert"));
        let plain = std::io::Error::new(std::io::ErrorKind::InvalidData, "bad chunk");
        assert!(tls_failure(&plain).is_none());
        assert!(tls_failure(&std::io::Error::other("tls: other kind")).is_none());
    }

    #[test]
    fn network_errors_list_their_causes() {
        let inner = std::io::Error::other("reset by peer");
        let err = std::io::Error::other(Wrapper(inner));
        let message = network_error(&err).to_string();
        assert_eq!(message, "network: wrapped: reset by peer");
    }

    #[derive(Debug)]
    struct Wrapper(std::io::Error);

    impl std::fmt::Display for Wrapper {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("wrapped")
        }
    }

    impl std::error::Error for Wrapper {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }
}
