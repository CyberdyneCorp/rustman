//! Connecting to a Kafka cluster (messaging-requests "Kafka security"):
//! `PLAINTEXT`, `SSL` through `rustman-http`'s shared TLS settings
//! builder, and SASL `PLAIN`, `SCRAM-SHA-256` and `SCRAM-SHA-512` over
//! either. Connection attempts give up after the connect timeout instead
//! of retrying forever, the bootstrap brokers dialed are the ones the
//! safety policy checked, and response frames are bounded by
//! [`max_frame_bytes`].

use std::sync::Arc;
use std::time::Duration;

use rskafka::BackoffConfig;
use rskafka::client::{Client, ClientBuilder, Credentials, SaslConfig};
use rustman_core::model::{KafkaSecurity, MessagingLimits, SaslMechanism};
use rustman_core::vars::ResolvedMessaging;

use crate::error::MessagingError;

const PROTOCOL: &str = "kafka";

/// Whether the connection uses TLS: an `SSL` or `SASL_SSL` protocol, or a
/// `tls` block.
pub fn uses_tls(resolved: &ResolvedMessaging) -> bool {
    let protocol = resolved
        .security
        .as_ref()
        .map(KafkaSecurity::effective_protocol);
    resolved.tls || protocol.is_some_and(|p| p.uses_tls())
}

/// The SASL settings of `security`, when its protocol is `SASL_*`.
pub fn sasl_config(security: Option<&KafkaSecurity>) -> Option<SaslConfig> {
    let security = security.filter(|s| s.effective_protocol().uses_sasl())?;
    let credentials = Credentials::new(
        security.username.clone().unwrap_or_default(),
        security.password.clone().unwrap_or_default(),
    );
    let config = match security.mechanism? {
        SaslMechanism::Plain => SaslConfig::Plain(credentials),
        SaslMechanism::ScramSha256 => SaslConfig::ScramSha256(credentials),
        SaslMechanism::ScramSha512 => SaslConfig::ScramSha512(credentials),
    };
    Some(config)
}

/// Bytes a response frame may add around the records it carries.
pub const FRAME_OVERHEAD: usize = 1024 * 1024;

/// The largest response frame read from a broker: a fetch of
/// [`super::consume::FETCH_MAX_BYTES`], or one record of
/// `limits.max_message_bytes` (a broker returns at least one whole batch),
/// plus [`FRAME_OVERHEAD`]. rskafka's own default is 100 MB.
pub fn max_frame_bytes(limits: &MessagingLimits) -> usize {
    let record = usize::try_from(limits.effective_max_message_bytes()).unwrap_or(usize::MAX);
    let fetch = super::consume::FETCH_MAX_BYTES as usize;
    record.max(fetch).saturating_add(FRAME_OVERHEAD)
}

/// The bootstrap addresses of `resolved`, as parsed for the safety
/// policy (see [`crate::endpoint::kafka`]).
pub fn bootstrap(resolved: &ResolvedMessaging) -> Result<Vec<String>, MessagingError> {
    let endpoints = crate::endpoint::of(resolved)?;
    Ok(endpoints
        .iter()
        .map(crate::endpoint::Endpoint::address)
        .collect())
}

/// A client for the cluster of `resolved`.
pub async fn connect(
    resolved: &ResolvedMessaging,
    timeout: Duration,
) -> Result<Client, MessagingError> {
    let backoff = BackoffConfig {
        deadline: Some(timeout),
        ..BackoffConfig::default()
    };
    let mut builder = ClientBuilder::new(bootstrap(resolved)?)
        .client_id("rustman")
        .max_message_size(max_frame_bytes(&resolved.limits))
        .backoff_config(backoff);
    if uses_tls(resolved) {
        builder = builder.tls_config(Arc::new(crate::tls::client_config(resolved)?));
    }
    let security = resolved.security.as_ref();
    if let Some(sasl) = sasl_config(security) {
        builder = builder.sasl_config(sasl);
    }
    let mechanism = security.and_then(|s| s.mechanism).map(|m| m.as_str());
    builder
        .build()
        .await
        .map_err(|e| connect_error(&e.to_string(), mechanism))
}

/// The error of a failed connection. rskafka folds the per-broker errors
/// into one message, so a SASL failure is recognized by its text.
pub fn connect_error(message: &str, mechanism: Option<&'static str>) -> MessagingError {
    match mechanism {
        Some(mechanism) if is_sasl_failure(message) => MessagingError::AuthFailed {
            protocol: PROTOCOL,
            mechanism,
        },
        _ => MessagingError::broker(PROTOCOL, format!("cannot connect: {message}")),
    }
}

fn is_sasl_failure(message: &str) -> bool {
    message.contains("Sasl handshake failed")
}

#[cfg(test)]
mod tests {
    use rustman_core::model::KafkaProtocol;

    use super::*;

    fn security(protocol: KafkaProtocol, mechanism: Option<SaslMechanism>) -> KafkaSecurity {
        KafkaSecurity {
            protocol: Some(protocol),
            mechanism,
            username: Some("app".into()),
            password: Some("pw".into()),
        }
    }

    #[test]
    fn sasl_mechanisms_map_to_rskafka_configs() {
        let plain = security(KafkaProtocol::SaslPlaintext, Some(SaslMechanism::Plain));
        assert!(matches!(
            sasl_config(Some(&plain)),
            Some(SaslConfig::Plain(_))
        ));
        let scram = security(KafkaProtocol::SaslSsl, Some(SaslMechanism::ScramSha256));
        assert!(matches!(
            sasl_config(Some(&scram)),
            Some(SaslConfig::ScramSha256(_))
        ));
        let scram = security(KafkaProtocol::SaslSsl, Some(SaslMechanism::ScramSha512));
        let Some(SaslConfig::ScramSha512(credentials)) = sasl_config(Some(&scram)) else {
            panic!("scram-sha-512");
        };
        assert_eq!(
            (credentials.username.as_str(), credentials.password.as_str()),
            ("app", "pw")
        );
        let ssl = security(KafkaProtocol::Ssl, None);
        assert!(sasl_config(Some(&ssl)).is_none());
        assert!(sasl_config(None).is_none());
    }

    #[test]
    fn response_frames_are_bounded_by_the_message_limit() {
        // Regression: rskafka's default let a broker send 100 MB frames,
        // and 256 of them could wait for the collector.
        let mib = 1024 * 1024;
        let default = MessagingLimits::default();
        assert_eq!(max_frame_bytes(&default), 5 * mib);
        let big = MessagingLimits {
            max_message_bytes: Some(32 * 1024 * 1024),
            ..MessagingLimits::default()
        };
        assert_eq!(max_frame_bytes(&big), 33 * mib);
        assert_eq!(super::super::queue_capacity(5 * mib), 12);
        assert_eq!(super::super::queue_capacity(33 * mib), 1);
        assert_eq!(super::super::queue_capacity(1), 256);
        for limits in [default, big] {
            let frame = max_frame_bytes(&limits);
            let queued = super::super::queue_capacity(frame) * frame;
            assert!(queued <= super::super::QUEUE_BYTES.max(frame), "{queued}");
        }
    }

    #[test]
    fn sasl_failures_become_authentication_errors() {
        let text = "Connection error: all retries failed: Retry exceeded deadline. Source: error connecting to broker \"k:9092\": Sasl handshake failed: API error: SaslAuthenticationFailed";
        let err = connect_error(text, Some("SCRAM-SHA-512"));
        assert_eq!(
            err.to_string(),
            "kafka: authentication failed (SCRAM-SHA-512)"
        );
        let err = connect_error("Connection refused (os error 61)", Some("PLAIN"));
        assert_eq!(
            err.to_string(),
            "kafka: cannot connect: Connection refused (os error 61)"
        );
        let err = connect_error(text, None);
        assert!(
            err.to_string().starts_with("kafka: cannot connect"),
            "{err}"
        );
    }
}
