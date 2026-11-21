//! Safety classification of messaging operations (messaging-requests
//! "Safety gating", design D6): producing is a mutation, consuming from a
//! named AMQP queue without requeue, or through a persistent MQTT session
//! (which takes the messages the broker queued for that client id), is
//! destructive, and every other consume is a read. Humans are not gated; agent paths check the class
//! against their permission before anything connects.

use std::fmt;

use rustman_core::model::{MessagingKind, Operation};
use rustman_core::safety::{MethodClass, Permission};
use rustman_core::vars::ResolvedMessaging;

use crate::MessagingError;
use crate::endpoint::{self, Endpoint};

/// Why an agent cannot produce.
pub const PRODUCE_NEEDS_MUTATIONS: &str = "produce requires allow_mutations";

/// Why an agent cannot drain a named queue.
pub const DESTRUCTIVE_NEEDS_PERMISSION: &str =
    "consuming from an existing queue requires allow_destructive";

/// Why an agent cannot resume a persistent MQTT session.
pub const PERSISTENT_SESSION_NEEDS_PERMISSION: &str =
    "consuming through a persistent session (clean_session: false) requires allow_destructive";

/// The policy class of `resolved`.
pub fn classify(resolved: &ResolvedMessaging) -> MethodClass {
    match resolved.operation {
        Operation::Produce => MethodClass::Mutation,
        Operation::Consume if drains_queue(resolved) => MethodClass::Destructive,
        Operation::Consume => MethodClass::ReadOnly,
    }
}

/// An AMQP consume from a named queue that removes what it reads, or an
/// MQTT subscribe that resumes a persistent session.
fn drains_queue(resolved: &ResolvedMessaging) -> bool {
    match resolved.kind {
        MessagingKind::Amqp => resolved.queue.is_some() && !resolved.amqp.requeue,
        MessagingKind::Mqtt => resumes_session(resolved),
        MessagingKind::Kafka => false,
    }
}

/// An MQTT request with `clean_session: false`.
fn resumes_session(resolved: &ResolvedMessaging) -> bool {
    resolved.kind == MessagingKind::Mqtt && !resolved.mqtt.clean_session
}

/// The Schema Registry URL a Kafka request fetches schemas from: an
/// egress target of its own.
pub fn registry_url(resolved: &ResolvedMessaging) -> Option<&str> {
    let registry = resolved.schema_registry.as_ref()?;
    (resolved.kind == MessagingKind::Kafka).then_some(registry.http.url.text.as_str())
}

/// The `http` or `https` URL each broker of `resolved` is checked as by
/// an allowlist, from the very endpoints the connectors dial
/// ([`crate::endpoint::of`]): a Kafka `host:port` as `http://host:port`,
/// `mqtt://`, `tcp://` and `amqp://` as `http://`, `mqtts://`, `ssl://`
/// and `amqps://` as `https://`, WebSocket transports as their HTTP
/// origin, always with the port. Never carries credentials. A broker
/// that does not parse is an error, and is refused.
pub fn broker_urls(resolved: &ResolvedMessaging) -> Result<Vec<String>, MessagingError> {
    let endpoints = endpoint::of(resolved)?;
    Ok(endpoints.iter().map(Endpoint::policy_url).collect())
}

/// What an audit line names as the target of `resolved`: its policy URLs,
/// or [`endpoint::INVALID_BROKER`]; never a raw broker string.
pub fn audit_target(resolved: &ResolvedMessaging) -> String {
    broker_urls(resolved).map_or_else(|_| endpoint::INVALID_BROKER.to_owned(), |u| u.join(","))
}

/// `url` with a broker scheme (`mqtt`, `mqtts`, `tcp`, `ssl`, `ws`,
/// `wss`, `amqp`, `amqps`) as the HTTP URL an allowlist names; `None` for
/// any other scheme or a broker that does not parse.
pub fn http_url(url: &str) -> Option<String> {
    let (scheme, _) = url.split_once("://")?;
    let endpoint = match scheme.to_ascii_lowercase().as_str() {
        "mqtt" | "mqtts" | "tcp" | "ssl" | "ws" | "wss" => endpoint::mqtt(url).ok()?,
        "amqp" | "amqps" => endpoint::amqp(url).ok()?.0,
        _ => return None,
    };
    Some(endpoint.policy_url())
}

/// The safety policy refused an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal(pub &'static str);

impl Refusal {
    /// The reason, without the `blocked:` prefix.
    pub fn reason(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "blocked: {}", self.0)
    }
}

/// Refuse `resolved` unless `permission` allows its class.
pub fn check(permission: Permission, resolved: &ResolvedMessaging) -> Result<(), Refusal> {
    match classify(resolved) {
        class if permission.allows(class) => Ok(()),
        MethodClass::Mutation => Err(Refusal(PRODUCE_NEEDS_MUTATIONS)),
        _ if resumes_session(resolved) => Err(Refusal(PERSISTENT_SESSION_NEEDS_PERMISSION)),
        _ => Err(Refusal(DESTRUCTIVE_NEEDS_PERMISSION)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_schemes_read_as_http_origins_with_default_ports() {
        let cases = [
            ("mqtt://broker.local", "http://broker.local:1883"),
            ("mqtts://broker.local:9883/x", "https://broker.local:9883"),
            ("tcp://app:pw@broker.local", "http://broker.local:1883"),
            ("ssl://broker.local", "https://broker.local:8883"),
            ("amqp://guest:pw@rabbit:5673/%2f", "http://rabbit:5673"),
            ("amqps://rabbit", "https://rabbit:5671"),
            ("ws://127.0.0.1:9001/mqtt", "http://127.0.0.1:9001"),
            ("wss://[::1]/mqtt", "https://[::1]:443"),
        ];
        for (url, http) in cases {
            assert_eq!(http_url(url).as_deref(), Some(http), "{url}");
        }
        assert_eq!(http_url("https://api.example.com"), None);
        assert_eq!(http_url("localhost:9092"), None);
    }

    #[test]
    fn a_backslash_is_not_read_as_the_host_after_it() {
        // Regression: the policy took the host after the last `@` while
        // the WebSocket connector took the one before the backslash.
        assert_eq!(http_url("ws://evil.example\\@allowed.example/mqtt"), None);
        assert_eq!(http_url("amqp://evil.example\\@allowed.example/"), None);
    }
}
