//! One hyper-util client per distinct transport setting, with the timeout
//! and timing logic around each request.

use std::error::Error as _;
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
}

/// Which phase one request is in: `connect` until the client hands it a
/// connection (fresh or pooled), `response` after. Kept per request, since
/// requests share the connector.
#[derive(Default)]
pub(crate) struct PhaseProbe(Mutex<Option<CaptureConnection>>);

impl PhaseProbe {
    /// Follow `request` (the latest hop) from now on.
    fn watch(&self, request: &mut Request<Full<Bytes>>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(capture_connection(request));
    }

    pub fn phase(&self) -> Phase {
        let captured = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        match captured.as_ref() {
            Some(capture) if capture.connection_metadata().is_some() => Phase::Response,
            _ => Phase::Connect,
        }
    }
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
        let client = Client::builder(TokioExecutor::new())
            .pool_timer(TokioTimer::new())
            .timer(TokioTimer::new())
            .http2_only(key.http2_prior_knowledge)
            .build(connector);
        Ok(Self { client })
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
        probe.watch(&mut request);
        let started = Instant::now();
        let response = self.client.request(request).await.map_err(client_error)?;
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
