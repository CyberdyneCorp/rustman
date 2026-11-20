//! A raw HTTP/1.1 byte stream to an origin, opened by the engine's own
//! connector: DNS, the connect timeout, proxies (HTTP `CONNECT`, HTTPS and
//! SOCKS5 tunnels; a plain HTTP proxy is tunnelled too, never asked to
//! forward) and TLS from [`TlsSettings`] (`ca_bundle`, client certificate,
//! `verify_tls`) with ALPN pinned to `http/1.1`.
//!
//! WebSocket handshakes run over it, and so does the stress wire-fault
//! writer, the one caller that writes hand-built HTTP. This module only
//! opens the stream: it neither writes nor parses HTTP.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use http::Uri;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::connect::{Connector, ConnectorConfig};
use crate::dns::Dns;
use crate::engine::SendOptions;
use crate::error::RequestError;
use crate::tls::TlsSettings;
use crate::tunnel::BoxIo;

/// The only ALPN protocol a raw stream offers.
pub const ALPN_HTTP1: &[&[u8]] = &[b"http/1.1"];

/// A connection to an origin: TCP, a proxy tunnel, or TLS over either.
pub struct RawStream(BoxIo);

impl std::fmt::Debug for RawStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RawStream")
    }
}

/// Open a raw stream to the origin of `uri` (`http` or `https`), within
/// `options.connect_timeout`.
pub async fn open_raw_stream(uri: &Uri, options: &SendOptions) -> Result<RawStream, RequestError> {
    let proxy_tls = TlsSettings {
        client_auth: None,
        ..options.tls.clone()
    };
    let config = ConnectorConfig {
        tls: Arc::new(options.tls.client_config(ALPN_HTTP1)?),
        proxy_tls: Arc::new(proxy_tls.client_config(ALPN_HTTP1)?),
        proxies: options.proxy.clone(),
        // A proxy that forwards requests would rewrite or refuse what the
        // caller writes: always tunnel.
        tunnel_plain: true,
        timeout: options.connect_timeout,
    };
    let mut connector = Connector::new(Arc::new(Dns::default()), config);
    let conn = tower_service::Service::call(&mut connector, uri.clone()).await?;
    Ok(RawStream(conn.into_io()))
}

impl AsyncRead for RawStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for RawStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

impl RawStream {
    pub(crate) fn into_io(self) -> BoxIo {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[tokio::test]
    async fn a_raw_stream_carries_bytes_both_ways() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let echo = tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 5];
            tcp.read_exact(&mut buf).await.unwrap();
            tcp.write_all(&buf).await.unwrap();
        });
        let uri: Uri = format!("http://{addr}/").parse().unwrap();
        let mut stream = open_raw_stream(&uri, &SendOptions::default())
            .await
            .unwrap();
        stream.write_all(b"hello").await.unwrap();
        let mut back = [0u8; 5];
        stream.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"hello");
        echo.await.unwrap();
    }

    #[tokio::test]
    async fn a_closed_port_is_a_connect_error() {
        // A bound socket that never listens keeps the port reserved, so no
        // other test can take it, and every connect to it is refused.
        let reserved = tokio::net::TcpSocket::new_v4().unwrap();
        reserved.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = reserved.local_addr().unwrap();
        let uri: Uri = format!("http://{addr}/").parse().unwrap();
        assert!(
            open_raw_stream(&uri, &SendOptions::default())
                .await
                .is_err()
        );
        drop(reserved);
    }
}
