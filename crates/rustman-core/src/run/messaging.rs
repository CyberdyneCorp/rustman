//! What a messaging operation adds to its request record
//! (messaging-requests "History recording"): the protocol, the operation
//! and the exchange (brokers, destination, per-message metadata and
//! payloads).
//!
//! Records are redacted with [`MessagingRecord::redacted`] before they
//! reach any sink. Redaction sees each key and value whole and only then
//! cuts one larger than [`MAX_RECORDED_PAYLOAD`] to a text preview with
//! `truncated: true`; the message's `size` keeps its full size.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::graphql::redact_value;
use super::realtime::MAX_RECORDED_PAYLOAD;
use crate::redact::Redactor;

/// The messaging side of one executed `kind: kafka`, `kind: mqtt` or
/// `kind: amqp` request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessagingRecord {
    /// `kafka`, `mqtt` or `amqp`.
    pub protocol: String,
    /// `produce` or `consume`.
    pub operation: String,
    /// The safety policy refused the operation before anything was sent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub blocked: bool,
    /// The exchange: `brokers`, `destination`, `produced` and `messages`
    /// (each with its metadata, key, headers, value and `size`) and
    /// `warnings`. Absent when nothing was sent or received.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exchange: Option<Value>,
}

impl MessagingRecord {
    /// This record with every secret `redactor` knows replaced, then every
    /// key and value over [`MAX_RECORDED_PAYLOAD`] bytes cut to a preview.
    #[must_use]
    pub fn redacted(mut self, redactor: &Redactor) -> Self {
        self.exchange = self.exchange.map(|exchange| {
            let mut exchange = redact_value(&exchange, redactor);
            for list in ["produced", "messages"] {
                if let Some(Value::Array(messages)) = exchange.get_mut(list) {
                    messages.iter_mut().for_each(preview_message);
                }
            }
            exchange
        });
        self
    }
}

/// Cut the key and value of `message` to their previews.
fn preview_message(message: &mut Value) {
    let Some(fields) = message.as_object_mut() else {
        return;
    };
    let mut cut = false;
    for name in ["key", "value"] {
        if let Some(payload) = fields.get_mut(name) {
            cut |= preview(payload);
        }
    }
    if cut {
        fields.insert("truncated".into(), Value::Bool(true));
    }
}

/// Replace `payload` by its first [`MAX_RECORDED_PAYLOAD`] bytes as text
/// (JSON for a structured value) when it is larger; whether it was cut.
fn preview(payload: &mut Value) -> bool {
    let text = match &*payload {
        Value::Null => return false,
        Value::String(text) if text.len() <= MAX_RECORDED_PAYLOAD => return false,
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    if text.len() <= MAX_RECORDED_PAYLOAD {
        return false;
    }
    let mut end = MAX_RECORDED_PAYLOAD;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    *payload = Value::String(text[..end].to_owned());
    true
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::secret::Secret;

    fn record(exchange: Value) -> MessagingRecord {
        MessagingRecord {
            protocol: "kafka".into(),
            operation: "consume".into(),
            blocked: false,
            exchange: Some(exchange),
        }
    }

    #[test]
    fn a_200_kib_value_keeps_a_64_kib_preview_and_its_size() {
        let big = "x".repeat(200 * 1024);
        let redacted = record(json!({
            "messages": [
                {"offset": 0, "value": big, "size": 204_800},
                {"offset": 1, "value": {"id": 1}, "size": 8},
            ]
        }))
        .redacted(&Redactor::new());
        let messages = &redacted.exchange.unwrap()["messages"];
        let first = &messages[0];
        assert_eq!(first["value"].as_str().unwrap().len(), MAX_RECORDED_PAYLOAD);
        assert_eq!(first["size"], 204_800);
        assert_eq!(first["truncated"], true);
        assert_eq!(messages[1]["value"], json!({"id": 1}));
        assert!(messages[1].get("truncated").is_none());
    }

    #[test]
    fn large_json_values_become_text_previews_cut_at_a_character() {
        let items: Vec<String> = (0..20_000).map(|i| format!("é{i}")).collect();
        let mut value = json!({"items": items});
        assert!(preview(&mut value));
        let text = value.as_str().unwrap();
        assert!(text.len() <= MAX_RECORDED_PAYLOAD);
        assert!(text.starts_with("{\"items\":[\"é0\""));
    }

    #[test]
    fn secrets_are_replaced_before_the_cut() {
        let redactor = Redactor::new().with(&Secret::new("kafka_pw", "pw-canary-1".to_owned()));
        let mut big = "y".repeat(MAX_RECORDED_PAYLOAD - 4);
        big.push_str("pw-canary-1");
        let redacted = record(json!({
            "security": {"password": "pw-canary-1"},
            "produced": [{"key": "pw-canary-1", "value": big}]
        }))
        .redacted(&redactor);
        let text = serde_json::to_string(&redacted).unwrap();
        assert!(!text.contains("pw-canary"), "{text}");
        assert!(text.contains("«redacted:kafka_pw»"));
    }
}
