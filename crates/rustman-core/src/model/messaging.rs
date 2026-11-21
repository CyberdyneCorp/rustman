//! The messaging request kinds `kafka`, `mqtt` and `amqp`
//! (messaging-requests "Operation field", "Message payloads"): one file
//! shape across the three brokers, either a `produce` or a `consume`.
//!
//! Clients run in the `rustman-messaging` crate; this module only holds
//! the file model and its load-time checks. Interpolation lives in
//! [`crate::vars::resolve_messaging`].

use std::fmt;
use std::path::Path;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::assertion::Assertion;
use super::auth::Auth;
use super::capture::Capture;
use super::common::{Checked, Validate, Variables};
use super::http::RequestSettings;
use super::ws::{RealtimeTls, tls_paths};
use crate::error::{LoadError, Position};
use crate::format::{JsonValue, Located, from_yaml};
use crate::secret::Hidden;

mod options;

pub use options::{AckMode, AsyncApiRef, MessageProperties, MqttVersion, NAMED_QUEUE_ERROR, Qos};

/// The load error of a missing or unknown `operation`.
pub const OPERATION_ERROR: &str = "messaging: operation must be produce or consume";

/// Messages after which a headless consume stops.
pub const MESSAGING_DEFAULT_MAX_MESSAGES: u32 = 10;

/// How long a headless consume runs at most.
pub const MESSAGING_DEFAULT_DURATION_MS: u64 = 30_000;

/// The largest message a consume keeps whole (1 MiB); larger values are
/// cut to this size and flagged.
pub const MESSAGING_DEFAULT_MAX_MESSAGE_BYTES: u64 = 1024 * 1024;

/// The three messaging kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MessagingKind {
    Kafka,
    Mqtt,
    Amqp,
}

impl MessagingKind {
    pub const ALL: [Self; 3] = [Self::Kafka, Self::Mqtt, Self::Amqp];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kafka => "kafka",
            Self::Mqtt => "mqtt",
            Self::Amqp => "amqp",
        }
    }

    pub fn parse(kind: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == kind)
    }
}

impl fmt::Display for MessagingKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `operation`: publish messages or read them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Operation {
    Produce,
    Consume,
}

impl Operation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Produce => "produce",
            Self::Consume => "consume",
        }
    }

    pub fn parse(operation: &str) -> Option<Self> {
        match operation {
            "produce" => Some(Self::Produce),
            "consume" => Some(Self::Consume),
            _ => None,
        }
    }
}

/// A `kind: kafka`, `kind: mqtt` or `kind: amqp` request file. Saving
/// writes `source` back unchanged.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessagingRequest {
    pub version: u64,
    pub kind: String,
    pub name: String,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    pub docs: Option<super::docs::DocsMeta>,
    /// `produce` or `consume`; checked by [`MessagingRequest::parse`].
    pub operation: Option<Located<String>>,
    /// Kafka bootstrap brokers: a list, or one comma-separated string.
    pub brokers: Option<Brokers>,
    /// The MQTT or AMQP broker URL.
    pub broker: Option<String>,
    /// The Kafka topic, or the MQTT topic (filter, for subscribe).
    pub topic: Option<String>,
    /// The AMQP exchange (`""` for the default exchange).
    pub exchange: Option<String>,
    pub routing_key: Option<String>,
    pub binding_key: Option<String>,
    /// An existing AMQP queue to consume from.
    pub queue: Option<String>,
    /// Kafka produce: the partition every message goes to.
    pub partition: Option<i32>,
    /// Kafka consume: the partitions read (default all).
    pub partitions: Option<Vec<i32>>,
    /// Kafka consume: where reading starts (default `latest`).
    pub offset: Option<StartOffset>,
    /// Kafka produce: the acknowledgement asked for (default `all`).
    pub acks: Option<Acks>,
    /// Kafka connection security: protocol and SASL credentials.
    pub security: Option<Checked<KafkaSecurity>>,
    /// MQTT and AMQP credentials.
    pub auth: Option<BrokerAuth>,
    pub tls: Option<RealtimeTls>,
    pub schema_registry: Option<SchemaRegistry>,
    /// MQTT: `3.1.1` or `5` (default `5`).
    pub protocol_version: Option<MqttVersion>,
    /// MQTT: the quality of service (default 1).
    pub qos: Option<Qos>,
    /// MQTT publish: ask the broker to retain the message.
    pub retain: Option<bool>,
    /// MQTT: a fixed client id (default `rustman-<8 hex>`).
    pub client_id: Option<String>,
    /// MQTT subscribe: start a clean session (default true).
    pub clean_session: Option<bool>,
    /// MQTT 5 and AMQP produce: properties sent with every message.
    pub properties: Option<MessageProperties>,
    /// AMQP publish: fail when no queue is bound for the routing key.
    pub mandatory: Option<bool>,
    /// AMQP consume from a named queue: accept that messages are removed.
    pub destructive: Option<bool>,
    /// AMQP consume from a named queue: `auto` or `manual`.
    pub ack: Option<AckMode>,
    /// AMQP consume from a named queue: reject every delivery back onto
    /// the queue, so nothing is removed.
    pub requeue: Option<bool>,
    /// The AsyncAPI message payloads are validated against.
    pub asyncapi: Option<AsyncApiRef>,
    /// Produce: the messages sent, in order.
    pub messages: Option<Vec<Located<Message>>>,
    pub limits: Option<Checked<MessagingLimits>>,
    /// When a consume starts within a run (default `immediately`).
    pub start: Option<Start>,
    pub variables: Option<Variables>,
    pub assertions: Option<Vec<Located<Assertion>>>,
    pub captures: Option<Vec<Checked<Capture>>>,
    /// The file text, written back unchanged on save.
    #[serde(skip)]
    pub source: String,
}

impl MessagingRequest {
    /// Parse and check the messaging file `file` whose text is `text`.
    pub fn parse(file: &Path, text: &str) -> Result<Self, LoadError> {
        let mut request: Self = from_yaml(file, text)?;
        let operation = request.checked_operation(file)?;
        let kind = MessagingKind::parse(&request.kind).unwrap_or(MessagingKind::Kafka);
        request
            .check_fields(kind, operation)
            .map_err(|message| LoadError::new(file, Some(START), message))?;
        request.source = text.to_owned();
        Ok(request)
    }

    fn checked_operation(&self, file: &Path) -> Result<Operation, LoadError> {
        let at = |o: &Located<String>| o.position();
        match &self.operation {
            Some(o) => Operation::parse(o.get())
                .ok_or_else(|| LoadError::new(file, at(o).or(Some(START)), OPERATION_ERROR)),
            None => Err(LoadError::new(file, Some(START), OPERATION_ERROR)),
        }
    }

    /// The messaging kind (`kafka` for a request built in code with
    /// another kind).
    pub fn messaging_kind(&self) -> MessagingKind {
        MessagingKind::parse(&self.kind).unwrap_or(MessagingKind::Kafka)
    }

    /// The operation (`consume` for a request built in code without one).
    pub fn effective_operation(&self) -> Operation {
        self.operation
            .as_ref()
            .and_then(|o| Operation::parse(o.get()))
            .unwrap_or(Operation::Consume)
    }

    pub fn effective_limits(&self) -> MessagingLimits {
        self.limits.as_deref().cloned().unwrap_or_default()
    }

    pub fn effective_start(&self) -> Start {
        self.start.unwrap_or_default()
    }

    pub fn effective_offset(&self) -> StartOffset {
        self.offset.clone().unwrap_or(StartOffset::Latest)
    }

    pub fn effective_acks(&self) -> Acks {
        self.acks.unwrap_or_default()
    }

    /// The messages, in file order.
    pub fn message_list(&self) -> Vec<Message> {
        let messages = self.messages.as_deref().unwrap_or_default();
        messages.iter().map(|m| m.get().clone()).collect()
    }

    /// The TLS settings, as a `kind: http` request has them.
    pub fn http_settings(&self) -> Option<RequestSettings> {
        self.tls.as_ref().map(RealtimeTls::settings)
    }

    /// MQTT: the protocol version (default `5`).
    pub fn effective_protocol_version(&self) -> MqttVersion {
        self.protocol_version.unwrap_or_default()
    }

    /// MQTT: the quality of service (default 1).
    pub fn effective_qos(&self) -> Qos {
        self.qos.unwrap_or_default()
    }

    /// AMQP: a consume that removes messages from a named queue.
    pub fn is_destructive_consume(&self) -> bool {
        self.messaging_kind() == MessagingKind::Amqp
            && self.effective_operation() == Operation::Consume
            && self.queue.is_some()
            && self.requeue != Some(true)
    }

    /// Every workspace file the request refers to.
    pub fn file_paths(&self) -> Vec<&Located<String>> {
        let mut paths = tls_paths(self.tls.as_ref());
        if let Some(asyncapi) = &self.asyncapi {
            paths.push(&asyncapi.document);
        }
        for message in self.messages.as_deref().unwrap_or_default() {
            let m = message.get();
            for payload in [&m.key, &m.value].into_iter().flatten() {
                if let Payload::File(path) = payload {
                    paths.push(path);
                }
            }
        }
        paths
    }

    /// The fields given, by name.
    fn given_fields(&self) -> [(&'static str, bool); 22] {
        [
            ("brokers", self.brokers.is_some()),
            ("broker", self.broker.is_some()),
            ("topic", self.topic.is_some()),
            ("exchange", self.exchange.is_some()),
            ("routing_key", self.routing_key.is_some()),
            ("binding_key", self.binding_key.is_some()),
            ("queue", self.queue.is_some()),
            ("partition", self.partition.is_some()),
            ("partitions", self.partitions.is_some()),
            ("offset", self.offset.is_some()),
            ("acks", self.acks.is_some()),
            ("security", self.security.is_some()),
            ("protocol_version", self.protocol_version.is_some()),
            ("qos", self.qos.is_some()),
            ("retain", self.retain.is_some()),
            ("client_id", self.client_id.is_some()),
            ("clean_session", self.clean_session.is_some()),
            ("properties", self.properties.is_some()),
            ("mandatory", self.mandatory.is_some()),
            ("destructive", self.destructive.is_some()),
            ("ack", self.ack.is_some()),
            ("requeue", self.requeue.is_some()),
        ]
    }

    /// The MQTT and AMQP fields, for their combined checks.
    fn broker_fields(
        &self,
        kind: MessagingKind,
        operation: Operation,
    ) -> options::BrokerFields<'_> {
        let messages = self.messages.as_deref().unwrap_or_default();
        options::BrokerFields {
            kind,
            operation,
            version: self.protocol_version,
            client_id: self.client_id.is_some(),
            clean_session: self.clean_session,
            has_headers: messages.iter().any(|m| m.get().headers.is_some()),
            has_keys: messages.iter().any(|m| m.get().key.is_some()),
            properties: self.properties.as_ref(),
            queue: self.queue.is_some(),
            exchange: self.exchange.is_some(),
            routing_key: self.routing_key.is_some(),
            destructive: self.destructive == Some(true),
            ack: self.ack,
            requeue: self.requeue == Some(true),
        }
    }

    /// Refuse fields that do not apply to `kind` and `operation`, and
    /// require the ones that must be there.
    fn check_fields(&self, kind: MessagingKind, operation: Operation) -> Result<(), String> {
        let allowed = allowed_fields(kind, operation);
        let extra = self
            .given_fields()
            .into_iter()
            .find(|(name, given)| *given && !allowed.contains(name));
        if let Some((name, _)) = extra {
            return Err(format!(
                "{kind}: '{name}' does not apply to {} requests",
                operation.as_str()
            ));
        }
        if kind == MessagingKind::Kafka && self.auth.is_some() {
            return Err("kafka: credentials go in 'security', not 'auth'".to_owned());
        }
        self.check_required(kind, operation)?;
        self.broker_fields(kind, operation).check()
    }

    fn check_required(&self, kind: MessagingKind, operation: Operation) -> Result<(), String> {
        let missing = |field: &str| Err(format!("{kind}: '{field}' is required"));
        match kind {
            MessagingKind::Kafka if self.brokers.is_none() => return missing("brokers"),
            MessagingKind::Kafka if self.topic.is_none() => return missing("topic"),
            MessagingKind::Mqtt | MessagingKind::Amqp if self.broker.is_none() => {
                return missing("broker");
            }
            MessagingKind::Mqtt if self.topic.is_none() => return missing("topic"),
            _ => {}
        }
        let has_messages = !self.messages.as_deref().unwrap_or_default().is_empty();
        match operation {
            Operation::Produce if !has_messages => Err(format!(
                "{kind}: a produce needs at least one entry in 'messages'"
            )),
            Operation::Consume if self.messages.is_some() => Err(format!(
                "{kind}: 'messages' only applies to produce requests"
            )),
            _ => Ok(()),
        }
    }
}

/// Where a load error without its own position is reported.
const START: Position = Position { line: 1, column: 1 };

/// The kind-specific fields `kind` accepts for `operation` (common fields
/// such as `tls` and `messages` are checked elsewhere).
fn allowed_fields(kind: MessagingKind, operation: Operation) -> &'static [&'static str] {
    match (kind, operation) {
        (MessagingKind::Kafka, Operation::Produce) => {
            &["brokers", "topic", "partition", "acks", "security"]
        }
        (MessagingKind::Kafka, Operation::Consume) => {
            &["brokers", "topic", "partitions", "offset", "security"]
        }
        (MessagingKind::Mqtt, Operation::Produce) => &[
            "broker",
            "topic",
            "protocol_version",
            "qos",
            "retain",
            "client_id",
            "properties",
        ],
        (MessagingKind::Mqtt, Operation::Consume) => &[
            "broker",
            "topic",
            "protocol_version",
            "qos",
            "client_id",
            "clean_session",
        ],
        (MessagingKind::Amqp, Operation::Produce) => &[
            "broker",
            "exchange",
            "routing_key",
            "mandatory",
            "properties",
        ],
        (MessagingKind::Amqp, Operation::Consume) => &[
            "broker",
            "exchange",
            "binding_key",
            "queue",
            "destructive",
            "ack",
            "requeue",
        ],
    }
}

/// Kafka bootstrap brokers, each `host:port`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Brokers(pub Vec<String>);

impl<'de> Deserialize<'de> for Brokers {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            One(String),
            Many(Vec<String>),
        }
        let list = match Raw::deserialize(deserializer)? {
            Raw::One(text) => vec![text],
            Raw::Many(list) => list,
        };
        if list.iter().all(|b| b.trim().is_empty()) {
            return Err(de::Error::custom("brokers must name at least one broker"));
        }
        Ok(Self(list))
    }
}

/// `offset`: where a Kafka consume starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartOffset {
    Earliest,
    Latest,
    /// An absolute offset in every partition read.
    Absolute(i64),
    /// The first message at or after an RFC 3339 time (interpolated).
    Timestamp(String),
}

impl<'de> Deserialize<'de> for StartOffset {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct At {
            timestamp: String,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Number(i64),
            Name(String),
            At(At),
        }
        let invalid = "offset must be earliest, latest, an offset number or {timestamp: RFC 3339}";
        match Raw::deserialize(deserializer).map_err(|_| de::Error::custom(invalid))? {
            Raw::Number(n) if n >= 0 => Ok(Self::Absolute(n)),
            Raw::Name(name) if name == "earliest" => Ok(Self::Earliest),
            Raw::Name(name) if name == "latest" => Ok(Self::Latest),
            Raw::At(at) => Ok(Self::Timestamp(at.timestamp)),
            _ => Err(de::Error::custom(invalid)),
        }
    }
}

/// `acks`: what a Kafka produce waits for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Acks {
    #[default]
    All,
    Leader,
    None,
}

impl Acks {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Leader => "leader",
            Self::None => "none",
        }
    }
}

/// `start`: when a consume step begins within a run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Start {
    /// The step runs on its own, in order.
    #[default]
    Immediately,
    /// The consume subscribes, the next step runs, then the consume ends.
    BeforeNext,
}

/// `security.protocol` of a Kafka connection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub enum KafkaProtocol {
    #[default]
    #[serde(rename = "PLAINTEXT")]
    Plaintext,
    #[serde(rename = "SSL")]
    Ssl,
    #[serde(rename = "SASL_PLAINTEXT")]
    SaslPlaintext,
    #[serde(rename = "SASL_SSL")]
    SaslSsl,
}

impl KafkaProtocol {
    pub fn uses_tls(self) -> bool {
        matches!(self, Self::Ssl | Self::SaslSsl)
    }

    pub fn uses_sasl(self) -> bool {
        matches!(self, Self::SaslPlaintext | Self::SaslSsl)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plaintext => "PLAINTEXT",
            Self::Ssl => "SSL",
            Self::SaslPlaintext => "SASL_PLAINTEXT",
            Self::SaslSsl => "SASL_SSL",
        }
    }
}

/// `security.mechanism`: the SASL mechanism.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum SaslMechanism {
    #[serde(rename = "PLAIN")]
    Plain,
    #[serde(rename = "SCRAM-SHA-256")]
    ScramSha256,
    #[serde(rename = "SCRAM-SHA-512")]
    ScramSha512,
}

impl SaslMechanism {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "PLAIN",
            Self::ScramSha256 => "SCRAM-SHA-256",
            Self::ScramSha512 => "SCRAM-SHA-512",
        }
    }
}

/// `security`: the Kafka protocol and, for `SASL_*`, the mechanism and
/// credentials. The password should come from an environment secret;
/// `Debug` hides it.
#[derive(Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KafkaSecurity {
    pub protocol: Option<KafkaProtocol>,
    pub mechanism: Option<SaslMechanism>,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl KafkaSecurity {
    pub fn effective_protocol(&self) -> KafkaProtocol {
        self.protocol.unwrap_or_default()
    }
}

impl Validate for KafkaSecurity {
    fn validate(&self) -> Result<(), String> {
        let sasl = self.effective_protocol().uses_sasl();
        let given = [
            ("mechanism", self.mechanism.is_some()),
            ("username", self.username.is_some()),
            ("password", self.password.is_some()),
        ];
        let protocol = self.effective_protocol().as_str();
        match given.iter().find(|(_, is_given)| *is_given != sasl) {
            Some((field, _)) if sasl => Err(format!("security: {protocol} requires '{field}'")),
            Some((field, _)) => Err(format!(
                "security: '{field}' needs protocol SASL_PLAINTEXT or SASL_SSL"
            )),
            None => Ok(()),
        }
    }
}

impl fmt::Debug for KafkaSecurity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KafkaSecurity")
            .field("protocol", &self.protocol)
            .field("mechanism", &self.mechanism)
            .field("username", &self.username)
            .field(
                "password",
                &self.password.as_ref().map(|_| Hidden("password")),
            )
            .finish()
    }
}

/// `auth`: MQTT and AMQP username and password. `Debug` hides the
/// password.
#[derive(Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerAuth {
    pub username: String,
    pub password: Option<String>,
}

impl fmt::Debug for BrokerAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrokerAuth")
            .field("username", &self.username)
            .field(
                "password",
                &self.password.as_ref().map(|_| Hidden("password")),
            )
            .finish()
    }
}

/// `schema_registry`: a Confluent-compatible Schema Registry used to
/// decode (and, with `subject`, encode) payloads in the wire format.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaRegistry {
    pub url: String,
    /// Produce: encode values with this subject's schema.
    pub subject: Option<String>,
    /// Produce: the subject version (default `latest`).
    pub schema_version: Option<SchemaVersion>,
    /// How requests to the registry authenticate (basic or bearer).
    pub auth: Option<Auth>,
    pub tls: Option<RealtimeTls>,
}

/// `schema_version`: a version number or `latest`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum SchemaVersion {
    Number(u32),
    Name(String),
}

impl SchemaVersion {
    /// The version as the registry's URL names it.
    pub fn path_segment(&self) -> String {
        match self {
            Self::Number(n) => n.to_string(),
            Self::Name(name) => name.clone(),
        }
    }
}

/// `limits`: headless bounds and the size kept per message.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MessagingLimits {
    pub max_messages: Option<u32>,
    pub duration_ms: Option<u64>,
    pub max_message_bytes: Option<u64>,
}

impl Validate for MessagingLimits {
    fn validate(&self) -> Result<(), String> {
        let zero = [
            ("max_messages", self.max_messages == Some(0)),
            ("duration_ms", self.duration_ms == Some(0)),
            ("max_message_bytes", self.max_message_bytes == Some(0)),
        ];
        match zero.iter().find(|(_, is_zero)| *is_zero) {
            Some((name, _)) => Err(format!("limits.{name} must be at least 1")),
            None => Ok(()),
        }
    }
}

impl MessagingLimits {
    pub fn effective_max_messages(&self) -> u32 {
        self.max_messages.unwrap_or(MESSAGING_DEFAULT_MAX_MESSAGES)
    }

    pub fn effective_duration_ms(&self) -> u64 {
        self.duration_ms.unwrap_or(MESSAGING_DEFAULT_DURATION_MS)
    }

    pub fn effective_max_message_bytes(&self) -> u64 {
        self.max_message_bytes
            .unwrap_or(MESSAGING_DEFAULT_MAX_MESSAGE_BYTES)
    }
}

/// One message to produce.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub key: Option<Payload>,
    /// `null` or absent sends no value (a Kafka tombstone).
    pub value: Option<Payload>,
    /// Ordered name/value pairs.
    pub headers: Option<Vec<MessageHeader>>,
}

/// A message header.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageHeader {
    pub name: String,
    pub value: String,
}

/// The bytes of a key or value.
#[derive(Clone, Debug, PartialEq)]
pub enum Payload {
    /// A string, sent as UTF-8 text.
    Text(String),
    /// Any other YAML/JSON value, sent as compact JSON.
    Json(Value),
    /// `{base64: ...}`: raw bytes.
    Base64(String),
    /// `{file: path}`: a file inside the workspace, relative to the
    /// request file.
    File(Located<String>),
}

impl<'de> Deserialize<'de> for Payload {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let JsonValue(value) = JsonValue::deserialize(deserializer)?;
        Ok(Self::from_value(value))
    }
}

impl Payload {
    /// A payload from a YAML/JSON value: a one-key `base64` or `file`
    /// mapping with a string is bytes, a string is text, anything else
    /// JSON.
    pub fn from_value(value: Value) -> Self {
        if let Value::Object(map) = &value
            && map.len() == 1
            && let Some((name, Value::String(text))) = map.iter().next()
        {
            match name.as_str() {
                "base64" => return Self::Base64(text.clone()),
                "file" => return Self::File(Located::new(text.clone())),
                _ => {}
            }
        }
        match value {
            Value::String(text) => Self::Text(text),
            other => Self::Json(other),
        }
    }
}

#[cfg(test)]
mod tests;
