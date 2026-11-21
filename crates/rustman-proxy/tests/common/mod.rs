//! A proxy on a free loopback port and clients that use it: absolute-form
//! HTTP/1.1, `CONNECT` tunnels, and TLS over them with chosen roots.

#![allow(dead_code)]

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http::{Request, Response};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::RootCertStore;
use rustls::pki_types::ServerName;
use rustman_proxy::ca::LocalCa;
use rustman_proxy::{Capture, CaptureStore, Proxy, ProxyConfig};
use rustman_test_support::certs::certs_from_pem;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_util::sync::CancellationToken;

/// A running proxy; stops when dropped.
pub struct Running {
    pub addr: std::net::SocketAddr,
    pub store: CaptureStore,
    cancel: CancellationToken,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Running {
    /// Wait (generously) until the store holds `n` captures.
    pub async fn captures(&self, n: usize) -> Vec<Arc<Capture>> {
        for _ in 0..200 {
            let all = self.store.snapshot();
            if all.len() >= n {
                return all;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("expected {n} captures, got {:#?}", self.store.snapshot());
    }
}

/// A proxy on a free port with `config` (its port is replaced).
pub async fn start(config: ProxyConfig, ca: Option<&LocalCa>) -> Running {
    let config = ProxyConfig { port: 0, ..config };
    let store = CaptureStore::new(config.max_captures, config.max_buffer_bytes);
    let proxy = Proxy::bind(&config, store.clone(), ca).await.unwrap();
    let addr = proxy.local_addr();
    let cancel = CancellationToken::new();
    tokio::spawn(proxy.run(cancel.clone()));
    Running {
        addr,
        store,
        cancel,
    }
}

/// Upstream TLS that trusts the test CA.
pub fn trusting(ca_pem: &str) -> rustman_http::TlsSettings {
    rustman_http::TlsSettings {
        ca_bundle: Some(ca_pem.as_bytes().to_vec()),
        ..rustman_http::TlsSettings::default()
    }
}

/// Send `request` over HTTP/1.1 on `io` and collect the response.
pub async fn send_http1<I>(io: I, request: Request<Full<Bytes>>) -> Response<Bytes>
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io))
        .await
        .unwrap();
    tokio::spawn(conn);
    collect(sender.send_request(request).await.unwrap()).await
}

/// Send `request` over HTTP/2 on `io` and collect the response.
pub async fn send_h2<I>(io: I, request: Request<Full<Bytes>>) -> Response<Bytes>
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, conn) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(io))
            .await
            .unwrap();
    tokio::spawn(conn);
    collect(sender.send_request(request).await.unwrap()).await
}

async fn collect(response: Response<hyper::body::Incoming>) -> Response<Bytes> {
    let (parts, body) = response.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    Response::from_parts(parts, bytes)
}

pub fn get(uri: &str) -> Request<Full<Bytes>> {
    Request::get(uri).body(Full::default()).unwrap()
}

/// A TCP connection to the proxy, tunnelled to `authority` by `CONNECT`.
pub async fn connect(proxy: &Running, authority: &str) -> TcpStream {
    let mut tcp = TcpStream::connect(proxy.addr).await.unwrap();
    let head = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
    tcp.write_all(head.as_bytes()).await.unwrap();
    let mut reply = Vec::new();
    while !reply.ends_with(b"\r\n\r\n") {
        let mut byte = [0u8; 1];
        assert_eq!(tcp.read(&mut byte).await.unwrap(), 1, "proxy closed");
        reply.push(byte[0]);
    }
    let reply = String::from_utf8(reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    tcp
}

/// A TLS client config trusting only the CA in `ca_pem`, offering `alpn`.
pub fn client_tls(ca_pem: &str, alpn: &[&[u8]]) -> TlsConnector {
    let mut roots = RootCertStore::empty();
    roots.add_parsable_certificates(certs_from_pem(ca_pem));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    TlsConnector::from(Arc::new(config))
}

/// A TLS handshake with `host` over `io`.
pub async fn handshake<I>(io: I, connector: &TlsConnector, host: &str) -> io::Result<TlsStream<I>>
where
    I: AsyncRead + AsyncWrite + Unpin,
{
    let name = ServerName::try_from(host.to_owned()).unwrap();
    connector.connect(name, io).await
}

/// A stream counting what is written to it and read from it.
pub struct Counting<S> {
    pub inner: S,
    pub written: Arc<AtomicU64>,
    pub read: Arc<AtomicU64>,
}

impl<S> Counting<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            written: Arc::default(),
            read: Arc::default(),
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Counting<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let poll = Pin::new(&mut self.inner).poll_read(cx, buf);
        let read = (buf.filled().len() - before) as u64;
        self.read.fetch_add(read, Ordering::SeqCst);
        poll
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Counting<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let poll = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &poll {
            self.written.fetch_add(*n as u64, Ordering::SeqCst);
        }
        poll
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
