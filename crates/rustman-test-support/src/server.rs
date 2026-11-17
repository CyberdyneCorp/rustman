//! An in-process hyper server on `127.0.0.1` and, where IPv6 works, on `::1`
//! with the same port, so `localhost` connects at once whichever address the
//! resolver lists first.
//!
//! Cleartext servers speak HTTP/1.1 and h2c (prior knowledge); TLS servers
//! negotiate `h2`/`http/1.1` by ALPN and can require a client certificate.

use std::net::SocketAddr;
use std::sync::Arc;

use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use rustls::ServerConfig;
use rustls::server::WebPkiClientVerifier;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use crate::certs::{TestCerts, certs_from_pem, key_from_pem};
use crate::routes;

/// What a TLS test server offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tls {
    /// ALPN `h2` and `http/1.1`, signed by the test CA.
    H2AndHttp1,
    /// ALPN `http/1.1` only.
    Http1Only,
    /// Requires a client certificate signed by the test CA.
    ClientAuth,
    /// Asks for a client certificate but accepts none.
    OptionalClientAuth,
    /// The self-signed certificate.
    SelfSigned,
}

/// A running server; stops when dropped.
pub struct TestServer {
    addr: SocketAddr,
    scheme: &'static str,
    task: JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl TestServer {
    /// Cleartext HTTP/1.1 and h2c.
    pub async fn http() -> Self {
        let listener = bind().await;
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(serve(tcp));
            }
        });
        Self {
            addr,
            scheme: "http",
            task,
        }
    }

    /// HTTPS with the given certificate and ALPN setup.
    pub async fn https(certs: &TestCerts, tls: Tls) -> Self {
        let acceptor = TlsAcceptor::from(Arc::new(server_config(certs, tls)));
        let listener = bind().await;
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    if let Ok(stream) = acceptor.accept(tcp).await {
                        serve(stream).await;
                    }
                });
            }
        });
        Self {
            addr,
            scheme: "https",
            task,
        }
    }

    /// Accepts TCP connections and never sends a byte: a TLS handshake or an
    /// HTTP request on it hangs.
    pub async fn stalled(scheme: &'static str) -> Self {
        let listener = bind().await;
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((tcp, _)) = listener.accept().await {
                held.push(tcp);
            }
        });
        Self { addr, scheme, task }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// `<scheme>://localhost:<port><path>`.
    pub fn url(&self, path: &str) -> String {
        format!("{}://localhost:{}{path}", self.scheme, self.addr.port())
    }
}

/// Loopback listeners sharing one port: IPv4 always, IPv6 when available.
struct Listener {
    v4: TcpListener,
    v6: Option<TcpListener>,
}

impl Listener {
    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.v4.local_addr()
    }

    async fn accept(&self) -> std::io::Result<(TcpStream, SocketAddr)> {
        match &self.v6 {
            Some(v6) => tokio::select! {
                accepted = self.v4.accept() => accepted,
                accepted = v6.accept() => accepted,
            },
            None => self.v4.accept().await,
        }
    }
}

async fn bind() -> Listener {
    // The IPv6 port may be taken; retry with a fresh one a few times.
    for _ in 0..8 {
        let v4 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = v4.local_addr().unwrap().port();
        match TcpListener::bind(("::1", port)).await {
            Ok(v6) => return Listener { v4, v6: Some(v6) },
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(_) => return Listener { v4, v6: None },
        }
    }
    let v4 = TcpListener::bind("127.0.0.1:0").await.unwrap();
    Listener { v4, v6: None }
}

async fn serve<S>(stream: S)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let _ = auto::Builder::new(TokioExecutor::new())
        .serve_connection(TokioIo::new(stream), service_fn(routes::handle))
        .await;
}

pub fn server_config(certs: &TestCerts, tls: Tls) -> ServerConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap();
    let builder = if matches!(tls, Tls::ClientAuth | Tls::OptionalClientAuth) {
        let mut roots = rustls::RootCertStore::empty();
        roots.add_parsable_certificates(certs_from_pem(&certs.ca_pem));
        let mut verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider);
        if tls == Tls::OptionalClientAuth {
            verifier = verifier.allow_unauthenticated();
        }
        builder.with_client_cert_verifier(verifier.build().unwrap())
    } else {
        builder.with_no_client_auth()
    };
    let (cert, key) = if tls == Tls::SelfSigned {
        (&certs.self_signed_cert_pem, &certs.self_signed_key_pem)
    } else {
        (&certs.server_cert_pem, &certs.server_key_pem)
    };
    let mut config = builder
        .with_single_cert(certs_from_pem(cert), key_from_pem(key))
        .unwrap();
    config.alpn_protocols = match tls {
        Tls::Http1Only => vec![b"http/1.1".to_vec()],
        _ => vec![b"h2".to_vec(), b"http/1.1".to_vec()],
    };
    config
}

/// A `TcpStream` connected to `server`.
pub async fn tcp(server: &TestServer) -> TcpStream {
    TcpStream::connect(server.addr()).await.unwrap()
}
