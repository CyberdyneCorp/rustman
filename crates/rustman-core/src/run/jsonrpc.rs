//! What a JSON-RPC exchange adds to its request record (add-jsonrpc-client
//! "Exchange recording and redaction"): the envelopes sent and received
//! per call, every attempt, and a subscription's events (the first
//! [`MAX_RPC_EVENTS`]).
//!
//! Records are redacted with [`JsonrpcRecord::redacted`] before they reach
//! any sink: secrets inside params, ids, responses and events are replaced
//! like every other text of a [`super::RequestRecord`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::graphql::redact_value;
use crate::redact::Redactor;

/// How many events of a subscription a record keeps.
pub const MAX_RPC_EVENTS: usize = 1000;

/// The JSON-RPC side of one executed request: a call, a batch or a
/// subscription.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JsonrpcRecord {
    /// `http` or `ws`.
    pub transport: String,
    /// The calls went out as one JSON array.
    pub batch: bool,
    /// Every call, in file order.
    pub calls: Vec<RpcCallRecord>,
    /// Every try of an HTTP exchange (more than one with retries).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<RpcAttemptRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription: Option<RpcSubscriptionRecord>,
    /// `server rejected the batch` when one error answered a whole batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Why nothing was sent: the catalog or a method guard refused it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<RpcRefusal>,
    pub elapsed_ms: f64,
}

/// What stopped a call before it was sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcRefusal {
    /// The method or params do not match the OpenRPC catalog.
    Validation,
    /// A method guard refused the method.
    Guard,
}

/// One call of the exchange.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcCallRecord {
    pub method: String,
    /// The id sent; `None` for a notification.
    pub id: Option<Value>,
    /// The envelope sent.
    pub request: Value,
    /// The envelope that answered this call.
    pub response: Option<Value>,
    /// `result`, `error`, `notification` or `failed`.
    pub outcome: String,
    /// `rpc error <code>: <message>`, or why no usable response came back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

/// One try at sending the request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcAttemptRecord {
    pub status: Option<u16>,
    pub error: Option<String>,
    pub elapsed_ms: f64,
    /// The wait before the next attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_in_ms: Option<f64>,
}

/// The events a subscription collected.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcSubscriptionRecord {
    /// The result of the subscribe call.
    pub subscription_id: Value,
    /// Every event received, including those not kept.
    pub event_count: u64,
    /// The first [`MAX_RPC_EVENTS`] notification messages, in order.
    pub events: Vec<Value>,
    /// `max_events`, `duration`, `stopped` or `closed`.
    pub ended: String,
    /// Whether the unsubscribe call was sent.
    pub unsubscribed: bool,
    pub elapsed_ms: f64,
}

impl RpcSubscriptionRecord {
    /// A record keeping the first [`MAX_RPC_EVENTS`] of `events`.
    pub fn new(subscription_id: Value, event_count: u64, mut events: Vec<Value>) -> Self {
        events.truncate(MAX_RPC_EVENTS);
        Self {
            subscription_id,
            event_count,
            events,
            ended: String::new(),
            unsubscribed: false,
            elapsed_ms: 0.0,
        }
    }
}

impl JsonrpcRecord {
    /// This record with every secret `redactor` knows replaced in its
    /// envelopes, failures, attempts and events.
    #[must_use]
    pub fn redacted(mut self, redactor: &Redactor) -> Self {
        let text = |t: &str| redactor.text(t).into_owned();
        for call in &mut self.calls {
            call.id = call.id.as_ref().map(|id| redact_value(id, redactor));
            call.request = redact_value(&call.request, redactor);
            call.response = call.response.as_ref().map(|r| redact_value(r, redactor));
            call.failure = call.failure.as_deref().map(text);
        }
        for attempt in &mut self.attempts {
            attempt.error = attempt.error.as_deref().map(text);
        }
        if let Some(subscription) = &mut self.subscription {
            subscription.subscription_id = redact_value(&subscription.subscription_id, redactor);
            for event in &mut subscription.events {
                *event = redact_value(event, redactor);
            }
        }
        self
    }

    /// The failures of the calls, in call order.
    pub fn failures(&self) -> impl Iterator<Item = &str> {
        self.calls.iter().filter_map(|c| c.failure.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::Secret;
    use serde_json::json;

    #[test]
    fn only_the_first_thousand_events_are_kept() {
        let events = (0..1500).map(|n| json!({"n": n})).collect();
        let record = RpcSubscriptionRecord::new(json!("0x1"), 1500, events);
        assert_eq!(record.events.len(), MAX_RPC_EVENTS);
        assert_eq!(record.event_count, 1500);
        assert_eq!(record.events[999]["n"], 999);
    }

    #[test]
    fn params_responses_and_events_are_redacted() {
        let redactor = Redactor::new().with(&Secret::new("api_key", "k3y".to_owned()));
        let record = JsonrpcRecord {
            transport: "ws".into(),
            batch: false,
            calls: vec![RpcCallRecord {
                method: "getBalance".into(),
                id: Some(json!(1)),
                request: json!({"jsonrpc":"2.0","method":"getBalance","params":{"key":"k3y"},"id":1}),
                response: Some(json!({"jsonrpc":"2.0","id":1,"result":"echo k3y"})),
                outcome: "result".into(),
                failure: Some("rpc error 1: k3y".into()),
            }],
            attempts: vec![RpcAttemptRecord {
                status: Some(429),
                error: Some("HTTP 429 k3y".into()),
                elapsed_ms: 1.0,
                retry_in_ms: Some(10.0),
            }],
            subscription: Some(RpcSubscriptionRecord::new(
                json!("k3y"),
                1,
                vec![json!({"params":{"result":"k3y"}})],
            )),
            note: None,
            refusal: None,
            elapsed_ms: 2.0,
        };
        let shown = serde_json::to_string(&record.redacted(&redactor)).unwrap();
        assert!(!shown.contains("k3y"), "{shown}");
        assert!(
            shown.contains(r#""params":{"key":"«redacted:api_key»"}"#),
            "{shown}"
        );
    }
}
