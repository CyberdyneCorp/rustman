//! The MQTT and AMQP options of a messaging file (messaging-requests
//! "MQTT publish", "MQTT subscribe", "AMQP publish", "AMQP consume") and
//! the AsyncAPI message a file can be validated against.

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use super::{MessagingKind, Operation};
use crate::format::Located;

/// The load error of a named-queue consume without an opt-in.
pub const NAMED_QUEUE_ERROR: &str =
    "amqp: consuming from an existing queue removes messages; set destructive: true or use requeue";

/// `protocol_version` of an MQTT connection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum MqttVersion {
    #[serde(rename = "3.1.1")]
    V311,
    #[default]
    #[serde(rename = "5")]
    V5,
}

impl MqttVersion {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::V311 => "3.1.1",
            Self::V5 => "5",
        }
    }
}

impl<'de> Deserialize<'de> for MqttVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Number(u64),
            Text(String),
        }
        match Raw::deserialize(deserializer) {
            Ok(Raw::Number(5)) => Ok(Self::V5),
            Ok(Raw::Text(t)) if t == "5" || t == "5.0" => Ok(Self::V5),
            Ok(Raw::Text(t)) if t == "3.1.1" => Ok(Self::V311),
            _ => Err(de::Error::custom("protocol_version must be 3.1.1 or 5")),
        }
    }
}

/// An MQTT quality of service level: 0, 1 or 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Qos(u8);

impl Qos {
    pub const AT_MOST_ONCE: Self = Self(0);
    pub const AT_LEAST_ONCE: Self = Self(1);
    pub const EXACTLY_ONCE: Self = Self(2);

    /// The level, `0..=2`; `None` for anything else.
    pub fn new(level: u8) -> Option<Self> {
        (level <= 2).then_some(Self(level))
    }

    pub fn level(self) -> u8 {
        self.0
    }
}

impl Default for Qos {
    fn default() -> Self {
        Self::AT_LEAST_ONCE
    }
}

impl<'de> Deserialize<'de> for Qos {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let level = u8::deserialize(deserializer).ok().and_then(Self::new);
        level.ok_or_else(|| de::Error::custom("qos must be 0, 1 or 2"))
    }
}

/// `ack`: how an AMQP consume acknowledges deliveries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AckMode {
    /// The broker removes each message as it is delivered.
    #[default]
    Auto,
    /// Rustman acknowledges (or, with `requeue`, rejects) each delivery.
    Manual,
}

/// `properties`: message properties sent with every produced message.
/// Which ones apply depends on the kind (see [`MessageProperties::check`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MessageProperties {
    /// MQTT 5 and AMQP: the payload's MIME type.
    pub content_type: Option<String>,
    /// MQTT 5 (correlation data) and AMQP.
    pub correlation_id: Option<String>,
    /// AMQP: where replies go.
    pub reply_to: Option<String>,
    /// MQTT 5: where replies go.
    pub response_topic: Option<String>,
    /// MQTT 5: seconds until the broker discards an undelivered message.
    pub message_expiry: Option<u32>,
    /// AMQP: 1 (transient) or 2 (persistent).
    pub delivery_mode: Option<u8>,
    /// AMQP: per-message TTL in milliseconds.
    pub expiration: Option<u64>,
}

impl MessageProperties {
    /// The properties given, by name.
    fn given(&self) -> [(&'static str, bool); 7] {
        [
            ("content_type", self.content_type.is_some()),
            ("correlation_id", self.correlation_id.is_some()),
            ("reply_to", self.reply_to.is_some()),
            ("response_topic", self.response_topic.is_some()),
            ("message_expiry", self.message_expiry.is_some()),
            ("delivery_mode", self.delivery_mode.is_some()),
            ("expiration", self.expiration.is_some()),
        ]
    }

    /// Refuse properties `kind` cannot send.
    pub fn check(&self, kind: MessagingKind) -> Result<(), String> {
        let allowed: &[&str] = match kind {
            MessagingKind::Mqtt => &[
                "content_type",
                "correlation_id",
                "response_topic",
                "message_expiry",
            ],
            MessagingKind::Amqp => &[
                "content_type",
                "correlation_id",
                "reply_to",
                "delivery_mode",
                "expiration",
            ],
            MessagingKind::Kafka => &[],
        };
        let extra = self
            .given()
            .into_iter()
            .find(|(n, g)| *g && !allowed.contains(n));
        if let Some((name, _)) = extra {
            return Err(format!("{kind}: property '{name}' does not apply"));
        }
        match self.delivery_mode {
            Some(1 | 2) | None => Ok(()),
            Some(_) => Err("amqp: delivery_mode must be 1 or 2".to_owned()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.given().iter().all(|(_, given)| !given)
    }
}

/// `asyncapi`: the AsyncAPI message whose payload schema validates
/// produced and consumed values.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AsyncApiRef {
    /// The document, relative to the request file, inside the workspace.
    pub document: Located<String>,
    /// The message name (its key under `components/messages` or a
    /// channel's `messages`, or its `name`).
    pub message: String,
}

/// The MQTT and AMQP fields of a messaging file, checked together.
pub(super) struct BrokerFields<'a> {
    pub kind: MessagingKind,
    pub operation: Operation,
    pub version: Option<MqttVersion>,
    pub client_id: bool,
    pub clean_session: Option<bool>,
    pub has_headers: bool,
    pub has_keys: bool,
    pub properties: Option<&'a MessageProperties>,
    pub queue: bool,
    pub exchange: bool,
    pub routing_key: bool,
    pub destructive: bool,
    pub ack: Option<AckMode>,
    pub requeue: bool,
}

impl BrokerFields<'_> {
    pub fn check(&self) -> Result<(), String> {
        if let Some(properties) = self.properties {
            properties.check(self.kind)?;
        }
        if self.has_keys && self.kind != MessagingKind::Kafka {
            return Err(format!("{}: messages have no 'key'", self.kind));
        }
        match self.kind {
            MessagingKind::Mqtt => self.check_mqtt(),
            MessagingKind::Amqp => self.check_amqp(),
            MessagingKind::Kafka => Ok(()),
        }
    }

    fn check_mqtt(&self) -> Result<(), String> {
        let v311 = self.version == Some(MqttVersion::V311);
        let has_properties = self.properties.is_some_and(|p| !p.is_empty());
        if v311 && self.has_headers {
            return Err(
                "mqtt: message headers are user properties and need protocol_version 5".into(),
            );
        }
        if v311 && has_properties {
            return Err("mqtt: 'properties' need protocol_version 5".into());
        }
        if self.clean_session == Some(false) && !self.client_id {
            return Err("mqtt: clean_session: false needs a fixed client_id".into());
        }
        Ok(())
    }

    fn check_amqp(&self) -> Result<(), String> {
        match self.operation {
            Operation::Produce if !self.routing_key => {
                Err("amqp: 'routing_key' is required".into())
            }
            Operation::Produce => Ok(()),
            Operation::Consume => self.check_amqp_consume(),
        }
    }

    fn check_amqp_consume(&self) -> Result<(), String> {
        if self.queue == self.exchange {
            return Err("amqp: a consume needs either 'exchange' or 'queue'".into());
        }
        if !self.queue && (self.destructive || self.requeue || self.ack.is_some()) {
            return Err("amqp: 'destructive', 'ack' and 'requeue' apply to a named 'queue'".into());
        }
        if self.requeue && self.ack == Some(AckMode::Auto) {
            return Err("amqp: requeue needs ack: manual".into());
        }
        if self.queue && !self.destructive && !self.requeue {
            return Err(NAMED_QUEUE_ERROR.into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qos_and_versions_parse() {
        let qos: Qos = serde_json::from_str("2").unwrap();
        assert_eq!(qos.level(), 2);
        assert!(serde_json::from_str::<Qos>("3").is_err());
        assert_eq!(Qos::default().level(), 1);
        for (text, version) in [("5", MqttVersion::V5), ("\"3.1.1\"", MqttVersion::V311)] {
            assert_eq!(serde_json::from_str::<MqttVersion>(text).unwrap(), version);
        }
        let err = serde_json::from_str::<MqttVersion>("4").unwrap_err();
        assert!(err.to_string().contains("3.1.1 or 5"), "{err}");
    }

    #[test]
    fn properties_are_checked_per_kind() {
        let p = MessageProperties {
            reply_to: Some("r".into()),
            ..MessageProperties::default()
        };
        assert!(p.check(MessagingKind::Amqp).is_ok());
        assert_eq!(
            p.check(MessagingKind::Mqtt).unwrap_err(),
            "mqtt: property 'reply_to' does not apply"
        );
        let p = MessageProperties {
            delivery_mode: Some(3),
            ..MessageProperties::default()
        };
        assert_eq!(
            p.check(MessagingKind::Amqp).unwrap_err(),
            "amqp: delivery_mode must be 1 or 2"
        );
        assert!(MessageProperties::default().is_empty());
    }
}
