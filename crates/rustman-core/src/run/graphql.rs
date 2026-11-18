//! What a GraphQL operation adds to its request record (add-graphql-client
//! "History for GraphQL exchanges"): the operation, its variables, the
//! result status and, for a subscription, its events.
//!
//! Records are redacted with [`GraphqlRecord::redacted`] before they reach
//! any sink, like every other text of a [`super::RequestRecord`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::redact::Redactor;

/// How many events of a subscription a record keeps.
pub const MAX_RECORDED_EVENTS: usize = 100;

/// The GraphQL side of one executed request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphqlRecord {
    /// The operation's own name; `None` for an anonymous operation.
    pub operation_name: Option<String>,
    /// `query`, `mutation` or `subscription`.
    pub operation_type: String,
    /// `http`, `ws` or `sse`.
    pub transport: String,
    /// The subscription protocol in use, such as `graphql-transport-ws` or
    /// `legacy subscriptions-transport-ws`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// The variables sent (uploads as `null`), redacted.
    pub variables: Value,
    /// `ok`, `partial` or `errors`; `None` when the response was not a
    /// GraphQL result.
    pub result: Option<String>,
    /// `APQ hit`, `APQ miss (registered)` or `APQ miss (not registered)`,
    /// with `apq: true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apq: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription: Option<SubscriptionRecord>,
}

/// The events a subscription received.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubscriptionRecord {
    /// Every event received, including those not kept.
    pub event_count: usize,
    /// The first [`MAX_RECORDED_EVENTS`] events, in order.
    pub events: Vec<EventRecord>,
    /// Why collection stopped: `complete`, `max_events`, `duration`,
    /// `stopped`, `closed` or `error`.
    pub ended: String,
    /// Why it failed when it did not end cleanly (closed without
    /// `complete`, with the WebSocket close code; an event too large).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

/// One event of a subscription.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventRecord {
    /// When it arrived, RFC 3339 UTC.
    pub ts: String,
    /// Milliseconds since the subscription started.
    pub elapsed_ms: f64,
    /// The execution result it carried (`data`, `errors`).
    pub payload: Value,
}

impl SubscriptionRecord {
    /// A record of `events`, keeping the first [`MAX_RECORDED_EVENTS`].
    pub fn new(mut events: Vec<EventRecord>, ended: &str) -> Self {
        let event_count = events.len();
        events.truncate(MAX_RECORDED_EVENTS);
        Self {
            event_count,
            events,
            ended: ended.to_owned(),
            failure: None,
        }
    }
}

impl GraphqlRecord {
    /// This record with every secret `redactor` knows replaced in its
    /// variables, operation name and event payloads.
    #[must_use]
    pub fn redacted(mut self, redactor: &Redactor) -> Self {
        let text = |t: &str| redactor.text(t).into_owned();
        self.operation_name = self.operation_name.as_deref().map(text);
        self.variables = redact_value(&self.variables, redactor);
        if let Some(subscription) = &mut self.subscription {
            for event in &mut subscription.events {
                event.payload = redact_value(&event.payload, redactor);
            }
            subscription.failure = subscription.failure.as_deref().map(text);
        }
        self
    }
}

/// `value` with every secret replaced in its strings and keys.
pub fn redact_value(value: &Value, redactor: &Redactor) -> Value {
    match value {
        Value::String(s) => Value::String(redactor.text(s).into_owned()),
        Value::Array(items) => {
            Value::Array(items.iter().map(|v| redact_value(v, redactor)).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (redactor.text(k).into_owned(), redact_value(v, redactor)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::Secret;
    use serde_json::json;

    fn event(n: usize) -> EventRecord {
        EventRecord {
            ts: "2026-11-18T00:00:00.000Z".into(),
            elapsed_ms: n as f64,
            payload: json!({"data": {"n": n}}),
        }
    }

    #[test]
    fn only_the_first_hundred_events_are_kept() {
        let record = SubscriptionRecord::new((0..150).map(event).collect(), "max_events");
        assert_eq!(record.event_count, 150);
        assert_eq!(record.events.len(), MAX_RECORDED_EVENTS);
        assert_eq!(record.events[99].payload["data"]["n"], 99);
    }

    #[test]
    fn variables_and_events_are_redacted() {
        let redactor = Redactor::new().with(&Secret::new("token", "s3cr3t".to_owned()));
        let record = GraphqlRecord {
            operation_name: Some("Login".into()),
            operation_type: "subscription".into(),
            transport: "ws".into(),
            protocol: Some("graphql-transport-ws".into()),
            variables: json!({"auth": {"token": "s3cr3t"}, "list": ["a s3cr3t"], "n": 1}),
            result: Some("ok".into()),
            apq: None,
            subscription: Some(SubscriptionRecord {
                failure: Some("closed (4403 s3cr3t)".into()),
                ..SubscriptionRecord::new(
                    vec![EventRecord {
                        payload: json!({"data": {"echo": "s3cr3t"}}),
                        ..event(0)
                    }],
                    "closed",
                )
            }),
        };
        let shown = serde_json::to_string(&record.redacted(&redactor)).unwrap();
        assert!(!shown.contains("s3cr3t"), "{shown}");
        assert!(shown.contains(r#""token":"«redacted:token»""#), "{shown}");
        assert!(shown.contains(r#""n":1"#), "{shown}");
    }
}
