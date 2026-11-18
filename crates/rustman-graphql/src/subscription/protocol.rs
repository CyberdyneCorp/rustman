//! The two GraphQL WebSocket protocols as messages: what the client sends
//! and how it reads what the server sends. No I/O here.
//!
//! - `graphql-transport-ws` (the graphql-ws library): `connection_init`,
//!   `connection_ack`, `ping`/`pong`, `subscribe`, `next`, `error`,
//!   `complete`.
//! - `graphql-ws` (the legacy subscriptions-transport-ws): `connection_init`,
//!   `connection_ack`, `ka`, `start`, `data`, `error`, `complete`, `stop`,
//!   `connection_terminate`, `connection_error`.

use std::fmt;

use serde_json::{Map, Value, json};

/// A GraphQL WebSocket protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireProtocol {
    TransportWs,
    /// subscriptions-transport-ws.
    Legacy,
}

/// A server message, as the client reads it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Incoming {
    Ack,
    /// `ping`: answer with `pong`.
    Ping,
    /// `pong` or `ka`: nothing to do.
    KeepAlive,
    /// An execution result (`next` or `data`).
    Next(Value),
    /// An `error` for the operation: its payload.
    Error(Value),
    Complete,
    /// The legacy `connection_error`: its payload.
    ConnectionError(Value),
    /// Anything else: ignored.
    Other,
}

impl WireProtocol {
    /// The `Sec-WebSocket-Protocol` value.
    pub fn subprotocol(self) -> &'static str {
        match self {
            Self::TransportWs => "graphql-transport-ws",
            Self::Legacy => "graphql-ws",
        }
    }

    /// The protocol of a negotiated subprotocol; `None` for any other.
    pub fn from_subprotocol(value: &str) -> Option<Self> {
        match value.trim() {
            "graphql-transport-ws" => Some(Self::TransportWs),
            "graphql-ws" => Some(Self::Legacy),
            _ => None,
        }
    }

    /// As shown to users.
    pub fn label(self) -> &'static str {
        match self {
            Self::TransportWs => "graphql-transport-ws",
            Self::Legacy => "legacy subscriptions-transport-ws",
        }
    }

    /// `connection_init`, with `params` as its payload.
    pub(crate) fn init(self, params: Option<&Map<String, Value>>) -> String {
        match params {
            Some(params) => json!({"type": "connection_init", "payload": params}),
            None => json!({"type": "connection_init"}),
        }
        .to_string()
    }

    /// Start operation `id` with `payload` (`query`, `operationName`,
    /// `variables`).
    pub(crate) fn subscribe(self, id: &str, payload: &Map<String, Value>) -> String {
        let kind = match self {
            Self::TransportWs => "subscribe",
            Self::Legacy => "start",
        };
        json!({"type": kind, "id": id, "payload": payload}).to_string()
    }

    /// What tells the server the client stopped operation `id`.
    pub(crate) fn stop(self, id: &str) -> Vec<String> {
        match self {
            Self::TransportWs => vec![json!({"type": "complete", "id": id}).to_string()],
            Self::Legacy => vec![
                json!({"type": "stop", "id": id}).to_string(),
                json!({"type": "connection_terminate"}).to_string(),
            ],
        }
    }

    /// The answer to a `ping`.
    pub(crate) fn pong(self) -> String {
        json!({"type": "pong"}).to_string()
    }

    /// Read a text message from the server.
    pub(crate) fn read(self, text: &str) -> Incoming {
        let Ok(Value::Object(mut message)) = serde_json::from_str::<Value>(text) else {
            return Incoming::Other;
        };
        let payload = message.remove("payload").unwrap_or(Value::Null);
        let kind = message
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match (self, kind) {
            (_, "connection_ack") => Incoming::Ack,
            (_, "complete") => Incoming::Complete,
            (_, "error") => Incoming::Error(payload),
            (Self::TransportWs, "ping") => Incoming::Ping,
            (Self::TransportWs, "pong") | (Self::Legacy, "ka") => Incoming::KeepAlive,
            (Self::TransportWs, "next") | (Self::Legacy, "data") => Incoming::Next(payload),
            (Self::Legacy, "connection_error") => Incoming::ConnectionError(payload),
            _ => Incoming::Other,
        }
    }
}

impl fmt::Display for WireProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// An `error` payload as the result an event records: the legacy protocol
/// sends one error object, the modern one a list.
pub(crate) fn error_result(payload: Value) -> Value {
    match payload {
        Value::Array(errors) => json!({"errors": errors}),
        Value::Object(error) if error.contains_key("errors") => Value::Object(error),
        other => json!({"errors": [other]}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODERN: WireProtocol = WireProtocol::TransportWs;
    const LEGACY: WireProtocol = WireProtocol::Legacy;

    fn parsed(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn the_client_messages_follow_each_protocol() {
        let params: Map<String, Value> = [("token".to_owned(), json!("t"))].into_iter().collect();
        assert_eq!(
            parsed(&MODERN.init(Some(&params))),
            json!({"type": "connection_init", "payload": {"token": "t"}})
        );
        assert_eq!(
            parsed(&LEGACY.init(None)),
            json!({"type": "connection_init"})
        );
        let payload: Map<String, Value> = [("query".to_owned(), json!("subscription { a }"))]
            .into_iter()
            .collect();
        assert_eq!(
            parsed(&MODERN.subscribe("1", &payload))["type"],
            "subscribe"
        );
        assert_eq!(parsed(&LEGACY.subscribe("1", &payload))["type"], "start");
        assert_eq!(
            parsed(&LEGACY.subscribe("1", &payload))["payload"],
            json!(payload)
        );
        let stop: Vec<Value> = LEGACY.stop("1").iter().map(|m| parsed(m)).collect();
        assert_eq!(stop[0], json!({"type": "stop", "id": "1"}));
        assert_eq!(stop[1], json!({"type": "connection_terminate"}));
        assert_eq!(
            parsed(&MODERN.stop("1")[0]),
            json!({"type": "complete", "id": "1"})
        );
        assert_eq!(parsed(&MODERN.pong()), json!({"type": "pong"}));
    }

    #[test]
    fn server_messages_are_read_per_protocol() {
        let next = r#"{"type":"next","id":"1","payload":{"data":{"a":1}}}"#;
        assert_eq!(MODERN.read(next), Incoming::Next(json!({"data": {"a": 1}})));
        assert_eq!(LEGACY.read(next), Incoming::Other);
        let data = r#"{"type":"data","id":"1","payload":{"data":{"a":1}}}"#;
        assert_eq!(LEGACY.read(data), Incoming::Next(json!({"data": {"a": 1}})));
        assert_eq!(MODERN.read(r#"{"type":"connection_ack"}"#), Incoming::Ack);
        assert_eq!(MODERN.read(r#"{"type":"ping"}"#), Incoming::Ping);
        assert_eq!(LEGACY.read(r#"{"type":"ka"}"#), Incoming::KeepAlive);
        assert_eq!(
            MODERN.read(r#"{"type":"complete","id":"1"}"#),
            Incoming::Complete
        );
        assert_eq!(
            LEGACY.read(r#"{"type":"connection_error","payload":{"message":"no"}}"#),
            Incoming::ConnectionError(json!({"message": "no"}))
        );
        assert_eq!(MODERN.read("not json"), Incoming::Other);
    }

    #[test]
    fn error_payloads_become_results_with_errors() {
        let list = json!([{"message": "a"}]);
        assert_eq!(error_result(list), json!({"errors": [{"message": "a"}]}));
        let one = json!({"message": "b"});
        assert_eq!(error_result(one), json!({"errors": [{"message": "b"}]}));
        assert_eq!(
            WireProtocol::from_subprotocol(" graphql-ws"),
            Some(WireProtocol::Legacy)
        );
        assert_eq!(WireProtocol::from_subprotocol("chat"), None);
        assert_eq!(LEGACY.to_string(), "legacy subscriptions-transport-ws");
    }
}
