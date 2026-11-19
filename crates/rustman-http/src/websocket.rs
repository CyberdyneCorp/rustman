//! WebSocket connections shared by the protocol clients (GraphQL
//! subscriptions, JSON-RPC): `tokio-tungstenite` for the handshake and
//! framing over a connection the engine's own connector opens, so DNS,
//! the connect timeout, proxies (HTTP `CONNECT`, HTTPS and SOCKS5) and TLS
//! (`ca_bundle`, client certificate, `verify_tls`) work as for HTTP
//! requests. The handshake carries the request's headers, auth helpers and
//! cookies, as [`request_head`] builds them.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use http::{HeaderMap, HeaderValue, Uri};
use rustman_core::vars::{Resolved, ResolvedBody, ResolvedRequest};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::{Request, Response};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::connect::{Connector, ConnectorConfig};
use crate::dns::Dns;
use crate::engine::SendOptions;
use crate::error::RequestError;
use crate::stream::request_head;
use crate::tls::TlsSettings;
use crate::tunnel::BoxIo;

/// The only ALPN protocol a WebSocket handshake can use.
const ALPN_WEBSOCKET: &[&[u8]] = &[b"http/1.1"];

/// Handshake headers the WebSocket client sets itself, or that do not
/// belong in a handshake.
const NOT_FORWARDED: &[&str] = &[
    "host",
    "connection",
    "upgrade",
    "content-length",
    "content-type",
    "sec-websocket-key",
    "sec-websocket-version",
    "sec-websocket-protocol",
    "sec-websocket-extensions",
];

/// The byte stream under a [`WebSocket`]: TCP, a proxy tunnel, or TLS.
pub struct WsIo(BoxIo);

impl AsyncRead for WsIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for WsIo {
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

/// An open WebSocket.
pub type WebSocket = WebSocketStream<WsIo>;

/// A WebSocket after a successful handshake.
pub struct OpenedWebSocket {
    pub socket: WebSocket,
    /// The server's handshake response.
    pub response: Response,
    /// The handshake request as recorded: `GET`, the `ws(s)://` URL and
    /// the request's headers.
    pub request: ResolvedRequest,
}

/// Why a WebSocket did not open. Transport errors are redacted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebSocketError {
    /// DNS, connect (including its timeout), proxy or TLS.
    Transport(RequestError),
    /// The server refused the upgrade or broke the handshake protocol.
    Handshake(String),
    /// The whole handshake took longer than the caller's limit.
    TimedOut(Duration),
}

impl From<RequestError> for WebSocketError {
    fn from(err: RequestError) -> Self {
        Self::Transport(err)
    }
}

/// Which family of schemes a URL should use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UrlScheme {
    Http,
    Ws,
}

/// `url` with its scheme as `http(s)` or `ws(s)`; other schemes are kept.
pub fn with_scheme(url: &str, scheme: UrlScheme) -> String {
    let pairs = [("ws://", "http://"), ("wss://", "https://")];
    for (ws, http) in pairs {
        let (from, to) = match scheme {
            UrlScheme::Http => (ws, http),
            UrlScheme::Ws => (http, ws),
        };
        if let Some(rest) = url.strip_prefix(from) {
            return format!("{to}{rest}");
        }
    }
    url.to_owned()
}

/// Open a WebSocket to the URL of `request` (`http(s)://` and `ws(s)://`
/// both work), offering `subprotocols` (a `Sec-WebSocket-Protocol` value)
/// when given. The handshake as a whole must finish within `limit`.
/// Messages and frames may be up to the in-memory body cap.
pub async fn open_websocket(
    request: &ResolvedRequest,
    options: &SendOptions,
    subprotocols: Option<&str>,
    limit: Duration,
) -> Result<OpenedWebSocket, WebSocketError> {
    let redactor = rustman_core::Redactor::for_request(request);
    let mut handshake = request.clone();
    handshake.method = "GET".to_owned();
    handshake.body = ResolvedBody::None;
    handshake.url = Resolved {
        text: with_scheme(&handshake.url.text, UrlScheme::Http),
        ..handshake.url
    };
    let (uri, headers) = request_head(&handshake, options).await?;
    let ws_url = with_scheme(&uri.to_string(), UrlScheme::Ws);
    let client_request = client_request(&ws_url, &headers, subprotocols)?;
    let config = config(options.body.max_in_memory);
    let shake = async {
        let stream = connect(&uri, options).await?;
        tokio_tungstenite::client_async_with_config(client_request, stream, Some(config))
            .await
            .map_err(handshake_error)
    };
    let (socket, response) = match tokio::time::timeout(limit, shake).await {
        Ok(Ok(opened)) => opened,
        Ok(Err(WebSocketError::Transport(e))) => {
            return Err(WebSocketError::Transport(e.redacted(&redactor)));
        }
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err(WebSocketError::TimedOut(limit)),
    };
    handshake.url.text = with_scheme(&handshake.url.text, UrlScheme::Ws);
    Ok(OpenedWebSocket {
        socket,
        response,
        request: handshake,
    })
}

/// Messages and frames up to `max_bytes` (saturating on 32-bit targets).
fn config(max_bytes: u64) -> WebSocketConfig {
    let max = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    WebSocketConfig::default()
        .max_message_size(Some(max))
        .max_frame_size(Some(max))
}

/// The handshake request: the WebSocket headers, the subprotocols to offer
/// and the request's own headers.
fn client_request(
    url: &str,
    headers: &HeaderMap,
    subprotocols: Option<&str>,
) -> Result<Request, WebSocketError> {
    let mut request = url
        .into_client_request()
        .map_err(|e| WebSocketError::Handshake(format!("invalid WebSocket URL: {e}")))?;
    for (name, value) in headers {
        if !NOT_FORWARDED.contains(&name.as_str()) {
            request.headers_mut().append(name.clone(), value.clone());
        }
    }
    if let Some(offer) = subprotocols {
        let value = HeaderValue::from_str(offer)
            .map_err(|_| WebSocketError::Handshake(format!("invalid subprotocols '{offer}'")))?;
        request
            .headers_mut()
            .insert("sec-websocket-protocol", value);
    }
    Ok(request)
}

/// A connection to the host of `uri` through the engine's connector:
/// proxies, the connect timeout and TLS (`http/1.1` only) apply.
async fn connect(uri: &Uri, options: &SendOptions) -> Result<WsIo, WebSocketError> {
    let proxy_tls = TlsSettings {
        client_auth: None,
        ..options.tls.clone()
    };
    let config = ConnectorConfig {
        tls: Arc::new(options.tls.client_config(ALPN_WEBSOCKET)?),
        proxy_tls: Arc::new(proxy_tls.client_config(ALPN_WEBSOCKET)?),
        proxies: options.proxy.clone(),
        // An upgrade cannot be forwarded by an HTTP proxy: always tunnel.
        tunnel_plain: true,
        timeout: options.connect_timeout,
    };
    let mut connector = Connector::new(Arc::new(Dns::default()), config);
    let conn = tower_service::Service::call(&mut connector, uri.clone()).await?;
    Ok(WsIo(conn.into_io()))
}

fn handshake_error(err: WsError) -> WebSocketError {
    let message = match err {
        WsError::Http(response) => format!(
            "WebSocket handshake failed: HTTP {}",
            response.status().as_u16()
        ),
        WsError::Protocol(e) => format!("WebSocket handshake failed: {e}"),
        other => format!("WebSocket: {other}"),
    };
    WebSocketError::Handshake(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_switch_between_http_and_websocket_schemes() {
        assert_eq!(
            with_scheme("ws://h/graphql", UrlScheme::Http),
            "http://h/graphql"
        );
        assert_eq!(with_scheme("wss://h/g", UrlScheme::Http), "https://h/g");
        assert_eq!(with_scheme("https://h/g", UrlScheme::Ws), "wss://h/g");
        assert_eq!(with_scheme("http://h/g", UrlScheme::Ws), "ws://h/g");
        assert_eq!(with_scheme("http://h/g", UrlScheme::Http), "http://h/g");
    }

    #[test]
    fn the_handshake_keeps_the_request_headers_and_offers_subprotocols() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer t"));
        headers.insert("host", HeaderValue::from_static("other"));
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        let offer = Some("graphql-transport-ws, graphql-ws");
        let request = client_request("ws://h:1/graphql", &headers, offer).unwrap();
        let h = request.headers();
        assert_eq!(h["authorization"], "Bearer t");
        assert_eq!(h["host"], "h:1");
        assert!(h.get("content-type").is_none());
        assert_eq!(
            h["sec-websocket-protocol"],
            "graphql-transport-ws, graphql-ws"
        );
        let plain = client_request("ws://h/", &HeaderMap::new(), None).unwrap();
        assert!(plain.headers().get("sec-websocket-protocol").is_none());
    }
}
