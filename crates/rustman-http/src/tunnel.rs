//! Proxy tunnels over an open connection: a hand-written HTTP `CONNECT`
//! and SOCKS5 through `tokio-socks` (design D2).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use http::HeaderValue;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_socks::tcp::Socks5Stream;

use crate::error::RequestError;
use crate::proxy::ProxyUrl;

/// Any stream a connection can run over.
pub(crate) trait Io: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

pub(crate) type BoxIo = Box<dyn Io>;

/// The longest proxy response head accepted.
const MAX_HEAD: usize = 16 * 1024;

/// `Proxy-Authorization: Basic …` for a proxy with credentials.
pub(crate) fn proxy_authorization(proxy: &ProxyUrl) -> Option<HeaderValue> {
    let (user, password) = proxy.credentials.as_ref()?;
    let token = STANDARD.encode(format!("{user}:{password}"));
    let mut value = HeaderValue::from_str(&format!("Basic {token}")).ok()?;
    value.set_sensitive(true);
    Some(value)
}

/// Ask an HTTP proxy to open a tunnel to `authority` (`host:port`).
pub(crate) async fn http_connect(
    io: &mut BoxIo,
    proxy: &ProxyUrl,
    authority: &str,
) -> Result<(), RequestError> {
    let mut head = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(auth) = proxy_authorization(proxy) {
        head.push_str("Proxy-Authorization: ");
        head.push_str(auth.to_str().unwrap_or_default());
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    let failed = |reason: String| {
        RequestError::Proxy(format!(
            "proxy: CONNECT to {authority} via {proxy} failed: {reason}"
        ))
    };
    io.write_all(head.as_bytes())
        .await
        .map_err(|e| failed(e.to_string()))?;
    io.flush().await.map_err(|e| failed(e.to_string()))?;
    let status_line = read_status_line(io).await.map_err(failed)?;
    match status_code(&status_line) {
        Some(code) if (200..300).contains(&code) => Ok(()),
        _ => Err(failed(status_line)),
    }
}

/// Read the response head one byte at a time, so no tunneled byte is
/// consumed, and return its status line.
async fn read_status_line(io: &mut BoxIo) -> Result<String, String> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HEAD {
            return Err("response head too large".into());
        }
        let byte = io.read_u8().await.map_err(|e| match e.kind() {
            std::io::ErrorKind::UnexpectedEof => "connection closed by the proxy".to_owned(),
            _ => e.to_string(),
        })?;
        head.push(byte);
    }
    let head = String::from_utf8_lossy(&head);
    Ok(head.lines().next().unwrap_or_default().trim().to_owned())
}

fn status_code(status_line: &str) -> Option<u16> {
    let mut parts = status_line.split_whitespace();
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse().ok()
}

/// Open a SOCKS5 tunnel to `host:port` over `tcp`; the proxy resolves `host`.
pub(crate) async fn socks5(
    tcp: TcpStream,
    proxy: &ProxyUrl,
    host: &str,
    port: u16,
) -> Result<BoxIo, RequestError> {
    let target = (host, port);
    let stream = match &proxy.credentials {
        Some((user, password)) => {
            Socks5Stream::connect_with_password_and_socket(tcp, target, user, password).await
        }
        None => Socks5Stream::connect_with_socket(tcp, target).await,
    };
    let stream = stream.map_err(|e| {
        RequestError::Proxy(format!(
            "proxy: SOCKS5 connect to {host}:{port} via {proxy} failed: {e}"
        ))
    })?;
    Ok(Box::new(stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy(text: &str) -> ProxyUrl {
        ProxyUrl::parse(text).unwrap()
    }

    #[test]
    fn status_codes_come_from_http_status_lines() {
        assert_eq!(
            status_code("HTTP/1.1 200 Connection established"),
            Some(200)
        );
        assert_eq!(status_code("HTTP/1.0 407 Auth"), Some(407));
        assert_eq!(status_code("SSH-2.0-OpenSSH"), None);
        assert_eq!(status_code(""), None);
    }

    #[test]
    fn credentials_become_a_basic_header() {
        let value = proxy_authorization(&proxy("http://ana:pw@p:1")).unwrap();
        assert_eq!(value, "Basic YW5hOnB3");
        assert!(value.is_sensitive());
        assert!(proxy_authorization(&proxy("http://p:1")).is_none());
    }

    async fn connect_against(reply: &'static [u8]) -> (Result<(), RequestError>, Vec<u8>) {
        let (client, mut server) = tokio::io::duplex(4096);
        let mut io: BoxIo = Box::new(client);
        let serve = tokio::spawn(async move {
            let mut request = vec![0; 1024];
            let n = server.read(&mut request).await.unwrap();
            server.write_all(reply).await.unwrap();
            request.truncate(n);
            request
        });
        let result = http_connect(&mut io, &proxy("http://u:p@proxy:3128"), "h:443").await;
        (result, serve.await.unwrap())
    }

    #[tokio::test]
    async fn connect_sends_the_authority_and_credentials() {
        let (result, request) = connect_against(b"HTTP/1.1 200 OK\r\n\r\n").await;
        result.unwrap();
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with("CONNECT h:443 HTTP/1.1\r\nHost: h:443\r\n"));
        assert!(request.contains("Proxy-Authorization: Basic dTpw\r\n"));
    }

    #[tokio::test]
    async fn a_refused_connect_is_a_proxy_error() {
        let (result, _) =
            connect_against(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").await;
        let err = result.unwrap_err();
        assert_eq!(
            err.to_string(),
            "proxy: CONNECT to h:443 via http://proxy:3128 failed: \
             HTTP/1.1 407 Proxy Authentication Required"
        );
    }

    #[tokio::test]
    async fn a_closed_proxy_is_reported() {
        let (result, _) = connect_against(b"HTTP/1.1 200").await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .ends_with("failed: connection closed by the proxy")
        );
    }
}
