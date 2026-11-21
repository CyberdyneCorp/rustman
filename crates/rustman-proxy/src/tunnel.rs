//! `CONNECT` targets and opaque tunnels (spec: Interception scope, design
//! D4): bytes are relayed both ways with nothing decrypted, and only the
//! endpoint and the byte counts are recorded.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use http::Uri;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

use crate::capture::{Recorded, Tunnel};
use crate::guard::is_refused_ip;
use crate::idle::{Activity, Tracked};
use crate::recorder::Recorder;

/// The `host:port` of a `CONNECT` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    /// Without IPv6 brackets.
    pub host: String,
    pub port: u16,
}

impl Target {
    /// The target of an authority-form URI; the port is required.
    pub fn parse(uri: &Uri) -> Option<Self> {
        let authority = uri.authority()?;
        let host = authority
            .host()
            .trim_start_matches('[')
            .trim_end_matches(']');
        Some(Self {
            host: host.to_ascii_lowercase(),
            port: authority.port_u16()?,
        })
    }

    /// `host:port`, IPv6 hosts in brackets.
    pub fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// A TCP connection to `target` within `timeout`. With `deny_internal`
/// (a client on the network), a target that resolves to any address that
/// is not globally routable is refused. The addresses checked are the ones
/// connected to (one lookup), so DNS rebinding cannot slip past the check.
pub(crate) async fn connect(
    target: &Target,
    timeout: Duration,
    deny_internal: bool,
) -> io::Result<TcpStream> {
    let open = async {
        let address = (target.host.as_str(), target.port);
        let addresses: Vec<SocketAddr> = tokio::net::lookup_host(address).await?.collect();
        if deny_internal && addresses.iter().any(|a| is_refused_ip(a.ip())) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refused: the target resolves to a non-public address",
            ));
        }
        TcpStream::connect(&addresses[..]).await
    };
    match tokio::time::timeout(timeout, open).await {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "connect timeout")),
    }
}

/// Relay `client` and `upstream` until both sides close, or no byte moved
/// either way for `idle_timeout`, then record the tunnel.
pub(crate) async fn relay<C, U>(
    client: C,
    upstream: U,
    target: Target,
    idle_timeout: Duration,
    recorder: &Recorder,
) where
    C: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    let started = SystemTime::now();
    let clock = Instant::now();
    let activity = Activity::new();
    let mut client = Tracked::new(client, &activity);
    let mut upstream = Tracked::new(upstream, &activity);
    let (up, down) = (client.read_count(), upstream.read_count());
    let error = tokio::select! {
        result = tokio::io::copy_bidirectional(&mut client, &mut upstream) => {
            result.err().map(|e| e.to_string())
        }
        () = activity.idle(idle_timeout) => Some(idle_error(idle_timeout)),
    };
    let tunnel = Tunnel {
        host: target.host,
        port: target.port,
        bytes_up: up.load(Ordering::Relaxed),
        bytes_down: down.load(Ordering::Relaxed),
        duration: clock.elapsed(),
        error,
    };
    recorder.record(started, Recorded::Tunnel(tunnel));
}

/// Why a connection closed for lack of traffic.
pub(crate) fn idle_error(limit: Duration) -> String {
    format!("idle timeout: no traffic for {} s", limit.as_secs_f64())
}

/// Record a tunnel that could not be opened.
pub(crate) fn record_failure(target: Target, error: &io::Error, recorder: &Recorder) {
    let tunnel = Tunnel {
        host: target.host,
        port: target.port,
        bytes_up: 0,
        bytes_down: 0,
        duration: Duration::ZERO,
        error: Some(error.to_string()),
    };
    recorder.record(SystemTime::now(), Recorded::Tunnel(tunnel));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_need_a_port_and_lose_ipv6_brackets() {
        let parse = |s: &str| Target::parse(&s.parse::<Uri>().unwrap());
        let target = parse("API.example.com:443").unwrap();
        assert_eq!(
            (target.host.as_str(), target.port),
            ("api.example.com", 443)
        );
        let v6 = parse("[::1]:8443").unwrap();
        assert_eq!(v6.host, "::1");
        assert_eq!(v6.authority(), "[::1]:8443");
        assert!(parse("example.com").is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_tunnel_is_closed_and_recorded_with_the_reason() {
        let store = crate::store::CaptureStore::new(10, 1024);
        let recorder = Recorder {
            store: store.clone(),
            filter: crate::filter::CaptureFilter::new(&crate::filter::FilterRules::default())
                .unwrap(),
        };
        let (client, mut client_peer) = tokio::io::duplex(64);
        let (upstream, _upstream_peer) = tokio::io::duplex(64);
        let target = Target {
            host: "idle.test".into(),
            port: 443,
        };
        let limit = Duration::from_secs(300);
        let relay = relay(client, upstream, target, limit, &recorder);
        tokio::pin!(relay);
        tokio::io::AsyncWriteExt::write_all(&mut client_peer, b"hello")
            .await
            .unwrap();
        let open = tokio::time::timeout(Duration::from_secs(299), &mut relay).await;
        assert!(open.is_err(), "closed before the idle timeout");
        tokio::time::timeout(Duration::from_secs(2), &mut relay)
            .await
            .expect("closed after the idle timeout");
        let captures = store.snapshot();
        let Recorded::Tunnel(tunnel) = &captures[0].recorded else {
            panic!("{captures:?}");
        };
        assert_eq!(tunnel.bytes_up, 5);
        assert_eq!(
            tunnel.error.as_deref(),
            Some("idle timeout: no traffic for 300 s")
        );
    }
}
