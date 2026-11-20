//! WebSocket connections shared by the protocol clients (GraphQL
//! subscriptions, JSON-RPC): `tokio-tungstenite` for the handshake and
//! framing over a connection the engine's own connector opens, so DNS,
//! the connect timeout, proxies (HTTP `CONNECT`, HTTPS and SOCKS5) and TLS
//! (`ca_bundle`, client certificate, `verify_tls`) work as for HTTP
//! requests. The handshake carries the request's headers, auth helpers and
//! cookies, as [`request_head`] builds them.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use http::{HeaderMap, HeaderValue, Uri};
use rustman_core::Redactor;
use rustman_core::vars::{Resolved, ResolvedBody, ResolvedRequest};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::{Request, Response};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::engine::{SendOptions, add_credentials, header_pairs};
use crate::error::RequestError;
use crate::raw_stream::open_raw_stream;
use crate::stream::request_head;
use crate::tunnel::BoxIo;

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
    /// What the handshake added on the wire (auth helpers, cookies from
    /// the jar) and the response's credentials: values to redact.
    pub credentials: Redactor,
}

/// The most bytes of a refused upgrade's body a record shows (64 KiB).
pub const MAX_REFUSED_BODY: usize = 64 * 1024;

/// Bytes kept past [`MAX_REFUSED_BODY`] so that a secret crossing that cut
/// is still whole when the caller redacts the body before cutting it.
pub const REFUSED_BODY_SLACK: usize = 4 * 1024;

/// Why a WebSocket did not open. Transport errors are redacted.
#[derive(Clone, Debug)]
pub enum WebSocketError {
    /// DNS, connect (including its timeout), proxy or TLS.
    Transport(RequestError),
    /// The server refused the upgrade or broke the handshake protocol.
    Handshake(HandshakeFailure),
    /// The whole handshake took longer than the caller's limit.
    TimedOut(Duration),
}

/// A failed handshake: the message GraphQL and JSON-RPC show
/// (`WebSocket handshake failed: HTTP 401`), and the response of a refused
/// upgrade when the server sent one.
#[derive(Clone, Debug)]
pub struct HandshakeFailure {
    pub message: String,
    pub refused: Option<Box<RefusedUpgrade>>,
}

impl HandshakeFailure {
    fn new(message: String) -> Self {
        Self {
            message,
            refused: None,
        }
    }
}

impl std::fmt::Display for HandshakeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// The response of a server that answered the upgrade with another status
/// than `101`. Not redacted: callers redact what they show or record,
/// adding `credentials`.
#[derive(Clone, Debug)]
pub struct RefusedUpgrade {
    pub status: u16,
    /// The standard reason phrase of the status (`Unauthorized`), when it
    /// has one: the handshake client does not keep the phrase the server
    /// sent.
    pub reason: Option<String>,
    pub headers: Vec<(String, String)>,
    /// The body bytes that arrived with the response head (the handshake
    /// client reads no further), at most [`MAX_REFUSED_BODY`] plus
    /// [`REFUSED_BODY_SLACK`]: redact it, then cut it to
    /// [`MAX_REFUSED_BODY`].
    pub body: Vec<u8>,
    /// What the handshake added on the wire and the response's
    /// credentials: values to redact.
    pub credentials: Redactor,
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
/// Messages and frames may be up to `max_message_bytes` (GraphQL and
/// JSON-RPC pass the in-memory body cap, realtime sessions their own
/// `limits.max_message_bytes`).
pub async fn open_websocket(
    request: &ResolvedRequest,
    options: &SendOptions,
    subprotocols: Option<&str>,
    limit: Duration,
    max_message_bytes: u64,
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
    let mut credentials = Redactor::new();
    add_credentials(&mut credentials, &headers);
    let config = config(max_message_bytes);
    let shake = async {
        let stream = connect(&uri, options).await?;
        tokio_tungstenite::client_async_with_config(client_request, stream, Some(config))
            .await
            .map_err(|e| handshake_error(e, &credentials))
    };
    let (socket, response) = match tokio::time::timeout(limit, shake).await {
        Ok(Ok(opened)) => opened,
        Ok(Err(WebSocketError::Transport(e))) => {
            return Err(WebSocketError::Transport(e.redacted(&redactor)));
        }
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err(WebSocketError::TimedOut(limit)),
    };
    add_credentials(&mut credentials, response.headers());
    handshake.url.text = with_scheme(&handshake.url.text, UrlScheme::Ws);
    Ok(OpenedWebSocket {
        socket,
        response,
        request: handshake,
        credentials,
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
        .map_err(|e| handshake(format!("invalid WebSocket URL: {e}")))?;
    for (name, value) in headers {
        if !NOT_FORWARDED.contains(&name.as_str()) {
            request.headers_mut().append(name.clone(), value.clone());
        }
    }
    if let Some(offer) = subprotocols {
        let value = HeaderValue::from_str(offer)
            .map_err(|_| handshake(format!("invalid subprotocols '{offer}'")))?;
        request
            .headers_mut()
            .insert("sec-websocket-protocol", value);
    }
    Ok(request)
}

/// The handshake's connection: a raw HTTP/1.1 stream from the engine's
/// connector (an upgrade cannot be forwarded by an HTTP proxy, so proxies
/// always tunnel).
async fn connect(uri: &Uri, options: &SendOptions) -> Result<WsIo, WebSocketError> {
    Ok(WsIo(open_raw_stream(uri, options).await?.into_io()))
}

fn handshake(message: String) -> WebSocketError {
    WebSocketError::Handshake(HandshakeFailure::new(message))
}

/// Why the handshake failed; `credentials` are those the handshake sent.
fn handshake_error(err: WsError, credentials: &Redactor) -> WebSocketError {
    let message = match err {
        WsError::Http(response) => return refused(&response, credentials.clone()),
        WsError::Protocol(e) => format!("WebSocket handshake failed: {e}"),
        other => format!("WebSocket: {other}"),
    };
    handshake(message)
}

/// A refused upgrade, with what the server answered.
fn refused(
    response: &http::Response<Option<Vec<u8>>>,
    mut credentials: Redactor,
) -> WebSocketError {
    let status = response.status();
    add_credentials(&mut credentials, response.headers());
    let mut body = response.body().clone().unwrap_or_default();
    body.truncate(MAX_REFUSED_BODY + REFUSED_BODY_SLACK);
    WebSocketError::Handshake(HandshakeFailure {
        message: format!("WebSocket handshake failed: HTTP {}", status.as_u16()),
        refused: Some(Box::new(RefusedUpgrade {
            status: status.as_u16(),
            reason: status.canonical_reason().map(str::to_owned),
            headers: header_pairs(response.headers()),
            body,
            credentials,
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_upgrade_keeps_its_status_reason_headers_and_body() {
        let response = http::Response::builder()
            .status(401)
            .header("www-authenticate", "Bearer")
            .body(Some(vec![b'x'; MAX_REFUSED_BODY + REFUSED_BODY_SLACK + 10]))
            .unwrap();
        let mut sent = HeaderMap::new();
        sent.insert("cookie", HeaderValue::from_static("sid=JARCOOKIE1a2b"));
        let mut credentials = Redactor::new();
        add_credentials(&mut credentials, &sent);
        let error = handshake_error(WsError::Http(Box::new(response)), &credentials);
        let WebSocketError::Handshake(failure) = error else {
            panic!("a handshake failure");
        };
        assert_eq!(failure.to_string(), "WebSocket handshake failed: HTTP 401");
        let refused = failure.refused.unwrap();
        assert_eq!(refused.status, 401);
        assert_eq!(refused.reason.as_deref(), Some("Unauthorized"));
        assert_eq!(
            refused.headers,
            [("www-authenticate".to_owned(), "Bearer".to_owned())]
        );
        assert_eq!(refused.body.len(), MAX_REFUSED_BODY + REFUSED_BODY_SLACK);
        let shown = refused.credentials.text("echo: sid=JARCOOKIE1a2b");
        assert!(!shown.contains("JARCOOKIE1a2b"), "{shown}");
    }

    #[test]
    fn the_message_cap_comes_from_the_caller() {
        let config = config(1024);
        assert_eq!(config.max_message_size, Some(1024));
        assert_eq!(config.max_frame_size, Some(1024));
    }

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
