//! Kafka, MQTT and AMQP clients for Rustman (add-messaging-client): one
//! produce or consume per request file, pure-Rust clients over rustls,
//! bounded memory for consumed messages, and a redacted exchange record
//! for output and history.
//!
//! The request-file model and its interpolation live in `rustman-core`
//! ([`rustman_core::model::MessagingRequest`],
//! [`rustman_core::vars::resolve_messaging`]).

pub mod adhoc;
pub mod amqp;
pub mod asyncapi;
pub mod endpoint;
mod error;
mod exchange;
pub mod kafka;
pub mod mqtt;
mod payload;
pub mod registry;
mod runner;
pub mod safety;
pub mod tls;

use std::time::{Duration, Instant};

use rustman_core::model::{MessagingKind, MessagingLimits};
use rustman_core::vars::ResolvedMessaging;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant as Deadline;
use tokio_util::sync::CancellationToken;

pub use error::MessagingError;
pub use exchange::{ConsumedMessage, MessagingExchange, ProducedMessage, SecuritySummary};
pub use payload::{EncodedMessage, Header, compact_json, cut, encode, encode_message, render};
pub use runner::MessagingRunner;

/// How long connecting to a broker may take.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// When a headless consume stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub max_messages: u64,
    pub duration: Duration,
}

impl Bounds {
    /// The bounds `limits` set, with their defaults.
    pub fn of(limits: &MessagingLimits) -> Self {
        Self {
            max_messages: u64::from(limits.effective_max_messages()),
            duration: Duration::from_millis(limits.effective_duration_ms()),
        }
    }
}

/// How one operation runs.
#[derive(Debug)]
pub struct RunOptions {
    pub connect_timeout: Duration,
    /// Headless bounds; `None` consumes until `cancel` fires.
    pub bounds: Option<Bounds>,
    pub cancel: CancellationToken,
    /// Fired once a consume is subscribed (offsets resolved, SUBACK,
    /// consume-ok), so a following step cannot race it.
    pub ready: Option<oneshot::Sender<()>>,
    /// Gets every consumed message as it is recorded (not redacted).
    pub feed: Option<mpsc::UnboundedSender<ConsumedMessage>>,
}

impl RunOptions {
    /// Headless options for `resolved`: its own limits.
    pub fn headless(resolved: &ResolvedMessaging) -> Self {
        Self {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            bounds: Some(Bounds::of(&resolved.limits)),
            cancel: CancellationToken::new(),
            ready: None,
            feed: None,
        }
    }
}

/// Run the produce or consume of `resolved`. Error messages are redacted.
pub async fn run(
    resolved: &ResolvedMessaging,
    options: RunOptions,
) -> Result<MessagingExchange, MessagingError> {
    let started = Instant::now();
    let redactor = resolved.redactor();
    preflight(resolved)
        .await
        .map_err(|e| e.redacted(&redactor))?;
    let validator = schema_validator(resolved)
        .await
        .map_err(|e| e.redacted(&redactor))?;
    let result = match resolved.kind {
        MessagingKind::Kafka => kafka::run(resolved, options).await,
        MessagingKind::Mqtt => mqtt::run(resolved, options).await,
        MessagingKind::Amqp => amqp::run(resolved, options).await,
    };
    let mut exchange = result.map_err(|e| e.redacted(&redactor))?;
    if let Some(validator) = &validator {
        exchange.asyncapi = Some(validator.message.clone());
        asyncapi::consumed_violations(validator, &mut exchange.messages);
    }
    exchange.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(exchange)
}

/// Input problems found before anything connects: a Kafka offset
/// timestamp that does not parse and payload files that cannot be read.
async fn preflight(resolved: &ResolvedMessaging) -> Result<(), MessagingError> {
    if let rustman_core::model::StartOffset::Timestamp(at) = &resolved.offset {
        kafka::parse_timestamp(at)?;
    }
    if resolved.operation == rustman_core::model::Operation::Produce {
        for message in &resolved.messages {
            payload::check_files(message).await?;
        }
    }
    Ok(())
}

/// The payload validator of the request's `asyncapi` message; a produce
/// is checked here, before anything is sent.
async fn schema_validator(
    resolved: &ResolvedMessaging,
) -> Result<Option<asyncapi::Validator>, MessagingError> {
    let Some(reference) = &resolved.asyncapi else {
        return Ok(None);
    };
    let invalid = |e: asyncapi::AsyncApiError| MessagingError::Invalid(e.to_string());
    let document = asyncapi::load_file(&reference.document.path)
        .await
        .map_err(invalid)?;
    let Some(validator) =
        asyncapi::Validator::new(&document, &reference.message).map_err(invalid)?
    else {
        return Ok(None);
    };
    if resolved.operation == rustman_core::model::Operation::Produce {
        let mut values = Vec::new();
        for message in &resolved.messages {
            values.push(encode_message(message).await?.value);
        }
        asyncapi::validate_produce(&validator, &values)?;
    }
    Ok(Some(validator))
}

/// The output of `work`, or `None` when the run is cancelled or the
/// deadline passes first (slow Schema Registry fetches never stretch a
/// consume past its duration).
pub(crate) async fn until_stopped<F: std::future::Future>(
    cancel: &CancellationToken,
    deadline: Option<Deadline>,
    work: F,
) -> Option<F::Output> {
    tokio::select! {
        () = cancel.cancelled() => None,
        () = sleep_until(deadline) => None,
        output = work => Some(output),
    }
}

/// Sleep until `deadline`, or forever without one.
pub(crate) async fn sleep_until(deadline: Option<Deadline>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn slow_work_stops_at_the_deadline_or_on_cancel() {
        let cancel = CancellationToken::new();
        let slow = tokio::time::sleep(Duration::from_secs(60));
        let deadline = Deadline::now() + Duration::from_millis(100);
        let started = Deadline::now();
        assert_eq!(until_stopped(&cancel, Some(deadline), slow).await, None);
        assert!(started.elapsed() < Duration::from_secs(30));
        assert_eq!(until_stopped(&cancel, None, async { 7 }).await, Some(7));
        cancel.cancel();
        let pending = std::future::pending::<()>();
        assert_eq!(until_stopped(&cancel, None, pending).await, None);
    }
}
