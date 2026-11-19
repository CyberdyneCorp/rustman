//! Subscriptions over WebSocket (design D3), on the WebSocket stack that
//! `rustman-http` shares between protocol clients
//! ([`rustman_http::open_websocket`]): its connector, proxies and TLS
//! settings (`ca_bundle`, client certificate, `verify_tls`), and the
//! request's headers, auth helpers and cookies in the handshake.
//!
//! Events are held in memory, so a message (and a frame) may not be larger
//! than the in-memory body cap, `[http] max_body_bytes`. Headless, the
//! handshake and `connection_ack` wait end with the request's `duration`.
//!
//! With `ws_protocol: auto`, both subprotocols are offered,
//! `graphql-transport-ws` first; a server that only speaks the legacy
//! `graphql-ws` picks that one and the legacy protocol is used.

use std::time::{Duration, Instant};

use futures_util::{Sink, SinkExt, Stream, StreamExt};
use rustman_core::model::WsProtocol;
use rustman_core::vars::{ResolvedGraphql, ResolvedRequest};
use rustman_http::{SendOptions, WebSocket, WebSocketError, open_websocket};
use tokio_tungstenite::tungstenite::error::CapacityError;
use tokio_tungstenite::tungstenite::handshake::client::Response;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use super::protocol::{Incoming, WireProtocol, error_result};
use super::{Collected, Collector, Ended, SubscribeOptions, Subscription, SubscriptionProtocol};
use crate::error::GraphqlError;
use crate::execute::HttpOperation;

/// The id of the one operation each connection runs.
const OPERATION_ID: &str = "1";

/// How long telling the server the client stopped may take: a server that
/// stops reading must not hold a headless run past its limits.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Run `operation` as a subscription over a WebSocket to the endpoint of
/// `target` (`http(s)://` endpoints become `ws(s)://`), speaking
/// `preferred`. Errors are redacted.
pub async fn subscribe_ws(
    target: &ResolvedGraphql,
    operation: &HttpOperation,
    preferred: WsProtocol,
    send_options: &SendOptions,
    options: &SubscribeOptions,
) -> Result<Subscription, GraphqlError> {
    let started = Instant::now();
    let opened = open(target, preferred, send_options, options, started).await;
    let (mut socket, response, request) = opened.map_err(|e| e.redacted(&target.redactor()))?;
    let protocol = negotiated(&response, preferred).map_err(|e| e.redacted(&target.redactor()))?;
    let connected = started.elapsed();
    let collected = converse(&mut socket, protocol, target, operation, options, started).await;
    let (collected, ended) = collected.map_err(|e| e.redacted(&target.redactor()))?;
    Ok(Subscription {
        protocol: SubscriptionProtocol::Ws(protocol),
        insecure: request.url.text.starts_with("wss:") && !send_options.tls.verify,
        request,
        status: response.status().as_u16(),
        headers: response.headers().clone(),
        events: collected.events,
        event_count: collected.event_count,
        ended,
        failure: collected.failure,
        connected,
        elapsed: started.elapsed(),
        warnings: Vec::new(),
    })
}

/// Connect and shake hands. Returns the socket, the server's handshake
/// response and the handshake request as recorded.
async fn open(
    target: &ResolvedGraphql,
    preferred: WsProtocol,
    send_options: &SendOptions,
    options: &SubscribeOptions,
    started: Instant,
) -> Result<(WebSocket, Response, ResolvedRequest), GraphqlError> {
    let limit = options.within(started, send_options.connect_timeout);
    let offer = Some(offered(preferred));
    let opened = open_websocket(&target.http, send_options, offer, limit)
        .await
        .map_err(|e| match e {
            WebSocketError::Transport(e) => GraphqlError::Transport(e),
            WebSocketError::Handshake(message) => GraphqlError::Subscription(message),
            WebSocketError::TimedOut(limit) => GraphqlError::Subscription(format!(
                "WebSocket handshake timed out after {} ms",
                limit.as_millis()
            )),
        })?;
    Ok((opened.socket, opened.response, opened.request))
}

/// The `Sec-WebSocket-Protocol` offer for `preferred`.
fn offered(preferred: WsProtocol) -> &'static str {
    match preferred {
        WsProtocol::Auto => "graphql-transport-ws, graphql-ws",
        WsProtocol::GraphqlTransportWs => "graphql-transport-ws",
        WsProtocol::GraphqlWs => "graphql-ws",
    }
}

/// The protocol the server chose. Without a choice, a forced protocol is
/// assumed (some servers do not echo it); under `auto` it is an error.
fn negotiated(response: &Response, preferred: WsProtocol) -> Result<WireProtocol, GraphqlError> {
    let chosen = response
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok());
    let forced = match preferred {
        WsProtocol::Auto => None,
        WsProtocol::GraphqlTransportWs => Some(WireProtocol::TransportWs),
        WsProtocol::GraphqlWs => Some(WireProtocol::Legacy),
    };
    chosen
        .and_then(WireProtocol::from_subprotocol)
        .or(forced)
        .ok_or_else(|| {
            GraphqlError::Subscription(format!(
                "the server accepted none of the subprotocols {}",
                offered(preferred)
            ))
        })
}

/// One read from the socket.
enum Read {
    Text(String),
    /// A control or binary frame: nothing for the protocol.
    Skip,
    /// The connection is over: the server's close frame, when it sent one.
    Closed(Option<CloseFrame>),
    /// A message or frame larger than the limit.
    TooLarge(usize),
}

fn read(message: Option<Result<Message, WsError>>) -> Result<Read, GraphqlError> {
    match message {
        None => Ok(Read::Closed(None)),
        Some(Ok(Message::Close(frame))) => Ok(Read::Closed(frame)),
        Some(Ok(Message::Text(text))) => Ok(Read::Text(text.as_str().to_owned())),
        Some(Ok(_)) => Ok(Read::Skip),
        Some(Err(WsError::ConnectionClosed | WsError::AlreadyClosed)) => Ok(Read::Closed(None)),
        Some(Err(WsError::Capacity(CapacityError::MessageTooLong { max_size, .. }))) => {
            Ok(Read::TooLarge(max_size))
        }
        Some(Err(e)) => Err(GraphqlError::Subscription(format!("WebSocket: {e}"))),
    }
}

/// ` (4403 Forbidden)`: the close code and reason of `frame`, empty
/// without one. `graphql-transport-ws` ends a failed connection with a
/// 4400-4499 code.
fn close_details(frame: Option<&CloseFrame>) -> String {
    let Some(frame) = frame else {
        return String::new();
    };
    let code = u16::from(frame.code);
    match frame.reason.trim() {
        "" => format!(" ({code})"),
        reason => format!(" ({code} {reason})"),
    }
}

async fn send<S>(socket: &mut S, text: String) -> Result<(), GraphqlError>
where
    S: Sink<Message, Error = WsError> + Unpin,
{
    socket
        .send(Message::text(text))
        .await
        .map_err(|e| GraphqlError::Subscription(format!("WebSocket: {e}")))
}

/// Initialize, subscribe and collect events until the subscription ends,
/// then tell the server when the client ended it.
async fn converse<S>(
    socket: &mut S,
    protocol: WireProtocol,
    target: &ResolvedGraphql,
    operation: &HttpOperation,
    options: &SubscribeOptions,
    started: Instant,
) -> Result<(Collected, Ended), GraphqlError>
where
    S: Stream<Item = Result<Message, WsError>> + Sink<Message, Error = WsError> + Unpin,
{
    send(socket, protocol.init(target.connection_params.as_ref())).await?;
    wait_for_ack(socket, protocol, options, started).await?;
    let payload = operation.payload(true);
    send(socket, protocol.subscribe(OPERATION_ID, &payload)).await?;
    let mut collector = Collector::new(options, started);
    let ended = collect(socket, protocol, &mut collector).await?;
    let stop = if ended.by_client() {
        protocol.stop(OPERATION_ID)
    } else {
        Vec::new()
    };
    teardown(socket, stop, TEARDOWN_TIMEOUT).await;
    Ok((collector.finish(), ended))
}

/// Send `stop` and close the connection, giving up after `limit`. The
/// server may already be gone or not reading; the events are kept either
/// way.
async fn teardown<S>(socket: &mut S, stop: Vec<String>, limit: Duration)
where
    S: Sink<Message, Error = WsError> + Unpin,
{
    let close = async {
        for message in stop {
            if send(socket, message).await.is_err() {
                break;
            }
        }
        let _ = socket.close().await;
    };
    let _ = tokio::time::timeout(limit, close).await;
}

/// Wait for `connection_ack`, within the ack timeout and what is left of
/// the duration.
async fn wait_for_ack<S>(
    socket: &mut S,
    protocol: WireProtocol,
    options: &SubscribeOptions,
    started: Instant,
) -> Result<(), GraphqlError>
where
    S: Stream<Item = Result<Message, WsError>> + Sink<Message, Error = WsError> + Unpin,
{
    let acked = async {
        loop {
            let text = match read(socket.next().await)? {
                Read::Text(text) => text,
                Read::Skip => continue,
                Read::Closed(frame) => return Err(closed_before_ack(frame.as_ref())),
                Read::TooLarge(limit) => return Err(GraphqlError::Subscription(too_large(limit))),
            };
            match protocol.read(&text) {
                Incoming::Ack => return Ok(()),
                Incoming::Ping => send(socket, protocol.pong()).await?,
                Incoming::ConnectionError(payload) | Incoming::Error(payload) => {
                    return Err(GraphqlError::Subscription(format!(
                        "the server refused the connection: {payload}"
                    )));
                }
                _ => {}
            }
        }
    };
    tokio::select! {
        biased;
        () = options.cancel.cancelled() => Err(GraphqlError::Subscription("stopped before connection_ack".into())),
        acked = tokio::time::timeout(options.within(started, options.ack_timeout), acked) => acked.map_err(|_| GraphqlError::AckTimeout)?,
    }
}

fn too_large(limit: usize) -> String {
    format!("WebSocket: a message is larger than the {limit} byte limit (max_body_bytes)")
}

fn closed_before_ack(frame: Option<&CloseFrame>) -> GraphqlError {
    GraphqlError::Subscription(format!(
        "the server closed the connection before connection_ack{}",
        close_details(frame)
    ))
}

/// Collect events until the server completes, errors or closes, or the
/// limits or the caller stop the subscription.
async fn collect<S>(
    socket: &mut S,
    protocol: WireProtocol,
    collector: &mut Collector<'_>,
) -> Result<Ended, GraphqlError>
where
    S: Stream<Item = Result<Message, WsError>> + Sink<Message, Error = WsError> + Unpin,
{
    loop {
        let message = tokio::select! {
            biased;
            () = collector.options.cancel.cancelled() => return Ok(Ended::Stopped),
            () = collector.deadline() => return Ok(Ended::Duration),
            message = socket.next() => message,
        };
        let text = match read(message)? {
            Read::Text(text) => text,
            Read::Skip => continue,
            Read::Closed(frame) => {
                collector.fail(format!(
                    "the server closed the connection without complete{}",
                    close_details(frame.as_ref())
                ));
                return Ok(Ended::Closed);
            }
            Read::TooLarge(limit) => {
                collector.fail(too_large(limit));
                return Ok(Ended::Error);
            }
        };
        match protocol.read(&text) {
            Incoming::Next(payload) => {
                if collector.push(payload) {
                    return Ok(Ended::MaxEvents);
                }
            }
            Incoming::Error(payload) | Incoming::ConnectionError(payload) => {
                collector.push(error_result(payload));
                return Ok(Ended::Error);
            }
            Incoming::Complete => return Ok(Ended::Complete),
            Incoming::Ping => {
                if let Some(ended) = answer_ping(socket, protocol, collector).await? {
                    return Ok(ended);
                }
            }
            _ => {}
        }
    }
}

/// Send `pong`, unless the limits or the caller stop the subscription
/// first: a server that stops reading must not hold it past them.
async fn answer_ping<S>(
    socket: &mut S,
    protocol: WireProtocol,
    collector: &Collector<'_>,
) -> Result<Option<Ended>, GraphqlError>
where
    S: Sink<Message, Error = WsError> + Unpin,
{
    tokio::select! {
        biased;
        () = collector.options.cancel.cancelled() => Ok(Some(Ended::Stopped)),
        () = collector.deadline() => Ok(Some(Ended::Duration)),
        sent = send(socket, protocol.pong()) => sent.map(|()| None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subprotocol_offers_follow_the_preference() {
        assert_eq!(
            offered(WsProtocol::Auto),
            "graphql-transport-ws, graphql-ws"
        );
        assert_eq!(offered(WsProtocol::GraphqlWs), "graphql-ws");
        assert_eq!(
            offered(WsProtocol::GraphqlTransportWs),
            "graphql-transport-ws"
        );
    }

    #[test]
    fn close_frames_carry_their_code_and_reason() {
        use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
        let forbidden = CloseFrame {
            code: CloseCode::from(4403),
            reason: "Forbidden".into(),
        };
        assert_eq!(close_details(Some(&forbidden)), " (4403 Forbidden)");
        let bare = CloseFrame {
            code: CloseCode::Normal,
            reason: "".into(),
        };
        assert_eq!(close_details(Some(&bare)), " (1000)");
        assert_eq!(close_details(None), "");
        let err = closed_before_ack(Some(&forbidden)).to_string();
        assert!(
            err.ends_with("before connection_ack (4403 Forbidden)"),
            "{err}"
        );
    }

    /// A socket whose peer stopped reading: sending never completes.
    struct Stalled;

    impl Sink<Message> for Stalled {
        type Error = WsError;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), WsError>> {
            std::task::Poll::Pending
        }

        fn start_send(self: std::pin::Pin<&mut Self>, _: Message) -> Result<(), WsError> {
            Ok(())
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), WsError>> {
            std::task::Poll::Pending
        }

        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), WsError>> {
            std::task::Poll::Pending
        }
    }

    /// A server that sends one `ping`, then neither sends nor reads.
    struct Pinging {
        pinged: bool,
    }

    impl Stream for Pinging {
        type Item = Result<Message, WsError>;

        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            if std::mem::replace(&mut self.pinged, true) {
                return std::task::Poll::Pending;
            }
            std::task::Poll::Ready(Some(Ok(Message::text(r#"{"type":"ping"}"#))))
        }
    }

    impl Sink<Message> for Pinging {
        type Error = WsError;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), WsError>> {
            std::pin::Pin::new(&mut Stalled).poll_ready(cx)
        }

        fn start_send(self: std::pin::Pin<&mut Self>, _: Message) -> Result<(), WsError> {
            Ok(())
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), WsError>> {
            std::pin::Pin::new(&mut Stalled).poll_flush(cx)
        }

        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), WsError>> {
            std::pin::Pin::new(&mut Stalled).poll_close(cx)
        }
    }

    fn pinging() -> Pinging {
        Pinging { pinged: false }
    }

    #[tokio::test]
    async fn an_unanswerable_ping_does_not_outlast_the_duration_or_a_stop() {
        use super::super::Limits;
        use tokio_util::sync::CancellationToken;

        let limits = Limits {
            max_events: None,
            duration: Some(Duration::from_millis(100)),
        };
        let options = SubscribeOptions::new(limits, CancellationToken::new());
        let mut collector = Collector::new(&options, Instant::now());
        let mut socket = pinging();
        let collected = collect(&mut socket, WireProtocol::TransportWs, &mut collector);
        let ended = tokio::time::timeout(Duration::from_secs(30), collected).await;
        assert_eq!(ended.expect("bounded").unwrap(), Ended::Duration);

        let cancel = CancellationToken::new();
        let options = SubscribeOptions::new(Limits::interactive(), cancel.clone());
        let mut collector = Collector::new(&options, Instant::now());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.cancel();
        });
        let mut socket = pinging();
        let collected = collect(&mut socket, WireProtocol::TransportWs, &mut collector);
        let ended = tokio::time::timeout(Duration::from_secs(30), collected).await;
        assert_eq!(ended.expect("bounded").unwrap(), Ended::Stopped);
    }

    #[tokio::test]
    async fn teardown_gives_up_on_a_server_that_stops_reading() {
        let stop = WireProtocol::TransportWs.stop(OPERATION_ID);
        let mut socket = Stalled;
        let teardown = teardown(&mut socket, stop, Duration::from_millis(50));
        let bounded = tokio::time::timeout(Duration::from_secs(30), teardown).await;
        assert!(bounded.is_ok(), "teardown must not wait for a stalled peer");
    }

    #[test]
    fn a_forced_protocol_is_assumed_when_the_server_does_not_echo_it() {
        let plain = Response::new(None);
        assert_eq!(
            negotiated(&plain, WsProtocol::GraphqlWs).unwrap(),
            WireProtocol::Legacy
        );
        let err = negotiated(&plain, WsProtocol::Auto).unwrap_err();
        assert!(err.to_string().contains("accepted none"), "{err}");
    }
}
