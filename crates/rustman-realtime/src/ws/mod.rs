//! WebSocket sessions (realtime-sessions "WebSocket handshake" to "Ping,
//! pong and close"): the handshake through `rustman_http::open_websocket`
//! (the engine's connector, proxies and TLS settings), then the steps of
//! the file, then listening until the bounds, a close or a cancel.
//!
//! One task drives a session: it reads the socket, answers pings, runs
//! the steps and the commands sent to it, and records every frame in the
//! timeline.

mod frames;
mod live;
mod steps;

use std::time::Instant;

use rustman_core::Redactor;
use rustman_core::vars::ResolvedWs;
use rustman_http::{OpenedWebSocket, SendOptions, WebSocketError, header_pairs, open_websocket};
use tokio_tungstenite::tungstenite::handshake::client::Response;

use crate::error::RealtimeError;
use crate::exchange::{Handshake, RealtimeExchange};
use crate::session::SessionOptions;
use live::Live;

/// How long a close waits for the server's close frame.
pub const CLOSE_WAIT: std::time::Duration = std::time::Duration::from_millis(2000);

/// Run the WebSocket session `target` with `options` (TLS, proxies, the
/// connect timeout) until its steps and bounds end it. The exchange
/// always comes back; its `error` says why the session failed.
pub async fn run_ws(
    target: &ResolvedWs,
    options: &SendOptions,
    session: SessionOptions,
) -> RealtimeExchange {
    let started = Instant::now();
    let max_timeline = target.limits.effective_max_timeline();
    let mut exchange = RealtimeExchange::new("ws", target.http.clone(), started, max_timeline);
    let opened = match connect(target, options).await {
        Ok(opened) => opened,
        Err(Refusal {
            error,
            handshake,
            credentials,
        }) => {
            exchange.handshake = handshake;
            exchange.credentials = credentials;
            exchange.error = Some(error);
            exchange.elapsed = started.elapsed();
            return exchange;
        }
    };
    exchange.request = opened.request;
    exchange.handshake = Some(handshake_of(&opened.response));
    exchange.credentials = opened.credentials;
    exchange.trace = opened.trace;
    let max = target.limits.effective_max_message_bytes();
    let mut live = Live::new(opened.socket, exchange, target.http.clone(), max, session);
    live.connected();
    let result = live.drive(&target.steps).await;
    let mut exchange = live.finish(result).await;
    exchange.elapsed = started.elapsed();
    exchange
}

/// Why the socket did not open: the error, what the server answered to a
/// refused upgrade, and what the handshake sent that must be redacted.
struct Refusal {
    error: RealtimeError,
    handshake: Option<Handshake>,
    credentials: Redactor,
}

/// Open the socket, offering the file's subprotocols in order. A refused
/// upgrade comes back with what the server answered.
async fn connect(target: &ResolvedWs, options: &SendOptions) -> Result<OpenedWebSocket, Refusal> {
    let offer = (!target.subprotocols.is_empty()).then(|| target.subprotocols.join(", "));
    let max = target.limits.effective_max_message_bytes();
    let limit = options.connect_timeout;
    open_websocket(&target.http, options, offer.as_deref(), limit, max)
        .await
        .map_err(refused)
}

fn refused(err: WebSocketError) -> Refusal {
    let (error, handshake) = match err {
        WebSocketError::Transport(e) => (RealtimeError::Transport(e), None),
        WebSocketError::TimedOut(after) => (RealtimeError::HandshakeTimedOut(after), None),
        WebSocketError::Handshake(failure) => match failure.refused {
            Some(response) => {
                let handshake = Handshake {
                    status: response.status,
                    headers: response.headers,
                    subprotocol: None,
                    body: Some(String::from_utf8_lossy(&response.body).into_owned()),
                };
                let error = RealtimeError::Refused {
                    status: response.status,
                    reason: response.reason,
                };
                return Refusal {
                    error,
                    handshake: Some(handshake),
                    credentials: response.credentials,
                };
            }
            None => (RealtimeError::Handshake(failure.message), None),
        },
    };
    Refusal {
        error,
        handshake,
        credentials: Redactor::new(),
    }
}

/// The recorded `101` response: status, headers and the chosen
/// subprotocol.
fn handshake_of(response: &Response) -> Handshake {
    let mut handshake = Handshake {
        status: response.status().as_u16(),
        headers: header_pairs(response.headers()),
        subprotocol: None,
        body: None,
    };
    handshake.subprotocol = handshake
        .header("sec-websocket-protocol")
        .map(|p| p.trim().to_owned());
    handshake
}
