//! HTTPS interception of in-scope tunnels (spec: Interception scope,
//! Certificate pinning failures): TLS is terminated with a leaf minted for
//! the `CONNECT` host, HTTP/1.1 or HTTP/2 is served by ALPN, and every
//! request goes to that same host, whatever its `Host` or `:authority`
//! says, so a client cannot steer decrypted traffic out of scope.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use http::uri::{Authority, Scheme};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use rustls::server::Acceptor;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::LazyConfigAcceptor;

use crate::ca::leaf::Minter;
use crate::capture::Recorded;
use crate::forward::{Leg, Origin, Upstream, forward};
use crate::idle::{Activity, Tracked};
use crate::scope::Scope;
use crate::tunnel::Target;

/// Requests an intercepted HTTP/2 connection may have open at once: each
/// holds up to two capture buffers, so fewer than hyper's default keeps a
/// connection's memory in check.
const MAX_CONCURRENT_STREAMS: u32 = 100;

/// The hosts to decrypt and the leaves to decrypt them with.
pub(crate) struct Interception {
    pub scope: Scope,
    pub minter: Minter,
}

/// One intercepted tunnel: where it goes and who opened it.
pub(crate) struct Tunnel {
    pub target: Target,
    pub upstream: Arc<Upstream>,
    /// Bounds the TLS handshake and each request head.
    pub timeout: Duration,
    /// Closes the decrypted connection after this long without traffic.
    pub idle_timeout: Duration,
    /// Opened by a client on the network.
    pub remote: bool,
}

/// Terminate TLS on `io` and forward what the tunnel carries. A handshake
/// the client aborts is recorded as `TLS REJECTED`; one that takes longer
/// than the timeout is dropped.
pub(crate) async fn intercept<I>(io: I, tunnel: Tunnel, minter: &Minter)
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let handshake = async {
        // Not a TLS client: nothing to decrypt or record.
        let start = LazyConfigAcceptor::new(Acceptor::default(), io)
            .await
            .ok()?;
        let config = minter.server_config(&tunnel.target.host).ok()?;
        Some(start.into_stream(config).await)
    };
    let Ok(Some(handshake)) = tokio::time::timeout(tunnel.timeout, handshake).await else {
        return;
    };
    match handshake {
        Ok(tls) => serve(tls, tunnel).await,
        Err(_) => {
            let rejected = Recorded::TlsRejected {
                host: tunnel.target.host,
                port: tunnel.target.port,
            };
            tunnel.upstream.recorder.record(SystemTime::now(), rejected);
        }
    }
}

/// Serve the decrypted connection; a request head that takes longer than
/// the timeout to arrive closes it, so a client cannot hold a connection
/// slot by trickling bytes, and so does a connection (HTTP/1.1 keep-alive
/// or HTTP/2) on which no byte moved for the idle timeout.
async fn serve<S>(tls: S, tunnel: Tunnel)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let Tunnel {
        target,
        upstream,
        timeout,
        idle_timeout,
        remote,
    } = tunnel;
    let Ok(authority) = target.authority().parse::<Authority>() else {
        return;
    };
    let leg = Leg {
        intercepted: true,
        remote,
    };
    let origin = Arc::new(Origin {
        scheme: Scheme::HTTPS,
        authority,
    });
    let service = service_fn(move |request| {
        let origin = Arc::clone(&origin);
        let upstream = Arc::clone(&upstream);
        async move { Ok::<_, Infallible>(forward(request, &origin, leg, &upstream).await) }
    });
    let mut builder = auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(timeout);
    builder
        .http2()
        .timer(TokioTimer::new())
        .max_concurrent_streams(MAX_CONCURRENT_STREAMS);
    let activity = Activity::new();
    let io = TokioIo::new(Tracked::new(tls, &activity));
    tokio::select! {
        _ = builder.serve_connection(io, service) => {}
        () = activity.idle(idle_timeout) => {}
    }
}
