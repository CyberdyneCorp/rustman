//! The forward proxy (spec: Forward proxy command, Loopback by default):
//! hyper parses every request (design D1); absolute-form requests are
//! forwarded and recorded, `CONNECT` tunnels are intercepted when in scope
//! and relayed opaquely otherwise.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use http::uri::Scheme;
use http::{Method, Request, Response, StatusCode};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use rustman_http::{RawConnector, RequestError, SendOptions};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::body::{Budget, ProxyBody, empty};
use crate::ca::leaf::Minter;
use crate::ca::{CaError, LocalCa};
use crate::config::{BindError, ProxyConfig, bind_reason};
use crate::filter::CaptureFilter;
use crate::forward::{Leg, Origin, Upstream, forward, plain};
use crate::guard::{INTERNAL_REFUSED, is_own_address, is_refused_host, is_refused_ip};
use crate::idle::{Activity, Tracked};
use crate::intercept::{Interception, Tunnel, intercept};
use crate::recorder::Recorder;
use crate::scope::Scope;
use crate::store::CaptureStore;
use crate::tunnel::{self, Target};

/// Why the proxy did not start.
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error(transparent)]
    Bind(#[from] BindError),
    #[error("{0}")]
    Scope(String),
    #[error("{0}")]
    Filter(String),
    #[error("record: invalid --upstream {0:?}: use an origin such as https://api.example.com")]
    Upstream(String),
    #[error("record: --upstream {0:?} is this proxy's own address; requests would loop")]
    UpstreamLoop(String),
    #[error(
        "record: --intercept needs a local CA; run 'rustman record ca init' \
         (then 'rustman record ca install' so clients trust it)"
    )]
    NoCa,
    #[error(transparent)]
    Ca(#[from] CaError),
    #[error("record: upstream TLS settings: {0}")]
    UpstreamTls(#[from] RequestError),
}

/// What every connection shares.
struct Shared {
    upstream: Arc<Upstream>,
    interception: Option<Interception>,
    connect_timeout: Duration,
    /// See [`ProxyConfig::client_timeout`].
    client_timeout: Duration,
    /// See [`ProxyConfig::idle_timeout`].
    idle_timeout: Duration,
    /// Reverse-proxy mode: every request goes to this origin.
    reverse: Option<Origin>,
}

/// One client connection: whether it comes from the network, and its
/// place among the connections served, held until its tunnel ends too.
struct Client {
    remote: bool,
    _permit: OwnedSemaphorePermit,
}

impl Shared {
    /// The minter for `host` when its tunnel is decrypted.
    fn interceptor(&self, host: &str) -> Option<&Minter> {
        self.interception
            .as_ref()
            .filter(|i| i.scope.matches(host))
            .map(|i| &i.minter)
    }
}

/// A bound proxy, not yet accepting.
pub struct Proxy {
    tcp: TcpListener,
    addr: SocketAddr,
    shared: Arc<Shared>,
    connections: Arc<Semaphore>,
    /// Whether a peer is a client on the network (replaced in tests, which
    /// have no second machine).
    is_remote: fn(SocketAddr) -> bool,
}

/// A client on the network: any peer but this machine's loopback.
fn is_network_peer(peer: SocketAddr) -> bool {
    !peer.ip().is_loopback()
}

impl Proxy {
    /// Bind the address of `config`; captures go to `store`. Interception
    /// needs `ca` whenever `config.intercept` is not empty.
    pub async fn bind(
        config: &ProxyConfig,
        store: CaptureStore,
        ca: Option<&LocalCa>,
    ) -> Result<Self, ProxyError> {
        let interception = interception(config, ca)?;
        let filter = CaptureFilter::new(&config.filter).map_err(ProxyError::Filter)?;
        let reverse = config.upstream.as_deref().map(reverse_origin).transpose()?;
        let addr = config.bind_addr()?;
        let bind_error = |err: std::io::Error| BindError::Bind {
            addr,
            reason: bind_reason(&err),
        };
        let tcp = TcpListener::bind(addr).await.map_err(bind_error)?;
        let addr = tcp.local_addr().map_err(bind_error)?;
        if let (Some(origin), Some(url)) = (&reverse, &config.upstream)
            && is_own_address(origin.authority.host(), origin.port(), addr)
        {
            return Err(ProxyError::UpstreamLoop(url.clone()));
        }
        let options = SendOptions {
            tls: config.upstream_tls.clone(),
            connect_timeout: config.connect_timeout,
            ..SendOptions::default()
        };
        let upstream = Arc::new(Upstream {
            direct: RawConnector::new(&options)?,
            guarded: RawConnector::denying(&options, is_refused_ip)?,
            recorder: Recorder { store, filter },
            max_body: config.max_body,
            budget: Budget::new(config.max_buffer_bytes),
            response_timeout: config.response_timeout,
        });
        let shared = Arc::new(Shared {
            upstream,
            interception,
            connect_timeout: config.connect_timeout,
            client_timeout: config.client_timeout,
            idle_timeout: config.idle_timeout,
            reverse,
        });
        let connections = Arc::new(Semaphore::new(config.max_connections.max(1)));
        Ok(Self {
            tcp,
            addr,
            shared,
            connections,
            is_remote: is_network_peer,
        })
    }

    /// The bound address, with the actual port when `0` was asked.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Accept connections until `cancel` fires, at most
    /// `max_connections` at once.
    pub async fn run(self, cancel: CancellationToken) {
        loop {
            let next = async {
                let permit = Arc::clone(&self.connections).acquire_owned().await;
                (permit, self.tcp.accept().await)
            };
            let (permit, accepted) = tokio::select! {
                () = cancel.cancelled() => return,
                next = next => next,
            };
            if let (Ok(permit), Ok((stream, peer))) = (permit, accepted) {
                let client = Arc::new(Client {
                    remote: (self.is_remote)(peer),
                    _permit: permit,
                });
                tokio::spawn(serve_client(stream, Arc::clone(&self.shared), client));
            }
        }
    }
}

fn interception(
    config: &ProxyConfig,
    ca: Option<&LocalCa>,
) -> Result<Option<Interception>, ProxyError> {
    let scope = Scope::new(&config.intercept).map_err(ProxyError::Scope)?;
    if scope.is_empty() {
        return Ok(None);
    }
    let ca = ca.ok_or(ProxyError::NoCa)?;
    Ok(Some(Interception {
        scope,
        minter: Minter::new(ca)?,
    }))
}

/// The answer to an absolute-form URL with `user:password@`: refused, so
/// the password is neither sent in `Host` nor recorded.
const USERINFO_REFUSED: &str = "rustman record: credentials in the URL (user:password@) are \
     refused; send them in an Authorization header\n";

/// The origin of `--upstream`: `http` or `https`, a host, no path.
fn reverse_origin(url: &str) -> Result<Origin, ProxyError> {
    let invalid = || ProxyError::Upstream(url.to_owned());
    let uri: http::Uri = url.parse().map_err(|_| invalid())?;
    let origin = Origin::of(&uri).ok_or_else(invalid)?;
    let web = origin.scheme == Scheme::HTTP || origin.scheme == Scheme::HTTPS;
    let bare = uri.path_and_query().is_none_or(|p| p.as_str() == "/");
    if !web || !bare || origin.has_userinfo() {
        return Err(invalid());
    }
    Ok(origin)
}

/// The lines printed when the proxy starts: its URL and the exports that
/// route a CLI through it.
pub fn banner(addr: SocketAddr) -> [String; 3] {
    [
        format!("Recording proxy on http://{addr}"),
        format!("export HTTP_PROXY=http://{addr}"),
        format!("export HTTPS_PROXY=http://{addr}"),
    ]
}

/// Serve one client connection. A request head that takes longer than the
/// client timeout closes it, and so does a connection on which no byte
/// moved for the idle timeout (a stalled body or keep-alive), so a client
/// cannot hold a connection slot forever. A `CONNECT` tunnel, once
/// upgraded, leaves this connection and has its own idle timeout.
async fn serve_client(stream: TcpStream, shared: Arc<Shared>, client: Arc<Client>) {
    let timeout = shared.client_timeout;
    let idle_timeout = shared.idle_timeout;
    let service = service_fn(move |request| {
        let shared = Arc::clone(&shared);
        let client = Arc::clone(&client);
        async move { Ok::<_, Infallible>(handle(request, shared, client).await) }
    });
    let activity = Activity::new();
    let io = TokioIo::new(Tracked::new(stream, &activity));
    let connection = hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(timeout)
        .serve_connection(io, service)
        .with_upgrades();
    tokio::select! {
        _ = connection => {}
        () = activity.idle(idle_timeout) => {}
    }
}

async fn handle(
    request: Request<Incoming>,
    shared: Arc<Shared>,
    client: Arc<Client>,
) -> Response<ProxyBody> {
    if let Some(origin) = &shared.reverse {
        return reverse(request, origin, &shared.upstream).await;
    }
    // A fast, explicit answer for literal internal targets; names and other
    // spellings are refused by the guarded connector after DNS.
    let internal = request.uri().host().is_some_and(is_refused_host);
    if client.remote && internal {
        return plain(StatusCode::FORBIDDEN, INTERNAL_REFUSED);
    }
    if request.method() == Method::CONNECT {
        return connect(request, shared, client).await;
    }
    let leg = Leg {
        intercepted: false,
        remote: client.remote,
    };
    match Origin::of(request.uri()) {
        Some(origin) if origin.has_userinfo() => plain(StatusCode::BAD_REQUEST, USERINFO_REFUSED),
        Some(origin) if origin.scheme == Scheme::HTTP || origin.scheme == Scheme::HTTPS => {
            forward(request, &origin, leg, &shared.upstream).await
        }
        _ => plain(
            StatusCode::BAD_REQUEST,
            "rustman record is a forward proxy: send absolute-form requests \
             (http://host/path) or CONNECT\n",
        ),
    }
}

/// Reverse-proxy mode (spec: Reverse-proxy mode, design D6): whatever the
/// request names, it goes to the fixed upstream origin.
async fn reverse(
    request: Request<Incoming>,
    origin: &Origin,
    upstream: &Upstream,
) -> Response<ProxyBody> {
    if request.method() == Method::CONNECT {
        return plain(
            StatusCode::METHOD_NOT_ALLOWED,
            "rustman record --upstream is a reverse proxy: CONNECT is not accepted\n",
        );
    }
    // The upstream is the user's own choice, loopback included.
    let leg = Leg {
        intercepted: false,
        remote: false,
    };
    forward(request, origin, leg, upstream).await
}

/// Answer a `CONNECT`, then intercept or relay the tunnel once upgraded.
async fn connect(
    request: Request<Incoming>,
    shared: Arc<Shared>,
    client: Arc<Client>,
) -> Response<ProxyBody> {
    let Some(target) = Target::parse(request.uri()) else {
        return plain(StatusCode::BAD_REQUEST, "CONNECT needs host:port\n");
    };
    if shared.interceptor(&target.host).is_some() {
        let remote = client.remote;
        tokio::spawn(async move {
            let _client = client;
            let Ok(upgraded) = hyper::upgrade::on(request).await else {
                return;
            };
            let Some(minter) = shared.interceptor(&target.host) else {
                return;
            };
            let tunnel = Tunnel {
                target,
                upstream: Arc::clone(&shared.upstream),
                timeout: shared.client_timeout,
                idle_timeout: shared.idle_timeout,
                remote,
            };
            intercept(TokioIo::new(upgraded), tunnel, minter).await;
        });
        return Response::new(empty());
    }
    match tunnel::connect(&target, shared.connect_timeout, client.remote).await {
        Ok(upstream) => {
            tokio::spawn(async move {
                let _client = client;
                if let Ok(upgraded) = hyper::upgrade::on(request).await {
                    let recorder = &shared.upstream.recorder;
                    let client = TokioIo::new(upgraded);
                    let idle = shared.idle_timeout;
                    tunnel::relay(client, upstream, target, idle, recorder).await;
                }
            });
            Response::new(empty())
        }
        Err(err) => {
            let message = format!(
                "rustman record: cannot reach {}: {err}\n",
                target.authority()
            );
            tunnel::record_failure(target, &err, &shared.upstream.recorder);
            plain(StatusCode::BAD_GATEWAY, &message)
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// A proxy that takes every client for one on the network.
    async fn remote_proxy(config: ProxyConfig, ca: Option<&LocalCa>) -> (SocketAddr, CaptureStore) {
        let config = ProxyConfig { port: 0, ..config };
        let store = CaptureStore::new(10, 1 << 20);
        let mut proxy = Proxy::bind(&config, store.clone(), ca).await.unwrap();
        proxy.is_remote = |_| true;
        let addr = proxy.local_addr();
        tokio::spawn(proxy.run(CancellationToken::new()));
        (addr, store)
    }

    /// Write `head` to the proxy and return the response's status line.
    async fn status_line(proxy: SocketAddr, head: &str) -> String {
        let mut tcp = TcpStream::connect(proxy).await.unwrap();
        tcp.write_all(head.as_bytes()).await.unwrap();
        let mut buf = vec![0u8; 512];
        let n = tokio::time::timeout(Duration::from_secs(20), tcp.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        String::from_utf8_lossy(&buf[..n])
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned()
    }

    #[test]
    fn only_loopback_peers_are_local() {
        assert!(!is_network_peer("127.0.0.1:1".parse().unwrap()));
        assert!(!is_network_peer("[::1]:1".parse().unwrap()));
        assert!(is_network_peer("192.168.1.9:1".parse().unwrap()));
    }

    #[tokio::test]
    async fn network_clients_are_refused_internal_targets_on_every_path() {
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = target.local_addr().unwrap().port();
        let config = ProxyConfig {
            intercept: vec!["localhost".into()],
            ..ProxyConfig::default()
        };
        let ca = LocalCa::generate("Rustman Test").unwrap();
        let (proxy, store) = remote_proxy(config, Some(&ca)).await;
        let heads = [
            format!("GET http://127.0.0.1:{port}/ HTTP/1.1\r\nHost: x\r\n\r\n"),
            format!("GET http://[::ffff:127.0.0.1]:{port}/ HTTP/1.1\r\nHost: x\r\n\r\n"),
            format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\nHost: x\r\n\r\n"),
            // In the interception scope: refused before any TLS.
            format!("CONNECT localhost:{port} HTTP/1.1\r\nHost: x\r\n\r\n"),
        ];
        for head in &heads {
            let line = status_line(proxy, head).await;
            assert!(line.contains(" 403 "), "{head:?} -> {line}");
        }
        let accepted = tokio::time::timeout(Duration::from_millis(200), target.accept()).await;
        assert!(accepted.is_err(), "a network client reached loopback");
        assert!(store.is_empty());
    }

    #[tokio::test]
    async fn network_clients_are_refused_private_and_other_non_public_targets() {
        let (proxy, store) = remote_proxy(ProxyConfig::default(), None).await;
        let heads = [
            "GET http://10.255.255.1/ HTTP/1.1\r\nHost: x\r\n\r\n",
            "GET http://192.168.0.1:8080/admin HTTP/1.1\r\nHost: x\r\n\r\n",
            "GET http://100.100.100.200/latest/meta-data HTTP/1.1\r\nHost: x\r\n\r\n",
            "GET http://[fd00:ec2::254]/ HTTP/1.1\r\nHost: x\r\n\r\n",
            "CONNECT 172.16.0.1:443 HTTP/1.1\r\nHost: x\r\n\r\n",
            "CONNECT [::ffff:192.168.1.1]:443 HTTP/1.1\r\nHost: x\r\n\r\n",
            "CONNECT [64:ff9b::a00:1]:443 HTTP/1.1\r\nHost: x\r\n\r\n",
        ];
        for head in heads {
            let line = status_line(proxy, head).await;
            assert!(line.contains(" 403 "), "{head:?} -> {line}");
        }
        assert!(store.is_empty());
    }

    #[tokio::test]
    async fn names_resolving_to_non_public_addresses_are_refused_after_dns() {
        // The decision is made on the resolved addresses, the ones then
        // connected to: a name is never trusted for what it looks like.
        let target = Target {
            host: "localhost".into(),
            port: 9,
        };
        let err = tunnel::connect(&target, Duration::from_secs(5), true)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
    }
}
