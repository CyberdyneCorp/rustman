//! Calls, batches and subscriptions over WebSocket (design D2), on the
//! WebSocket stack `rustman-http` shares with the GraphQL client. Each
//! envelope goes out as one text frame; the answer is the frame whose id
//! matches, and unrelated frames (such as notifications) are skipped.
//!
//! A [`WsPool`] keeps connections open for one run, keyed by endpoint and
//! handshake headers; without one, the connection closes when the exchange
//! ends.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use http::HeaderMap;
use rustman_core::trace::{SentTrace, TRACERESPONSE, b3, baggage::BAGGAGE, w3c};
use rustman_core::vars::{ResolvedCall, ResolvedJsonrpc, ResolvedRequest};
use rustman_http::{
    Phase, ProxySettings, RequestError, SendOptions, TlsSettings, UrlScheme, WebSocket,
    WebSocketError, open_websocket, request_head, with_scheme,
};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::error::CapacityError;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::envelope::{Envelope, IdSequence, is_response, request_body, response_id};
use crate::error::JsonrpcError;
use crate::outcome::{
    Attempt, CallResult, RpcExchange, TransportKind, batch_results, notification_result,
    single_result,
};
use crate::retry::{RetryPolicy, limit_exceeded, pause};
use crate::subscription::{self, SubscribeOptions};

/// How long closing a connection may take: a server that stops reading
/// must not hold a run.
pub const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// One open WebSocket with its id sequence.
pub struct WsConnection {
    socket: WebSocket,
    ids: IdSequence,
    /// The handshake request as recorded (`ws(s)://` URL, headers).
    pub handshake: ResolvedRequest,
    /// The handshake response status (101).
    pub status: u16,
    /// The trace context the handshake carried: every call on this
    /// connection shares it.
    pub trace: Option<SentTrace>,
    /// The handshake response.s `traceresponse` header.
    pub traceresponse: Option<String>,
}

impl WsConnection {
    /// Open a WebSocket to the endpoint of `http`; the handshake must
    /// finish within the connect timeout. Errors are redacted.
    pub async fn open(http: &ResolvedRequest, options: &SendOptions) -> Result<Self, JsonrpcError> {
        let limit = options.connect_timeout;
        let opened = open_websocket(http, options, None, limit, options.body.max_in_memory)
            .await
            .map_err(|e| match e {
                WebSocketError::Transport(e) => JsonrpcError::Transport(e),
                WebSocketError::Handshake(failure) => JsonrpcError::WebSocket(failure.message),
                WebSocketError::TimedOut(after) => JsonrpcError::Transport(RequestError::Timeout {
                    after,
                    phase: Phase::Connect,
                }),
            })?;
        Ok(Self {
            socket: opened.socket,
            ids: IdSequence::default(),
            handshake: opened.request,
            status: opened.response.status().as_u16(),
            trace: opened.trace,
            traceresponse: opened
                .response
                .headers()
                .get(TRACERESPONSE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned),
        })
    }

    /// The next sequential id of this connection.
    pub fn next_id(&mut self) -> Value {
        self.ids.take()
    }

    pub(crate) async fn send_text(&mut self, text: String) -> Result<(), JsonrpcError> {
        self.socket
            .send(Message::text(text))
            .await
            .map_err(|e| JsonrpcError::WebSocket(format!("WebSocket: {e}")))
    }

    /// The next text frame parsed as JSON; frames that are not JSON and
    /// control frames are skipped.
    pub(crate) async fn next_json(&mut self) -> Result<(Value, String), JsonrpcError> {
        loop {
            let text = match self.socket.next().await {
                Some(Ok(Message::Text(text))) => text.as_str().to_owned(),
                Some(
                    Ok(Message::Close(_)) | Err(WsError::ConnectionClosed | WsError::AlreadyClosed),
                )
                | None => return Err(closed()),
                Some(Ok(_)) => continue,
                Some(Err(WsError::Capacity(CapacityError::MessageTooLong {
                    max_size, ..
                }))) => return Err(JsonrpcError::message_too_large(max_size)),
                Some(Err(e)) => return Err(JsonrpcError::WebSocket(format!("WebSocket: {e}"))),
            };
            if let Ok(value) = serde_json::from_str(&text) {
                return Ok((value, text));
            }
        }
    }

    /// Read until a frame `wanted` accepts, within `timeout`. Skipped
    /// frames go to `skipped`.
    pub(crate) async fn read_matching(
        &mut self,
        wanted: impl Fn(&Value) -> bool,
        skipped: &mut dyn FnMut(Value),
        timeout: Duration,
    ) -> Result<(Value, String), JsonrpcError> {
        let read = async {
            loop {
                let (value, text) = self.next_json().await?;
                if wanted(&value) {
                    return Ok((value, text));
                }
                skipped(value);
            }
        };
        tokio::time::timeout(timeout, read)
            .await
            .unwrap_or(Err(JsonrpcError::Transport(RequestError::Timeout {
                after: timeout,
                phase: Phase::Response,
            })))
    }

    /// Send `calls` (one envelope, or an array for a batch) and read the
    /// answer within `timeout`. Returns the results, the text sent and the
    /// text received.
    pub async fn exchange(
        &mut self,
        calls: &[ResolvedCall],
        batch: bool,
        timeout: Duration,
    ) -> (
        Result<Vec<CallResult>, JsonrpcError>,
        String,
        Option<String>,
    ) {
        let (envelopes, body) = request_body(calls, batch, &mut self.ids);
        let sent = body.to_string();
        if let Err(e) = self.send_text(sent.clone()).await {
            return (Err(e), sent, None);
        }
        if envelopes.iter().all(|e| e.id.is_none()) {
            return (
                Ok(envelopes.iter().map(notification_result).collect()),
                sent,
                None,
            );
        }
        let answer = match envelopes.as_slice() {
            [single] if !batch => self.single(single, timeout).await,
            many => self.batch(many, timeout).await,
        };
        match answer {
            Ok((calls, text)) => (Ok(calls), sent, Some(text)),
            Err(e) => (Err(e), sent, None),
        }
    }

    async fn single(
        &mut self,
        envelope: &Envelope,
        timeout: Duration,
    ) -> Result<(Vec<CallResult>, String), JsonrpcError> {
        let id = envelope.id.clone().unwrap_or(Value::Null);
        let wanted = |v: &Value| answers(v, &id);
        let (value, text) = self.read_matching(wanted, &mut |_| {}, timeout).await?;
        Ok((vec![single_result(envelope, &value)?], text))
    }

    async fn batch(
        &mut self,
        envelopes: &[Envelope],
        timeout: Duration,
    ) -> Result<(Vec<CallResult>, String), JsonrpcError> {
        let wanted = |v: &Value| v.is_array() || answers(v, &Value::Null);
        let (value, text) = self.read_matching(wanted, &mut |_| {}, timeout).await?;
        Ok((batch_results(envelopes, &value)?, text))
    }

    /// Close the connection, giving up after [`TEARDOWN_TIMEOUT`].
    pub async fn close(&mut self) {
        let _ = tokio::time::timeout(TEARDOWN_TIMEOUT, self.socket.close(None)).await;
    }
}

/// Whether `value` is the response to the request with id `id` (or a
/// server error about the request itself, with id `null`).
fn answers(value: &Value, id: &Value) -> bool {
    if !value.is_object() || !is_response(value) {
        return false;
    }
    let got = response_id(value);
    got == id || (got.is_null() && value.get("error").is_some())
}

fn closed() -> JsonrpcError {
    JsonrpcError::WebSocket("the server closed the WebSocket connection".to_owned())
}

/// A connection shared by the requests of one run.
pub type SharedConnection = Arc<Mutex<WsConnection>>;

/// Connections kept open for one run, keyed by the endpoint, the
/// handshake headers (auth included) and the TLS and proxy settings.
/// Closed with [`WsPool::close_all`].
#[derive(Clone, Default)]
pub struct WsPool {
    connections: Arc<Mutex<HashMap<PoolKey, SharedConnection>>>,
}

/// What files must have in common to share a connection: everything the
/// connection was opened with.
#[derive(Clone, PartialEq, Eq, Hash)]
struct PoolKey {
    uri: String,
    headers: Vec<(String, Vec<u8>)>,
    tls: TlsSettings,
    proxy: ProxySettings,
}

impl WsPool {
    /// The open connection for `http`, opening one when there is none.
    pub async fn connection(
        &self,
        http: &ResolvedRequest,
        options: &SendOptions,
    ) -> Result<SharedConnection, JsonrpcError> {
        let key = pool_key(http, options).await?;
        let mut connections = self.connections.lock().await;
        if let Some(found) = connections.get(&key) {
            return Ok(found.clone());
        }
        let opened = Arc::new(Mutex::new(WsConnection::open(http, options).await?));
        connections.insert(key, opened.clone());
        Ok(opened)
    }

    /// Forget the connection of `http`, after it broke.
    pub async fn evict(&self, http: &ResolvedRequest, options: &SendOptions) {
        if let Ok(key) = pool_key(http, options).await {
            self.connections.lock().await.remove(&key);
        }
    }

    /// How many connections are open.
    pub async fn len(&self) -> usize {
        self.connections.lock().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }

    /// Close every connection.
    pub async fn close_all(&self) {
        let drained: Vec<SharedConnection> = self
            .connections
            .lock()
            .await
            .drain()
            .map(|(_, c)| c)
            .collect();
        for connection in drained {
            connection.lock().await.close().await;
        }
    }
}

async fn pool_key(http: &ResolvedRequest, options: &SendOptions) -> Result<PoolKey, JsonrpcError> {
    let mut handshake = http.clone();
    handshake.method = "GET".to_owned();
    handshake.body = rustman_core::vars::ResolvedBody::None;
    handshake.url.text = with_scheme(&handshake.url.text, UrlScheme::Http);
    let (uri, headers) = request_head(&handshake, options).await?;
    Ok(PoolKey {
        uri: uri.to_string(),
        headers: header_list(&headers),
        tls: options.tls.clone(),
        proxy: options.proxy.clone(),
    })
}

/// The handshake headers as a pool key. Trace headers are left out: each
/// handshake head gets fresh span ids, and calls that share a connection
/// share the context of the handshake that opened it.
fn header_list(headers: &HeaderMap) -> Vec<(String, Vec<u8>)> {
    let mut list: Vec<(String, Vec<u8>)> = headers
        .iter()
        .filter(|(n, _)| !is_trace_header(n.as_str()))
        .map(|(n, v)| (n.as_str().to_owned(), v.as_bytes().to_vec()))
        .collect();
    list.sort();
    list
}

fn is_trace_header(name: &str) -> bool {
    [w3c::TRACEPARENT, w3c::TRACESTATE, BAGGAGE, b3::B3]
        .into_iter()
        .chain(b3::MULTI)
        .any(|trace| trace.eq_ignore_ascii_case(name))
}

/// Send `target` over WebSocket: a call, a batch or a subscription, on a
/// pooled connection when `pool` is given. Idempotent requests are retried
/// on a fresh connection.
pub async fn send_ws(
    options: &SendOptions,
    target: &ResolvedJsonrpc,
    pool: Option<&WsPool>,
    subscribe: &SubscribeOptions,
) -> RpcExchange {
    let started = Instant::now();
    let mut exchange = RpcExchange::new(TransportKind::WebSocket, target.batch);
    let policy = RetryPolicy::of(target);
    let mut retry = 0;
    loop {
        let attempt_started = Instant::now();
        let error = attempt(options, target, pool, subscribe, &mut exchange).await;
        let retryable = match &error {
            Some(e) => e.is_transient(),
            None => limit_exceeded(&exchange.calls),
        };
        let mut record = Attempt {
            status: None,
            error: error
                .as_ref()
                .map(ToString::to_string)
                .or_else(|| retryable.then(|| exchange.failures().join("; "))),
            elapsed: attempt_started.elapsed(),
            retry_in: None,
        };
        exchange.error = error;
        if retryable && retry < policy.retries && exchange.subscription.is_none() {
            retry += 1;
            let delay = policy.delay(retry, None);
            record.retry_in = Some(delay);
            exchange.attempts.push(record);
            if pause(delay, &subscribe.cancel).await {
                continue;
            }
            break;
        }
        exchange.attempts.push(record);
        break;
    }
    exchange.error = exchange
        .error
        .take()
        .map(|e| e.redacted(&target.redactor()));
    exchange.elapsed = started.elapsed();
    exchange
}

/// One attempt; the exchange error, if any.
async fn attempt(
    options: &SendOptions,
    target: &ResolvedJsonrpc,
    pool: Option<&WsPool>,
    subscribe: &SubscribeOptions,
    exchange: &mut RpcExchange,
) -> Option<JsonrpcError> {
    let connection = match pool {
        Some(pool) => pool.connection(&target.http, options).await,
        None => WsConnection::open(&target.http, options)
            .await
            .map(|c| Arc::new(Mutex::new(c))),
    };
    let connection = match connection {
        Ok(connection) => connection,
        Err(e) => return Some(e),
    };
    let mut connection = connection.lock().await;
    exchange.trace.clone_from(&connection.trace);
    exchange.traceresponse.clone_from(&connection.traceresponse);
    let error = converse(&mut connection, options, target, subscribe, exchange).await;
    match (pool, &error) {
        (None, _) => connection.close().await,
        (Some(pool), Some(e)) if e.is_transient() || matches!(e, JsonrpcError::TooLarge(_)) => {
            connection.close().await;
            pool.evict(&target.http, options).await;
        }
        _ => {}
    }
    error
}

async fn converse(
    connection: &mut WsConnection,
    options: &SendOptions,
    target: &ResolvedJsonrpc,
    subscribe: &SubscribeOptions,
    exchange: &mut RpcExchange,
) -> Option<JsonrpcError> {
    if let (Some(spec), [call]) = (&target.subscription, target.calls.as_slice()) {
        let run = subscription::run(connection, spec, call, subscribe, options.timeout).await;
        exchange.sent = run.sent;
        exchange.received = run.received;
        return match run.result {
            Ok((call, record)) => {
                exchange.calls = vec![call];
                exchange.subscription = record;
                None
            }
            Err(e) => Some(e),
        };
    }
    let (calls, sent, received) = connection
        .exchange(&target.calls, target.batch, options.timeout)
        .await;
    exchange.sent = sent;
    exchange.received = received;
    match calls {
        Ok(calls) => {
            exchange.calls = calls;
            None
        }
        Err(e) => Some(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::model::RequestSettings;
    use rustman_core::vars::{Resolved, ResolvedBody, TlsFiles};
    use serde_json::json;

    fn endpoint(url: &str) -> ResolvedRequest {
        ResolvedRequest {
            method: "POST".to_owned(),
            url: Resolved::literal(url),
            query: Vec::new(),
            headers: Vec::new(),
            body: ResolvedBody::None,
            auth: None,
            settings: RequestSettings::default(),
            tls: TlsFiles::default(),
            secrets: Vec::new(),
        }
    }

    #[tokio::test]
    async fn the_pool_key_includes_tls_and_proxy_settings() {
        let http = endpoint("ws://127.0.0.1:9/rpc");
        let base = SendOptions::default();
        let mut insecure = base.clone();
        insecure.tls.verify = false;
        let mut ca = base.clone();
        ca.tls.ca_bundle = Some(b"-----BEGIN CERTIFICATE-----".to_vec());
        let mut proxied = base.clone();
        proxied.proxy = ProxySettings::explicit("http://127.0.0.1:3128").unwrap();
        let mut keys = Vec::new();
        for options in [&base, &insecure, &ca, &proxied] {
            keys.push(pool_key(&http, options).await.unwrap());
        }
        assert!(pool_key(&http, &base.clone()).await.unwrap() == keys[0]);
        for (i, a) in keys.iter().enumerate() {
            for b in &keys[i + 1..] {
                assert!(a != b, "distinct settings must not share a connection");
            }
        }
    }

    #[test]
    fn only_responses_with_the_id_answer_a_call() {
        let id = json!(1);
        assert!(answers(&json!({"jsonrpc":"2.0","id":1,"result":1}), &id));
        assert!(answers(
            &json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"m"}}),
            &id
        ));
        assert!(!answers(&json!({"jsonrpc":"2.0","id":2,"result":1}), &id));
        let notification = json!({"jsonrpc":"2.0","method":"eth_subscription","params":{"subscription":"0x1","result":{}}});
        assert!(!answers(&notification, &id));
        assert!(!answers(&json!([{"id":1,"result":1}]), &id));
    }
}
