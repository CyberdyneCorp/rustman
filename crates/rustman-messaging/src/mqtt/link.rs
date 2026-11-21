//! The byte link to an MQTT broker (messaging-requests "MQTT security"):
//! `mqtt://` (TCP, port 1883), `mqtts://` (rustls, port 8883), and
//! `ws://`/`wss://` (WebSocket with the `mqtt` subprotocol, one packet
//! per binary message). TLS uses the shared `rustman-http` settings.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use futures_util::{SinkExt, StreamExt};
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

use super::PROTOCOL;
use crate::MessagingError;
pub use crate::endpoint::{Endpoint, Transport};

/// The endpoint of an MQTT `broker` (see [`crate::endpoint::mqtt`]).
pub fn endpoint(broker: &str) -> Result<Endpoint, MessagingError> {
    crate::endpoint::mqtt(broker)
}

pub trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

/// An open connection that carries MQTT packets.
pub enum Link {
    Stream(Box<dyn Io>),
    Ws(Box<WebSocketStream<Box<dyn Io>>>),
}

impl Link {
    /// Connect to `endpoint`, with `tls` for `mqtts://` and `wss://`.
    pub async fn open(
        endpoint: &Endpoint,
        tls: Option<Arc<ClientConfig>>,
        timeout: Duration,
    ) -> Result<Self, MessagingError> {
        let open = Self::open_inner(endpoint, tls);
        tokio::time::timeout(timeout, open).await.map_err(|_| {
            MessagingError::broker(
                PROTOCOL,
                format!(
                    "cannot connect to {}: timed out after {} ms",
                    endpoint.address(),
                    timeout.as_millis()
                ),
            )
        })?
    }

    async fn open_inner(
        endpoint: &Endpoint,
        tls: Option<Arc<ClientConfig>>,
    ) -> Result<Self, MessagingError> {
        let address = endpoint.address();
        let tcp = TcpStream::connect((endpoint.host.as_str(), endpoint.port))
            .await
            .map_err(|e| {
                MessagingError::broker(PROTOCOL, format!("cannot connect to {address}: {e}"))
            })?;
        let _ = tcp.set_nodelay(true);
        let stream: Box<dyn Io> = match tls.filter(|_| endpoint.transport.uses_tls()) {
            Some(config) => Box::new(handshake(tcp, config, &endpoint.host, &address).await?),
            None => Box::new(tcp),
        };
        match endpoint.transport {
            Transport::Tcp | Transport::Tls => Ok(Self::Stream(stream)),
            Transport::Ws | Transport::Wss => {
                let url = endpoint.ws_url.as_deref().unwrap_or_default();
                websocket(stream, url).await
            }
        }
    }

    /// Write one encoded packet.
    pub async fn send(&mut self, bytes: BytesMut) -> io::Result<()> {
        match self {
            Self::Stream(stream) => {
                stream.write_all(&bytes).await?;
                stream.flush().await
            }
            Self::Ws(ws) => ws
                .send(Message::Binary(bytes.freeze()))
                .await
                .map_err(io::Error::other),
        }
    }

    /// Append received bytes to `buf`; `false` once the broker closed the
    /// connection. Cancel-safe.
    pub async fn recv(&mut self, buf: &mut BytesMut) -> io::Result<bool> {
        match self {
            Self::Stream(stream) => Ok(stream.read_buf(buf).await? > 0),
            Self::Ws(ws) => loop {
                match ws.next().await {
                    Some(Ok(Message::Binary(data))) => {
                        buf.extend_from_slice(&data);
                        return Ok(true);
                    }
                    Some(Ok(Message::Close(_))) | None => return Ok(false),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(io::Error::other(e)),
                }
            },
        }
    }
}

async fn handshake(
    tcp: TcpStream,
    config: Arc<ClientConfig>,
    host: &str,
    address: &str,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, MessagingError> {
    let name = ServerName::try_from(host.to_owned())
        .map_err(|e| MessagingError::Tls(format!("mqtt: invalid TLS server name '{host}': {e}")))?;
    TlsConnector::from(config)
        .connect(name, tcp)
        .await
        .map_err(|e| MessagingError::Tls(format!("mqtt: TLS handshake with {address} failed: {e}")))
}

async fn websocket(stream: Box<dyn Io>, url: &str) -> Result<Link, MessagingError> {
    let failed = |e: String| {
        MessagingError::broker(
            PROTOCOL,
            format!("WebSocket handshake with {url} failed: {e}"),
        )
    };
    let mut request = url
        .into_client_request()
        .map_err(|e| failed(e.to_string()))?;
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("mqtt"));
    let (ws, _) = tokio_tungstenite::client_async(request, stream)
        .await
        .map_err(|e| failed(e.to_string()))?;
    Ok(Link::Ws(Box::new(ws)))
}
