//! The record of one produce or consume: what was sent or received,
//! with broker metadata, ready for output and history. Secrets are
//! replaced by [`MessagingExchange::to_json`].

use rustman_core::Redactor;
use rustman_core::model::{KafkaSecurity, MessagingKind, Operation};
use rustman_core::vars::ResolvedMessaging;
use serde::Serialize;
use serde_json::Value;

use crate::payload::{Header, cut, render};

/// One produced message and where the broker stored it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ProducedMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
    pub key: Option<Value>,
    pub headers: Vec<Header>,
    pub value: Option<Value>,
    /// Key, value and header bytes.
    pub size: usize,
    /// The Schema Registry schema the value was encoded with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_id: Option<u32>,
    /// How the broker acknowledged the message: `PUBACK`,
    /// `PUBREC/PUBREL/PUBCOMP` (MQTT) or `confirm` (AMQP).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ack: Option<String>,
}

/// One received message.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ConsumedMessage {
    /// The MQTT topic the message was published to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// The MQTT quality of service it was delivered with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qos: Option<u8>,
    /// The MQTT retain flag.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub retain: bool,
    /// The AMQP exchange and routing key it was published with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exchange: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing_key: Option<String>,
    /// AMQP: the broker delivered it before.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub redelivered: bool,
    /// MQTT 5 or AMQP message properties.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
    /// RFC 3339.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub key: Option<Value>,
    pub headers: Vec<Header>,
    pub value: Option<Value>,
    /// The value's size in bytes, as received.
    pub size: usize,
    /// The value was cut to `limits.max_message_bytes`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// The Schema Registry schema the value was decoded with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_schema_id: Option<u32>,
    /// Where the value breaks the AsyncAPI message schema.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub schema_violations: Vec<String>,
}

impl ConsumedMessage {
    /// A message whose value is `bytes`, kept up to `max_bytes`.
    pub fn with_value(bytes: &[u8], max_bytes: usize) -> Self {
        Self {
            value: Some(render(cut(bytes, max_bytes))),
            size: bytes.len(),
            truncated: bytes.len() > max_bytes,
            ..Self::default()
        }
    }
}

/// The connection security as recorded; the password is replaced on
/// output.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SecuritySummary {
    pub protocol: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mechanism: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

impl SecuritySummary {
    pub fn of(security: &KafkaSecurity) -> Self {
        Self {
            protocol: security.effective_protocol().as_str(),
            mechanism: security.mechanism.map(|m| m.as_str()),
            username: security.username.clone(),
            password: security.password.clone(),
        }
    }
}

/// One produce or consume.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MessagingExchange {
    pub protocol: &'static str,
    pub operation: &'static str,
    /// The broker address(es).
    pub brokers: String,
    pub destination: String,
    /// AMQP consume: the queue read (the temporary queue's server name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security: Option<SecuritySummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub produced: Vec<ProducedMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<ConsumedMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// The AsyncAPI message payloads were validated against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asyncapi: Option<String>,
    pub duration_ms: u64,
}

impl MessagingExchange {
    /// An empty exchange for `resolved`.
    pub fn start(resolved: &ResolvedMessaging) -> Self {
        Self {
            protocol: protocol_name(resolved.kind),
            operation: operation_name(resolved.operation),
            brokers: resolved.connection.url.text.clone(),
            destination: resolved.destination(),
            queue: None,
            security: resolved.security.as_ref().map(SecuritySummary::of),
            produced: Vec::new(),
            messages: Vec::new(),
            warnings: Vec::new(),
            asyncapi: None,
            duration_ms: 0,
        }
    }

    /// The exchange as JSON, not redacted. A consume always has a
    /// `messages` list, empty when nothing arrived.
    pub fn to_value(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or(Value::Null);
        let consume = self.operation == Operation::Consume.as_str();
        if let (true, Some(object)) = (consume, value.as_object_mut()) {
            object
                .entry("messages")
                .or_insert_with(|| Value::Array(Vec::new()));
        }
        value
    }

    /// The exchange as JSON with every secret of `redactor` replaced.
    pub fn to_json(&self, redactor: &Redactor) -> Value {
        let mut value = self.to_value();
        redactor.json(&mut value);
        value
    }
}

fn protocol_name(kind: MessagingKind) -> &'static str {
    kind.as_str()
}

fn operation_name(operation: Operation) -> &'static str {
    operation.as_str()
}
