//! Opening an HTTP/2 channel to a target (spec "Transport security"):
//! plaintext h2c, or TLS through the shared `rustman-http` `TlsSettings`
//! builder with ALPN `h2` (design D5), so CA bundles, client certificates
//! (mTLS) and `insecure` behave as for HTTP requests.
//!
//! The connection is made by our own connector, which also times the TCP
//! connect and the TLS handshake.

use std::error::Error as _;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use http::Uri;
use hyper_util::rt::TokioIo;
use rustls::ClientConfig;
use rustls_pki_types::ServerName;
use rustman_core::vars::ResolvedGrpc;
use rustman_http::TlsSettings;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_util::either::Either;
use tonic::transport::{Channel, Endpoint};
use tower_service::Service;

use crate::error::GrpcError;
use crate::target::GrpcTarget;

/// How long connecting may take when nothing else is set.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long schema discovery by reflection may take when nothing else is
/// set: connecting, listing and fetching every file.
pub const DEFAULT_SCHEMA_TIMEOUT: Duration = Duration::from_secs(30);

/// ALPN of gRPC over TLS: HTTP/2 only.
const ALPN_H2: &[&[u8]] = &[b"h2"];

/// A target and how to reach it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub target: GrpcTarget,
    /// TLS, as [`GrpcTarget::uses_tls`] decided it.
    pub tls: bool,
    /// Verification, CA bundle and client certificate, when `tls` is on.
    pub tls_settings: TlsSettings,
    pub connect_timeout: Duration,
    /// The limit on schema discovery by reflection, connecting included.
    pub schema_timeout: Duration,
}

/// How long connecting took.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConnectTimings {
    /// The TCP connect.
    pub connect: Option<Duration>,
    /// The TLS handshake.
    pub tls: Option<Duration>,
}

/// An open channel and how long opening it took.
#[derive(Clone, Debug)]
pub struct Connection {
    pub channel: Channel,
    pub timings: ConnectTimings,
}

impl Remote {
    /// `target` with TLS from `tls_enabled` (`tls.enabled`), else from the
    /// target itself, verified against the platform roots.
    pub fn new(target: GrpcTarget, tls_enabled: Option<bool>) -> Self {
        let tls = target.uses_tls(tls_enabled);
        Self {
            target,
            tls,
            tls_settings: TlsSettings::default(),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            schema_timeout: DEFAULT_SCHEMA_TIMEOUT,
        }
    }

    /// Parse `text` and reach it as written.
    pub fn parse(text: &str) -> Result<Self, GrpcError> {
        GrpcTarget::parse(text).map(|t| Self::new(t, None))
    }

    /// The target of a resolved request, with its `tls.enabled`.
    pub fn for_request(resolved: &ResolvedGrpc) -> Result<Self, GrpcError> {
        GrpcTarget::parse(resolved.target()).map(|t| Self::new(t, resolved.tls_enabled))
    }

    /// The same remote with `settings` for TLS.
    #[must_use]
    pub fn with_tls_settings(mut self, settings: TlsSettings) -> Self {
        self.tls_settings = settings;
        self
    }

    /// The same remote with `timeout` on schema discovery.
    #[must_use]
    pub fn with_schema_timeout(mut self, timeout: Duration) -> Self {
        self.schema_timeout = timeout;
        self
    }

    /// The canonical target, the descriptor cache key.
    pub fn key(&self) -> String {
        self.target.canonical(self.tls)
    }

    /// Connect, failing fast when the server cannot be reached.
    pub async fn channel(&self) -> Result<Channel, GrpcError> {
        self.connect().await.map(|c| c.channel)
    }

    /// Connect and time it.
    pub async fn connect(&self) -> Result<Connection, GrpcError> {
        let tls = self.tls_config()?;
        let probe = Probe::default();
        let connector = Connector {
            authority: self.target.authority(),
            server_name: server_name(&self.target.host)?,
            tls,
            probe: probe.clone(),
        };
        let uri = format!("http://{}", self.target.authority());
        let endpoint = Endpoint::from_shared(uri)
            .map_err(|_| GrpcError::Target(self.target.text.clone()))?
            .connect_timeout(self.connect_timeout);
        match endpoint.connect_with_connector(connector).await {
            Ok(channel) => Ok(Connection {
                channel,
                timings: probe.timings(),
            }),
            Err(e) => Err(self.connect_error(&e, &probe)),
        }
    }

    fn tls_config(&self) -> Result<Option<Arc<ClientConfig>>, GrpcError> {
        if !self.tls {
            return Ok(None);
        }
        let config = self
            .tls_settings
            .client_config(ALPN_H2)
            .map_err(|e| GrpcError::Tls {
                target: self.target.text.clone(),
                message: e.to_string(),
            })?;
        Ok(Some(Arc::new(config)))
    }

    fn connect_error(&self, error: &tonic::transport::Error, probe: &Probe) -> GrpcError {
        let target = self.target.text.clone();
        match probe.tls_error() {
            Some(message) => GrpcError::Tls { target, message },
            None => GrpcError::Connect {
                target,
                message: innermost(error),
            },
        }
    }
}

/// The TLS server name of `host`, IPv6 brackets removed.
fn server_name(host: &str) -> Result<ServerName<'static>, GrpcError> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    ServerName::try_from(bare.to_owned()).map_err(|_| GrpcError::Target(host.to_owned()))
}

/// What the connector saw, shared with the caller.
#[derive(Clone, Debug, Default)]
struct Probe(Arc<Mutex<ProbeState>>);

#[derive(Debug, Default)]
struct ProbeState {
    timings: ConnectTimings,
    tls_error: Option<String>,
}

impl Probe {
    fn update(&self, f: impl FnOnce(&mut ProbeState)) {
        f(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner));
    }

    fn timings(&self) -> ConnectTimings {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .timings
    }

    fn tls_error(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .tls_error
            .clone()
    }
}

type Stream = Either<TcpStream, TlsStream<TcpStream>>;

/// Connects over TCP, then TLS when configured, timing both.
#[derive(Clone)]
struct Connector {
    authority: String,
    server_name: ServerName<'static>,
    tls: Option<Arc<ClientConfig>>,
    probe: Probe,
}

impl Connector {
    async fn open(self) -> io::Result<TokioIo<Stream>> {
        let started = Instant::now();
        let tcp = TcpStream::connect(&self.authority).await?;
        tcp.set_nodelay(true)?;
        let connected = Instant::now();
        self.probe
            .update(|p| p.timings.connect = Some(connected - started));
        let Some(config) = self.tls else {
            return Ok(TokioIo::new(Either::Left(tcp)));
        };
        let tls = TlsConnector::from(config)
            .connect(self.server_name, tcp)
            .await
            .inspect_err(|e| self.probe.update(|p| p.tls_error = Some(e.to_string())))?;
        self.probe
            .update(|p| p.timings.tls = Some(connected.elapsed()));
        Ok(TokioIo::new(Either::Right(tls)))
    }
}

impl Service<Uri> for Connector {
    type Response = TokioIo<Stream>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<TokioIo<Stream>>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: Uri) -> Self::Future {
        Box::pin(self.clone().open())
    }
}

/// The message of the innermost cause of `error`, which names the reason
/// (`Connection refused`) where the outer ones say `transport error`.
fn innermost(error: &tonic::transport::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message = cause.to_string();
        source = cause.source();
    }
    message
}
