//! The timed connector: a `tower::Service<Uri>` for the hyper-util client that
//! times DNS, TCP connect and the TLS handshake separately (design D2).

use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use http::Uri;
use hyper::rt::{Read, ReadBufCursor, Write};
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::rt::TokioIo;
use rustls::ClientConfig;
use rustls_pki_types::ServerName;
use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::dns::Dns;
use crate::error::{Phase, RequestError};
use crate::proxy::{ProxyKind, ProxySettings, ProxyUrl};
use crate::timing::{ConnectTimings, FreshConnection};
use crate::tunnel::{self, BoxIo, Io};

/// Everything that shapes how the connector opens connections.
pub(crate) struct ConnectorConfig {
    /// TLS to origin servers (ALPN `h2`, `http/1.1`).
    pub tls: Arc<ClientConfig>,
    /// TLS to `https://` proxies (ALPN `http/1.1`).
    pub proxy_tls: Arc<ClientConfig>,
    pub proxies: ProxySettings,
    /// Tunnel plain HTTP through HTTP proxies instead of forwarding it
    /// (needed for h2c prior knowledge).
    pub tunnel_plain: bool,
    pub timeout: Duration,
}

#[derive(Clone)]
pub(crate) struct Connector {
    inner: Arc<Inner>,
}

struct Inner {
    dns: Arc<Dns>,
    config: ConnectorConfig,
    /// Connections being opened right now (DNS, TCP, tunnel, TLS).
    opening: Arc<AtomicUsize>,
}

impl Connector {
    pub fn new(dns: Arc<Dns>, config: ConnectorConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                dns,
                config,
                opening: Arc::default(),
            }),
        }
    }

    /// How many connections this connector is opening at the moment.
    pub fn opening(&self) -> Arc<AtomicUsize> {
        self.inner.opening.clone()
    }
}

/// Counts one connection attempt for as long as it lives, including when
/// its future is dropped by a timeout.
struct Opening(Arc<AtomicUsize>);

impl Opening {
    fn start(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::AcqRel);
        Self(count.clone())
    }
}

impl Drop for Opening {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl tower_service::Service<Uri> for Connector {
    type Response = TimedConn;
    type Error = RequestError;
    type Future = Pin<Box<dyn Future<Output = Result<TimedConn, RequestError>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let inner = self.inner.clone();
        let opening = Opening::start(&inner.opening);
        Box::pin(async move {
            let _opening = opening;
            let timeout = inner.config.timeout;
            match tokio::time::timeout(timeout, inner.open(&uri)).await {
                Ok(result) => result,
                Err(_) => Err(RequestError::Timeout {
                    after: timeout,
                    phase: Phase::Connect,
                }),
            }
        })
    }
}

/// Where to connect, taken from the request URI.
#[derive(Debug)]
struct Target {
    host: String,
    port: u16,
    tls: bool,
}

impl Target {
    fn from_uri(uri: &Uri) -> Result<Self, RequestError> {
        let tls = match uri.scheme_str() {
            Some("https") => true,
            Some("http") => false,
            other => {
                return Err(RequestError::Invalid(format!(
                    "unsupported URL scheme '{}'",
                    other.unwrap_or_default()
                )));
            }
        };
        let host = uri
            .host()
            .ok_or_else(|| RequestError::Invalid(format!("URL has no host: {uri}")))?;
        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
        Ok(Self { host, port, tls })
    }

    /// `host:port`, with brackets around an IPv6 address.
    fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// A connection to the server or through a proxy, before TLS to the server.
struct Dialed {
    io: BoxIo,
    dns: Option<Duration>,
    /// TCP connect, plus the proxy handshake and tunnel when proxied.
    connect: Duration,
    /// Plain HTTP sent to an HTTP proxy in absolute form.
    forwarding: bool,
}

impl Inner {
    async fn open(&self, uri: &Uri) -> Result<TimedConn, RequestError> {
        let target = Target::from_uri(uri)?;
        let dialed = match self.config.proxies.route(target.tls, &target.host) {
            None => self.dial(&target).await?,
            Some(proxy) => self.dial_proxy(proxy, &target).await?,
        };
        let (io, tls, h2) = if target.tls {
            let started = Instant::now();
            let stream = handshake(&self.config.tls, &target.host, dialed.io).await?;
            let h2 = stream.get_ref().1.alpn_protocol() == Some(b"h2");
            (Box::new(stream) as BoxIo, Some(started.elapsed()), h2)
        } else {
            (dialed.io, None, false)
        };
        let timings = ConnectTimings {
            dns: dialed.dns,
            connect: dialed.connect,
            tls,
            ready_at: Instant::now(),
        };
        Ok(TimedConn {
            io: TokioIo::new(io),
            tls: target.tls,
            h2,
            forwarding: dialed.forwarding,
            fresh: FreshConnection::new(timings),
        })
    }

    async fn dial(&self, target: &Target) -> Result<Dialed, RequestError> {
        let (addrs, dns) = self.resolve(&target.host, target.port).await?;
        let started = Instant::now();
        let tcp = connect_any(&target.host, target.port, &addrs).await?;
        Ok(Dialed {
            io: Box::new(tcp),
            dns,
            connect: started.elapsed(),
            forwarding: false,
        })
    }

    async fn dial_proxy(&self, proxy: &ProxyUrl, target: &Target) -> Result<Dialed, RequestError> {
        let (addrs, dns) = self.resolve(&proxy.host, proxy.port).await?;
        let started = Instant::now();
        let tcp = connect_any(&proxy.host, proxy.port, &addrs)
            .await
            .map_err(|e| RequestError::Proxy(format!("proxy: {e}")))?;
        let (io, forwarding) = self.tunnel(proxy, tcp, target).await?;
        Ok(Dialed {
            io,
            dns,
            connect: started.elapsed(),
            forwarding,
        })
    }

    /// The stream to talk to `target` through `proxy`, and whether requests
    /// are forwarded (plain HTTP) instead of tunneled.
    async fn tunnel(
        &self,
        proxy: &ProxyUrl,
        tcp: TcpStream,
        target: &Target,
    ) -> Result<(BoxIo, bool), RequestError> {
        let mut io: BoxIo = match proxy.kind {
            ProxyKind::Socks5 => {
                let io = tunnel::socks5(tcp, proxy, &target.host, target.port).await?;
                return Ok((io, false));
            }
            ProxyKind::Https => {
                Box::new(handshake(&self.config.proxy_tls, &proxy.host, tcp).await?)
            }
            ProxyKind::Http => Box::new(tcp),
        };
        if !target.tls && !self.config.tunnel_plain {
            return Ok((io, true));
        }
        tunnel::http_connect(&mut io, proxy, &target.authority()).await?;
        Ok((io, false))
    }

    /// The addresses to try, and the DNS time (`None` for an IP literal).
    async fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> Result<(Vec<SocketAddr>, Option<Duration>), RequestError> {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok((vec![SocketAddr::new(ip, port)], None));
        }
        let started = Instant::now();
        let addrs = self.dns.lookup(host, port).await?;
        Ok((addrs, Some(started.elapsed())))
    }
}

async fn handshake<S: Io>(
    config: &Arc<ClientConfig>,
    host: &str,
    stream: S,
) -> Result<TlsStream<S>, RequestError> {
    let name = ServerName::try_from(host.to_owned())
        .map_err(|e| RequestError::Tls(format!("tls: invalid server name {host}: {e}")))?;
    TlsConnector::from(config.clone())
        .connect(name, stream)
        .await
        .map_err(|e| handshake_error(host, &e))
}

/// How long one connection attempt runs alone before the next address is
/// tried alongside it (RFC 8305 recommends 250 ms).
const ATTEMPT_DELAY: Duration = Duration::from_millis(250);

/// Happy Eyeballs, simplified (RFC 8305): try the addresses in resolver
/// order, starting the next one when an attempt fails or has not connected
/// within [`ATTEMPT_DELAY`]; the first connection wins and the others are
/// dropped. Without it, `localhost` on Windows (`::1` first, while many dev
/// servers listen on `127.0.0.1` only) stalls for seconds per request.
async fn connect_any(
    host: &str,
    port: u16,
    addrs: &[SocketAddr],
) -> Result<TcpStream, RequestError> {
    let mut pending = addrs.iter().copied();
    let mut attempts = JoinSet::new();
    let mut last = None;
    start_next(&mut pending, &mut attempts);
    while !attempts.is_empty() {
        tokio::select! {
            Some(done) = attempts.join_next() => match done {
                Ok(Ok(tcp)) => {
                    tcp.set_nodelay(true).ok();
                    return Ok(tcp);
                }
                Ok(Err(e)) => {
                    last = Some(e);
                    start_next(&mut pending, &mut attempts);
                }
                Err(e) => last = Some(io::Error::other(e)),
            },
            () = tokio::time::sleep(ATTEMPT_DELAY), if pending.len() > 0 => {
                start_next(&mut pending, &mut attempts);
            }
        }
    }
    let reason = last.map_or_else(|| "no addresses".to_owned(), |e| e.to_string());
    Err(RequestError::Network(format!(
        "cannot connect to {host}:{port}: {reason}"
    )))
}

fn start_next(
    pending: &mut impl Iterator<Item = SocketAddr>,
    attempts: &mut JoinSet<io::Result<TcpStream>>,
) {
    if let Some(addr) = pending.next() {
        attempts.spawn(TcpStream::connect(addr));
    }
}

fn handshake_error(host: &str, err: &io::Error) -> RequestError {
    let rustls_error = err
        .get_ref()
        .and_then(|e| e.downcast_ref::<rustls::Error>());
    let message = match rustls_error {
        Some(rustls::Error::InvalidCertificate(reason)) => {
            format!("tls: certificate verification failed for {host}: {reason:?}")
        }
        Some(other) => format!("tls: handshake with {host} failed: {other}"),
        None => format!("tls: handshake with {host} failed: {err}"),
    };
    RequestError::Tls(message)
}

/// An open connection that carries its timings to the first response.
pub(crate) struct TimedConn {
    io: TokioIo<BoxIo>,
    /// TLS to the server: a read failing with invalid data is a TLS failure.
    tls: bool,
    h2: bool,
    forwarding: bool,
    fresh: FreshConnection,
}

impl Connection for TimedConn {
    fn connected(&self) -> Connected {
        let connected = Connected::new()
            .proxy(self.forwarding)
            .extra(self.fresh.clone());
        if self.h2 {
            connected.negotiated_h2()
        } else {
            connected
        }
    }
}

/// The prefix of a TLS error after the handshake, such as an alert for a
/// rejected client certificate. rustls reports these as `InvalidData` read
/// errors, and HTTP/2 keeps only their kind and text, so the text carries it.
pub(crate) const TLS_FAILURE: &str = "tls: ";

impl Read for TimedConn {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        let tls = self.tls;
        Pin::new(&mut self.io).poll_read(cx, buf).map_err(|e| {
            if tls && e.kind() == io::ErrorKind::InvalidData {
                io::Error::new(e.kind(), format!("{TLS_FAILURE}{e}"))
            } else {
                e
            }
        })
    }
}

impl Write for TimedConn {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TEST-NET-1 (RFC 5737): never routed, so a connect there either hangs
    /// or fails at once, depending on the host's routing table.
    const UNREACHABLE: &str = "192.0.2.1:9";

    #[tokio::test]
    async fn a_stalled_address_does_not_block_the_next_one() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let good = listener.local_addr().unwrap();
        let addrs = [UNREACHABLE.parse().unwrap(), good];
        let started = Instant::now();
        let tcp = connect_any("localhost", good.port(), &addrs).await.unwrap();
        assert_eq!(tcp.peer_addr().unwrap(), good);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn every_address_failing_reports_the_last_error() {
        // Port 0 is never connectable, so no concurrent test can take it.
        let closed: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let err = connect_any("localhost", closed.port(), &[closed])
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with(&format!("cannot connect to localhost:{}: ", closed.port())),
            "{err}"
        );
    }

    #[tokio::test]
    async fn no_addresses_is_an_error() {
        let err = connect_any("nowhere", 80, &[]).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "cannot connect to nowhere:80: no addresses"
        );
    }

    #[test]
    fn target_defaults_ports_and_strips_ipv6_brackets() {
        let t = Target::from_uri(&"https://example.com/x".parse().unwrap()).unwrap();
        assert_eq!((t.host.as_str(), t.port, t.tls), ("example.com", 443, true));
        let t = Target::from_uri(&"http://[::1]:8080/".parse().unwrap()).unwrap();
        assert_eq!((t.host.as_str(), t.port, t.tls), ("::1", 8080, false));
        assert_eq!(t.authority(), "[::1]:8080");
    }

    #[test]
    fn target_rejects_other_schemes() {
        let err = Target::from_uri(&"ftp://example.com/".parse().unwrap()).unwrap_err();
        assert_eq!(err.to_string(), "unsupported URL scheme 'ftp'");
    }
}
